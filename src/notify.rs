//! Optional desktop notifications for temperature thresholds.
//!
//! Disabled unless switched on in the config. Alerts are rate-limited per
//! sensor so a machine sitting above its threshold does not spam the session.

use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use crate::{
    config::Notifications,
    sensors::Temps,
    system::{self, command_stdout},
};

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

pub struct Notifier {
    config: Notifications,
    /// When each sensor last raised an alert.
    last: [Option<Instant>; 2],
    /// The most recent `notify-send`, kept only so it can be reaped.
    pending: Option<Child>,
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
            pending: None,
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
            self.pending = spawn_notification(&summary, &body);
        }
    }

    /// Collects the previous notification process so finished ones do not pile
    /// up as zombies over a long session.
    fn reap(&mut self) {
        if let Some(child) = &mut self.pending
            && matches!(child.try_wait(), Ok(Some(_)) | Err(_))
        {
            self.pending = None;
        }
    }
}

/// Starts `notify-send` without waiting for it.
///
/// Under `sudo` the notification has to be delivered to the invoking user's
/// session bus, since root's own bus is not what the desktop is listening on.
/// If that user cannot be resolved, the alert is skipped rather than lost in
/// root's session.
#[cfg_attr(test, allow(dead_code, reason = "only called outside test builds"))]
fn spawn_notification(summary: &str, body: &str) -> Option<Child> {
    let mut command = match session_target() {
        Some((user, uid)) => {
            let mut command = Command::new("sudo");
            command
                .arg("-u")
                .arg(&user)
                .arg(format!("DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{uid}/bus"))
                .arg("notify-send");
            command
        }
        None => Command::new("notify-send"),
    };

    command
        .args(["--app-name=gigabytectl", "--urgency=critical", summary, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

/// The `(user, uid)` whose session should receive notifications, or `None` when
/// we are already that user.
#[cfg_attr(test, allow(dead_code, reason = "only called outside test builds"))]
fn session_target() -> Option<(String, u32)> {
    if !system::is_root() {
        return None;
    }
    let user = system::sudo_user()?;
    let uid = command_stdout("id", &["-u", &user])?.parse().ok()?;
    Some((user, uid))
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
        };
        let mut notifier = Notifier::new(&config);
        notifier.check(Temps::default());
        assert!(notifier.last.iter().all(Option::is_none));
    }
}
