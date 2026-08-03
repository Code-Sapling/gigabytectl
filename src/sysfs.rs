//! Access to the `gigabyte-laptop-wmi` sysfs interface.
//!
//! Every node lives under [`ROOT`] and holds a single scalar value, so reads and
//! writes are plain text file operations.

use std::{borrow::Cow, fs, io, ops::RangeInclusive, path::Path};

use anyhow::{Context, Result, anyhow, ensure};

/// Declares the driver root plus one `&'static str` constant per attribute, so
/// the root path is written exactly once.
macro_rules! sysfs_nodes {
    ($root:literal, $($name:ident => $file:literal),+ $(,)?) => {
        /// Platform directory exposed by the `gigabyte-laptop-wmi` module.
        pub const ROOT: &str = $root;
        $(pub const $name: &str = concat!($root, "/", $file);)+
    };
}

sysfs_nodes! {
    "/sys/devices/platform/aorus_laptop",
    FAN_MODE => "fan_mode",
    FAN_CUSTOM_SPEED => "fan_custom_speed",
    FAN_PWM => "fan_pwm",
    FAN_CURVE_INDEX => "fan_curve_index",
    FAN_CURVE_DATA => "fan_curve_data",
    CHARGE_MODE => "charge_mode",
    CHARGE_LIMIT => "charge_limit",
    BATTERY_CYCLE => "battery_cycle",
    GPU_BOOST => "gpu_boost",
    LIGHT_SENSOR => "light_sensor",
}

/// Fan modes, indexed by the value stored in [`FAN_MODE`].
pub const FAN_MODES: [&str; 6] = ["Normal", "Silent", "Gaming", "Custom", "Auto", "Fixed"];
/// Charging modes, indexed by the value stored in [`CHARGE_MODE`].
pub const CHARGE_MODES: [&str; 2] = ["Normal", "Custom"];
/// Number of (temperature, speed) points in the hardware fan curve.
pub const FAN_CURVE_POINTS: usize = 15;

pub const FAN_SPEED_RANGE: RangeInclusive<i32> = 0..=255;
pub const CHARGE_LIMIT_RANGE: RangeInclusive<i32> = 60..=100;
pub const CURVE_TEMP_RANGE: RangeInclusive<i32> = 0..=100;
pub const CURVE_SPEED_RANGE: RangeInclusive<i32> = 0..=255;

// --- Node access ---

pub fn driver_present() -> bool {
    Path::new(ROOT).exists()
}

pub fn driver_missing_message() -> String {
    format!("{ROOT} does not exist. Please install gigabyte-laptop-wmi and ensure it is running.")
}

/// Reads a node as a trimmed string, or `None` if it is missing or unreadable.
pub fn read_trimmed(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Reads a node as an integer, or `None` if it is missing or not a number.
pub fn read_i32(path: &str) -> Option<i32> {
    read_trimmed(path).and_then(|s| s.parse().ok())
}

/// Writes an integer to a node, translating the common `io` failures into
/// messages that say what to do about them.
pub fn write_value(path: &str, value: i32) -> Result<()> {
    fs::write(path, format!("{value}\n")).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => anyhow!("Node not found: {path}"),
        io::ErrorKind::PermissionDenied => anyhow!("Permission denied writing {path} (run as root)"),
        _ => anyhow::Error::new(e).context(format!("writing {value} to {path}")),
    })
}

// --- Validation ---

fn check_range(value: i32, range: &RangeInclusive<i32>, what: &str) -> Result<()> {
    ensure!(range.contains(&value), "{what} must be {}..{}", range.start(), range.end());
    Ok(())
}

pub fn validate_fan_speed(value: i32) -> Result<()> {
    check_range(value, &FAN_SPEED_RANGE, "Fan speed")
}

pub fn validate_charge_limit(value: i32) -> Result<()> {
    check_range(value, &CHARGE_LIMIT_RANGE, "Charge limit")
}

pub fn validate_curve_index(index: usize) -> Result<()> {
    ensure!(index < FAN_CURVE_POINTS, "Index must be 0..{FAN_CURVE_POINTS}");
    Ok(())
}

// --- Named values ---

