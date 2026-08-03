//! Command-line interface: argument definitions and one-shot commands.

use std::{fs, io, path::Path, thread, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use serde::Serialize;

use crate::{
    config::{self, Config, Profile, Units},
    ppd,
    sensors::{Fan, Sensors},
    sysfs::{self, HwState},
    system,
};

#[derive(Parser)]
#[command(
    name = "gigabytectl",
    version,
    about = "Control panel for gigabyte-laptop-wmi",
    long_about = "Control panel for gigabyte-laptop-wmi.\n\nRun without a subcommand to launch the interactive TUI, or pass a subcommand to run a one-shot, scriptable command."
)]
pub struct Cli {
    /// Run a one-shot command instead of launching the TUI
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
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

impl Commands {
    /// Whether the command touches the driver, and so needs root and a loaded
    /// `gigabyte-laptop-wmi` module. The rest read only sensors or user config,
    /// or check their own requirements as they go.
    fn needs_hardware(&self) -> bool {
        !matches!(
            self,
            Self::Monitor { .. }
                | Self::Profile(_)
                | Self::Sync { .. }
                | Self::Completions { .. }
                | Self::InstallService
        )
    }
}

#[derive(Args)]
pub struct ProfileArgs {
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
pub enum FanModeAction {
    /// Print the current value
    Get,
    /// Set a new value
    Set { mode: FanModeArg },
}

#[derive(Subcommand)]
pub enum ValueAction {
    /// Print the current value
    Get,
    /// Set a new value
    Set { value: i32 },
}

#[derive(Subcommand)]
pub enum ChargeModeAction {
    /// Print the current value
    Get,
    /// Set a new value
    Set { mode: ChargeModeArg },
}

#[derive(Subcommand)]
pub enum OnOffAction {
    /// Print the current value
    Get,
    /// Set a new value
    Set { state: OnOff },
}

#[derive(Subcommand)]
pub enum FanCurveAction {
    /// Print one point (if index given) or the whole curve
    Get { index: Option<usize> },
    /// Set the (temp, speed) pair at an index (0..15)
    Set { index: usize, temp: i32, speed: i32 },
}

#[derive(ValueEnum, Clone, Copy)]
#[value(rename_all = "kebab-case")]
pub enum FanModeArg {
    Normal,
    Silent,
    Gaming,
    Custom,
    Auto,
    Fixed,
}

#[derive(ValueEnum, Clone, Copy)]
#[value(rename_all = "kebab-case")]
pub enum ChargeModeArg {
    Normal,
    Custom,
}

#[derive(ValueEnum, Clone, Copy)]
#[value(rename_all = "kebab-case")]
pub enum OnOff {
    On,
    Off,
}

impl FanModeArg {
    fn value(self) -> i32 {
        match self {
            Self::Normal => 0,
            Self::Silent => 1,
            Self::Gaming => 2,
            Self::Custom => 3,
            Self::Auto => 4,
            Self::Fixed => 5,
        }
    }
}

impl ChargeModeArg {
    fn value(self) -> i32 {
        match self {
            Self::Normal => 0,
            Self::Custom => 1,
        }
    }
}

impl OnOff {
    fn value(self) -> i32 {
        match self {
            Self::On => 1,
            Self::Off => 0,
        }
    }
}

// --- Dispatch ---

pub fn run(command: Commands, config: &Config) -> Result<()> {
    if command.needs_hardware() {
        require_hardware()?;
    }

    match command {
        Commands::Status { json } => print_status(json, config.units)?,
        Commands::FanMode { action } => match action {
            FanModeAction::Get => println!("{}", sysfs::fan_mode_name(sysfs::read_i32(sysfs::FAN_MODE)).to_lowercase()),
            FanModeAction::Set { mode } => sysfs::write_value(sysfs::FAN_MODE, mode.value())?,
        },
        Commands::FanSpeed { action } => match action {
            ValueAction::Get => println!("{}", sysfs::value_or_na(sysfs::read_i32(sysfs::FAN_CUSTOM_SPEED))),
            ValueAction::Set { value } => {
                sysfs::validate_fan_speed(value)?;
                sysfs::write_value(sysfs::FAN_CUSTOM_SPEED, value)?;
            }
        },
        Commands::ChargeMode { action } => match action {
            ChargeModeAction::Get => {
                println!(
                    "{}",
                    sysfs::charge_mode_name(sysfs::read_i32(sysfs::CHARGE_MODE)).to_lowercase()
                );
            }
            ChargeModeAction::Set { mode } => sysfs::write_value(sysfs::CHARGE_MODE, mode.value())?,
        },
        Commands::ChargeLimit { action } => match action {
            ValueAction::Get => println!("{}", sysfs::value_or_na(sysfs::read_i32(sysfs::CHARGE_LIMIT))),
            ValueAction::Set { value } => {
                sysfs::validate_charge_limit(value)?;
                sysfs::write_value(sysfs::CHARGE_LIMIT, value)?;
            }
        },
        Commands::GpuBoost { action } => match action {
            OnOffAction::Get => println!("{}", sysfs::gpu_boost_name(sysfs::read_i32(sysfs::GPU_BOOST)).to_lowercase()),
            OnOffAction::Set { state } => sysfs::write_value(sysfs::GPU_BOOST, state.value())?,
        },
        Commands::BatteryCycle => {
            println!(
                "{}",
                sysfs::battery_cycle_text(sysfs::read_trimmed(sysfs::BATTERY_CYCLE).as_deref())
            );
        }
        Commands::LightSensor => println!("{}", HwState::read().light_sensor_text()),
        Commands::FanPwm => println!("{}", sysfs::value_or_na(sysfs::read_i32(sysfs::FAN_PWM))),
        Commands::Fans => print_fans(&Sensors::new().read_fans()),
        Commands::FanCurve { action } => run_fan_curve(action)?,
        Commands::Monitor { interval, json } => run_monitor(interval, json, config)?,
        Commands::Profile(args) => run_profile(&args)?,
        Commands::Sync { once } => run_sync(once)?,
        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            generate(shell, &mut cmd, name, &mut io::stdout());
        }
        Commands::InstallService => install_service()?,
    }
    Ok(())
}

fn require_hardware() -> Result<()> {
    ensure!(
        system::is_root(),
        "gigabytectl: must be run as root (try: sudo gigabytectl ...)"
    );
    ensure!(sysfs::driver_present(), "{}", sysfs::driver_missing_message());
    Ok(())
}

// --- Commands ---

fn run_fan_curve(action: FanCurveAction) -> Result<()> {
    match action {
        FanCurveAction::Get { index } => {
            let curve = sysfs::read_fan_curve()?;
            let points: Vec<(usize, (i32, i32))> = match index {
                Some(index) => {
                    sysfs::validate_curve_index(index)?;
                    vec![(index, curve[index])]
                }
                None => curve.into_iter().enumerate().collect(),
            };
            for (index, (temp, speed)) in points {
                println!("{index} {temp} {speed}");
            }
        }
        FanCurveAction::Set { index, temp, speed } => sysfs::write_fan_curve_point(index, temp, speed)?,
    }
    Ok(())
}

fn print_fans(fans: &[Fan]) {
    if fans.is_empty() {
        println!("No live fan readings available");
    }
    for fan in fans {
        println!("{}: {} RPM", fan.name, fan.rpm);
    }
}

/// JSON shape of `status --json`. Field order here is the emitted order.
#[derive(Serialize)]
struct StatusJson<'a> {
    fan_mode: String,
    fan_speed: String,
    charge_mode: String,
    charge_limit: String,
    gpu_boost: String,
    battery_cycle: String,
    light_sensor: String,
    fan_pwm: String,
    cpu_temp: Option<f64>,
    gpu_temp: Option<f64>,
    fans: Vec<FanJson<'a>>,
}

