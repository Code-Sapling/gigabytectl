//! Access to the `gigabyte-laptop-wmi` sysfs interface.
//!
//! Every node lives under [`ROOT`] and holds a single scalar value, so reads and
//! writes are plain text file operations.

use std::{borrow::Cow, fs, io, ops::RangeInclusive, path::Path};

use anyhow::{Context, Result, anyhow, ensure};

/// Declares the driver root plus one `&'static str` constant per attribute, so
/// the root path is written exactly once, and a table of them all for
/// reporting which attributes a model actually supports.
macro_rules! sysfs_nodes {
    ($root:literal, $($name:ident => $file:literal),+ $(,)?) => {
        /// Platform directory exposed by the `gigabyte-laptop-wmi` module.
        pub const ROOT: &str = $root;
        $(pub const $name: &str = concat!($root, "/", $file);)+

        /// Every attribute this tool knows about, as `(name, path)`.
        pub const ALL_NODES: [(&str, &str); [$($file),+].len()] = [$(($file, $name)),+];
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

// --- Light sensor ---

/// Decodes the `light_sensor` node.
///
/// Newer models (VE and later) report four bytes as `<version> <low> <medium>
/// <high>`, where the reading is the last three recombined; older models report
/// a single 32-bit value. Either way `0` means no sensor is fitted.
pub fn parse_light_sensor(raw: &str) -> Option<u32> {
    let parts: Vec<u32> = raw.split_whitespace().map(str::parse).collect::<Result<_, _>>().ok()?;
    match parts[..] {
        [value] => Some(value),
        // The leading byte is a format version, not part of the reading.
        [_, low, medium, high] => Some((high << 16) | (medium << 8) | low),
        _ => None,
    }
}

/// Human-readable light sensor reading.
pub fn light_sensor_text(raw: Option<&str>) -> String {
    match raw.map(str::trim) {
        None => "N/A".to_string(),
        Some(raw) => match parse_light_sensor(raw) {
            Some(0) => "Not equipped".to_string(),
            Some(value) => value.to_string(),
            // Show whatever the driver said rather than hiding an unknown format.
            None => format!("Unknown ({raw})"),
        },
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

/// Checks that a whole curve is non-decreasing in both temperature and speed,
/// which is what the driver documents the hardware expects. A curve that dips
/// can make the fans slow down as the machine gets hotter.
pub fn validate_fan_curve(curve: &[(i32, i32)]) -> Result<()> {
    for (index, &(temp, speed)) in curve.iter().enumerate() {
        check_range(temp, &CURVE_TEMP_RANGE, "Temperature")?;
        check_range(speed, &CURVE_SPEED_RANGE, "Speed")?;
        let Some(&(previous_temp, previous_speed)) = index.checked_sub(1).and_then(|i| curve.get(i)) else {
            continue;
        };
        ensure!(
            temp >= previous_temp,
            "Fan curve must not go backwards: point {index} temperature {temp} is below point {} ({previous_temp})",
            index - 1
        );
        ensure!(
            speed >= previous_speed,
            "Fan curve must not go backwards: point {index} speed {speed} is below point {} ({previous_speed})",
            index - 1
        );
    }
    Ok(())
}

/// The values a single point may take without breaking the ordering of the rest
/// of `curve`, as `(min, max)` for each of temperature and speed.
fn curve_point_bounds(curve: &[(i32, i32)], index: usize) -> ((i32, i32), (i32, i32)) {
    let before = index.checked_sub(1).and_then(|i| curve.get(i));
    let after = curve.get(index + 1);
    (
        (
            before.map_or(*CURVE_TEMP_RANGE.start(), |p| p.0),
            after.map_or(*CURVE_TEMP_RANGE.end(), |p| p.0),
        ),
        (
            before.map_or(*CURVE_SPEED_RANGE.start(), |p| p.1),
            after.map_or(*CURVE_SPEED_RANGE.end(), |p| p.1),
        ),
    )
}

/// Checks one edited point against the curve it is going into, reporting the
/// range it may take so the value can be corrected without guesswork.
pub fn validate_curve_point_in(curve: &[(i32, i32)], index: usize, temp: i32, speed: i32) -> Result<()> {
    validate_curve_index(index)?;
    check_range(temp, &CURVE_TEMP_RANGE, "Temperature")?;
    check_range(speed, &CURVE_SPEED_RANGE, "Speed")?;
    let ((temp_min, temp_max), (speed_min, speed_max)) = curve_point_bounds(curve, index);
    ensure!(
        (temp_min..=temp_max).contains(&temp),
        "Temperature must be {temp_min}..{temp_max} to keep the curve in order (neighbouring points)"
    );
    ensure!(
        (speed_min..=speed_max).contains(&speed),
        "Speed must be {speed_min}..{speed_max} to keep the curve in order (neighbouring points)"
    );
    Ok(())
}

/// Writes one `(temperature, speed)` point of the fan curve.
///
/// This does not check the point against its neighbours; callers editing a
/// loaded curve should use [`validate_curve_point_in`] first.
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

// --- Setting dependencies ---

/// Fan mode values that other settings depend on.
const FAN_MODE_CUSTOM: i32 = 3;
const FAN_MODE_AUTO: i32 = 4;
const FAN_MODE_FIXED: i32 = 5;
const CHARGE_MODE_CUSTOM: i32 = 1;

/// A setting whose value only reaches the hardware while another node is in a
/// particular mode. Writing one of these otherwise succeeds and does nothing,
/// which is the most common source of confusion with this driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dependent {
    FanCustomSpeed,
    FanCurve,
    ChargeLimit,
}

impl Dependent {
    /// What has to be true for this setting to take effect.
    pub fn requirement(self) -> &'static str {
        match self {
            Self::FanCustomSpeed => "requires Auto or Fixed fan mode",
            Self::FanCurve => "requires Custom fan mode",
            Self::ChargeLimit => "requires Custom charge mode",
        }
    }
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
        light_sensor_text(self.light_sensor.as_deref())
    }

    /// Whether a mode-dependent setting is currently doing anything.
    pub fn is_active(&self, setting: Dependent) -> bool {
        match setting {
            Dependent::FanCustomSpeed => matches!(self.fan_mode, Some(FAN_MODE_AUTO | FAN_MODE_FIXED)),
            Dependent::FanCurve => self.fan_mode == Some(FAN_MODE_CUSTOM),
            Dependent::ChargeLimit => self.charge_mode == Some(CHARGE_MODE_CUSTOM),
        }
    }

    /// `Some(reason)` when the setting currently has no effect. Reads that
    /// failed are reported as active, so an unreadable node never produces a
    /// misleading warning.
    pub fn inactive_reason(&self, setting: Dependent) -> Option<&'static str> {
        let known = match setting {
            Dependent::FanCustomSpeed | Dependent::FanCurve => self.fan_mode.is_some(),
            Dependent::ChargeLimit => self.charge_mode.is_some(),
        };
        (known && !self.is_active(setting)).then(|| setting.requirement())
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
    fn mode_dependent_settings_report_when_they_are_inert() {
        let with_modes = |fan_mode, charge_mode| HwState {
            fan_mode: Some(fan_mode),
            charge_mode: Some(charge_mode),
            ..HwState::default()
        };

        // Custom fan speed only applies in Auto (4) or Fixed (5).
        let fixed = with_modes(5, 0);
        assert!(fixed.is_active(Dependent::FanCustomSpeed));
        assert_eq!(fixed.inactive_reason(Dependent::FanCustomSpeed), None);
        assert!(with_modes(4, 0).is_active(Dependent::FanCustomSpeed));
        assert_eq!(
            with_modes(0, 0).inactive_reason(Dependent::FanCustomSpeed),
            Some("requires Auto or Fixed fan mode")
        );

        // The fan curve only applies in Custom (3).
        assert!(with_modes(3, 0).is_active(Dependent::FanCurve));
        assert_eq!(
            with_modes(5, 0).inactive_reason(Dependent::FanCurve),
            Some("requires Custom fan mode")
        );

        // The charge limit only applies in Custom charge mode (1).
        assert!(with_modes(0, 1).is_active(Dependent::ChargeLimit));
        assert_eq!(
            with_modes(0, 0).inactive_reason(Dependent::ChargeLimit),
            Some("requires Custom charge mode")
        );

        // An unreadable mode must not produce a misleading warning.
        let unknown = HwState::default();
        assert_eq!(unknown.inactive_reason(Dependent::FanCustomSpeed), None);
        assert_eq!(unknown.inactive_reason(Dependent::ChargeLimit), None);
    }

    #[test]
    fn curves_must_not_go_backwards() {
        let rising: Vec<(i32, i32)> = (0..FAN_CURVE_POINTS).map(|i| (i as i32 * 5, i as i32 * 10)).collect();
        assert!(validate_fan_curve(&rising).is_ok());
        // Repeated points are allowed; the driver asks for non-decreasing.
        assert!(validate_fan_curve(&[(40, 100), (40, 100), (50, 120)]).is_ok());
        assert!(validate_fan_curve(&[]).is_ok());

        let dipping_temp = validate_fan_curve(&[(40, 100), (30, 120)]).unwrap_err().to_string();
        assert!(dipping_temp.contains("temperature 30 is below point 0 (40)"), "{dipping_temp}");
        let dipping_speed = validate_fan_curve(&[(40, 100), (50, 80)]).unwrap_err().to_string();
        assert!(dipping_speed.contains("speed 80 is below point 0 (100)"), "{dipping_speed}");
        // Out-of-range values are still caught.
        assert!(validate_fan_curve(&[(200, 10)]).is_err());
    }

    #[test]
    fn editing_a_point_is_bounded_by_its_neighbours() {
        let curve = [(30, 40), (50, 100), (70, 200)];
        // The middle point may move between its neighbours, but not past them.
        assert!(validate_curve_point_in(&curve, 1, 60, 150).is_ok());
        assert!(validate_curve_point_in(&curve, 1, 30, 40).is_ok());
        assert!(validate_curve_point_in(&curve, 1, 29, 100).is_err());
        assert!(validate_curve_point_in(&curve, 1, 71, 100).is_err());
        assert!(validate_curve_point_in(&curve, 1, 60, 201).is_err());

        // The ends are bounded by the hardware range on the open side.
        assert!(validate_curve_point_in(&curve, 0, 0, 0).is_ok());
        assert!(validate_curve_point_in(&curve, 2, 100, 255).is_ok());

        let message = validate_curve_point_in(&curve, 1, 80, 100).unwrap_err().to_string();
        assert!(message.contains("must be 30..70"), "{message}");
    }

    #[test]
    fn light_sensor_decodes_both_reporting_formats() {
        // Newer models: leading version byte, then low/medium/high.
        assert_eq!(parse_light_sensor("247 1 2 3"), Some(3 * 65536 + 2 * 256 + 1));
        assert_eq!(parse_light_sensor("247 255 255 255"), Some(0x00FF_FFFF));
        // Older models: a single 32-bit value.
        assert_eq!(parse_light_sensor("4096"), Some(4096));
        // Anything else is not guessed at.
        assert_eq!(parse_light_sensor("1 2"), None);
        assert_eq!(parse_light_sensor(""), None);
        assert_eq!(parse_light_sensor("-1"), None);
    }

    #[test]
    fn light_sensor_zero_means_no_sensor_fitted() {
        assert_eq!(light_sensor_text(Some("0")), "Not equipped");
        assert_eq!(light_sensor_text(Some("247 0 0 0")), "Not equipped");
        assert_eq!(light_sensor_text(Some("247 16 1 0")), "272");
        assert_eq!(light_sensor_text(Some("1 2")), "Unknown (1 2)");
        assert_eq!(light_sensor_text(None), "N/A");
    }

    #[test]
    fn battery_cycle_zero_means_unsupported() {
        assert_eq!(battery_cycle_text(Some("0")), "Device does not support this feature");
        assert_eq!(battery_cycle_text(Some("42")), "42");
        assert_eq!(battery_cycle_text(None), "N/A");
    }
}