/// Looks up the display name for a node value in `table`.
fn name_of(table: &[&'static str], value: Option<i32>) -> Cow<'static, str> {
    match value {
        Some(i) => usize::try_from(i)
            .ok()
            .and_then(|i| table.get(i))
            .map_or_else(|| Cow::Owned(format!("Unknown ({i})")), |name| Cow::Borrowed(*name)),
        None => Cow::Borrowed("N/A"),
    }
}

/// Resolves a case-insensitive name from `table` back to its node value.
fn value_of(table: &[&'static str], name: &str, what: &str) -> Result<i32> {
    table
        .iter()
        .position(|candidate| candidate.eq_ignore_ascii_case(name.trim()))
        .map(|i| i as i32)
        .ok_or_else(|| anyhow!("Unknown {what} '{name}' (expected one of: {})", table.join(", ").to_lowercase()))
}

pub fn fan_mode_name(value: Option<i32>) -> Cow<'static, str> {
    name_of(&FAN_MODES, value)
}

pub fn charge_mode_name(value: Option<i32>) -> Cow<'static, str> {
    name_of(&CHARGE_MODES, value)
}

pub fn fan_mode_value(name: &str) -> Result<i32> {
    value_of(&FAN_MODES, name, "fan mode")
}

pub fn charge_mode_value(name: &str) -> Result<i32> {
    value_of(&CHARGE_MODES, name, "charge mode")
}

pub fn gpu_boost_name(value: Option<i32>) -> &'static str {
    match value {
        Some(0) => "OFF",
        Some(_) => "ON",
        None => "N/A",
    }
}

/// Parses the boolean spellings accepted in profiles (`on`/`off`, `1`/`0`, ...).
pub fn on_off_value(name: &str) -> Result<i32> {
    match name.trim().to_ascii_lowercase().as_str() {
        "on" | "1" | "true" | "yes" => Ok(1),
        "off" | "0" | "false" | "no" => Ok(0),
        other => Err(anyhow!("Expected on/off, got '{other}'")),
    }
}

/// Formats an optional node value for display.
pub fn value_or_na(value: Option<i32>) -> String {
    value.map_or_else(|| "N/A".to_string(), |v| v.to_string())
}

/// The driver reports `0` cycles on models without a battery cycle counter.
pub fn battery_cycle_text(value: Option<&str>) -> String {
    match value {
        Some("0") => "Device does not support this feature".to_string(),
        Some(text) => text.to_string(),
        None => "N/A".to_string(),
    }
}

// --- Fan curve ---

/// Reads all [`FAN_CURVE_POINTS`] `(temperature, speed)` points.
///
/// Each point is selected by writing its index to [`FAN_CURVE_INDEX`] before
/// reading [`FAN_CURVE_DATA`], so this both writes and reads the device.
pub fn read_fan_curve() -> Result<Vec<(i32, i32)>> {
    (0..FAN_CURVE_POINTS).map(read_fan_curve_point).collect()
}

fn read_fan_curve_point(index: usize) -> Result<(i32, i32)> {
    write_value(FAN_CURVE_INDEX, index as i32).with_context(|| format!("selecting fan curve index {index}"))?;
    let data = read_trimmed(FAN_CURVE_DATA).with_context(|| format!("reading fan curve data at index {index}"))?;
    parse_fan_curve_point(&data).with_context(|| format!("parsing fan curve data {data:?} at index {index}"))
}

/// Parses the `"<temp> <speed>"` pair reported by [`FAN_CURVE_DATA`].
fn parse_fan_curve_point(data: &str) -> Result<(i32, i32)> {
    let mut parts = data.split_whitespace().map(str::parse::<i32>);
    match (parts.next(), parts.next()) {
        (Some(Ok(temp)), Some(Ok(speed))) => Ok((temp, speed)),
        _ => Err(anyhow!("expected two integers")),
    }
}

/// Writes one `(temperature, speed)` point of the fan curve.
pub fn write_fan_curve_point(index: usize, temp: i32, speed: i32) -> Result<()> {
    validate_curve_index(index)?;
    check_range(temp, &CURVE_TEMP_RANGE, "Temperature")?;
    check_range(speed, &CURVE_SPEED_RANGE, "Speed")?;
    write_value(FAN_CURVE_INDEX, index as i32)?;
    write_value(FAN_CURVE_DATA, pack_fan_curve_point(temp, speed))
}

/// The data node takes both halves of a point packed into one integer.
fn pack_fan_curve_point(temp: i32, speed: i32) -> i32 {
    (speed * 256) + temp
}

// --- Snapshot ---

