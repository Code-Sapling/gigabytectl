//! Command-line interface: argument definitions and one-shot commands.

use std::{fs, io, path::Path, thread, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use serde::Serialize;

use crate::{
    config::{self, Config, Notifications, Profile, Units},
    notify::{self, Notifier},
    ppd, selfcmd,
    sensors::{Fan, Sensors, Temps},
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
        /// Output as CSV with a header line and a unix timestamp column
        #[arg(long, conflicts_with = "json")]
        csv: bool,
        /// Stop after this many samples instead of running until interrupted
        #[arg(short = 'n', long)]
        count: Option<u64>,
    },
    /// Apply, list, or save profiles (~/.config/gigabytectl/profiles.toml)
    Profile(ProfileArgs),
    /// Background service: apply the mapped profile whenever the system power
    /// profile changes, and raise temperature alerts if they are switched on.
    /// Runs until interrupted (for a systemd service).
    Sync {
        /// Apply the profile mapped to the current power profile once, then exit
        #[arg(long)]
        once: bool,
    },
    /// Read or change settings in config.toml
    Config(ConfigArgs),
    /// Report driver, sensor, and configuration state for this machine
    Doctor,
    /// Generate shell completions (bash, zsh, fish, ...)
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
    /// Install and enable the sync systemd service. Seeds
    /// /etc/gigabytectl/{profiles,config}.toml from yours so the root-run
    /// service can find them (run with sudo).
    InstallService,
    /// Manage this installation itself (update or uninstall)
    #[command(name = "self")]
    Manage {
        #[command(subcommand)]
        action: ManageAction,
    },
}

#[derive(Subcommand)]
pub enum ManageAction {
    /// Check GitHub for a newer release and install it over this binary
    Update {
        /// Only report whether an update is available
        #[arg(long)]
        check: bool,
        /// Report what would be downloaded and installed, without doing it
        #[arg(long, conflicts_with = "check")]
        dry_run: bool,
    },
    /// Remove the binary, its configuration, and the systemd service
    Uninstall {
        /// Skip the confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
        /// List what would be removed, without removing anything
        #[arg(long)]
        dry_run: bool,
        /// Keep configuration and profiles
        #[arg(long)]
        keep_config: bool,
    },
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
                | Self::Config(_)
                | Self::Doctor
                | Self::Sync { .. }
                | Self::Completions { .. }
                | Self::InstallService
                | Self::Manage { .. }
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
    /// Delete a saved profile
    #[arg(long, value_name = "NAME")]
    delete: Option<String>,
    /// Print a saved profile
    #[arg(long, value_name = "NAME")]
    show: Option<String>,
    /// power-profiles-daemon profile to map to, used with --save
    #[arg(long, value_name = "PROFILE", requires = "save")]
    ppd: Option<String>,
    /// Copy profiles to /etc/gigabytectl so the root-run sync service sees them
    #[arg(long)]
    sync_system: bool,
}

