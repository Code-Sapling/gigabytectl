//! Process-level helpers: privilege checks and running external commands.

use std::{
    os::unix::fs::MetadataExt,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};

/// Seconds since the Unix epoch, or `0` on a clock set before it.
pub fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// True when the process is running as root.
pub fn is_root() -> bool {
    // `/proc/self` is owned by the process UID, which avoids depending on libc.
    std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

/// Re-runs the current binary under `sudo` with the same arguments, then exits
/// with whatever status it returned. Only returns on failure to start `sudo`.
pub fn run_sudo() -> Result<std::convert::Infallible> {
    let exe = std::env::current_exe().context("resolving current executable path")?;
    let status = Command::new("sudo")
        .arg(exe)
        .args(std::env::args().skip(1))
        .status()
        .context("running sudo")?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Runs a command and returns its trimmed stdout, or `None` if it could not be
/// started or exited non-zero.
pub fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Like [`command_stdout`], but reports why the command failed.
pub fn checked_stdout(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running `{program} {}`", args.join(" ")))?;
    ensure!(
        output.status.success(),
        "`{program} {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Runs a command with inherited stdio and fails if it reports an error.
pub fn checked_status(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("running `{program} {}`", args.join(" ")))?;
    ensure!(status.success(), "`{program} {}` failed ({status})", args.join(" "));
    Ok(())
}

/// The user who invoked `sudo`, if any.
pub fn sudo_user() -> Option<String> {
    std::env::var("SUDO_USER").ok().filter(|user| !user.is_empty())
}

/// Looks up a user's home directory in the passwd database.
pub fn home_of_user(user: &str) -> Option<String> {
    let entry = command_stdout("getent", &["passwd", user])?;
    entry
        .split(':')
        .nth(5)
        .map(str::to_string)
        .filter(|home| !home.is_empty())
}

/// When running under sudo, hands ownership of freshly-written config back to
/// the invoking user so they can edit it without root. Best-effort.
pub fn chown_to_sudo_user(dir: &Path) {
    if let Some(user) = sudo_user() {
        let _ = Command::new("chown")
            .arg("-R")
            .arg(format!("{user}:"))
            .arg(dir)
            .status();
    }
}