/// One read of every scalar node, taken together so the UI and the CLI show a
/// consistent picture.
#[derive(Debug, Default, Clone)]
pub struct HwState {
    pub fan_mode: Option<i32>,
    pub fan_custom_speed: Option<i32>,
    pub fan_pwm: Option<i32>,
    pub charge_mode: Option<i32>,
    pub charge_limit: Option<i32>,
    pub battery_cycle: Option<String>,
    pub gpu_boost: Option<i32>,
    pub light_sensor: Option<String>,
}

impl HwState {
    pub fn read() -> Self {
        Self {
            fan_mode: read_i32(FAN_MODE),
            fan_custom_speed: read_i32(FAN_CUSTOM_SPEED),
            fan_pwm: read_i32(FAN_PWM),
            charge_mode: read_i32(CHARGE_MODE),
            charge_limit: read_i32(CHARGE_LIMIT),
            battery_cycle: read_trimmed(BATTERY_CYCLE),
            gpu_boost: read_i32(GPU_BOOST),
            light_sensor: read_trimmed(LIGHT_SENSOR),
        }
    }

    pub fn battery_cycle_text(&self) -> String {
        battery_cycle_text(self.battery_cycle.as_deref())
    }

    pub fn light_sensor_text(&self) -> String {
        self.light_sensor.clone().unwrap_or_else(|| "N/A".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_paths_hang_off_the_driver_root() {
        assert_eq!(FAN_MODE, "/sys/devices/platform/aorus_laptop/fan_mode");
        assert!(FAN_CURVE_DATA.starts_with(ROOT));
    }

    #[test]
    fn names_round_trip_through_values() {
        for (value, name) in FAN_MODES.iter().enumerate() {
            assert_eq!(fan_mode_name(Some(value as i32)), *name);
            assert_eq!(fan_mode_value(name).unwrap(), value as i32);
            assert_eq!(fan_mode_value(&name.to_uppercase()).unwrap(), value as i32);
        }
        assert_eq!(charge_mode_value("custom").unwrap(), 1);
    }

    #[test]
    fn unknown_names_and_values_are_reported_not_panicked_on() {
        assert_eq!(fan_mode_name(Some(99)), "Unknown (99)");
        assert_eq!(fan_mode_name(Some(-1)), "Unknown (-1)");
        assert_eq!(fan_mode_name(None), "N/A");
        assert!(fan_mode_value("turbo").is_err());
    }

    #[test]
    fn on_off_accepts_the_documented_spellings() {
        for yes in ["on", "ON", " 1 ", "true", "yes"] {
            assert_eq!(on_off_value(yes).unwrap(), 1);
        }
        for no in ["off", "0", "FALSE", "no"] {
            assert_eq!(on_off_value(no).unwrap(), 0);
        }
        assert!(on_off_value("maybe").is_err());
    }

    #[test]
    fn ranges_are_enforced_at_both_ends() {
        assert!(validate_fan_speed(0).is_ok() && validate_fan_speed(255).is_ok());
        assert!(validate_fan_speed(-1).is_err() && validate_fan_speed(256).is_err());
        assert!(validate_charge_limit(60).is_ok() && validate_charge_limit(100).is_ok());
        assert!(validate_charge_limit(59).is_err() && validate_charge_limit(101).is_err());
        assert!(validate_curve_index(FAN_CURVE_POINTS - 1).is_ok());
        assert!(validate_curve_index(FAN_CURVE_POINTS).is_err());
        assert_eq!(validate_fan_speed(300).unwrap_err().to_string(), "Fan speed must be 0..255");
    }

    #[test]
    fn fan_curve_points_pack_and_parse() {
        assert_eq!(pack_fan_curve_point(50, 128), 128 * 256 + 50);
        assert_eq!(parse_fan_curve_point("50 128").unwrap(), (50, 128));
        assert_eq!(parse_fan_curve_point("  7\t9\n").unwrap(), (7, 9));
        assert!(parse_fan_curve_point("50").is_err());
        assert!(parse_fan_curve_point("").is_err());
    }

    #[test]
    fn battery_cycle_zero_means_unsupported() {
        assert_eq!(battery_cycle_text(Some("0")), "Device does not support this feature");
        assert_eq!(battery_cycle_text(Some("42")), "42");
        assert_eq!(battery_cycle_text(None), "N/A");
    }
}
