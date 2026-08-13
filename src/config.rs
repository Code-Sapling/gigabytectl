//! User configuration and saved hardware profiles.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{ppd, sysfs, system};

/// System-wide config directory, used as a fallback when reading profiles so the
/// sync service (which runs as root under systemd with `HOME=/root` and no
/// `SUDO_USER`) can find profiles that don't live in any user's home.
pub const SYSTEM_CONFIG_DIR: &str = "/etc/gigabytectl";

const PROFILES_FILE: &str = "profiles.toml";
const CONFIG_FILE: &str = "config.toml";
const UPDATE_CHECK_FILE: &str = "update-check.json";
/// The TUI redraws between refreshes, so very small intervals only add load.
const MIN_REFRESH_INTERVAL: Duration = Duration::from_millis(100);
/// Temperatures move slowly, and the sync service polls them for the whole
/// uptime of the machine, so there is a floor on how often it may sample.
const MIN_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Units {
    #[default]
    Celsius,
    Fahrenheit,
}

impl Units {
    pub fn convert(self, celsius: f64) -> f64 {
        match self {
            Self::Celsius => celsius,
            Self::Fahrenheit => celsius * 9.0 / 5.0 + 32.0,
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Celsius => "°C",
            Self::Fahrenheit => "°F",
        }
    }

    /// `"58°C"`, or `"N/A"` when the sensor is unavailable.
    pub fn format(self, celsius: Option<f32>) -> String {
        match celsius {
            Some(c) => format!("{:.0}{}", self.convert(c.into()), self.symbol()),
            None => "N/A".to_string(),
        }
    }

    /// The value a machine-readable consumer should see: converted and rounded
    /// to one decimal place.
    pub fn to_json(self, celsius: Option<f32>) -> Option<f64> {
        celsius.map(|c| (self.convert(c.into()) * 10.0).round() / 10.0)
    }
}

/// Desktop notifications for temperature thresholds. Off unless switched on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notifications {
    /// Opt in to temperature alerts.
    pub enabled: bool,
    /// CPU threshold in degrees Celsius, whatever `units` is set to display.
    pub cpu_temp: f32,
    /// GPU threshold in degrees Celsius.
    pub gpu_temp: f32,
    /// Minimum gap between alerts for the same sensor.
    pub cooldown_secs: u64,
    /// How often the sync service samples temperatures. Only used there: the
    /// TUI and `monitor` alert on the samples they already take.
    pub poll_interval_secs: u64,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            enabled: false,
            cpu_temp: 90.0,
            gpu_temp: 90.0,
            cooldown_secs: 300,
            poll_interval_secs: 10,
        }
    }
}

impl Notifications {
    /// How often the sync service reads the sensors, floored so a mistyped
    /// value cannot turn the service into a busy loop.
    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.poll_interval_secs).max(MIN_POLL_INTERVAL)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// TUI auto-refresh interval in milliseconds (also the default for `monitor`).
    pub refresh_interval_ms: u64,
    /// Temperature unit used for display.
    pub units: Units,
    /// Number of samples kept in the TUI history graph.
    pub history_length: usize,
    /// Whether the TUI checks GitHub for a newer release in the background.
    pub update_check: bool,
    /// Temperature alerts (disabled by default).
    pub notifications: Notifications,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            refresh_interval_ms: 1000,
            units: Units::Celsius,
            history_length: 120,
            update_check: true,
            notifications: Notifications::default(),
        }
    }
}

impl Config {
    /// Every key `config get`/`config set` understands.
    pub const KEYS: [&'static str; 9] = [
        "refresh_interval_ms",
        "units",
        "history_length",
        "update_check",
        "notifications.enabled",
        "notifications.cpu_temp",
        "notifications.gpu_temp",
        "notifications.cooldown_secs",
        "notifications.poll_interval_secs",
    ];

    /// Loads the config from the user's directory, falling back to the
    /// system-wide copy and then to defaults. A file that fails to parse is
    /// reported rather than silently ignored.
    pub fn load() -> Self {
        for path in config_search_dirs().into_iter().map(|dir| dir.join(CONFIG_FILE)) {
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            return toml::from_str(&text).unwrap_or_else(|e| {
                eprintln!("Warning: failed to parse {}: {e}. Using defaults.", path.display());
                Self::default()
            });
        }
        Self::default()
    }