/// JSON shape of one `monitor --json` sample.
#[derive(Serialize)]
struct SampleJson<'a> {
    cpu_temp: Option<f64>,
    gpu_temp: Option<f64>,
    fans: Vec<FanJson<'a>>,
}

#[derive(Serialize)]
struct FanJson<'a> {
    name: &'a str,
    rpm: u32,
}

fn fans_json(fans: &[Fan]) -> Vec<FanJson<'_>> {
    fans.iter()
        .map(|fan| FanJson { name: &fan.name, rpm: fan.rpm })
        .collect()
}

fn print_status(json: bool, units: Units) -> Result<()> {
    let hw = HwState::read();
    let mut sensors = Sensors::new();
    let fans = sensors.read_fans();
    let temps = sensors.read_temps();

    let fan_mode = sysfs::fan_mode_name(hw.fan_mode);
    let charge_mode = sysfs::charge_mode_name(hw.charge_mode);
    let gpu_boost = sysfs::gpu_boost_name(hw.gpu_boost);
    let fan_speed = sysfs::value_or_na(hw.fan_custom_speed);
    let charge_limit = sysfs::value_or_na(hw.charge_limit);
    let fan_pwm = sysfs::value_or_na(hw.fan_pwm);
    let battery_cycle = hw.battery_cycle_text();
    let light_sensor = hw.light_sensor_text();

    if json {
        let status = StatusJson {
            fan_mode: fan_mode.to_lowercase(),
            fan_speed,
            charge_mode: charge_mode.to_lowercase(),
            charge_limit,
            gpu_boost: gpu_boost.to_lowercase(),
            battery_cycle,
            light_sensor,
            fan_pwm,
            cpu_temp: units.to_json(temps.cpu),
            gpu_temp: units.to_json(temps.gpu),
            fans: fans_json(&fans),
        };
        println!("{}", serde_json::to_string(&status).context("serializing status")?);
    } else {
        let cpu_temp = units.format(temps.cpu);
        let gpu_temp = units.format(temps.gpu);
        let rows: [(&str, &str); 10] = [
            ("Fan mode", &fan_mode),
            ("Fan speed", &fan_speed),
            ("Charge mode", &charge_mode),
            ("Charge limit", &charge_limit),
            ("GPU boost", gpu_boost),
            ("Battery cycle", &battery_cycle),
            ("Light sensor", &light_sensor),
            ("Fan PWM", &fan_pwm),
            ("CPU temp", &cpu_temp),
            ("GPU temp", &gpu_temp),
        ];
        for (label, value) in rows {
            println!("{:<15}{value}", format!("{label}:"));
        }
        if !fans.is_empty() {
            println!("Fans:");
            for fan in &fans {
                println!("  {}: {} RPM", fan.name, fan.rpm);
            }
        }
    }
    Ok(())
}

