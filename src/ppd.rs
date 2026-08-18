//! Two-way integration with `power-profiles-daemon` over the system D-Bus.
//!
//! `busctl` and `gdbus` are used instead of a D-Bus client library to keep the
//! dependency footprint small; both ship with systemd and glib respectively.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};

use crate::{
    config::Profile,
    system::{checked_stdout, command_stdout},
};

/// Candidate (bus name, object path) pairs. The interface name matches the bus
/// name in both cases. Newer builds live under the `UPower` namespace, older ones
/// under net.hadess, so each is probed in order.
const ENDPOINTS: [(&str, &str); 2] = [
    ("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles"),
    ("org.freedesktop.UPower.PowerProfiles", "/org/freedesktop/UPower/PowerProfiles"),
];

const PROPERTY: &str = "ActiveProfile";

/// Probing costs a `busctl` round trip, so a hit is remembered for the life of
/// the process. A miss deliberately is not: the sync service can start before
/// power-profiles-daemon has taken its bus name, and caching that miss would
/// strand the daemon believing PPD is absent for as long as it runs.
static ENDPOINT: OnceLock<(&'static str, &'static str)> = OnceLock::new();

/// How long [`wait_until_available`] gives power-profiles-daemon to appear, and
/// how often it re-probes while waiting.
const WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT_POLL: Duration = Duration::from_millis(500);

/// The first endpoint whose `ActiveProfile` can be read, or `None` when
/// power-profiles-daemon is not reachable on the system bus.
fn endpoint() -> Option<(&'static str, &'static str)> {
    if let Some(&found) = ENDPOINT.get() {
        return Some(found);
    }
    let found = ENDPOINTS
        .into_iter()
        .find(|&(dest, path)| command_stdout("busctl", &get_property_args(dest, path)).is_some())?;
    Some(*ENDPOINT.get_or_init(|| found))
}

pub fn is_available() -> bool {
    endpoint().is_some()
}

/// Waits for power-profiles-daemon to appear on the system bus, returning
/// whether it did.
///
/// The sync unit is not ordered `After=power-profiles-daemon.service` — that
/// ordering forms a boot-time cycle (see the unit's own comment) — so at boot
/// the two race. Polling here absorbs the race without the ordering, and costs
/// nothing when PPD is already up or genuinely not installed beyond the wait.
pub fn wait_until_available() -> bool {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        if is_available() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(WAIT_POLL);
    }
}

fn get_property_args<'a>(dest: &'a str, path: &'a str) -> [&'a str; 6] {
    ["--system", "get-property", dest, path, dest, PROPERTY]
}

fn require_endpoint() -> Result<(&'static str, &'static str)> {
    endpoint().context("power-profiles-daemon is not available on the system bus (is it installed and running?)")
}

/// Reads the currently-active power profile (e.g. `"balanced"`).
pub fn get() -> Result<String> {
    let (dest, path) = require_endpoint()?;
    let out = checked_stdout("busctl", &get_property_args(dest, path))?;
    parse_property_string(&out).with_context(|| format!("parsing {PROPERTY} from {out:?}"))
}

/// Sets the active power profile. Setting it to its current value is a no-op,
/// so this is safe to call from the sync daemon without causing feedback loops.
pub fn set(profile: &str) -> Result<()> {
    let (dest, path) = require_endpoint()?;
    checked_stdout(
        "busctl",
        &["--system", "set-property", dest, path, dest, PROPERTY, "s", profile],
    )
    .with_context(|| format!("setting power profile '{profile}'"))?;
    Ok(())
}

/// Parses a `busctl get-property` scalar string (`s "balanced"`).
fn parse_property_string(out: &str) -> Option<String> {
    Some(out.trim().strip_prefix("s ")?.trim().trim_matches('"').to_string())
}

/// Extracts the new profile from a `gdbus monitor` `PropertiesChanged` line of the
/// form `... {'ActiveProfile': <'performance'>} ...`.
fn parse_active_profile_change(line: &str) -> Option<String> {
    let key = "'ActiveProfile': <'";
    let rest = &line[line.find(key)? + key.len()..];
    Some(rest[..rest.find('\'')?].to_string())
}

/// Finds the saved profile mapped to the given power profile. If several map to
/// the same one, the alphabetically-first name wins so the choice is stable.
pub fn mapped_profile<'a>(profiles: &'a HashMap<String, Profile>, ppd: &str) -> Option<(&'a String, &'a Profile)> {
    profiles
        .iter()
        .filter(|(_, p)| p.ppd_profile.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(ppd)))
        .min_by_key(|(name, _)| *name)
}

/// Watches for power profile changes, calling `on_change` with each new profile
/// name. Only returns if the monitor stops, which is always an error.
pub fn watch(mut on_change: impl FnMut(&str)) -> Result<()> {
    let (dest, path) = require_endpoint()?;
    eprintln!("gigabytectl: watching {dest} for power profile changes (Ctrl-C to stop)...");

    let mut child = Command::new("gdbus")
        .args(["monitor", "--system", "--dest", dest, "--object-path", path])
        .stdout(Stdio::piped())
        .spawn()
        .context("spawning `gdbus monitor` (is gdbus/glib2 installed?)")?;

    let stdout = child.stdout.take().context("capturing gdbus stdout")?;
    let result = (|| -> Result<()> {
        for line in BufReader::new(stdout).lines() {
            let line = line.context("reading gdbus monitor output")?;
            if let Some(profile) = parse_active_profile_change(&line) {
                on_change(&profile);
            }
        }
        Ok(())
    })();

    // The monitor is only useful while we are reading it; don't leave it behind.
    let _ = child.kill();
    let status = child.wait().context("waiting on gdbus monitor")?;
    result?;
    anyhow::bail!("gdbus monitor exited unexpectedly ({status})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busctl_scalar_strings_are_unwrapped() {
        assert_eq!(parse_property_string("s \"balanced\"\n").unwrap(), "balanced");
        assert_eq!(parse_property_string("s \"power-saver\"").unwrap(), "power-saver");
        assert_eq!(parse_property_string("b true"), None);
        assert_eq!(parse_property_string(""), None);
    }

    #[test]
    fn property_change_lines_yield_the_new_profile() {
        let line = "/net/hadess/PowerProfiles: org.freedesktop.DBus.Properties.PropertiesChanged \
                    ('net.hadess.PowerProfiles', {'ActiveProfile': <'performance'>}, @as [])";
        assert_eq!(parse_active_profile_change(line).unwrap(), "performance");
        assert_eq!(parse_active_profile_change("unrelated chatter"), None);
        // A truncated line must not panic on slicing.
        assert_eq!(parse_active_profile_change("{'ActiveProfile': <'perfo"), None);
    }

    #[test]
    fn mapping_is_case_insensitive_and_deterministic() {
        let with_ppd = |ppd: &str| Profile { ppd_profile: Some(ppd.to_string()), ..Profile::default() };
        let profiles = HashMap::from([
            ("zulu".to_string(), with_ppd("Performance")),
            ("alpha".to_string(), with_ppd("performance")),
            ("quiet".to_string(), with_ppd("power-saver")),
        ]);
        assert_eq!(mapped_profile(&profiles, "performance").unwrap().0, "alpha");
        assert_eq!(mapped_profile(&profiles, "POWER-SAVER").unwrap().0, "quiet");
        assert!(mapped_profile(&profiles, "balanced").is_none());
    }
}