    pub fn refresh_interval(&self) -> Duration {
        Duration::from_millis(self.refresh_interval_ms).max(MIN_REFRESH_INTERVAL)
    }

    /// Reads one key by its dotted name.
    pub fn get(&self, key: &str) -> Result<String> {
        Ok(match key {
            "refresh_interval_ms" => self.refresh_interval_ms.to_string(),
            "units" => match self.units {
                Units::Celsius => "celsius".to_string(),
                Units::Fahrenheit => "fahrenheit".to_string(),
            },
            "history_length" => self.history_length.to_string(),
            "update_check" => self.update_check.to_string(),
            "notifications.enabled" => self.notifications.enabled.to_string(),
            "notifications.cpu_temp" => self.notifications.cpu_temp.to_string(),
            "notifications.gpu_temp" => self.notifications.gpu_temp.to_string(),
            "notifications.cooldown_secs" => self.notifications.cooldown_secs.to_string(),
            "notifications.poll_interval_secs" => self.notifications.poll_interval_secs.to_string(),
            other => bail!("Unknown config key '{other}' (known keys: {})", Self::KEYS.join(", ")),
        })
    }

    /// Writes one key by its dotted name, parsing `value` for that key's type.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let value = value.trim();
        match key {
            "refresh_interval_ms" => self.refresh_interval_ms = parse_value(key, value)?,
            "units" => {
                self.units = match value.to_ascii_lowercase().as_str() {
                    "celsius" | "c" => Units::Celsius,
                    "fahrenheit" | "f" => Units::Fahrenheit,
                    other => bail!("units must be celsius or fahrenheit, got '{other}'"),
                }
            }
            "history_length" => self.history_length = parse_value(key, value)?,
            "update_check" => self.update_check = parse_bool(key, value)?,
            "notifications.enabled" => self.notifications.enabled = parse_bool(key, value)?,
            "notifications.cpu_temp" => self.notifications.cpu_temp = parse_value(key, value)?,
            "notifications.gpu_temp" => self.notifications.gpu_temp = parse_value(key, value)?,
            "notifications.cooldown_secs" => self.notifications.cooldown_secs = parse_value(key, value)?,
            "notifications.poll_interval_secs" => self.notifications.poll_interval_secs = parse_value(key, value)?,
            other => bail!("Unknown config key '{other}' (known keys: {})", Self::KEYS.join(", ")),
        }
        Ok(())
    }
}

fn parse_value<T: std::str::FromStr>(key: &str, value: &str) -> Result<T> {
    value
        .parse()
        .map_err(|_| anyhow!("'{value}' is not a valid value for {key}"))
}

fn parse_bool(key: &str, value: &str) -> Result<bool> {
    Ok(match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => true,
        "false" | "no" | "off" | "0" => false,
        other => bail!("{key} must be true or false, got '{other}'"),
    })
}

/// Loads one specific config file, falling back to defaults when it is missing
/// or unreadable. Used when editing a file rather than the merged view.
pub fn load_config_file(path: &Path) -> Config {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| match toml::from_str(&text) {
            Ok(config) => Some(config),
            Err(e) => {
                eprintln!("Warning: failed to parse {}: {e}. Starting from defaults.", path.display());
                None
            }
        })
        .unwrap_or_default()
}

/// Writes the config to the user's directory, or the system-wide one.
pub fn save_config(config: &Config, system: bool) -> Result<PathBuf> {
    let dir = if system {
        PathBuf::from(SYSTEM_CONFIG_DIR)
    } else {
        config_dir()
    };
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(CONFIG_FILE);
    let text = toml::to_string_pretty(config).context("serializing config")?;
    fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
    if !system {
        system::chown_to_sudo_user(&dir);
        // Settings the sync service acts on (notifications) live in the same
        // file, so keep the copy it reads in step with the user's.
        if let Err(e) = sync_config_to_system(&text) {
            eprintln!("Warning: could not update {}: {e:#}", config_path(true).display());
        }
    }
    Ok(path)
}

