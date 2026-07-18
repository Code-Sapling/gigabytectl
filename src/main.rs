// src/main.rs

use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{self, Stdout},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
    process::Command
};

use anyhow::{Context, Result};
use clap::{CommandFactory, Args, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use serde::{Deserialize, Serialize};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Chart, Clear, Dataset, GraphType, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
    symbols
};

const ROOT: &str = "/sys/devices/platform/aorus_laptop";
const FAN_MODE: &str = "/sys/devices/platform/aorus_laptop/fan_mode";
const FAN_CUSTOM_SPEED: &str = "/sys/devices/platform/aorus_laptop/fan_custom_speed";
const CHARGE_MODE: &str = "/sys/devices/platform/aorus_laptop/charge_mode";
const CHARGE_LIMIT: &str = "/sys/devices/platform/aorus_laptop/charge_limit";
const GPU_BOOST: &str = "/sys/devices/platform/aorus_laptop/gpu_boost";
const BATTERY_CYCLE: &str = "/sys/devices/platform/aorus_laptop/battery_cycle";
const FAN_CURVE_INDEX: &str = "/sys/devices/platform/aorus_laptop/fan_curve_index";
const FAN_CURVE_DATA: &str = "/sys/devices/platform/aorus_laptop/fan_curve_data";
const LIGHT_SENSOR: &str = "/sys/devices/platform/aorus_laptop/light_sensor";
const FAN_PWM: &str = "/sys/devices/platform/aorus_laptop/fan_pwm";

const FAN_MODES: [&str; 6] = ["Normal", "Silent", "Gaming", "Custom", "Auto", "Fixed"];
const FAN_MODE_COUNT: i32 = FAN_MODES.len() as i32;
const CHARGE_MODE_COUNT: i32 = 2;
const FAN_CURVE_POINTS: usize = 15;

// --- Hardware Monitor Structs ---

#[derive(Debug, Clone)]
pub struct Fan {
    pub name: String,
    pub rpm: u32,
}

#[derive(Debug)]
pub struct GigabyteHwmon {
    hwmon_path: PathBuf,
}

impl GigabyteHwmon {
    pub fn new() -> Option<Self> {
        let base = "/sys/class/hwmon";

        if let Ok(entries) = fs::read_dir(base) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name_file = path.join("name");

                if let Ok(content) = fs::read_to_string(&name_file)
                    && content.trim() == "aorus_laptop"
                {
                    return Some(Self { hwmon_path: path });
                }
            }
        }
        None
    }

    pub fn read_fans(&self) -> Vec<Fan> {
        let mut fans = Vec::new();

        let entries = match fs::read_dir(&self.hwmon_path) {
            Ok(e) => e,
            Err(_) => return fans,
        };

        for entry in entries.flatten() {
            let path = entry.path();

            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };

            if file_name.starts_with("fan") && file_name.ends_with("_input") {
                let Ok(content) = fs::read_to_string(&path) else {
                    continue;
                };

                let Ok(rpm) = content.trim().parse::<u32>() else {
                    continue;
                };

                if rpm == 0 {
                    continue;
                }

                let num_part = &file_name[3..file_name.len() - 6];
                let display_name = format!("Fan {}", num_part);

                fans.push(Fan {
                    name: display_name,
                    rpm,
                });
            }
        }

        fans
    }
}

// --- Temperature Sensors ---

fn read_i32_at(path: &Path) -> Option<i32> {
    fs::read_to_string(path).ok().and_then(|s| s.trim().parse::<i32>().ok())
}

/// Reads `temp1_input` (millidegrees C) from the first `/sys/class/hwmon` device
/// whose `name` matches one of `names`, returning degrees Celsius.
fn hwmon_temp(names: &[&str]) -> Option<f32> {
    let base = "/sys/class/hwmon";
    for entry in fs::read_dir(base).ok()?.flatten() {
        let path = entry.path();
        let Ok(name) = fs::read_to_string(path.join("name")) else {
            continue;
        };
        if names.contains(&name.trim())
            && let Some(milli) = read_i32_at(&path.join("temp1_input"))
        {
            return Some(milli as f32 / 1000.0);
        }
    }
    None
}

/// Best-effort GPU temperature via the NVIDIA proprietary driver.
fn nvidia_temp() -> Option<f32> {
    let output = Command::new("nvidia-smi")
        .args(["--query-gpu=temperature.gpu", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .and_then(|l| l.trim().parse::<f32>().ok())
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Temps {
    pub cpu: Option<f32>,
    pub gpu: Option<f32>,
}

impl Temps {
    fn read() -> Self {
        let cpu = hwmon_temp(&["coretemp", "k10temp", "zenpower"]);
        let gpu = hwmon_temp(&["amdgpu", "nouveau"]).or_else(nvidia_temp);
        Self { cpu, gpu }
    }
}

// --- Config & Profiles ---

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Units {
    #[default]
    Celsius,
    Fahrenheit,
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
    fn load() -> Self {
        let path = config_dir().join("config.toml");
        match fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!("Warning: failed to parse {}: {}. Using defaults.", path.display(), e);
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        }
    }
}

/// A saved snapshot of controllable values. All fields optional so a profile can
/// set only what it cares about.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

fn fan_mode_value(name: &str) -> Result<i32> {
    match name.to_ascii_lowercase().as_str() {
        "normal" => Ok(0),
        "silent" => Ok(1),
        "gaming" => Ok(2),
        "custom" => Ok(3),
        "auto" => Ok(4),
        "fixed" => Ok(5),
        other => Err(anyhow::anyhow!("Unknown fan mode '{}'", other)),
    }
}

fn charge_mode_value(name: &str) -> Result<i32> {
    match name.to_ascii_lowercase().as_str() {
        "normal" => Ok(0),
        "custom" => Ok(1),
        other => Err(anyhow::anyhow!("Unknown charge mode '{}'", other)),
    }
}

fn on_off_value(name: &str) -> Result<i32> {
    match name.to_ascii_lowercase().as_str() {
        "on" | "1" | "true" => Ok(1),
        "off" | "0" | "false" => Ok(0),
        other => Err(anyhow::anyhow!("Expected on/off, got '{}'", other)),
    }
}

/// Resolves the config directory, accounting for being run under `sudo`.
/// Prefers the invoking user's home (via `$SUDO_USER`) so config lives in the
/// real user's `~/.config`, not root's.
fn config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("gigabytectl");
    }
    let home = std::env::var("SUDO_USER")
        .ok()
        .filter(|u| !u.is_empty())
        .and_then(|user| home_of_user(&user))
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| "/root".to_string());
    PathBuf::from(home).join(".config").join("gigabytectl")
}

/// System-wide config directory. Used as a fallback when reading profiles so the
/// sync service (which runs as root under systemd with `HOME=/root` and no
/// `SUDO_USER`) can find profiles that don't live in any user's home.
const SYSTEM_CONFIG_DIR: &str = "/etc/gigabytectl";

/// Ordered directories to search for `profiles.toml`. The invoking user's config
/// dir wins; the system-wide dir is the fallback that makes the root-run sync
/// service work.
fn profiles_search_dirs() -> Vec<PathBuf> {
    let user = config_dir();
    let system = PathBuf::from(SYSTEM_CONFIG_DIR);
    if user == system {
        vec![user]
    } else {
        vec![user, system]
    }
}

/// Looks up a user's home directory from the passwd database.
fn home_of_user(user: &str) -> Option<String> {
    let output = Command::new("getent").args(["passwd", user]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout);
    line.split(':').nth(5).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn load_profiles() -> Result<HashMap<String, Profile>> {
    for dir in profiles_search_dirs() {
        let path = dir.join("profiles.toml");
        match fs::read_to_string(&path) {
            Ok(text) => return toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
            Err(_) => continue,
        }
    }
    Ok(HashMap::new())
}

fn save_profiles(profiles: &HashMap<String, Profile>) -> Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("profiles.toml");
    let text = toml::to_string_pretty(profiles).context("serializing profiles")?;
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    chown_to_sudo_user(&dir);
    Ok(())
}

/// When run under sudo, hand ownership of freshly-written config back to the
/// invoking user so they can edit it without root. Best-effort.
fn chown_to_sudo_user(dir: &Path) {
    if let Ok(user) = std::env::var("SUDO_USER")
        && !user.is_empty()
    {
        let _ = Command::new("chown")
            .arg("-R")
            .arg(format!("{}:", user))
            .arg(dir)
            .status();
    }
}

