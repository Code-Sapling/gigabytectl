//! User configuration and saved hardware profiles.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{ppd, sysfs, system};

/// System-wide config directory, used as a fallback when reading profiles so the
/// sync service (which runs as root under systemd with `HOME=/root` and no
/// `SUDO_USER`) can find profiles that don't live in any user's home.
pub const SYSTEM_CONFIG_DIR: &str = "/etc/gigabytectl";

const PROFILES_FILE: &str = "profiles.toml";
const CONFIG_FILE: &str = "config.toml";
/// The TUI redraws between refreshes, so very small intervals only add load.
const MIN_REFRESH_INTERVAL: Duration = Duration::from_millis(100);

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// TUI auto-refresh interval in milliseconds (also the default for `monitor`).
    pub refresh_interval_ms: u64,
    /// Temperature unit used for display.
    pub units: Units,
    /// Number of samples kept in the TUI history graph.
    pub history_length: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            refresh_interval_ms: 1000,
            units: Units::Celsius,
            history_length: 120,
        }
    }
}

impl Config {
    /// Loads the config, falling back to defaults if it is missing or invalid.
    pub fn load() -> Self {
        let path = config_dir().join(CONFIG_FILE);
        let Ok(text) = fs::read_to_string(&path) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_else(|e| {
            eprintln!("Warning: failed to parse {}: {e}. Using defaults.", path.display());
            Self::default()
        })
    }

    pub fn refresh_interval(&self) -> Duration {
        Duration::from_millis(self.refresh_interval_ms).max(MIN_REFRESH_INTERVAL)
    }
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
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("gigabytectl");
    }
    let home = system::sudo_user()
        .and_then(|user| system::home_of_user(&user))
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| "/root".to_string());
    PathBuf::from(home).join(".config").join("gigabytectl")
}

/// Path profiles are written to.
pub fn profiles_path() -> PathBuf {
    config_dir().join(PROFILES_FILE)
}

/// Ordered directories searched for profiles. The invoking user's config dir
/// wins; the system-wide dir is the fallback that makes the root-run sync
/// service work.
fn profiles_search_dirs() -> Vec<PathBuf> {
    let user = config_dir();
    if user == Path::new(SYSTEM_CONFIG_DIR) {
        vec![user]
    } else {
        vec![user, PathBuf::from(SYSTEM_CONFIG_DIR)]
    }
}

pub fn load_profiles() -> Result<HashMap<String, Profile>> {
    for dir in profiles_search_dirs() {
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
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    system::chown_to_sudo_user(&dir);
    Ok(())
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