/// Mirrors the user's config into the system-wide copy the root-run sync
/// service reads. Only updates a copy that already exists, since creating one
/// implies installing system state — that is `install-service`'s job. Returns
/// whether it wrote.
pub fn sync_config_to_system(text: &str) -> Result<bool> {
    let path = config_path(true);
    if !path.exists() || config_dir() == Path::new(SYSTEM_CONFIG_DIR) {
        return Ok(false);
    }
    // Without root this is expected to fail; say so rather than looking broken.
    ensure!(system::is_root(), "{} needs root to update (re-run with sudo)", path.display());
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

/// Copies the current user config over the system-wide copy, creating it if
/// need be, so the sync service sees the same settings. Used by
/// `install-service`.
pub fn seed_system_config() -> Result<PathBuf> {
    let dir = Path::new(SYSTEM_CONFIG_DIR);
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let text = toml::to_string_pretty(&Config::load()).context("serializing config")?;
    let path = config_path(true);
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Ordered directories searched for `config.toml`, mirroring how profiles are
/// resolved so a root-run service sees the same settings.
fn config_search_dirs() -> Vec<PathBuf> {
    search_dirs()
}

pub fn config_path(system: bool) -> PathBuf {
    if system {
        PathBuf::from(SYSTEM_CONFIG_DIR)
    } else {
        config_dir()
    }
    .join(CONFIG_FILE)
}

/// A saved snapshot of controllable values. All fields are optional so a profile
/// can set only what it cares about.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fan_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fan_custom_speed: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charge_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charge_limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_boost: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fan_curve: Option<Vec<[i32; 2]>>,
    /// power-profiles-daemon profile this maps to (e.g. "performance",
    /// "balanced", "power-saver"). Used for two-way sync with PPD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ppd_profile: Option<String>,
}

impl Profile {
    /// Captures the current hardware state. Values the driver does not report
    /// are left unset rather than saved as placeholders, so the profile stays
    /// applicable.
    pub fn from_hardware() -> Self {
        let hw = sysfs::HwState::read();
        Self {
            fan_mode: saved_name(&sysfs::FAN_MODES, hw.fan_mode),
            fan_custom_speed: hw.fan_custom_speed,
            charge_mode: saved_name(&sysfs::CHARGE_MODES, hw.charge_mode),
            charge_limit: hw.charge_limit,
            gpu_boost: hw.gpu_boost.map(|v| if v == 0 { "off" } else { "on" }.to_string()),
            fan_curve: sysfs::read_fan_curve()
                .ok()
                .map(|curve| curve.into_iter().map(|(t, s)| [t, s]).collect()),
            ppd_profile: ppd::get().ok(),
        }
    }

    /// Applies the profile to the hardware, validating every field first so a
    /// bad entry cannot leave the machine half-configured.
    pub fn apply(&self) -> Result<()> {
        let fan_mode = self.fan_mode.as_deref().map(sysfs::fan_mode_value).transpose()?;
        let charge_mode = self.charge_mode.as_deref().map(sysfs::charge_mode_value).transpose()?;
        let gpu_boost = self.gpu_boost.as_deref().map(sysfs::on_off_value).transpose()?;
        if let Some(speed) = self.fan_custom_speed {
            sysfs::validate_fan_speed(speed)?;
        }
        if let Some(limit) = self.charge_limit {
            sysfs::validate_charge_limit(limit)?;
        }
        if let Some(curve) = &self.fan_curve {
            ensure!(
                curve.len() == sysfs::FAN_CURVE_POINTS,
                "Fan curve must have {} points, got {}",
                sysfs::FAN_CURVE_POINTS,
                curve.len()
            );
            let points: Vec<(i32, i32)> = curve.iter().map(|point| (point[0], point[1])).collect();
            sysfs::validate_fan_curve(&points)?;
        }

        for (node, value) in [
            (sysfs::FAN_MODE, fan_mode),
            (sysfs::FAN_CUSTOM_SPEED, self.fan_custom_speed),
            (sysfs::CHARGE_MODE, charge_mode),
            (sysfs::CHARGE_LIMIT, self.charge_limit),
            (sysfs::GPU_BOOST, gpu_boost),
        ] {
            if let Some(value) = value {
                sysfs::write_value(node, value)?;
            }
        }

        for (index, point) in self.fan_curve.iter().flatten().enumerate() {
            sysfs::write_fan_curve_point(index, point[0], point[1])?;
        }
        Ok(())
    }
}

/// The lowercase name for a node value, or `None` if the driver reported
/// nothing or something outside the known set.
fn saved_name(table: &[&'static str], value: Option<i32>) -> Option<String> {
    let index = usize::try_from(value?).ok()?;
    table.get(index).map(|name| name.to_lowercase())
}

/// Resolves the config directory, accounting for being run under `sudo`.
/// Prefers the invoking user's home (via `$SUDO_USER`) so config lives in the
/// real user's `~/.config`, not root's.
pub fn config_dir() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config")
}

/// Where cached state that is not configuration lives — currently only the
/// result of the update check. Resolved like [`config_dir`], so the TUI running
/// under `sudo` caches in the invoking user's home rather than root's.
pub fn cache_dir() -> PathBuf {
    xdg_dir("XDG_CACHE_HOME", ".cache")
}

pub fn cache_path() -> PathBuf {
    cache_dir().join(UPDATE_CHECK_FILE)
}

/// Our directory under an XDG base directory, honouring the environment
/// variable and falling back to `$HOME/<fallback>`.
fn xdg_dir(variable: &str, fallback: &str) -> PathBuf {
    if let Ok(xdg) = std::env::var(variable)
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("gigabytectl");
    }
    let home = system::sudo_user()
        .and_then(|user| system::home_of_user(&user))
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| "/root".to_string());
    PathBuf::from(home).join(fallback).join("gigabytectl")
}