/// Reads the current hardware state into a Profile (for `profile save`).
fn current_profile() -> Profile {
    Profile {
        fan_mode: Some(fan_mode_name(read_i32(FAN_MODE)).to_lowercase()),
        fan_custom_speed: read_i32(FAN_CUSTOM_SPEED),
        charge_mode: Some(charge_mode_name(read_i32(CHARGE_MODE)).to_lowercase()),
        charge_limit: read_i32(CHARGE_LIMIT),
        gpu_boost: read_i32(GPU_BOOST).map(|v| if v == 1 { "on".to_string() } else { "off".to_string() }),
        fan_curve: read_fan_curve().ok().map(|c| c.into_iter().map(|(t, s)| [t, s]).collect()),
        ppd_profile: ppd_get().ok(),
    }
}

/// Applies a profile's settings to the hardware, validating each field.
fn apply_profile(profile: &Profile) -> Result<()> {
    if let Some(mode) = &profile.fan_mode {
        write_value(FAN_MODE, fan_mode_value(mode)?)?;
    }
    if let Some(speed) = profile.fan_custom_speed {
        validate_fan_speed(speed)?;
        write_value(FAN_CUSTOM_SPEED, speed)?;
    }
    if let Some(mode) = &profile.charge_mode {
        write_value(CHARGE_MODE, charge_mode_value(mode)?)?;
    }
    if let Some(limit) = profile.charge_limit {
        validate_charge_limit(limit)?;
        write_value(CHARGE_LIMIT, limit)?;
    }
    if let Some(boost) = &profile.gpu_boost {
        write_value(GPU_BOOST, on_off_value(boost)?)?;
    }
    if let Some(curve) = &profile.fan_curve {
        anyhow::ensure!(curve.len() == FAN_CURVE_POINTS, "Fan curve must have {} points", FAN_CURVE_POINTS);
        for (idx, point) in curve.iter().enumerate() {
            validate_curve_temp(point[0])?;
            validate_curve_speed(point[1])?;
            write_fan_curve_point(idx, point[0], point[1])?;
        }
    }
    Ok(())
}

// --- power-profiles-daemon (PPD) integration ---

/// Candidate D-Bus (bus name, object path) pairs for power-profiles-daemon. The
/// interface name matches the bus name in both cases. Newer builds may live under
/// the UPower namespace, older ones under net.hadess, so we probe each in order.
const PPD_ENDPOINTS: [(&str, &str); 2] = [
    ("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles"),
    ("org.freedesktop.UPower.PowerProfiles", "/org/freedesktop/UPower/PowerProfiles"),
];

