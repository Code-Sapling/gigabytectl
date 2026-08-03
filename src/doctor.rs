//! Environment report, for working out why something does not work on a given
//! machine — models differ in which attributes the driver can actually offer.

use std::{fs, path::Path};

use anyhow::Result;

use crate::{config, ppd, sensors::Sensors, sysfs, system};

/// Outcome of a single check, which decides both the marker and the exit code.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Ok,
    Warn,
    Bad,
    Info,
}

impl Mark {
    fn symbol(self) -> &'static str {
        match self {
            Self::Ok => "[ok]",
            Self::Warn => "[--]",
            Self::Bad => "[!!]",
            Self::Info => "    ",
        }
    }
}

struct Report {
    failed: bool,
}

impl Report {
    fn section(&self, title: &str) {
        println!("\n{title}");
    }

    fn line(&mut self, mark: Mark, label: &str, value: impl AsRef<str>) {
        if mark == Mark::Bad {
            self.failed = true;
        }
        println!("  {} {:<22} {}", mark.symbol(), label, value.as_ref());
    }
}

/// Prints the report. Exits non-zero only when something is actually broken.
pub fn run() -> Result<bool> {
    let mut report = Report { failed: false };

    report.section("gigabytectl");
    report.line(Mark::Info, "version", env!("CARGO_PKG_VERSION"));
    report.line(
        if system::is_root() { Mark::Ok } else { Mark::Warn },
        "running as",
        if system::is_root() {
            "root".to_string()
        } else {
            format!(
                "unprivileged{}",
                system::sudo_user().map_or(String::new(), |u| format!(" (SUDO_USER={u})"))
            )
        },
    );

    report.section("Machine");
    for (label, file) in [
        ("vendor", "sys_vendor"),
        ("model", "product_name"),
        ("family", "product_family"),
        ("BIOS", "bios_version"),
    ] {
        report.line(Mark::Info, label, dmi(file).unwrap_or_else(|| "unknown".to_string()));
    }
    report.line(
        Mark::Info,
        "kernel",
        read_trimmed_path("/proc/sys/kernel/osrelease").unwrap_or_else(|| "unknown".to_string()),
    );

    report.section("Driver");
    let driver = sysfs::driver_present();
    report.line(
        if driver { Mark::Ok } else { Mark::Bad },
        "gigabyte-laptop-wmi",
        if driver {
            format!("loaded ({})", sysfs::ROOT)
        } else {
            sysfs::driver_missing_message()
        },
    );
    if driver {
        report.line(
            Mark::Info,
            "module version",
            read_trimmed_path("/sys/module/aorus_laptop/version")
                .or_else(module_version)
                .unwrap_or_else(|| "unknown".to_string()),
        );
    }

    if driver {
        report.section("Attributes");
        for (label, path) in sysfs::ALL_NODES {
            report.line(mark_for_node(path), label, describe_node(path));
        }
    }

    report.section("Sensors");
    let mut sensors = Sensors::new();
    let fans = sensors.read_fans();
    if fans.is_empty() {
        report.line(Mark::Warn, "fans", "no readings (driver hwmon device not found)");
    }
    for fan in &fans {
        report.line(Mark::Ok, &format!("fan {} ({})", fan.channel, fan.name), fan.reading());
    }
    let temps = sensors.read_temps();
    report.line(
        if temps.cpu.is_some() { Mark::Ok } else { Mark::Warn },
        "CPU temperature",
        temps.cpu.map_or("unavailable".to_string(), |t| format!("{t:.0} C")),
    );
    report.line(
        if temps.gpu.is_some() { Mark::Ok } else { Mark::Warn },
        "GPU temperature",
        temps.gpu.map_or("unavailable".to_string(), |t| format!("{t:.0} C")),
    );

    report.section("Power profiles daemon");
    match ppd::is_available() {
        true => report.line(
            Mark::Ok,
            "system bus",
            ppd::get().map_or_else(|e| format!("reachable, but: {e:#}"), |p| format!("active profile: {p}")),
        ),
        false => report.line(Mark::Warn, "system bus", "not reachable (sync and ppd_profile are inert)"),
    }
    let unit_installed = Path::new(crate::cli::SERVICE_PATH).exists();
    report.line(
        if unit_installed { Mark::Ok } else { Mark::Info },
        "sync service",
        if unit_installed {
            format!(
                "installed, {}",
                system::command_stdout("systemctl", &["is-active", crate::cli::SERVICE_NAME])
                    .unwrap_or_else(|| "state unknown".to_string())
            )
        } else {
            "not installed (gigabytectl install-service)".to_string()
        },
    );

    report.section("Configuration");
    report.line(Mark::Info, "config", describe_file(&config::config_path(false)));
    report.line(Mark::Info, "config (system)", describe_file(&config::config_path(true)));
    report.line(Mark::Info, "profiles", describe_file(&config::profiles_path()));
    report.line(Mark::Info, "profiles (system)", describe_file(&config::system_profiles_path()));
    match config::load_profiles() {
        Ok(profiles) if profiles.is_empty() => report.line(Mark::Info, "saved profiles", "none"),
        Ok(profiles) => {
            let mut names: Vec<&str> = profiles.keys().map(String::as_str).collect();
            names.sort_unstable();
            report.line(Mark::Ok, "saved profiles", names.join(", "));
        }
        Err(e) => report.line(Mark::Bad, "saved profiles", format!("{e:#}")),
    }

    println!();
    Ok(!report.failed)
}

fn mark_for_node(path: &str) -> Mark {
    if Path::new(path).exists() { Mark::Ok } else { Mark::Warn }
}

/// Reports what can be done with a node, since which attributes a model
/// supports (and whether this process may write them) is the usual question.
fn describe_node(path: &str) -> String {
    if !Path::new(path).exists() {
        return "not supported on this model".to_string();
    }
    let value = sysfs::read_trimmed(path);
    let readable = value.is_some();
    // Opening for writing does not modify the attribute; sysfs only runs its
    // store handler on an actual write.
    let writable = fs::OpenOptions::new().write(true).open(path).is_ok();
    let access = match (readable, writable) {
        (true, true) => "read/write",
        (true, false) => "read-only",
        (false, true) => "write-only",
        (false, false) => "no access",
    };
    match value {
        Some(value) if !value.is_empty() => format!("{access}, currently {value:?}"),
        _ => access.to_string(),
    }
}

fn describe_file(path: &Path) -> String {
    if path.exists() {
        format!("{} (present)", path.display())
    } else {
        format!("{} (not created yet)", path.display())
    }
}

fn dmi(file: &str) -> Option<String> {
    read_trimmed_path(&format!("/sys/class/dmi/id/{file}"))
}

fn read_trimmed_path(path: &str) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn module_version() -> Option<String> {
    let info = system::command_stdout("modinfo", &["aorus_laptop"])?;
    info.lines()
        .find_map(|line| line.strip_prefix("version:"))
        .map(|version| version.trim().to_string())
}
