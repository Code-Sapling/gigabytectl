//! Optional desktop notifications for temperature thresholds.
//!
//! Disabled unless switched on in the config. Alerts are rate-limited per
//! sensor so a machine sitting above its threshold does not spam the session.

use std::{
    fs,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{
    config::Notifications,
    sensors::{Sensors, Temps},
    system::{self, command_stdout},
};

/// Where per-user runtime state lives. A `bus` socket under it means that user
/// has a session a notification can be delivered to.
const RUNTIME_ROOT: &str = "/run/user";

/// Which sensor an alert came from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Cpu,
    Gpu,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Gpu => "GPU",
        }
    }
}

/// Polls the sensors until the process is stopped, alerting on anything over
/// its threshold. This is what lets the sync service raise alerts without the
/// TUI being open; it never returns.
pub fn watch(config: &Notifications) -> ! {
    let period = config.poll_interval();
    let mut sensors = Sensors::new();
    let mut notifier = Notifier::new(config);
    eprintln!(
        "gigabytectl: watching temperatures every {}s (CPU >= {:.0}C, GPU >= {:.0}C)",
        period.as_secs(),
        config.cpu_temp,
        config.gpu_temp
    );
    loop {
        notifier.check(sensors.read_temps());
        thread::sleep(period);
    }
}

pub struct Notifier {
    config: Notifications,
    /// When each sensor last raised an alert.
    last: [Option<Instant>; 2],
    /// Notifications still running, kept only so they can be reaped.
    pending: Vec<Child>,
    /// Under test the message is recorded instead of being sent, so running the
    /// suite does not pop notifications onto the developer's desktop.
    #[cfg(test)]
    sent: Vec<(String, String)>,
}

impl Notifier {
    pub fn new(config: &Notifications) -> Self {
        Self {
            config: config.clone(),
            last: [None; 2],
            pending: Vec::new(),
            #[cfg(test)]
            sent: Vec::new(),
        }
    }

    /// Raises an alert for any sensor over its threshold. Cheap and silent when
    /// notifications are off, which is the default.
    pub fn check(&mut self, temps: Temps) {
        if !self.config.enabled {
            return;
        }
        self.reap();
        for (source, temp, threshold) in [
            (Source::Cpu, temps.cpu, self.config.cpu_temp),
            (Source::Gpu, temps.gpu, self.config.gpu_temp),
        ] {
            let Some(temp) = temp else { continue };
            if temp >= threshold && self.due(source) {
                self.send(source, temp, threshold);
            }
        }
    }

    /// Whether enough time has passed since this sensor's last alert.
    fn due(&self, source: Source) -> bool {
        let cooldown = Duration::from_secs(self.config.cooldown_secs);
        self.last[source as usize].is_none_or(|at| at.elapsed() >= cooldown)
    }

    fn send(&mut self, source: Source, temp: f32, threshold: f32) {
        self.last[source as usize] = Some(Instant::now());
        let summary = format!("{} temperature high", source.label());
        let body = format!("{} is at {temp:.0}°C (threshold {threshold:.0}°C)", source.label());

        #[cfg(test)]
        self.sent.push((summary, body));
        #[cfg(not(test))]
        {
            self.pending = spawn_notifications(&summary, &body);
        }
    }

    /// Drops the notification processes that have finished so they do not pile
    /// up as zombies over the lifetime of a service.
    fn reap(&mut self) {
        self.pending.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
    }
}

/// A logged-in user a notification can be delivered to.
struct Session {
    /// Name accepted by `sudo -u`, or `#<uid>` when it cannot be resolved.
    user: String,
    uid: u32,
}

/// Whose desktop an alert should be shown on.
enum Delivery {
    /// This process's own session bus.
    Own,
    /// Other users' session buses, because we are root and root's bus is not
    /// what the desktop is listening on.
    Sessions(Vec<Session>),
}

/// Starts a `notify-send` per target without waiting for any of them.
#[cfg_attr(test, allow(dead_code, reason = "only called outside test builds"))]
fn spawn_notifications(summary: &str, body: &str) -> Vec<Child> {
    match delivery() {
        Delivery::Own => spawn(Command::new("notify-send"), summary, body).into_iter().collect(),
        Delivery::Sessions(sessions) => sessions
            .iter()
            .filter_map(|session| {
                let mut command = Command::new("sudo");
                command
                    .arg("-u")
                    .arg(&session.user)
                    .arg(format!("DBUS_SESSION_BUS_ADDRESS=unix:path={RUNTIME_ROOT}/{}/bus", session.uid))
                    .arg(format!("XDG_RUNTIME_DIR={RUNTIME_ROOT}/{}", session.uid))
                    .arg("notify-send");
                spawn(command, summary, body)
            })
            .collect(),
    }
}