/// Returns the first PPD (bus name, object path) whose `ActiveProfile` can be
/// read, or `None` if power-profiles-daemon is not reachable on the system bus.
fn ppd_endpoint() -> Option<(&'static str, &'static str)> {
    for &(dest, path) in &PPD_ENDPOINTS {
        let ok = Command::new("busctl")
            .args(["--system", "get-property", dest, path, dest, "ActiveProfile"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some((dest, path));
        }
    }
    None
}

/// Parses a `busctl get-property` scalar-string line (`s "balanced"`) into its
/// inner value.
fn parse_busctl_string(out: &str) -> Option<String> {
    let rest = out.trim().strip_prefix("s ")?;
    Some(rest.trim().trim_matches('"').to_string())
}

/// Reads the currently-active PPD profile (e.g. "balanced").
fn ppd_get() -> Result<String> {
    let (dest, path) = ppd_endpoint().context("power-profiles-daemon is not available on the system bus")?;
    let out = Command::new("busctl")
        .args(["--system", "get-property", dest, path, dest, "ActiveProfile"])
        .output()
        .context("running busctl to read ActiveProfile")?;
    anyhow::ensure!(out.status.success(), "busctl get-property failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    let text = String::from_utf8_lossy(&out.stdout);
    parse_busctl_string(&text).with_context(|| format!("parsing PPD ActiveProfile from {:?}", text))
}

/// Sets the active PPD profile. Setting it to its current value is a no-op, so
/// this is safe to call from the sync daemon without causing feedback loops.
fn ppd_set(profile: &str) -> Result<()> {
    let (dest, path) = ppd_endpoint().context("power-profiles-daemon is not available on the system bus")?;
    let out = Command::new("busctl")
        .args(["--system", "set-property", dest, path, dest, "ActiveProfile", "s", profile])
        .output()
        .context("running busctl to set ActiveProfile")?;
    anyhow::ensure!(
        out.status.success(),
        "setting power profile '{}' failed: {}",
        profile,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// Finds the saved profile mapped to the given PPD profile. If several profiles
/// map to the same PPD profile, the alphabetically-first name wins so the choice
/// is deterministic.
fn profile_for_ppd<'a>(profiles: &'a HashMap<String, Profile>, ppd: &str) -> Option<(&'a String, &'a Profile)> {
    let mut matches: Vec<(&String, &Profile)> = profiles
        .iter()
        .filter(|(_, p)| p.ppd_profile.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(ppd)))
        .collect();
    matches.sort_by(|a, b| a.0.cmp(b.0));
    matches.into_iter().next()
}

/// Extracts the new profile name from a `gdbus monitor` PropertiesChanged line
/// of the form `... {'ActiveProfile': <'performance'>} ...`.
fn parse_active_profile_change(line: &str) -> Option<String> {
    let key = "'ActiveProfile': <'";
    let start = line.find(key)? + key.len();
    let rest = &line[start..];
    let end = rest.find('\'')?;
    Some(rest[..end].to_string())
}

/// Applies the gigabytectl profile mapped to the currently-active PPD profile
/// (hardware only — it deliberately does not touch PPD).
fn apply_ppd_mapping() -> Result<()> {
    let current = ppd_get()?;
    let profiles = load_profiles()?;
    match profile_for_ppd(&profiles, &current) {
        Some((name, profile)) => {
            apply_profile(profile)?;
            eprintln!("Power profile '{}' -> applied gigabytectl profile '{}'", current, name);
        }
        None => eprintln!("Power profile '{}' has no mapped gigabytectl profile; nothing to apply", current),
    }
    Ok(())
}

// --- History (rolling samples for the TUI graph) ---

struct History {
    start: Instant,
    cpu: VecDeque<(f64, f64)>,
    gpu: VecDeque<(f64, f64)>,
    rpm: VecDeque<(f64, f64)>,
    max_len: usize,
}

impl History {
    fn new(max_len: usize) -> Self {
        Self {
            start: Instant::now(),
            cpu: VecDeque::new(),
            gpu: VecDeque::new(),
            rpm: VecDeque::new(),
            max_len: max_len.max(2),
        }
    }

    fn push(&mut self, temps: Temps, fans: &[Fan]) {
        let t = self.start.elapsed().as_secs_f64();
        if let Some(c) = temps.cpu {
            push_capped(&mut self.cpu, (t, c as f64), self.max_len);
        }
        if let Some(g) = temps.gpu {
            push_capped(&mut self.gpu, (t, g as f64), self.max_len);
        }
        if let Some(max_rpm) = fans.iter().map(|f| f.rpm).max() {
            push_capped(&mut self.rpm, (t, max_rpm as f64), self.max_len);
        }
    }
}

fn push_capped(buf: &mut VecDeque<(f64, f64)>, point: (f64, f64), max_len: usize) {
    buf.push_back(point);
    while buf.len() > max_len {
        buf.pop_front();
    }
}

// --- App State ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    FanMode,
    FanCustomSpeed,
    ChargeMode,
    ChargeLimit,
    GpuBoost,
    FanCurveView,
    FanCurveEdit,
    History,
    Refresh,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditTarget {
    FanCustomSpeed,
    ChargeLimit,
    FanCurveTemp(usize),
    FanCurveSpeed(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Normal,
    Editing,
    FanCurveList,
}

struct App {
    items: &'static [Item],
    selected: usize,
    focus: Focus,
    status: String,
    input: String,
    editing: Option<EditTarget>,

    fan_mode: Option<i32>,
    fan_custom_speed: Option<i32>,
    charge_mode: Option<i32>,
    charge_limit: Option<i32>,
    gpu_boost: Option<i32>,
    battery_cycle: Option<String>,
    light_sensor: Option<String>,
    fan_pwm: Option<i32>,
    
    fan_curve: Option<Vec<(i32, i32)>>,
    fan_curve_selected: usize,
    fan_curve_col: usize, // 0 = Temp, 1 = Speed

    hwmon: Option<GigabyteHwmon>,
    live_fans: Vec<Fan>,
    temps: Temps,

    config: Config,
    history: History,

    last_refresh: Instant,
}

impl App {
    fn new(config: Config) -> Self {
        let history = History::new(config.history_length);
        Self {
            items: &[
                Item::FanMode,
                Item::FanCustomSpeed,
                Item::ChargeMode,
                Item::ChargeLimit,
                Item::GpuBoost,
                Item::FanCurveView,
                Item::FanCurveEdit,
                Item::History,
                Item::Refresh,
                Item::Quit,
            ],
            selected: 0,
            focus: Focus::Normal,
            status: format!("Ready. Managing nodes in {}", ROOT),
            input: String::new(),
            editing: None,
            fan_mode: None,
            fan_custom_speed: None,
            charge_mode: None,
            charge_limit: None,
            gpu_boost: None,
            battery_cycle: None,
            light_sensor: None,
            fan_pwm: None,
            fan_curve: None,
            fan_curve_selected: 0,
            fan_curve_col: 0,
            hwmon: GigabyteHwmon::new(),
            live_fans: Vec::new(),
            temps: Temps::default(),
            config,
            history,
            last_refresh: Instant::now(),
        }
    }

    fn refresh(&mut self) {
        self.fan_mode = read_i32(FAN_MODE);
        self.fan_custom_speed = read_i32(FAN_CUSTOM_SPEED);
        self.charge_mode = read_i32(CHARGE_MODE);
        self.charge_limit = read_i32(CHARGE_LIMIT);
        self.gpu_boost = read_i32(GPU_BOOST);
        self.battery_cycle = read_trimmed(BATTERY_CYCLE);
        self.light_sensor = read_trimmed(LIGHT_SENSOR);
        self.fan_pwm = read_i32(FAN_PWM);
        self.fan_curve = read_fan_curve().ok();

        if let Some(hwmon) = &self.hwmon {
            self.live_fans = hwmon.read_fans();
        }
        self.temps = Temps::read();
        self.history.push(self.temps, &self.live_fans);

        self.last_refresh = Instant::now();
    }

    fn selected_item(&self) -> Item {
        self.items[self.selected]
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.items.len() as isize;
        self.selected = ((self.selected as isize + delta).rem_euclid(len)) as usize;
    }

    fn set_status<S: Into<String>>(&mut self, msg: S) {
        self.status = msg.into();
    }

    fn start_edit(&mut self, target: EditTarget, seed: Option<i32>) {
        self.focus = Focus::Editing;
        self.editing = Some(target);
        self.input = seed.map(|v| v.to_string()).unwrap_or_default();
    }

    fn cancel_edit(&mut self) {
        if let Some(EditTarget::FanCurveTemp(_)) | Some(EditTarget::FanCurveSpeed(_)) = self.editing {
            self.focus = Focus::FanCurveList;
        } else {
            self.focus = Focus::Normal;
        }
        self.editing = None;
        self.input.clear();
    }

    fn push_input(&mut self, c: char) {
        if c.is_ascii_digit() {
            self.input.push(c);
        }
    }

    fn backspace_input(&mut self) {
        self.input.pop();
    }

    fn apply_edit(&mut self) {
        let Some(target) = self.editing else { return; };
        let value: i32 = match self.input.trim().parse() {
            Ok(v) => v,
            Err(_) => {
                self.set_status("Invalid number");
                return;
            }
        };

        let result: Result<()> = match target {
            EditTarget::FanCustomSpeed => validate_fan_speed(value).and_then(|()| write_value(FAN_CUSTOM_SPEED, value)),
            EditTarget::ChargeLimit => validate_charge_limit(value).and_then(|()| write_value(CHARGE_LIMIT, value)),
            EditTarget::FanCurveTemp(idx) => validate_curve_temp(value).and_then(|()| {
                let curve = self.fan_curve.as_ref().ok_or_else(|| anyhow::anyhow!("Curve not loaded"))?;
                write_fan_curve_point(idx, value, curve[idx].1)
            }),
            EditTarget::FanCurveSpeed(idx) => validate_curve_speed(value).and_then(|()| {
                let curve = self.fan_curve.as_ref().ok_or_else(|| anyhow::anyhow!("Curve not loaded"))?;
                write_fan_curve_point(idx, curve[idx].0, value)
            }),
        };

        match result {
            Ok(()) => {
                self.set_status(format!("Applied {}", value));
                self.cancel_edit();
                self.refresh();
            }
            Err(e) => self.set_status(e.to_string()),
        }
    }

    fn cycle(&mut self, path: &str, cur: Option<i32>, max: i32, step: isize, label: &str) {
        let next = (((cur.unwrap_or(0) as isize) + step).rem_euclid(max as isize)) as i32;
        match write_value(path, next) {
            Ok(()) => {
                self.set_status(format!("{} -> {}", label, next));
                self.refresh();
            }
            Err(e) => self.set_status(e.to_string()),
        }
    }

    fn toggle_gpu_boost(&mut self) {
        let next = match self.gpu_boost.unwrap_or(0) {
            1 => 0,
            _ => 1,
        };
        match write_value(GPU_BOOST, next) {
            Ok(()) => {
                self.set_status(format!("GPU boost -> {}", if next == 1 { "ON" } else { "OFF" }));
                self.refresh();
            }
            Err(e) => self.set_status(e.to_string()),
        }
    }
}

// --- CLI ---

#[derive(Parser)]
#[command(
    name = "gigabytectl",
    version,
    about = "Control panel for gigabyte-laptop-wmi",
    long_about = "Control panel for gigabyte-laptop-wmi.\n\nRun without a subcommand to launch the interactive TUI, or pass a subcommand to run a one-shot, scriptable command."
)]
struct Cli {
    /// Run a one-shot command instead of launching the TUI
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Show current status of all controllable values
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Get or set the fan mode
    FanMode {
        #[command(subcommand)]
        action: FanModeAction,
    },
    /// Get or set the custom fan speed (0..255)
    FanSpeed {
        #[command(subcommand)]
        action: ValueAction,
    },
    /// Get or set the charging mode
    ChargeMode {
        #[command(subcommand)]
        action: ChargeModeAction,
    },
    /// Get or set the charge limit (60..100)
    ChargeLimit {
        #[command(subcommand)]
        action: ValueAction,
    },
    /// Get or set GPU boost
    GpuBoost {
        #[command(subcommand)]
        action: OnOffAction,
    },
    /// Show the battery cycle count
    BatteryCycle,
    /// Show light sensor data
    LightSensor,
    /// Show current CPU fan PWM
    FanPwm,
    /// Show live fan RPM readings
    Fans,
    /// Get or set fan curve points
    FanCurve {
        #[command(subcommand)]
        action: FanCurveAction,
    },
    /// Continuously print temps and fan RPM (Ctrl-C to stop)
    Monitor {
        /// Refresh interval in seconds (defaults to config refresh_interval_ms)
        #[arg(short, long)]
        interval: Option<f64>,
        /// Output as JSON, one object per line
        #[arg(long)]
        json: bool,
    },
    /// Apply, list, or save profiles (~/.config/gigabytectl/profiles.toml)
    Profile(ProfileArgs),
    /// Sync with power-profiles-daemon: apply the mapped profile whenever the
    /// system power profile changes. Runs until interrupted (for a systemd service).
    Sync {
        /// Apply the profile mapped to the current power profile once, then exit
        #[arg(long)]
        once: bool,
    },
    /// Generate shell completions (bash, zsh, fish, ...)
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
    /// Install and enable the power-profiles-daemon sync systemd service.
    /// Seeds /etc/gigabytectl/profiles.toml from your profiles so the
    /// root-run service can find them (run with sudo).
    InstallService,
}

#[derive(Args)]
struct ProfileArgs {
    /// Name of the profile to apply
    name: Option<String>,
    /// List available profiles
    #[arg(short, long)]
    list: bool,
    /// Save the current hardware settings as a new profile with this name
    #[arg(long, value_name = "NAME")]
    save: Option<String>,
}

#[derive(Subcommand)]
enum FanModeAction {
    /// Print the current value
    Get,
    /// Set a new value
    Set { mode: FanModeArg },
}

#[derive(Subcommand)]
enum ValueAction {
    /// Print the current value
    Get,
    /// Set a new value
    Set { value: i32 },
}

#[derive(Subcommand)]
enum ChargeModeAction {
    /// Print the current value
    Get,
    /// Set a new value
    Set { mode: ChargeModeArg },
}

#[derive(Subcommand)]
enum OnOffAction {
    /// Print the current value
    Get,
    /// Set a new value
    Set { state: OnOff },
}

#[derive(Subcommand)]
enum FanCurveAction {
    /// Print one point (if index given) or the whole curve
    Get { index: Option<usize> },
    /// Set the (temp, speed) pair at an index (0..15)
    Set { index: usize, temp: i32, speed: i32 },
}

#[derive(ValueEnum, Clone, Copy)]
#[value(rename_all = "kebab-case")]
enum FanModeArg {
    Normal,
    Silent,
    Gaming,
    Custom,
    Auto,
    Fixed,
}

impl FanModeArg {
    fn as_i32(self) -> i32 {
        match self {
            FanModeArg::Normal => 0,
            FanModeArg::Silent => 1,
            FanModeArg::Gaming => 2,
            FanModeArg::Custom => 3,
            FanModeArg::Auto => 4,
            FanModeArg::Fixed => 5,
        }
    }
}

#[derive(ValueEnum, Clone, Copy)]
#[value(rename_all = "kebab-case")]
enum ChargeModeArg {
    Normal,
    Custom,
}

#[derive(ValueEnum, Clone, Copy)]
#[value(rename_all = "kebab-case")]
enum OnOff {
    On,
    Off,
}

fn require_ready_for_cli() -> Result<()> {
    anyhow::ensure!(is_root(), "gigabytectl: must be run as root (try: sudo gigabytectl ...)");
    anyhow::ensure!(driver_present(), "{}", driver_missing_message());
    Ok(())
}

fn run_cli(command: Commands, config: &Config) -> Result<()> {
    // These commands are read-only or touch only user config, so they don't
    // require root or the driver to be present.
    match &command {
        Commands::Completions { shell } => return run_completions(*shell),
        Commands::Monitor { interval, json } => return run_monitor(*interval, *json, config),
        Commands::Profile(args) => return run_profile(args, config),
        Commands::Sync { once } => return run_sync(*once),
        Commands::InstallService => return run_install_service(),
        _ => {}
    }

    require_ready_for_cli()?;

    match command {
        Commands::Status { json } => print_status(json, config.units),
        Commands::FanMode { action } => match action {
            FanModeAction::Get => println!("{}", fan_mode_name(read_i32(FAN_MODE)).to_lowercase()),
            FanModeAction::Set { mode } => write_value(FAN_MODE, mode.as_i32())?,
        },
        Commands::FanSpeed { action } => match action {
            ValueAction::Get => println!("{}", value_or_na(read_i32(FAN_CUSTOM_SPEED))),
            ValueAction::Set { value } => {
                validate_fan_speed(value)?;
                write_value(FAN_CUSTOM_SPEED, value)?;
            }
        },
        Commands::ChargeMode { action } => match action {
            ChargeModeAction::Get => println!("{}", charge_mode_name(read_i32(CHARGE_MODE)).to_lowercase()),
            ChargeModeAction::Set { mode } => {
                let value = match mode {
                    ChargeModeArg::Normal => 0,
                    ChargeModeArg::Custom => 1,
                };
                write_value(CHARGE_MODE, value)?;
            }
        },
        Commands::ChargeLimit { action } => match action {
            ValueAction::Get => println!("{}", value_or_na(read_i32(CHARGE_LIMIT))),
            ValueAction::Set { value } => {
                validate_charge_limit(value)?;
                write_value(CHARGE_LIMIT, value)?;
            }
        },
        Commands::GpuBoost { action } => match action {
            OnOffAction::Get => println!("{}", gpu_boost_name(read_i32(GPU_BOOST)).to_lowercase()),
            OnOffAction::Set { state } => {
                let value = match state {
                    OnOff::On => 1,
                    OnOff::Off => 0,
                };
                write_value(GPU_BOOST, value)?;
            }
        },
        Commands::BatteryCycle => println!("{}", battery_cycle_text(read_trimmed(BATTERY_CYCLE))),
        Commands::LightSensor => println!("{}", read_trimmed(LIGHT_SENSOR).unwrap_or_else(|| "N/A".to_string())),
        Commands::FanPwm => println!("{}", value_or_na(read_i32(FAN_PWM))),
        Commands::Fans => {
            let fans = GigabyteHwmon::new().map(|h| h.read_fans()).unwrap_or_default();
            if fans.is_empty() {
                println!("No live fan readings available");
            }
            for fan in fans {
                println!("{}: {} RPM", fan.name, fan.rpm);
            }
        }
        Commands::FanCurve { action } => match action {
            FanCurveAction::Get { index } => {
                let curve = read_fan_curve()?;
                match index {
                    Some(idx) => {
                        validate_curve_index(idx)?;
                        let (temp, speed) = curve[idx];
                        println!("{} {} {}", idx, temp, speed);
                    }
                    None => {
                        for (i, (temp, speed)) in curve.iter().enumerate() {
                            println!("{} {} {}", i, temp, speed);
                        }
                    }
                }
            }
            FanCurveAction::Set { index, temp, speed } => {
                validate_curve_temp(temp)?;
                validate_curve_speed(speed)?;
                write_fan_curve_point(index, temp, speed)?;
            }
        },
        Commands::Monitor { .. }
        | Commands::Profile(_)
        | Commands::Sync { .. }
        | Commands::Completions { .. }
        | Commands::InstallService => unreachable!(),
    }

    Ok(())
}

fn run_completions(shell: Shell) -> Result<()> {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    generate(shell, &mut cmd, name, &mut io::stdout());
    Ok(())
}

fn run_monitor(interval: Option<f64>, json: bool, config: &Config) -> Result<()> {
    let secs = interval.unwrap_or(config.refresh_interval_ms as f64 / 1000.0).max(0.1);
    let period = Duration::from_secs_f64(secs);
    let units = config.units;

    loop {
        let temps = Temps::read();
        let fans = GigabyteHwmon::new().map(|h| h.read_fans()).unwrap_or_default();

        if json {
            let fans_json: Vec<String> = fans
                .iter()
                .map(|f| format!(r#"{{"name":"{}","rpm":{}}}"#, json_escape(&f.name), f.rpm))
                .collect();
            println!(
                r#"{{"cpu_temp":{},"gpu_temp":{},"fans":[{}]}}"#,
                temps.cpu.map(|c| format!("{:.1}", to_units(c, units))).unwrap_or_else(|| "null".to_string()),
                temps.gpu.map(|g| format!("{:.1}", to_units(g, units))).unwrap_or_else(|| "null".to_string()),
                fans_json.join(",")
            );
        } else {
            let fan_str: String = if fans.is_empty() {
                "no fan data".to_string()
            } else {
                fans.iter().map(|f| format!("{}: {} RPM", f.name, f.rpm)).collect::<Vec<_>>().join("   ")
            };
            println!(
                "CPU {}   GPU {}   {}",
                format_temp(temps.cpu, units),
                format_temp(temps.gpu, units),
                fan_str
            );
        }

        std::thread::sleep(period);
    }
}

fn run_profile(args: &ProfileArgs, _config: &Config) -> Result<()> {
    if let Some(name) = &args.save {
        anyhow::ensure!(is_root(), "gigabytectl: saving a profile reads hardware state; run as root (try: sudo gigabytectl profile --save {})", name);
        let mut profiles = load_profiles()?;
        profiles.insert(name.clone(), current_profile());
        save_profiles(&profiles)?;
        println!("Saved profile '{}'", name);
        return Ok(());
    }

    if args.list {
        let profiles = load_profiles()?;
        if profiles.is_empty() {
            println!("No profiles defined in {}", config_dir().join("profiles.toml").display());
        } else {
            let mut names: Vec<&String> = profiles.keys().collect();
            names.sort();
            for name in names {
                println!("{}", name);
            }
        }
        return Ok(());
    }

    let Some(name) = &args.name else {
        anyhow::bail!("Specify a profile to apply, or use --list / --save <name>");
    };

    require_ready_for_cli()?;
    let profiles = load_profiles()?;
    let profile = profiles
        .get(name)
        .with_context(|| format!("profile '{}' not found in {}", name, config_dir().join("profiles.toml").display()))?;
    apply_profile(profile)?;
    println!("Applied profile '{}'", name);

    // Forward sync: if this profile maps to a PPD profile, switch PPD to match.
    if let Some(ppd) = &profile.ppd_profile {
        match ppd_set(ppd) {
            Ok(()) => println!("Set power profile -> {}", ppd),
            Err(e) => eprintln!("Warning: {}", e),
        }
    }
    Ok(())
}

fn run_sync(once: bool) -> Result<()> {
    require_ready_for_cli()?;
    anyhow::ensure!(
        ppd_endpoint().is_some(),
        "power-profiles-daemon is not available on the system bus (is it installed and running?)"
    );

    // Apply the mapping for whatever profile is active right now.
    apply_ppd_mapping()?;
    if once {
        return Ok(());
    }

    let (dest, path) = ppd_endpoint().context("power-profiles-daemon disappeared")?;
    eprintln!("gigabytectl: watching {} for power profile changes (Ctrl-C to stop)...", dest);

    let mut child = Command::new("gdbus")
        .args(["monitor", "--system", "--dest", dest, "--object-path", path])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("spawning `gdbus monitor` (is gdbus/glib2 installed?)")?;

    let stdout = child.stdout.take().context("capturing gdbus stdout")?;
    let reader = io::BufReader::new(stdout);
    use std::io::BufRead;
    for line in reader.lines() {
        let line = line.context("reading gdbus monitor output")?;
        let Some(profile) = parse_active_profile_change(&line) else {
            continue;
        };
        let profiles = load_profiles()?;
        match profile_for_ppd(&profiles, &profile) {
            Some((name, p)) => match apply_profile(p) {
                Ok(()) => eprintln!("Power profile '{}' -> applied gigabytectl profile '{}'", profile, name),
                Err(e) => eprintln!("Warning: failed to apply gigabytectl profile '{}': {}", name, e),
            },
            None => eprintln!("Power profile '{}' has no mapped gigabytectl profile; ignoring", profile),
        }
    }

    let status = child.wait().context("waiting on gdbus monitor")?;
    anyhow::bail!("gdbus monitor exited unexpectedly ({})", status);
}

const SERVICE_NAME: &str = "gigabytectl-ppd-sync.service";
const SERVICE_PATH: &str = "/etc/systemd/system/gigabytectl-ppd-sync.service";

/// Installs and enables the systemd sync service. Because the service runs as
/// root (no `SUDO_USER`, `HOME=/root`), it can't see profiles in a user's home,
/// so we seed a system-wide copy in `/etc/gigabytectl` that `load_profiles`
/// falls back to.
fn run_install_service() -> Result<()> {
    anyhow::ensure!(
        is_root(),
        "gigabytectl: installing the sync service writes to /etc; run as root (try: sudo gigabytectl install-service)"
    );

    // Point the unit at whatever binary is running this command.
    let exe = std::env::current_exe().context("resolving current executable path")?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);

    // Seed the system-wide profiles from the invoking user's profiles so the
    // root-run service can find them.
    fs::create_dir_all(SYSTEM_CONFIG_DIR).with_context(|| format!("creating {}", SYSTEM_CONFIG_DIR))?;
    let system_profiles = PathBuf::from(SYSTEM_CONFIG_DIR).join("profiles.toml");
    let user_profiles = config_dir().join("profiles.toml");
    match fs::read_to_string(&user_profiles) {
        Ok(text) => {
            fs::write(&system_profiles, text)
                .with_context(|| format!("writing {}", system_profiles.display()))?;
            eprintln!("Copied profiles: {} -> {}", user_profiles.display(), system_profiles.display());
        }
        Err(_) if system_profiles.exists() => {
            eprintln!("Keeping existing profiles at {}", system_profiles.display());
        }
        Err(_) => {
            eprintln!("Note: no profiles found at {} yet.", user_profiles.display());
            eprintln!(
                "      Save one with `sudo gigabytectl profile --save <name>`, then re-run `sudo gigabytectl install-service`"
            );
            eprintln!("      (or edit {} directly).", system_profiles.display());
        }
    }

    // Write the unit, pointing ExecStart at the resolved binary path.
    let unit = format!(
        "[Unit]\n\
         Description=gigabytectl power-profiles-daemon sync\n\
         Documentation=https://github.com/Code-Sapling/gigabytectl\n\
         # Apply the mapped gigabytectl profile whenever the system power profile changes.\n\
         After=power-profiles-daemon.service\n\
         Wants=power-profiles-daemon.service\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exe} sync\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        exe = exe.display()
    );
    fs::write(SERVICE_PATH, unit).with_context(|| format!("writing {}", SERVICE_PATH))?;
    eprintln!("Wrote {}", SERVICE_PATH);

    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", "--now", SERVICE_NAME])?;
    eprintln!("Enabled and started {SERVICE_NAME}.");
    eprintln!("Check it with: systemctl status {SERVICE_NAME}");
    Ok(())
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .args(args)
        .status()
        .with_context(|| format!("running `systemctl {}`", args.join(" ")))?;
    anyhow::ensure!(status.success(), "`systemctl {}` failed ({})", args.join(" "), status);
    Ok(())
}

fn print_status(json: bool, units: Units) {
    let fan_mode = fan_mode_name(read_i32(FAN_MODE));
    let fan_speed = value_or_na(read_i32(FAN_CUSTOM_SPEED));
    let charge_mode = charge_mode_name(read_i32(CHARGE_MODE));
    let charge_limit = value_or_na(read_i32(CHARGE_LIMIT));
    let gpu_boost = gpu_boost_name(read_i32(GPU_BOOST));
    let battery_cycle = battery_cycle_text(read_trimmed(BATTERY_CYCLE));
    let light_sensor = read_trimmed(LIGHT_SENSOR).unwrap_or_else(|| "N/A".to_string());
    let fan_pwm = value_or_na(read_i32(FAN_PWM));
    let temps = Temps::read();
    let fans = GigabyteHwmon::new().map(|h| h.read_fans()).unwrap_or_default();

    if json {
        let fans_json: Vec<String> = fans
            .iter()
            .map(|f| format!(r#"{{"name":"{}","rpm":{}}}"#, json_escape(&f.name), f.rpm))
            .collect();
        println!(
            r#"{{"fan_mode":"{}","fan_speed":"{}","charge_mode":"{}","charge_limit":"{}","gpu_boost":"{}","battery_cycle":"{}","light_sensor":"{}","fan_pwm":"{}","cpu_temp":{},"gpu_temp":{},"fans":[{}]}}"#,
            json_escape(&fan_mode.to_lowercase()),
            json_escape(&fan_speed),
            json_escape(&charge_mode.to_lowercase()),
            json_escape(&charge_limit),
            json_escape(&gpu_boost.to_lowercase()),
            json_escape(&battery_cycle),
            json_escape(&light_sensor),
            json_escape(&fan_pwm),
            temps.cpu.map(|c| format!("{:.1}", to_units(c, units))).unwrap_or_else(|| "null".to_string()),
            temps.gpu.map(|g| format!("{:.1}", to_units(g, units))).unwrap_or_else(|| "null".to_string()),
            fans_json.join(",")
        );
    } else {
        println!("Fan mode:      {}", fan_mode);
        println!("Fan speed:     {}", fan_speed);
        println!("Charge mode:   {}", charge_mode);
        println!("Charge limit:  {}", charge_limit);
        println!("GPU boost:     {}", gpu_boost);
        println!("Battery cycle: {}", battery_cycle);
        println!("Light sensor:  {}", light_sensor);
        println!("Fan PWM:       {}", fan_pwm);
        println!("CPU temp:      {}", format_temp(temps.cpu, units));
        println!("GPU temp:      {}", format_temp(temps.gpu, units));
        if !fans.is_empty() {
            println!("Fans:");
            for fan in fans {
                println!("  {}: {} RPM", fan.name, fan.rpm);
            }
        }
    }
}

// --- Utilities ---

fn validate_fan_speed(value: i32) -> Result<()> {
    if (0..=255).contains(&value) {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Fan speed must be 0..255"))
    }
}

fn validate_charge_limit(value: i32) -> Result<()> {
    if (60..=100).contains(&value) {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Charge limit must be 60..100"))
    }
}

fn validate_curve_temp(value: i32) -> Result<()> {
    if (0..=100).contains(&value) {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Temperature must be 0..100"))
    }
}

fn validate_curve_speed(value: i32) -> Result<()> {
    if (0..=255).contains(&value) {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Speed must be 0..255"))
    }
}

fn read_trimmed(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn read_i32(path: &str) -> Option<i32> {
    read_trimmed(path).and_then(|s| s.parse::<i32>().ok())
}

fn write_value(path: &str, value: i32) -> Result<()> {
    anyhow::ensure!(Path::new(path).exists(), "Node not found: {}", path);
    fs::write(path, format!("{}\n", value)).with_context(|| format!("write {} -> {}", value, path))?;
    Ok(())
}

fn read_fan_curve() -> Result<Vec<(i32, i32)>> {
    let mut curve = Vec::with_capacity(FAN_CURVE_POINTS);
    for i in 0..FAN_CURVE_POINTS {
        write_value(FAN_CURVE_INDEX, i as i32).with_context(|| format!("selecting fan curve index {}", i))?;
        let data = read_trimmed(FAN_CURVE_DATA).with_context(|| format!("reading fan curve data at index {}", i))?;

        let mut parts = data.split_whitespace();
        let temp = parts
            .next()
            .and_then(|s| s.parse::<i32>().ok())
            .with_context(|| format!("parsing fan curve temperature at index {}", i))?;
        let speed = parts
            .next()
            .and_then(|s| s.parse::<i32>().ok())
            .with_context(|| format!("parsing fan curve speed at index {}", i))?;

        curve.push((temp, speed));
    }
    Ok(curve)
}

fn validate_curve_index(index: usize) -> Result<()> {
    anyhow::ensure!(index < FAN_CURVE_POINTS, "Index must be 0..{}", FAN_CURVE_POINTS);
    Ok(())
}

fn write_fan_curve_point(index: usize, temp: i32, speed: i32) -> Result<()> {
    validate_curve_index(index)?;
    write_value(FAN_CURVE_INDEX, index as i32)?;
    let data = (speed * 256) + temp;
    write_value(FAN_CURVE_DATA, data)?;
    Ok(())
}

fn fan_mode_name(v: Option<i32>) -> String {
    match v {
        Some(i) if (0..=5).contains(&i) => FAN_MODES[i as usize].to_string(),
        Some(i) => format!("Unknown ({})", i),
        None => "N/A".to_string(),
    }
}

fn gpu_boost_name(v: Option<i32>) -> &'static str {
    match v {
        Some(1) => "ON",
        Some(0) => "OFF",
        _ => "UNKNOWN",
    }
}

fn charge_mode_name(v: Option<i32>) -> &'static str {
    match v {
        Some(0) => "Normal",
        Some(1) => "Custom",
        _ => "UNKNOWN",
    }
}

fn value_or_na(v: Option<i32>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "N/A".to_string())
}

fn to_units(celsius: f32, units: Units) -> f32 {
    match units {
        Units::Celsius => celsius,
        Units::Fahrenheit => celsius * 9.0 / 5.0 + 32.0,
    }
}

fn unit_symbol(units: Units) -> &'static str {
    match units {
        Units::Celsius => "°C",
        Units::Fahrenheit => "°F",
    }
}

fn format_temp(celsius: Option<f32>, units: Units) -> String {
    match celsius {
        Some(c) => format!("{:.0}{}", to_units(c, units), unit_symbol(units)),
        None => "N/A".to_string(),
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn battery_cycle_text(v: Option<String>) -> String {
    match v.as_deref() {
        Some("0") => "Device does not support this feature".to_string(),
        Some(s) => s.to_string(),
        None => "N/A".to_string(),
    }
}

fn is_root() -> bool {
    fs::metadata("/proc/self").map(|m| m.uid() == 0).unwrap_or(false)
}

fn run_sudo() -> Result<()> {
    let exe = std::env::current_exe().context("failed to resolve current executable path")?;

    let status = Command::new("sudo")
        .arg(exe)
        .args(std::env::args().skip(1))
        .status()
        .context("failed to execute sudo")?;

    std::process::exit(status.code().unwrap_or(1));
}

fn driver_present() -> bool {
    Path::new(ROOT).exists()
}

fn driver_missing_message() -> String {
    format!("{} does not exist. Please install gigabyte-laptop-wmi and ensure it is running.", ROOT)
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

// --- UI Components ---

/// Computes [x, y] bounds spanning every point in the given datasets.
fn xy_bounds(datasets: &[&[(f64, f64)]]) -> Option<([f64; 2], [f64; 2])> {
    let (mut xmin, mut xmax) = (f64::MAX, f64::MIN);
    let (mut ymin, mut ymax) = (f64::MAX, f64::MIN);
    let mut any = false;
    for data in datasets {
        for &(x, y) in *data {
            any = true;
            xmin = xmin.min(x);
            xmax = xmax.max(x);
            ymin = ymin.min(y);
            ymax = ymax.max(y);
        }
    }
    any.then_some(([xmin, xmax], [ymin, ymax]))
}

fn time_labels(x: [f64; 2]) -> Vec<Span<'static>> {
    let mid = (x[0] + x[1]) / 2.0;
    vec![
        Span::raw(format!("{:.0}s", x[0])),
        Span::raw(format!("{:.0}s", mid)),
        Span::raw(format!("{:.0}s", x[1])),
    ]
}

fn value_labels(y: [f64; 2], suffix: &str) -> Vec<Span<'static>> {
    let mid = (y[0] + y[1]) / 2.0;
    vec![
        Span::raw(format!("{:.0}{}", y[0], suffix)),
        Span::raw(format!("{:.0}{}", mid, suffix)),
        Span::raw(format!("{:.0}{}", y[1], suffix)),
    ]
}

fn render_history(frame: &mut ratatui::Frame<'_>, area: Rect, history: &History, units: Units) {
    let halves = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // --- Temperature chart (CPU + GPU) ---
    let cpu: Vec<(f64, f64)> = history.cpu.iter().map(|&(t, v)| (t, to_units(v as f32, units) as f64)).collect();
    let gpu: Vec<(f64, f64)> = history.gpu.iter().map(|&(t, v)| (t, to_units(v as f32, units) as f64)).collect();

    let temp_title = format!("Temperature ({})", unit_symbol(units).trim_start_matches('°'));
    if let Some((xb, yb)) = xy_bounds(&[&cpu, &gpu]) {
        let pad = ((yb[1] - yb[0]) * 0.1).max(2.0);
        let yb = [(yb[0] - pad).max(0.0), yb[1] + pad];
        let datasets = vec![
            Dataset::default().name("CPU").marker(symbols::Marker::Braille).graph_type(GraphType::Line).style(Style::default().fg(Color::LightRed)).data(&cpu),
            Dataset::default().name("GPU").marker(symbols::Marker::Braille).graph_type(GraphType::Line).style(Style::default().fg(Color::Green)).data(&gpu),
        ];
        let chart = Chart::new(datasets)
            .block(Block::default().borders(Borders::ALL).title(temp_title))
            .x_axis(Axis::default().style(Style::default().fg(Color::Gray)).bounds(xb).labels(time_labels(xb)))
            .y_axis(Axis::default().style(Style::default().fg(Color::Gray)).bounds(yb).labels(value_labels(yb, unit_symbol(units))));
        frame.render_widget(chart, halves[0]);
    } else {
        frame.render_widget(
            Paragraph::new("Collecting temperature samples...").block(Block::default().borders(Borders::ALL).title(temp_title)),
            halves[0],
        );
    }

    // --- Fan RPM chart ---
    let rpm: Vec<(f64, f64)> = history.rpm.iter().copied().collect();
    if let Some((xb, mut yb)) = xy_bounds(&[&rpm]) {
        yb = [0.0, yb[1] * 1.1 + 1.0];
        let datasets = vec![Dataset::default()
            .name("Fan RPM")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Cyan))
            .data(&rpm)];
        let chart = Chart::new(datasets)
            .block(Block::default().borders(Borders::ALL).title("Fan RPM (max)"))
            .x_axis(Axis::default().style(Style::default().fg(Color::Gray)).bounds(xb).labels(time_labels(xb)))
            .y_axis(Axis::default().style(Style::default().fg(Color::Gray)).bounds(yb).labels(value_labels(yb, "")));
        frame.render_widget(chart, halves[1]);
    } else {
        frame.render_widget(
            Paragraph::new("Collecting fan samples...").block(Block::default().borders(Borders::ALL).title("Fan RPM (max)")),
            halves[1],
        );
    }
}