/// Path profiles are written to.
pub fn profiles_path() -> PathBuf {
    config_dir().join(PROFILES_FILE)
}

/// Ordered directories searched for configuration. The invoking user's config
/// dir wins; the system-wide dir is the fallback that makes the root-run sync
/// service work.
fn search_dirs() -> Vec<PathBuf> {
    let user = config_dir();
    if user == Path::new(SYSTEM_CONFIG_DIR) {
        vec![user]
    } else {
        vec![user, PathBuf::from(SYSTEM_CONFIG_DIR)]
    }
}

pub fn system_profiles_path() -> PathBuf {
    Path::new(SYSTEM_CONFIG_DIR).join(PROFILES_FILE)
}

pub fn load_profiles() -> Result<HashMap<String, Profile>> {
    for dir in search_dirs() {
        let path = dir.join(PROFILES_FILE);
        if let Ok(text) = fs::read_to_string(&path) {
            return toml::from_str(&text).with_context(|| format!("parsing {}", path.display()));
        }
    }
    Ok(HashMap::new())
}

pub fn save_profiles(profiles: &HashMap<String, Profile>) -> Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(PROFILES_FILE);
    let text = toml::to_string_pretty(profiles).context("serializing profiles")?;
    fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
    system::chown_to_sudo_user(&dir);

    // Keep the copy the root-run sync service reads in step, so edits do not
    // silently apply only to the interactive tool.
    if let Err(e) = sync_profiles_to_system(&text) {
        eprintln!("Warning: could not update {}: {e:#}", system_profiles_path().display());
    }
    Ok(())
}