#[cfg_attr(test, allow(dead_code, reason = "only called outside test builds"))]
fn spawn(mut command: Command, summary: &str, body: &str) -> Option<Child> {
    command
        .args(["--app-name=gigabytectl", "--urgency=critical", summary, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

/// Works out who should see the alert.
///
/// Run by hand it is simply this session. Under `sudo` it is the invoking user.
/// As a system service there is no invoking user, so every logged-in session
/// gets it — that is the case that lets the sync service notify a desktop.
#[cfg_attr(test, allow(dead_code, reason = "only called outside test builds"))]
fn delivery() -> Delivery {
    if !system::is_root() {
        return Delivery::Own;
    }
    if let Some(user) = system::sudo_user()
        && let Some(uid) = command_stdout("id", &["-u", &user]).and_then(|uid| uid.parse().ok())
    {
        return Delivery::Sessions(vec![Session { user, uid }]);
    }
    Delivery::Sessions(active_sessions())
}

/// Users with a running session bus. The user's own systemd instance creates
/// `/run/user/<uid>/bus` at login, so reading the directory needs no extra
/// tools and no D-Bus round trip.
#[cfg_attr(test, allow(dead_code, reason = "only called outside test builds"))]
fn active_sessions() -> Vec<Session> {
    let Ok(entries) = fs::read_dir(RUNTIME_ROOT) else {
        return Vec::new();
    };
    let mut sessions: Vec<Session> = entries
        .flatten()
        .filter_map(|entry| {
            let uid = runtime_dir_uid(&entry.file_name().to_string_lossy())?;
            entry
                .path()
                .join("bus")
                .exists()
                .then(|| Session { user: user_name(uid), uid })
        })
        .collect();
    // read_dir order is arbitrary; keep delivery order stable.
    sessions.sort_by_key(|session| session.uid);
    sessions
}

/// The uid a `/run/user` entry belongs to. Anything that is not a plain number
/// is not a runtime directory.
fn runtime_dir_uid(name: &str) -> Option<u32> {
    name.parse().ok()
}

/// The login name for a uid, falling back to the `#<uid>` form `sudo -u`
/// accepts when the passwd database cannot be read.
#[cfg_attr(test, allow(dead_code, reason = "only called outside test builds"))]
fn user_name(uid: u32) -> String {
    command_stdout("getent", &["passwd", &uid.to_string()])
        .and_then(|entry| entry.split(':').next().map(str::to_string))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("#{uid}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temps(cpu: f32) -> Temps {
        Temps { cpu: Some(cpu), gpu: None }
    }

    #[test]
    fn disabled_by_default_and_does_nothing() {
        let config = Notifications::default();
        assert!(!config.enabled);
        let mut notifier = Notifier::new(&config);
        notifier.check(temps(150.0));
        assert!(notifier.last.iter().all(Option::is_none));
    }

    #[test]
    fn alerts_only_above_the_threshold_and_then_respects_the_cooldown() {
        let config = Notifications {
            enabled: true,
            cpu_temp: 90.0,
            cooldown_secs: 3600,
            ..Default::default()
        };
        let mut notifier = Notifier::new(&config);

        notifier.check(temps(89.0));
        assert!(notifier.last[Source::Cpu as usize].is_none(), "below threshold must not alert");

        notifier.check(temps(91.0));
        let first = notifier.last[Source::Cpu as usize].expect("crossing the threshold alerts");
        assert_eq!(notifier.sent.len(), 1);
        assert_eq!(notifier.sent[0].0, "CPU temperature high");
        assert_eq!(notifier.sent[0].1, "CPU is at 91°C (threshold 90°C)");

        notifier.check(temps(99.0));
        assert_eq!(
            notifier.last[Source::Cpu as usize],
            Some(first),
            "a second alert must wait for the cooldown"
        );
        assert_eq!(notifier.sent.len(), 1, "cooldown must suppress the repeat");
    }

    #[test]
    fn a_zero_cooldown_allows_consecutive_alerts() {
        let config = Notifications {
            enabled: true,
            cpu_temp: 50.0,
            cooldown_secs: 0,
            ..Default::default()
        };
        let mut notifier = Notifier::new(&config);
        notifier.check(temps(60.0));
        let first = notifier.last[Source::Cpu as usize].unwrap();
        notifier.check(temps(60.0));
        assert!(notifier.last[Source::Cpu as usize].unwrap() >= first);
    }

    #[test]
    fn missing_readings_are_ignored() {
        let config = Notifications {
            enabled: true,
            cpu_temp: 1.0,
            gpu_temp: 1.0,
            cooldown_secs: 0,
            ..Default::default()
        };
        let mut notifier = Notifier::new(&config);
        notifier.check(Temps::default());
        assert!(notifier.last.iter().all(Option::is_none));
    }

    #[test]
    fn only_numeric_runtime_directories_are_treated_as_sessions() {
        assert_eq!(runtime_dir_uid("1000"), Some(1000));
        assert_eq!(runtime_dir_uid("0"), Some(0));
        assert_eq!(runtime_dir_uid("gdm"), None);
        assert_eq!(runtime_dir_uid(""), None);
    }
}