#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    action: ConfigAction,
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Print the effective configuration
    Show,
    /// List the keys that can be read and written
    Keys,
    /// Print one value
    Get { key: String },
    /// Change one value
    Set {
        key: String,
        value: String,
        /// Write /etc/gigabytectl/config.toml instead of the user's config
        #[arg(long)]
        system: bool,
    },
    /// Print where the configuration is read from and written to
    Path {
        /// Show the system-wide path
        #[arg(long)]
        system: bool,
    },
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
    Set {
        index: usize,
        temp: i32,
        speed: i32,
        /// Write even if it leaves the curve out of order (temperature and
        /// speed are meant to be non-decreasing across points)
        #[arg(long)]
        force: bool,
    },
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
                warn_if_inactive(sysfs::Dependent::FanCustomSpeed);
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
                warn_if_inactive(sysfs::Dependent::ChargeLimit);
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
        Commands::Monitor { interval, json, csv, count } => {
            let format = if json {
                SampleFormat::Json
            } else if csv {
                SampleFormat::Csv
            } else {
                SampleFormat::Text
            };
            run_monitor(interval, format, count, config)?;
        }
        Commands::Profile(args) => run_profile(&args)?,
        Commands::Config(args) => run_config(&args)?,
        Commands::Doctor => {
            if !crate::doctor::run()? {
                std::process::exit(1);
            }
        }
        Commands::Sync { once } => run_sync(once, config)?,
        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            generate(shell, &mut cmd, name, &mut io::stdout());
        }
        Commands::InstallService => install_service()?,
        Commands::Manage { action } => match action {
            ManageAction::Update { check, dry_run } => selfcmd::update(check, dry_run)?,
            ManageAction::Uninstall { yes, dry_run, keep_config } => selfcmd::uninstall(yes, dry_run, keep_config)?,
        },
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
        FanCurveAction::Set { index, temp, speed, force } => {
            if force {
                sysfs::write_fan_curve_point(index, temp, speed)?;
            } else {
                // Check the edit against the curve already on the device, so a
                // point cannot be dropped below its neighbours by accident.
                sysfs::validate_curve_index(index)?;
                let curve = sysfs::read_fan_curve()?;
                sysfs::validate_curve_point_in(&curve, index, temp, speed)
                    .context("refusing to write a fan curve that goes backwards (use --force to override)")?;
                sysfs::write_fan_curve_point(index, temp, speed)?;
            }
        }
    }
    Ok(())
}

fn print_fans(fans: &[Fan]) {
    if fans.is_empty() {
        println!("No live fan readings available");
    }
    for fan in fans {
        println!("{}: {}", fan.name, fan.reading());
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
    /// False when the fan mode means fan_speed is currently ignored.
    fan_speed_active: bool,
    /// False when the charge mode means charge_limit is currently ignored.
    charge_limit_active: bool,
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
    channel: u32,
    name: &'a str,
    rpm: u32,
    pwm: Option<u32>,
}

fn fans_json(fans: &[Fan]) -> Vec<FanJson<'_>> {
    fans.iter()
        .map(|fan| FanJson {
            channel: fan.channel,
            name: &fan.name,
            rpm: fan.rpm,
            pwm: fan.pwm,
        })
        .collect()
}

/// `"80  (inactive - requires Custom charge mode)"` when a setting is inert.
fn annotate(value: &str, reason: Option<&str>) -> String {
    match reason {
        Some(reason) => format!("{value}  (inactive - {reason})"),
        None => value.to_string(),
    }
}