fn run_monitor(interval: Option<f64>, json: bool, config: &Config) -> Result<()> {
    let period = match interval {
        Some(secs) => Duration::from_secs_f64(secs.max(0.1)),
        None => config.refresh_interval(),
    };
    let units = config.units;
    // Sensor paths are resolved once and reused for every sample.
    let mut sensors = Sensors::new();

    loop {
        let temps = sensors.read_temps();
        let fans = sensors.read_fans();

        if json {
            let sample = SampleJson {
                cpu_temp: units.to_json(temps.cpu),
                gpu_temp: units.to_json(temps.gpu),
                fans: fans_json(&fans),
            };
            println!("{}", serde_json::to_string(&sample).context("serializing sample")?);
        } else {
            let readings = if fans.is_empty() {
                "no fan data".to_string()
            } else {
                fans.iter()
                    .map(|f| format!("{}: {} RPM", f.name, f.rpm))
                    .collect::<Vec<_>>()
                    .join("   ")
            };
            println!("CPU {}   GPU {}   {readings}", units.format(temps.cpu), units.format(temps.gpu));
        }

        thread::sleep(period);
    }
}

fn run_profile(args: &ProfileArgs) -> Result<()> {
    if let Some(name) = &args.save {
        ensure!(
            system::is_root(),
            "gigabytectl: saving a profile reads hardware state; run as root (try: sudo gigabytectl profile --save {name})"
        );
        let mut profiles = config::load_profiles()?;
        profiles.insert(name.clone(), Profile::from_hardware());
        config::save_profiles(&profiles)?;
        println!("Saved profile '{name}'");
        return Ok(());
    }

    if args.list {
        let profiles = config::load_profiles()?;
        if profiles.is_empty() {
            println!("No profiles defined in {}", config::profiles_path().display());
        } else {
            let mut names: Vec<&String> = profiles.keys().collect();
            names.sort();
            for name in names {
                println!("{name}");
            }
        }
        return Ok(());
    }

    let Some(name) = &args.name else {
        bail!("Specify a profile to apply, or use --list / --save <name>");
    };

    require_hardware()?;
    let profiles = config::load_profiles()?;
    let profile = profiles
        .get(name)
        .with_context(|| format!("profile '{name}' not found in {}", config::profiles_path().display()))?;
    profile.apply()?;
    println!("Applied profile '{name}'");

    // Forward sync: if this profile maps to a power profile, switch PPD to match.
    if let Some(power_profile) = &profile.ppd_profile {
        match ppd::set(power_profile) {
            Ok(()) => println!("Set power profile -> {power_profile}"),
            Err(e) => eprintln!("Warning: {e:#}"),
        }
    }
    Ok(())
}