fn item_title(item: Item) -> &'static str {
    match item {
        Item::FanMode => "Fan mode",
        Item::FanCustomSpeed => "Fan custom speed",
        Item::ChargeMode => "Charging mode",
        Item::ChargeLimit => "Charging limit",
        Item::GpuBoost => "GPU boost",
        Item::FanCurveView => "Fan curve (View)",
        Item::FanCurveEdit => "Fan curve (Edit)",
        Item::History => "History graph",
        Item::Refresh => "Refresh values",
        Item::Quit => "Quit",
    }
}

fn item_hint(item: Item) -> &'static str {
    match item {
        Item::FanMode => "Left/Right to cycle names",
        Item::FanCustomSpeed => "Enter 0..255",
        Item::ChargeMode => "Left/Right toggles Normal/Custom",
        Item::ChargeLimit => "Enter 60..100",
        Item::GpuBoost => "Left/Right toggles ON/OFF",
        Item::FanCurveView => "Shows a visual graph of the current fan curve",
        Item::FanCurveEdit => "Press Enter to edit the fan curve table",
        Item::History => "Live CPU/GPU temperature and fan RPM over time",
        Item::Refresh => "Reload all sysfs nodes",
        Item::Quit => "Exit the app",
    }
}

fn item_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    }
}

fn badge_style(text: &str) -> Style {
    match text {
        "ON" | "Custom" | "Gaming" | "Fixed" => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        "OFF" | "Normal" => Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD),
        "Silent" | "Auto" => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        _ => Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
    }
}