/// Mirrors the user's profiles into the system-wide copy.
///
/// Only updates a copy that already exists: creating one implies installing
/// system state, which is `install-service`'s job. Returns whether it wrote.
pub fn sync_profiles_to_system(text: &str) -> Result<bool> {
    let path = system_profiles_path();
    if !path.exists() || config_dir() == Path::new(SYSTEM_CONFIG_DIR) {
        return Ok(false);
    }
    // Without root this is expected to fail; say so rather than looking broken.
    ensure!(
        system::is_root(),
        "{} needs root to update (re-run with sudo, or `sudo gigabytectl profile --sync-system`)",
        path.display()
    );
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

/// Copies the current user profiles over the system-wide copy, creating it if
/// need be. Used by `profile --sync-system` and `install-service`.
pub fn seed_system_profiles() -> Result<PathBuf> {
    let dir = Path::new(SYSTEM_CONFIG_DIR);
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let profiles = load_profiles()?;
    let text = toml::to_string_pretty(&profiles).context("serializing profiles")?;
    let path = system_profiles_path();
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fahrenheit_conversion_matches_the_formula() {
        assert_eq!(Units::Celsius.convert(50.0), 50.0);
        assert_eq!(Units::Fahrenheit.convert(0.0), 32.0);
        assert_eq!(Units::Fahrenheit.convert(100.0), 212.0);
        assert_eq!(Units::Celsius.format(Some(58.4)), "58°C");
        assert_eq!(Units::Fahrenheit.format(Some(100.0)), "212°F");
        assert_eq!(Units::Celsius.format(None), "N/A");
        assert_eq!(Units::Celsius.to_json(Some(58.46)), Some(58.5));
        assert_eq!(Units::Celsius.to_json(None), None);
    }

    #[test]
    fn config_defaults_apply_to_missing_fields() {
        let cfg: Config = toml::from_str("units = \"fahrenheit\"").unwrap();
        assert_eq!(cfg.units, Units::Fahrenheit);
        assert_eq!(cfg.refresh_interval_ms, Config::default().refresh_interval_ms);
    }

    #[test]
    fn refresh_interval_has_a_floor() {
        let cfg = Config { refresh_interval_ms: 0, ..Config::default() };
        assert_eq!(cfg.refresh_interval(), MIN_REFRESH_INTERVAL);
        let cfg = Config { refresh_interval_ms: 2000, ..Config::default() };
        assert_eq!(cfg.refresh_interval(), Duration::from_secs(2));
    }

    #[test]
    fn every_documented_key_round_trips_through_get_and_set() {
        let mut config = Config::default();
        for key in Config::KEYS {
            let value = config.get(key).expect("a documented key must be readable");
            config
                .set(key, &value)
                .expect("what get prints must be what set accepts");
            assert_eq!(config.get(key).unwrap(), value, "{key} changed on a round trip");
        }
        assert_eq!(config, Config::default());
        assert!(config.get("notifications.nope").is_err());
        assert!(config.set("notifications.nope", "1").is_err());
    }

    #[test]
    fn the_service_poll_interval_has_a_floor() {
        let notifications = Notifications { poll_interval_secs: 0, ..Notifications::default() };
        assert_eq!(notifications.poll_interval(), MIN_POLL_INTERVAL);
        let notifications = Notifications { poll_interval_secs: 30, ..Notifications::default() };
        assert_eq!(notifications.poll_interval(), Duration::from_secs(30));
    }

    #[test]
    fn unreadable_or_unknown_values_are_not_saved_into_profiles() {
        assert_eq!(saved_name(&sysfs::FAN_MODES, Some(2)), Some("gaming".to_string()));
        assert_eq!(saved_name(&sysfs::FAN_MODES, Some(99)), None);
        assert_eq!(saved_name(&sysfs::FAN_MODES, Some(-1)), None);
        assert_eq!(saved_name(&sysfs::FAN_MODES, None), None);
    }

    #[test]
    fn profiles_round_trip_through_toml_omitting_unset_fields() {
        let profile = Profile {
            fan_mode: Some("gaming".to_string()),
            ppd_profile: Some("performance".to_string()),
            ..Profile::default()
        };
        let profiles = HashMap::from([("gaming".to_string(), profile.clone())]);
        let text = toml::to_string_pretty(&profiles).unwrap();
        assert!(!text.contains("charge_limit"));
        let parsed: HashMap<String, Profile> = toml::from_str(&text).unwrap();
        assert_eq!(parsed["gaming"], profile);
    }

    #[test]
    fn applying_a_profile_rejects_bad_values_before_touching_hardware() {
        let bad_mode = Profile { fan_mode: Some("turbo".into()), ..Profile::default() };
        assert!(bad_mode.apply().unwrap_err().to_string().contains("Unknown fan mode"));

        let bad_speed = Profile { fan_custom_speed: Some(999), ..Profile::default() };
        assert!(bad_speed.apply().unwrap_err().to_string().contains("Fan speed"));

        let short_curve = Profile { fan_curve: Some(vec![[30, 40]]), ..Profile::default() };
        assert!(short_curve.apply().unwrap_err().to_string().contains("15 points"));
    }
}