fn run_sync(once: bool) -> Result<()> {
    require_hardware()?;
    ensure!(
        ppd::is_available(),
        "power-profiles-daemon is not available on the system bus (is it installed and running?)"
    );

    // Start from whatever profile is active right now.
    apply_mapped_profile(&ppd::get()?)?;
    if once {
        return Ok(());
    }

    ppd::watch(|power_profile| {
        if let Err(e) = apply_mapped_profile(power_profile) {
            eprintln!("Warning: {e:#}");
        }
    })
}

/// Applies the gigabytectl profile mapped to `power_profile` (hardware only —
/// it deliberately does not touch PPD, so there is no feedback loop).
fn apply_mapped_profile(power_profile: &str) -> Result<()> {
    let profiles = config::load_profiles()?;
    match ppd::mapped_profile(&profiles, power_profile) {
        Some((name, profile)) => {
            profile
                .apply()
                .with_context(|| format!("applying gigabytectl profile '{name}'"))?;
            eprintln!("Power profile '{power_profile}' -> applied gigabytectl profile '{name}'");
        }
        None => eprintln!("Power profile '{power_profile}' has no mapped gigabytectl profile; nothing to apply"),
    }
    Ok(())
}

const SERVICE_NAME: &str = "gigabytectl-ppd-sync.service";
const SERVICE_PATH: &str = "/etc/systemd/system/gigabytectl-ppd-sync.service";

/// Installs and enables the systemd sync service. Because the service runs as
/// root (no `SUDO_USER`, `HOME=/root`), it can't see profiles in a user's home,
/// so a system-wide copy is seeded in `/etc/gigabytectl`, which profile loading
/// falls back to.
fn install_service() -> Result<()> {
    ensure!(
        system::is_root(),
        "gigabytectl: installing the sync service writes to /etc; run as root (try: sudo gigabytectl install-service)"
    );

    let exe = std::env::current_exe().context("resolving current executable path")?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);

    seed_system_profiles()?;

    fs::write(SERVICE_PATH, service_unit(&exe)).with_context(|| format!("writing {SERVICE_PATH}"))?;
    eprintln!("Wrote {SERVICE_PATH}");

    system::checked_status("systemctl", &["daemon-reload"])?;
    system::checked_status("systemctl", &["enable", "--now", SERVICE_NAME])?;
    eprintln!("Enabled and started {SERVICE_NAME}.");
    eprintln!("Check it with: systemctl status {SERVICE_NAME}");
    Ok(())
}