/// Prints a warning when a value was written to a node that is currently being
/// ignored, so a set that appears to work but does nothing is not silent.
fn warn_if_inactive(setting: sysfs::Dependent) {
    if let Some(reason) = HwState::read().inactive_reason(setting) {
        eprintln!("Warning: value saved, but it has no effect right now - it {reason}");
    }
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
            fan_speed_active: hw.is_active(sysfs::Dependent::FanCustomSpeed),
            charge_limit_active: hw.is_active(sysfs::Dependent::ChargeLimit),
            fans: fans_json(&fans),
        };
        println!("{}", serde_json::to_string(&status).context("serializing status")?);
    } else {
        let cpu_temp = units.format(temps.cpu);
        let gpu_temp = units.format(temps.gpu);
        let fan_speed = annotate(&fan_speed, hw.inactive_reason(sysfs::Dependent::FanCustomSpeed));
        let charge_limit = annotate(&charge_limit, hw.inactive_reason(sysfs::Dependent::ChargeLimit));
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
                println!("  {}: {}", fan.name, fan.reading());
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SampleFormat {
    Text,
    Json,
    Csv,
}

fn run_monitor(interval: Option<f64>, format: SampleFormat, count: Option<u64>, config: &Config) -> Result<()> {
    let period = match interval {
        Some(secs) => Duration::from_secs_f64(secs.max(0.1)),
        None => config.refresh_interval(),
    };
    let units = config.units;
    // Sensor paths are resolved once and reused for every sample.
    let mut sensors = Sensors::new();
    let mut notifier = Notifier::new(&config.notifications);
    // CSV columns are fixed by the first sample so the table stays rectangular
    // even if a fan stops and drops out of later readings.
    let mut csv_channels: Option<Vec<u32>> = None;

    for taken in 0.. {
        if count.is_some_and(|limit| taken >= limit) {
            return Ok(());
        }
        if taken > 0 {
            thread::sleep(period);
        }

        let temps = sensors.read_temps();
        let fans = sensors.read_fans();
        notifier.check(temps);

        match format {
            SampleFormat::Json => {
                let sample = SampleJson {
                    cpu_temp: units.to_json(temps.cpu),
                    gpu_temp: units.to_json(temps.gpu),
                    fans: fans_json(&fans),
                };
                println!("{}", serde_json::to_string(&sample).context("serializing sample")?);
            }
            SampleFormat::Csv => {
                let channels = match &csv_channels {
                    Some(channels) => channels,
                    None => {
                        let channels: Vec<u32> = fans.iter().map(|fan| fan.channel).collect();
                        println!("{}", csv_header(&channels));
                        csv_channels.insert(channels)
                    }
                };
                println!("{}", csv_row(channels, temps, &fans, units));
            }
            SampleFormat::Text => {
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
        }
    }
    Ok(())
}

fn csv_header(channels: &[u32]) -> String {
    let mut columns = vec!["timestamp".to_string(), "cpu_temp".to_string(), "gpu_temp".to_string()];
    for channel in channels {
        columns.push(format!("fan{channel}_rpm"));
        columns.push(format!("fan{channel}_pwm"));
    }
    columns.join(",")
}

fn csv_row(channels: &[u32], temps: Temps, fans: &[Fan], units: Units) -> String {
    let number = |value: Option<f64>| value.map_or(String::new(), |v| format!("{v:.1}"));
    let mut columns = vec![
        system::unix_time().to_string(),
        number(units.to_json(temps.cpu)),
        number(units.to_json(temps.gpu)),
    ];
    for channel in channels {
        let fan = fans.iter().find(|fan| fan.channel == *channel);
        columns.push(fan.map_or(String::new(), |fan| fan.rpm.to_string()));
        columns.push(fan.and_then(|fan| fan.pwm).map_or(String::new(), |pwm| pwm.to_string()));
    }
    columns.join(",")
}

fn run_profile(args: &ProfileArgs) -> Result<()> {
    if let Some(name) = &args.save {
        ensure!(
            system::is_root(),
            "gigabytectl: saving a profile reads hardware state; run as root (try: sudo gigabytectl profile --save {name})"
        );
        let mut profiles = config::load_profiles()?;
        let mut profile = Profile::from_hardware();
        if let Some(power_profile) = &args.ppd {
            profile.ppd_profile = Some(power_profile.clone());
        }
        let replaced = profiles.insert(name.clone(), profile).is_some();
        config::save_profiles(&profiles)?;
        println!("{} profile '{name}'", if replaced { "Updated" } else { "Saved" });
        return Ok(());
    }

    if let Some(name) = &args.delete {
        let mut profiles = config::load_profiles()?;
        ensure!(
            profiles.remove(name).is_some(),
            "profile '{name}' not found in {}",
            config::profiles_path().display()
        );
        config::save_profiles(&profiles)?;
        println!("Deleted profile '{name}'");
        return Ok(());
    }

    if let Some(name) = &args.show {
        let profiles = config::load_profiles()?;
        let profile = profiles
            .get(name)
            .with_context(|| format!("profile '{name}' not found in {}", config::profiles_path().display()))?;
        print!("{}", toml::to_string_pretty(profile).context("serializing profile")?);
        return Ok(());
    }

    if args.sync_system {
        ensure!(
            system::is_root(),
            "gigabytectl: writing {} needs root (try: sudo gigabytectl profile --sync-system)",
            config::SYSTEM_CONFIG_DIR
        );
        let path = config::seed_system_profiles()?;
        println!("Copied profiles to {}", path.display());
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
        bail!("Specify a profile to apply, or use --list / --save / --show / --delete <name>");
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

fn run_config(args: &ConfigArgs) -> Result<()> {
    match &args.action {
        ConfigAction::Show => {
            let config = Config::load();
            print!("{}", toml::to_string_pretty(&config).context("serializing config")?);
        }
        ConfigAction::Keys => {
            for key in Config::KEYS {
                println!("{key}");
            }
        }
        ConfigAction::Get { key } => println!("{}", Config::load().get(key)?),
        ConfigAction::Set { key, value, system } => {
            if *system {
                ensure!(
                    crate::system::is_root(),
                    "gigabytectl: writing {} needs root (try: sudo gigabytectl config set --system {key} {value})",
                    config::config_path(true).display()
                );
            }
            // Edit the file being written rather than the merged view, so a
            // system-wide value is not silently copied into the user's config.
            let mut config = config::load_config_file(&config::config_path(*system));
            config.set(key, value)?;
            let path = config::save_config(&config, *system)?;
            println!("{key} = {} ({})", config.get(key)?, path.display());
        }
        ConfigAction::Path { system } => println!("{}", config::config_path(*system).display()),
    }
    Ok(())
}

/// The long-running service: PPD sync plus, when they are switched on,
/// temperature alerts. Running the alerts here is what frees them from the TUI,
/// so they keep working with nothing open.
fn run_sync(once: bool, config: &Config) -> Result<()> {
    require_hardware()?;
    let notifications = &config.notifications;
    const NO_PPD: &str = "power-profiles-daemon is not available on the system bus (is it installed and running?)";

    if once {
        // Apply whatever profile is active right now, then stop. Run by hand, so
        // it reports a missing PPD immediately rather than waiting one out.
        ensure!(ppd::is_available(), "{NO_PPD}");
        return apply_mapped_profile(&ppd::get()?);
    }

    // As a service this can start before power-profiles-daemon owns its bus name
    // (the unit is deliberately not ordered after it -- that ordering cycles at
    // boot), so give PPD a chance to appear before concluding it is absent.
    let ppd_available = ppd::wait_until_available();

    // Alerts alone are a legitimate reason to run this service, so PPD is only
    // required when there would otherwise be nothing to do.
    ensure!(
        ppd_available || notifications.enabled,
        "{NO_PPD}\nTurn on temperature alerts (gigabytectl config set notifications.enabled true) to run this service for alerts alone."
    );

    if !ppd_available {
        eprintln!("gigabytectl: power-profiles-daemon not reachable; running temperature alerts only");
        notify::watch(notifications);
    }

    start_notifier(notifications);
    // Start from whatever profile is active right now.
    if let Err(e) = apply_mapped_profile(&ppd::get()?) {
        eprintln!("Warning: {e:#}");
    }

    ppd::watch(|power_profile| {
        if let Err(e) = apply_mapped_profile(power_profile) {
            eprintln!("Warning: {e:#}");
        }
    })
}

/// Polls temperatures alongside the PPD watch, which blocks for the life of the
/// service. Does nothing unless alerts are switched on — they stay opt-in.
fn start_notifier(notifications: &Notifications) {
    if !notifications.enabled {
        return;
    }
    let notifications = notifications.clone();
    thread::spawn(move || notify::watch(&notifications));
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

pub const SERVICE_NAME: &str = "gigabytectl-ppd-sync.service";
pub const SERVICE_PATH: &str = "/etc/systemd/system/gigabytectl-ppd-sync.service";

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
    seed_system_config()?;

    fs::write(SERVICE_PATH, service_unit(&exe)).with_context(|| format!("writing {SERVICE_PATH}"))?;
    eprintln!("Wrote {SERVICE_PATH}");

    system::checked_status("systemctl", &["daemon-reload"])?;
    system::checked_status("systemctl", &["enable", "--now", SERVICE_NAME])?;
    eprintln!("Enabled and started {SERVICE_NAME}.");
    eprintln!("Check it with: systemctl status {SERVICE_NAME}");
    Ok(())
}

/// Copies the invoking user's settings to the system-wide location, since that
/// is where the service reads the notification thresholds it acts on.
fn seed_system_config() -> Result<()> {
    let path = config::seed_system_config()?;
    let config = Config::load();
    eprintln!("Copied settings to {}", path.display());
    if config.notifications.enabled {
        eprintln!(
            "Temperature alerts are on: the service will notify logged-in desktops (CPU >= {:.0}C, GPU >= {:.0}C).",
            config.notifications.cpu_temp, config.notifications.gpu_temp
        );
    } else {
        eprintln!("Temperature alerts are off. Turn them on with:");
        eprintln!("      gigabytectl config set notifications.enabled true");
        eprintln!("      sudo systemctl restart {SERVICE_NAME}");
    }
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
Description=gigabytectl power-profiles-daemon sync and temperature alerts
Documentation=https://github.com/Code-Sapling/gigabytectl
# Apply the mapped gigabytectl profile whenever the system power profile changes,
# and raise temperature alerts when notifications are enabled in the config.
#
# Deliberately not ordered After=power-profiles-daemon.service: PPD's own unit is
# After=multi-user.target and this one is WantedBy=multi-user.target, so that
# ordering closes a cycle and systemd silently drops this unit's start job at
# boot. `gigabytectl sync` waits for PPD on the bus instead.
Wants=power-profiles-daemon.service

[Service]
Type=simple
# Adjust the path if you installed elsewhere (e.g. /usr/bin/gigabytectl).
# This runs as root, so it reads profiles and settings from /etc/gigabytectl
# (not any user's ~/.config). `gigabytectl install-service` sets this up for you.
ExecStart={} sync
Restart=on-failure
RestartSec=2

[Install]
# multi-user.target so temperature alerts still run on machines without
# power-profiles-daemon; the PPD unit so sync starts with it when it is there.
WantedBy=multi-user.target power-profiles-daemon.service
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
        assert!(!Commands::Monitor { interval: None, json: false, csv: false, count: None }.needs_hardware());
        assert!(!Commands::Doctor.needs_hardware());
        assert!(!Commands::Completions { shell: Shell::Bash }.needs_hardware());
        assert!(!Commands::InstallService.needs_hardware());
        // `self` manages the install itself, and must work on a machine whose
        // driver is missing — that is a reason to uninstall, not a blocker.
        assert!(
            !Commands::Manage {
                action: ManageAction::Uninstall { yes: false, dry_run: true, keep_config: false }
            }
            .needs_hardware()
        );
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
            fan_speed_active: true,
            charge_limit_active: false,
            fans: vec![FanJson { channel: 1, name: "CPU fan", rpm: 2400, pwm: Some(60) }],
        };
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"fan_mode":"gaming","fan_speed":"200","charge_mode":"custom","charge_limit":"80","gpu_boost":"on","battery_cycle":"12","light_sensor":"N/A","fan_pwm":"160","cpu_temp":58.5,"gpu_temp":null,"fan_speed_active":true,"charge_limit_active":false,"fans":[{"channel":1,"name":"CPU fan","rpm":2400,"pwm":60}]}"#
        );
    }

    #[test]
    fn fan_names_with_quotes_stay_valid_json() {
        let fans = [Fan { name: "Fan \"1\"\n".to_string(), ..Fan::sample(1, 1) }];
        let encoded = serde_json::to_string(&fans_json(&fans)).unwrap();
        assert_eq!(encoded, r#"[{"channel":1,"name":"Fan \"1\"\n","rpm":1,"pwm":null}]"#);
    }

    #[test]
    fn packaged_unit_file_matches_the_installed_one() {
        assert_eq!(
            include_str!("../assets/gigabytectl-ppd-sync.service"),
            service_unit(Path::new("/usr/local/bin/gigabytectl"))
        );
    }
}