fn ui(frame: &mut ratatui::Frame<'_>, app: &App) {
    let area = frame.area();

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(2),
        ])
        .split(area);

    let top = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(" gigabytectl ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled("Gigabyte control panel", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled(" root ", Style::default().fg(Color::Black).bg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::raw(format!("last refresh: {}s ago", app.last_refresh.elapsed().as_secs())),
        ]),
    ])
    .block(Block::default().borders(Borders::ALL).style(Style::default().fg(Color::Cyan)));
    frame.render_widget(top, outer[0]);

    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(outer[1]);

    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(4)])
        .split(main[0]);

    let items: Vec<ListItem> = app
        .items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let is_selected = idx == app.selected;
            let marker = if is_selected { "▶" } else { "  " };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{} {}", marker, item_title(*item)), item_style(is_selected)),
            ]))
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(app.selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Controls"))
            .highlight_style(Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD)),
        left[0],
        &mut list_state,
    );

    let status_text = if app.focus == Focus::Editing {
        let label = match app.editing {
            Some(EditTarget::FanCustomSpeed) => "Enter fan custom speed".to_string(),
            Some(EditTarget::ChargeLimit) => "Enter charge limit".to_string(),
            Some(EditTarget::FanCurveTemp(idx)) => format!("Enter temp for idx {}", idx),
            Some(EditTarget::FanCurveSpeed(idx)) => format!("Enter speed for idx {}", idx),
            None => "Editing".to_string(),
        };
        format!("{}: {}", label, app.input)
    } else {
        app.status.clone()
    };

    frame.render_widget(
        Paragraph::new(status_text)
            .block(Block::default().borders(Borders::ALL).title("Status"))
            .wrap(Wrap { trim: true }),
        left[1],
    );

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(12), Constraint::Length(5)])
        .split(main[1]);

    let selected = app.selected_item();

    if selected == Item::FanCurveView {
        // --- 1. VIEW MODE (Read-only Chart) ---
        if let Some(curve) = &app.fan_curve {
            let data_points: Vec<(f64, f64)> = curve
                .iter()
                .map(|&(t, s)| (t as f64, s as f64))
                .collect();

            let datasets = vec![Dataset::default()
                .name("Curve")
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::Cyan))
                .data(&data_points)];

            let x_axis = Axis::default()
                .title("Temp (°C)")
                .style(Style::default().fg(Color::Gray))
                .bounds([0.0, 100.0])
                .labels(vec![Span::raw("0"), Span::raw("50"), Span::raw("100")]);

            let y_axis = Axis::default()
                .title("Speed")
                .style(Style::default().fg(Color::Gray))
                .bounds([0.0, 255.0])
                .labels(vec![Span::raw("0"), Span::raw("128"), Span::raw("255")]);

            let chart = Chart::new(datasets)
                .block(Block::default().borders(Borders::ALL).title("Fan Curve Graph"))
                .x_axis(x_axis)
                .y_axis(y_axis);

            frame.render_widget(chart, right[0]);
        } else {
            let fc_widget = Paragraph::new("Failed to read fan curve data.")
                .block(Block::default().borders(Borders::ALL).title("Fan Curve Graph"));
            frame.render_widget(fc_widget, right[0]);
        }
    } else if selected == Item::History {
        // --- HISTORY MODE (Live temp + RPM charts) ---
        render_history(frame, right[0], &app.history, app.config.units);
    } else if app.focus == Focus::FanCurveList || selected == Item::FanCurveEdit {
        // --- 2. EDIT MODE (Interactive Table) ---
        let mut lines = vec![];
        lines.push(Line::from(vec![
            Span::styled(format!("{:>3}  ", "Idx"), Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!("{:>9}", "Temp (°C)"), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("   "),
            Span::styled(format!("{:>13}", "Speed (0-255)"), Style::default().add_modifier(Modifier::BOLD)),
        ]));

        if let Some(curve) = &app.fan_curve {
            for (i, &(temp, speed)) in curve.iter().enumerate() {
                let is_selected_row = app.focus == Focus::FanCurveList && app.fan_curve_selected == i;
                let t_style = if is_selected_row && app.fan_curve_col == 0 { item_style(true) } else { Style::default() };
                let s_style = if is_selected_row && app.fan_curve_col == 1 { item_style(true) } else { Style::default() };

                lines.push(Line::from(vec![
                    Span::raw(format!("{:>3}  ", i)),
                    Span::styled(format!("{:>9}", temp), t_style),
                    Span::raw("   "),
                    Span::styled(format!("{:>13}", speed), s_style),
                ]));
            }
        } else {
            lines.push(Line::from("Failed to read fan curve data."));
        }

        let fc_widget = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Fan Curve Editor"))
            .wrap(Wrap { trim: true });
        frame.render_widget(fc_widget, right[0]);
    } else {
        // --- 3. NORMAL DASHBOARD MODE ---
        let fan_mode_text = fan_mode_name(app.fan_mode);
        let gpu_text = gpu_boost_name(app.gpu_boost);
        let charge_text = charge_mode_name(app.charge_mode);

        let mut dash_lines = vec![
            Line::from(vec![
                Span::styled("Fan mode       ", Style::default().fg(Color::White)),
                Span::styled(fan_mode_text.clone(), badge_style(&fan_mode_text)),
            ]),
            Line::from(vec![
                Span::styled("Fan speed      ", Style::default().fg(Color::White)),
                Span::styled(value_or_na(app.fan_custom_speed), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled("Charge mode    ", Style::default().fg(Color::White)),
                Span::styled(charge_text, badge_style(charge_text)),
            ]),
            Line::from(vec![
                Span::styled("Charge limit   ", Style::default().fg(Color::White)),
                Span::styled(value_or_na(app.charge_limit), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled("GPU boost      ", Style::default().fg(Color::White)),
                Span::styled(gpu_text, badge_style(gpu_text)),
            ]),
            Line::from(vec![
                Span::styled("Battery cycle  ", Style::default().fg(Color::White)),
                Span::styled(battery_cycle_text(app.battery_cycle.clone()), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled("Light sensor   ", Style::default().fg(Color::White)),
                Span::styled(app.light_sensor.clone().unwrap_or_else(|| "N/A".to_string()), Style::default().fg(Color::Cyan)),
            ]),
            Line::from(vec![
                Span::styled("Fan PWM        ", Style::default().fg(Color::White)),
                Span::styled(value_or_na(app.fan_pwm), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled("CPU temp       ", Style::default().fg(Color::White)),
                Span::styled(format_temp(app.temps.cpu, app.config.units), Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled("GPU temp       ", Style::default().fg(Color::White)),
                Span::styled(format_temp(app.temps.gpu, app.config.units), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            ]),
        ];

        // Real-time Fan Monitor Update
        if !app.live_fans.is_empty() {
            dash_lines.push(Line::from(""));
            dash_lines.push(Line::from(Span::styled("Fan readings:", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))));
            for fan in &app.live_fans {
                dash_lines.push(Line::from(vec![
                    Span::styled(format!("{:14} ", fan.name), Style::default().fg(Color::White)),
                    Span::styled(format!("{} RPM", fan.rpm), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                ]));
            }
        }

        let dashboard = Paragraph::new(dash_lines)
            .block(Block::default().borders(Borders::ALL).title("Current values"))
            .wrap(Wrap { trim: true });
        frame.render_widget(dashboard, right[0]);
    }

    let help_text = match app.focus {
        Focus::FanCurveList => vec![
            Line::from(vec![
                Span::styled("Editing: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled("Fan Curve", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled("Hint: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw("Temp: 0-100, Speed: 0-255. Maintain non-decreasing order."),
            ]),
            Line::from("↑/↓ row   ←/→ col   Enter edit   Esc back"),
        ],
        _ => vec![
            Line::from(vec![
                Span::styled("Selected: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(item_title(selected), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled("Hint: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw(item_hint(selected)),
            ]),
            Line::from("↑/↓ move   ←/→ action   Enter edit/apply   Esc cancel   r refresh   q quit"),
        ]
    };
    let help = Paragraph::new(help_text)
        .block(Block::default().borders(Borders::ALL).title("Help"))
        .wrap(Wrap { trim: true });
    frame.render_widget(help, right[1]);

    let footer = Paragraph::new(if app.focus == Focus::Editing {
        "Editing mode: type numbers only, then press Enter"
    } else {
        "Ready"
    })
    .block(Block::default().borders(Borders::ALL).style(Style::default().fg(Color::DarkGray)));
    frame.render_widget(footer, outer[2]);

    if app.focus == Focus::Editing {
        let popup = centered_rect(56, 24, area);
        frame.render_widget(Clear, popup);
        let border_style = Style::default().fg(Color::Magenta);
        let popup_text = vec![
            Line::from(vec![
                Span::styled("Input", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled("(Esc cancels)", Style::default().fg(Color::DarkGray)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Value: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(app.input.clone(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            ]),
            Line::from(""),
            Line::from("Fan speed: 0..255"),
            Line::from("Charge limit: 60..100"),
            Line::from("Curve Temp: 0..100 | Curve Speed: 0..255"),
        ];
        frame.render_widget(
            Paragraph::new(popup_text)
                .block(Block::default().borders(Borders::ALL).title("Edit value").border_style(border_style))
                .wrap(Wrap { trim: true }),
            popup,
        );
    }
}

// --- Event Handling ---

fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.focus == Focus::Editing {
        match key.code {
            KeyCode::Esc => app.cancel_edit(),
            KeyCode::Enter => app.apply_edit(),
            KeyCode::Backspace => app.backspace_input(),
            KeyCode::Char(c) => app.push_input(c),
            _ => {}
        }
        return false;
    }

    if app.focus == Focus::FanCurveList {
        match key.code {
            KeyCode::Esc => app.focus = Focus::Normal,
            KeyCode::Up => app.fan_curve_selected = app.fan_curve_selected.saturating_sub(1),
            KeyCode::Down => app.fan_curve_selected = (app.fan_curve_selected + 1).min(FAN_CURVE_POINTS - 1),
            KeyCode::Left => {
                app.fan_curve_col = 0;
            }
            KeyCode::Right => {
                app.fan_curve_col = 1;
            }
            KeyCode::Enter => {
                if let Some(curve) = &app.fan_curve {
                    let val = if app.fan_curve_col == 0 { curve[app.fan_curve_selected].0 } else { curve[app.fan_curve_selected].1 };
                    let target = if app.fan_curve_col == 0 { EditTarget::FanCurveTemp(app.fan_curve_selected) } else { EditTarget::FanCurveSpeed(app.fan_curve_selected) };
                    app.start_edit(target, Some(val));
                }
            }
            _ => {}
        }
        return false;
    }

    match key.code {
        KeyCode::Char('q') => return true,
        KeyCode::Char('r') => {
            app.refresh();
            app.set_status("Refreshed values");
        }
        KeyCode::Up => app.move_selection(-1),
        KeyCode::Down => app.move_selection(1),
        KeyCode::Left => match app.selected_item() {
            Item::FanMode => app.cycle(FAN_MODE, app.fan_mode, FAN_MODE_COUNT, -1, "Fan mode"),
            Item::ChargeMode => app.cycle(CHARGE_MODE, app.charge_mode, CHARGE_MODE_COUNT, -1, "Charge mode"),
            Item::GpuBoost => app.toggle_gpu_boost(),
            _ => {}
        },
        KeyCode::Right => match app.selected_item() {
            Item::FanMode => app.cycle(FAN_MODE, app.fan_mode, FAN_MODE_COUNT, 1, "Fan mode"),
            Item::ChargeMode => app.cycle(CHARGE_MODE, app.charge_mode, CHARGE_MODE_COUNT, 1, "Charge mode"),
            Item::GpuBoost => app.toggle_gpu_boost(),
            _ => {}
        },
        KeyCode::Enter => match app.selected_item() {
            Item::FanMode => app.cycle(FAN_MODE, app.fan_mode, FAN_MODE_COUNT, 1, "Fan mode"),
            Item::FanCustomSpeed => app.start_edit(EditTarget::FanCustomSpeed, app.fan_custom_speed),
            Item::ChargeMode => app.cycle(CHARGE_MODE, app.charge_mode, CHARGE_MODE_COUNT, 1, "Charge mode"),
            Item::ChargeLimit => app.start_edit(EditTarget::ChargeLimit, app.charge_limit),
            Item::GpuBoost => app.toggle_gpu_boost(),
            Item::FanCurveEdit => app.focus = Focus::FanCurveList,
            Item::FanCurveView => {},
            Item::History => {},
            Item::Refresh => {
                app.refresh();
                app.set_status("Refreshed values");
            }
            Item::Quit => return true,
        },
        KeyCode::Char('e') => match app.selected_item() {
            Item::FanCustomSpeed => app.start_edit(EditTarget::FanCustomSpeed, app.fan_custom_speed),
            Item::ChargeLimit => app.start_edit(EditTarget::ChargeLimit, app.charge_limit),
            Item::FanCurveEdit => app.focus = Focus::FanCurveList, 
            _ => {}
        },        
        _ => {}
    }

    false
}

// --- Entry Point & Setup ---

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    terminal::enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alternate screen")?;
    Terminal::new(CrosstermBackend::new(stdout)).context("create terminal")
}

fn restore_terminal(mut terminal: Terminal<CrosstermBackend<Stdout>>) {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
}

/// Ensures a panic doesn't leave the user's terminal stuck in raw/alternate-screen mode.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load();
    match cli.command {
        Some(command) => run_cli(command, &config),
        None => run_tui(config),
    }
}

fn run_tui(config: Config) -> Result<()> {
    if !is_root() {
        println!("This program requires root privileges.");
        print!("Do you want to run with sudo? [Y/n]: ");

        use std::io::{self, Write};
        io::stdout().flush().ok();

        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();

        let input = input.trim().to_lowercase();

        if input.is_empty() || input == "y" {
            run_sudo()?;
        } else {
            println!("Exiting.");
            std::process::exit(1);
        }
    }

    if !driver_present() {
        eprintln!("{}", driver_missing_message());
        std::process::exit(1);
    }

    install_panic_hook();
    let refresh_interval = Duration::from_millis(config.refresh_interval_ms.max(100));
    let mut terminal = setup_terminal()?;
    let mut app = App::new(config);
    app.refresh();

    let tick_rate = Duration::from_millis(250);
    let mut last_tick = Instant::now();

    let run = (|| -> Result<()> {
        loop {
            // Auto-refresh at the configured interval
            if app.last_refresh.elapsed() >= refresh_interval {
                app.refresh();
            }

            terminal.draw(|f| ui(f, &app)).context("draw ui")?;

            let timeout = tick_rate.saturating_sub(last_tick.elapsed());
            if event::poll(timeout).context("poll events")?
                && let Event::Key(key) = event::read().context("read event")?
                && handle_key(&mut app, key)
            {
                break;
            }

            if last_tick.elapsed() >= tick_rate {
                last_tick = Instant::now();
            }
        }

        Ok(())
    })();

    restore_terminal(terminal);
    run
}