/// Copies the invoking user's profiles to the system-wide location the
/// root-run service reads.
fn seed_system_profiles() -> Result<()> {
    fs::create_dir_all(config::SYSTEM_CONFIG_DIR).with_context(|| format!("creating {}", config::SYSTEM_CONFIG_DIR))?;
    let system_profiles = Path::new(config::SYSTEM_CONFIG_DIR).join("profiles.toml");
    let user_profiles = config::profiles_path();

    match fs::read_to_string(&user_profiles) {
        Ok(text) => {
            fs::write(&system_profiles, text).with_context(|| format!("writing {}", system_profiles.display()))?;
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
    Ok(())
}

/// The systemd unit installed by `install-service`. Kept in sync with
/// `assets/gigabytectl-ppd-sync.service` by a test.
fn service_unit(exe: &Path) -> String {
    format!(
        "[Unit]
Description=gigabytectl power-profiles-daemon sync
Documentation=https://github.com/Code-Sapling/gigabytectl
# Apply the mapped gigabytectl profile whenever the system power profile changes.
After=power-profiles-daemon.service
Wants=power-profiles-daemon.service

[Service]
Type=simple
# Adjust the path if you installed elsewhere (e.g. /usr/bin/gigabytectl).
# This runs as root, so it reads profiles from /etc/gigabytectl/profiles.toml
# (not any user's ~/.config). `gigabytectl install-service` sets this up for you.
ExecStart={} sync
Restart=on-failure
RestartSec=2

[Install]
WantedBy=power-profiles-daemon.service
",
        exe.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn only_driver_commands_require_root() {
        assert!(Commands::Status { json: false }.needs_hardware());
        assert!(Commands::BatteryCycle.needs_hardware());
        assert!(Commands::Fans.needs_hardware());
        assert!(!Commands::Monitor { interval: None, json: false }.needs_hardware());
        assert!(!Commands::Completions { shell: Shell::Bash }.needs_hardware());
        assert!(!Commands::InstallService.needs_hardware());
    }

    #[test]
    fn clap_values_match_the_driver_encoding() {
        for (arg, name) in [
            (FanModeArg::Normal, "normal"),
            (FanModeArg::Silent, "silent"),
            (FanModeArg::Gaming, "gaming"),
            (FanModeArg::Custom, "custom"),
            (FanModeArg::Auto, "auto"),
            (FanModeArg::Fixed, "fixed"),
        ] {
            assert_eq!(arg.value(), sysfs::fan_mode_value(name).unwrap());
        }
        assert_eq!(ChargeModeArg::Normal.value(), sysfs::charge_mode_value("normal").unwrap());
        assert_eq!(ChargeModeArg::Custom.value(), sysfs::charge_mode_value("custom").unwrap());
        assert_eq!(OnOff::On.value(), sysfs::on_off_value("on").unwrap());
        assert_eq!(OnOff::Off.value(), sysfs::on_off_value("off").unwrap());
    }

    #[test]
    fn status_json_keeps_its_documented_shape() {
        let status = StatusJson {
            fan_mode: "gaming".into(),
            fan_speed: "200".into(),
            charge_mode: "custom".into(),
            charge_limit: "80".into(),
            gpu_boost: "on".into(),
            battery_cycle: "12".into(),
            light_sensor: "N/A".into(),
            fan_pwm: "160".into(),
            cpu_temp: Some(58.5),
            gpu_temp: None,
            fans: vec![FanJson { name: "Fan 1", rpm: 2400 }],
        };
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"fan_mode":"gaming","fan_speed":"200","charge_mode":"custom","charge_limit":"80","gpu_boost":"on","battery_cycle":"12","light_sensor":"N/A","fan_pwm":"160","cpu_temp":58.5,"gpu_temp":null,"fans":[{"name":"Fan 1","rpm":2400}]}"#
        );
    }

    #[test]
    fn fan_names_with_quotes_stay_valid_json() {
        let fans = [Fan { name: "Fan \"1\"\n".to_string(), rpm: 1 }];
        let encoded = serde_json::to_string(&fans_json(&fans)).unwrap();
        assert_eq!(encoded, r#"[{"name":"Fan \"1\"\n","rpm":1}]"#);
    }

    #[test]
    fn packaged_unit_file_matches_the_installed_one() {
        assert_eq!(
            include_str!("../assets/gigabytectl-ppd-sync.service"),
            service_unit(Path::new("/usr/local/bin/gigabytectl"))
        );
    }
}
