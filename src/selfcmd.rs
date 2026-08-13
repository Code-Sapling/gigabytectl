//! `gigabytectl self` — updating and removing this installation.
//!
//! Both halves work on the binary that is currently running, so they resolve
//! it once and refuse to touch anything a package manager owns.

use std::{
    fs,
    io::{self, IsTerminal, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::{
    cli::{SERVICE_NAME, SERVICE_PATH},
    config,
    system::{self, checked_status, checked_stdout, command_stdout},
};

const RELEASES_API: &str = "https://api.github.com/repos/Code-Sapling/gigabytectl/releases/latest";
/// Name of the directory this tool owns wherever it stores data. Nothing
/// outside a directory with this name is ever removed.
const DATA_DIR_NAME: &str = "gigabytectl";
/// Long enough for a slow connection, short enough that a hung server does not
/// wedge the command forever.
const DOWNLOAD_TIMEOUT_SECS: &str = "300";

// --- self update ---

/// One release as reported by the GitHub API. Only the fields used here are
/// declared; the response carries many more.
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Checks the latest GitHub release and, unless only asked to look, installs it
/// over the running binary.
pub fn update(check: bool, dry_run: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let exe = current_exe()?;
    println!("Installed: {current} ({})", exe.display());

    let release = latest_release()?;
    let latest = release.tag_name.trim_start_matches('v').to_string();
    println!("Latest:    {latest} (tag {})", release.tag_name);

    if !is_newer(&latest, current) {
        println!("\nAlready up to date.");
        return Ok(());
    }
    println!("\nUpdate available: {current} -> {latest}");
    if check {
        return Ok(());
    }

    let asset = pick_asset(&release.assets, std::env::consts::ARCH).with_context(|| {
        format!(
            "release {} has no {} Linux tarball (assets: {})",
            release.tag_name,
            std::env::consts::ARCH,
            asset_names(&release.assets)
        )
    })?;

    if let Some(package) = owning_package(&exe) {
        bail!(
            "{} is owned by the package '{package}'; update it with your package manager instead",
            exe.display()
        );
    }

    println!("Download:  {}", asset.browser_download_url);
    println!("Install:   {}", exe.display());
    if dry_run {
        println!("\nDry run: nothing was downloaded or installed.");
        return Ok(());
    }

    ensure_writable(exe.parent().unwrap_or(Path::new("/")))?;
    install_asset(asset, &exe, &latest)?;
    println!("\nUpdated to {latest}.");
    restart_service_if_running(&exe);
    println!("Note: regenerate your shell completions (gigabytectl completions <shell>).");
    Ok(())
}

fn latest_release() -> Result<Release> {
    let body = checked_stdout(
        "curl",
        &[
            "-sSfL",
            "--max-time",
            "30",
            "-H",
            "Accept: application/vnd.github+json",
            RELEASES_API,
        ],
    )
    .context("querying the GitHub releases API (is curl installed and the network up?)")?;
    serde_json::from_str(&body).context("parsing the GitHub releases API response")
}

/// Downloads, unpacks, and swaps in the new binary.
fn install_asset(asset: &Asset, exe: &Path, version: &str) -> Result<()> {
    let workdir = std::env::temp_dir().join(format!("gigabytectl-update-{}", std::process::id()));
    let _ = fs::remove_dir_all(&workdir);
    fs::create_dir_all(&workdir).with_context(|| format!("creating {}", workdir.display()))?;
    let result = download_and_swap(asset, exe, version, &workdir);
    let _ = fs::remove_dir_all(&workdir);
    result
}

fn download_and_swap(asset: &Asset, exe: &Path, version: &str, workdir: &Path) -> Result<()> {
    let tarball = workdir.join(&asset.name);
    println!("Downloading {}...", asset.name);
    checked_status(
        "curl",
        &[
            "-sSfL",
            "--max-time",
            DOWNLOAD_TIMEOUT_SECS,
            "-o",
            &tarball.to_string_lossy(),
            &asset.browser_download_url,
        ],
    )
    .context("downloading the release tarball")?;

    checked_status("tar", &["-xzf", &tarball.to_string_lossy(), "-C", &workdir.to_string_lossy()])
        .context("unpacking the release tarball")?;

    let unpacked = workdir.join("gigabytectl");
    ensure!(unpacked.is_file(), "{} did not contain a `gigabytectl` binary", asset.name);
    fs::set_permissions(&unpacked, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("making {} executable", unpacked.display()))?;

    // Run the download before trusting it: a truncated or wrong-arch binary
    // should fail here rather than after it has replaced a working install.
    let reported = command_stdout(&unpacked.to_string_lossy(), &["--version"])
        .context("the downloaded binary did not run (wrong architecture, or a corrupt download)")?;
    ensure!(
        reported.contains(version),
        "the downloaded binary reports {reported:?}, not version {version}"
    );

    // Stage the replacement next to the target so the swap is a rename on the
    // same filesystem: the old binary is never left half-written, and renaming
    // over the running executable is fine on Linux.
    let staged = exe.with_file_name(".gigabytectl.new");
    fs::copy(&unpacked, &staged).with_context(|| format!("writing {}", staged.display()))?;
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("setting permissions on {}", staged.display()))?;
    fs::rename(&staged, exe).with_context(|| format!("replacing {}", exe.display()))?;
    Ok(())
}

/// The release asset for this machine: the Linux tarball built for `arch`.
fn pick_asset<'a>(assets: &'a [Asset], arch: &str) -> Option<&'a Asset> {
    assets
        .iter()
        .find(|asset| asset.name.contains(arch) && asset.name.contains("linux") && asset.name.ends_with(".tar.gz"))
}

fn asset_names(assets: &[Asset]) -> String {
    if assets.is_empty() {
        return "none".to_string();
    }
    assets
        .iter()
        .map(|asset| asset.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether `candidate` is a later release than `current`.
///
/// Versions that cannot be parsed are treated as "not newer", so a tag in an
/// unexpected shape never triggers a download.
fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// Parses `1.2.3`, `v1.2.3`, or `1.2.3-rc1` into comparable parts. A
/// pre-release suffix is dropped, so it compares equal to the final release.
fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let version = version.trim().trim_start_matches('v');
    let version = version.split(['-', '+']).next()?;
    let mut parts = version.split('.').map(str::parse::<u64>);
    let major = parts.next()?.ok()?;
    let minor = parts.next().transpose().ok()?.unwrap_or(0);
    let patch = parts.next().transpose().ok()?.unwrap_or(0);
    Some((major, minor, patch))
}

/// Restarts the sync service so it picks up the new binary — but only when the
/// binary just replaced is the one the unit actually runs, so updating some
/// other copy does not bounce the running service.
fn restart_service_if_running(exe: &Path) {
    if !service_runs(exe) || !service_is_active() {
        return;
    }
    match checked_status("systemctl", &["restart", SERVICE_NAME]) {
        Ok(()) => println!("Restarted {SERVICE_NAME}."),
        Err(e) => eprintln!("Warning: could not restart {SERVICE_NAME}: {e:#}"),
    }
}

/// Whether the installed unit starts this exact binary.
fn service_runs(exe: &Path) -> bool {
    fs::read_to_string(SERVICE_PATH).is_ok_and(|unit| {
        unit.lines()
            .filter_map(|line| line.trim().strip_prefix("ExecStart="))
            .any(|command| command.split_whitespace().next() == Some(&exe.to_string_lossy()))
    })
}

fn service_is_active() -> bool {
    command_stdout("systemctl", &["is-active", SERVICE_NAME]).is_some_and(|state| state == "active")
}

// --- self uninstall ---

/// Removes the service, the configuration, and the binary itself.
pub fn uninstall(yes: bool, dry_run: bool, keep_config: bool) -> Result<()> {
    let exe = current_exe()?;
    let package = owning_package(&exe);
    let service_installed = Path::new(SERVICE_PATH).exists();
    let dirs = if keep_config { Vec::new() } else { data_dirs() };
    let files = if keep_config { Vec::new() } else { completion_files() };

    println!("This will remove gigabytectl from this machine:");
    if service_installed {
        println!("  - disable and remove {SERVICE_PATH}");
    }
    for path in dirs.iter().chain(&files) {
        println!("  - {}", path.display());
    }
    match &package {
        Some(package) => println!(
            "  - keep {} (owned by the package '{package}' — remove it with your package manager)",
            exe.display()
        ),
        None => println!("  - {}", exe.display()),
    }
    if keep_config {
        println!("  (configuration and profiles are kept: --keep-config)");
    }

    if dry_run {
        println!("\nDry run: nothing was removed.");
        return Ok(());
    }
    ensure!(
        system::is_root(),
        "gigabytectl: uninstalling touches /etc and the installed binary; run as root (try: sudo gigabytectl self uninstall)"
    );
    if !yes {
        ensure!(confirm()?, "Aborted; nothing was removed.");
    }

    if service_installed {
        remove_service();
    }
    for dir in &dirs {
        remove_data_dir(dir);
    }
    for file in &files {
        remove_file(file);
    }
    // The binary goes last, so a failure earlier still leaves a working tool.
    if package.is_none() {
        remove_file(&exe);
        if exe.to_string_lossy().contains("/.cargo/") {
            println!(
                "Note: this looked like a `cargo install`; run `cargo uninstall gigabytectl` to tidy its metadata."
            );
        }
    }
    println!("\nDone.");
    Ok(())
}

fn confirm() -> Result<bool> {
    ensure!(io::stdin().is_terminal(), "not running interactively; pass --yes to confirm");
    print!("\nType 'yes' to continue: ");
    io::stdout().flush().ok();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).context("reading answer")?;
    Ok(answer.trim().eq_ignore_ascii_case("yes"))
}

fn remove_service() {
    // Not being enabled or running is not a failure worth reporting here.
    let _ = command_stdout("systemctl", &["disable", "--now", SERVICE_NAME]);
    remove_file(Path::new(SERVICE_PATH));
    let _ = command_stdout("systemctl", &["daemon-reload"]);
    println!("Disabled and removed {SERVICE_NAME}");
}

/// Every gigabytectl data directory on this machine: the system-wide one, the
/// invoking user's, and any other user's, since the tool is normally used
/// through `sudo` and can leave state in more than one home.
fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from(config::SYSTEM_CONFIG_DIR), config::config_dir()];
    for home in home_dirs() {
        dirs.push(home.join(".config").join(DATA_DIR_NAME));
    }
    dirs.retain(|dir| dir.is_dir());
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Documented completion locations (see the README), which are the only ones
/// that can be found without guessing.
fn completion_files() -> Vec<PathBuf> {
    let mut files = vec![PathBuf::from("/etc/bash_completion.d/gigabytectl")];
    for home in home_dirs() {
        files.push(home.join(".zsh/completions/_gigabytectl"));
        files.push(home.join(".config/fish/completions/gigabytectl.fish"));
    }
    files.retain(|file| file.is_file());
    files.sort();
    files.dedup();
    files
}

/// Home directories from the passwd database, plus root's.
fn home_dirs() -> Vec<PathBuf> {
    let mut homes = vec![PathBuf::from("/root")];
    if let Some(passwd) = command_stdout("getent", &["passwd"]) {
        homes.extend(
            passwd
                .lines()
                .filter_map(|line| line.split(':').nth(5))
                .filter(|home| home.starts_with("/home/") || *home == "/root")
                .map(PathBuf::from),
        );
    }
    homes.sort();
    homes.dedup();
    homes
}

/// Removes one of our own directories. The name is checked first so a
/// misresolved path can never turn this into a recursive delete of something
/// else.
fn remove_data_dir(dir: &Path) {
    if dir.file_name().and_then(|name| name.to_str()) != Some(DATA_DIR_NAME) {
        eprintln!("Warning: refusing to remove {} (not a gigabytectl directory)", dir.display());
        return;
    }
    match fs::remove_dir_all(dir) {
        Ok(()) => println!("Removed {}", dir.display()),
        Err(e) => eprintln!("Warning: could not remove {}: {e}", dir.display()),
    }
}

fn remove_file(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => println!("Removed {}", path.display()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("Warning: could not remove {}: {e}", path.display()),
    }
}

// --- shared ---

fn current_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("resolving current executable path")?;
    Ok(fs::canonicalize(&exe).unwrap_or(exe))
}

/// The package that owns `path`, if the distribution's package manager claims
/// it. Anything it owns must be left to it rather than edited underneath it.
fn owning_package(path: &Path) -> Option<String> {
    let path = path.to_string_lossy().to_string();
    if let Some(out) = command_stdout("pacman", &["-Qoq", &path]) {
        return Some(out.lines().next()?.trim().to_string()).filter(|name| !name.is_empty());
    }
    if let Some(out) = command_stdout("dpkg", &["-S", &path]) {
        return Some(out.split(':').next()?.trim().to_string()).filter(|name| !name.is_empty());
    }
    None
}

/// Fails with a useful message when the install directory cannot be written,
/// which in practice means the update needs `sudo`.
fn ensure_writable(dir: &Path) -> Result<()> {
    let probe = dir.join(".gigabytectl-write-test");
    match fs::write(&probe, b"") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => bail!(
            "cannot write to {} ({e}); re-run with sudo (try: sudo gigabytectl self update)",
            dir.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.to_string(),
            browser_download_url: format!("https://example/{name}"),
        }
    }

    #[test]
    fn versions_compare_by_component_not_by_string() {
        assert!(is_newer("0.10.0", "0.9.0"), "10 > 9 numerically");
        assert!(is_newer("v1.0.0", "0.5.0"));
        assert!(is_newer("0.5.1", "0.5.0"));
        assert!(!is_newer("0.5.0", "0.5.0"));
        assert!(!is_newer("0.4.9", "0.5.0"));
        // A tag we cannot read must never look like an update.
        assert!(!is_newer("nightly", "0.5.0"));
        assert!(!is_newer("", "0.5.0"));
    }

    #[test]
    fn version_parsing_tolerates_tags_and_suffixes() {
        assert_eq!(parse_version("v0.5.0"), Some((0, 5, 0)));
        assert_eq!(parse_version(" 1.2.3 "), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("2"), Some((2, 0, 0)));
        assert_eq!(parse_version("1.2.3-rc1"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.x"), None);
        assert_eq!(parse_version("v"), None);
    }

    #[test]
    fn the_asset_for_this_machine_is_the_matching_linux_tarball() {
        let assets = [
            asset("gigabytectl-v0.6.0-aarch64-unknown-linux-gnu.tar.gz"),
            asset("gigabytectl-v0.6.0-x86_64-unknown-linux-gnu.tar.gz"),
            asset("gigabytectl-v0.6.0-x86_64-unknown-linux-gnu.tar.gz.sha256"),
        ];
        assert_eq!(
            pick_asset(&assets, "x86_64").map(|a| a.name.as_str()),
            Some("gigabytectl-v0.6.0-x86_64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!(
            pick_asset(&assets, "aarch64").map(|a| a.name.as_str()),
            Some("gigabytectl-v0.6.0-aarch64-unknown-linux-gnu.tar.gz")
        );
        assert!(pick_asset(&assets, "riscv64").is_none());
        assert!(pick_asset(&[], "x86_64").is_none());
    }

    #[test]
    fn the_releases_response_is_read_from_the_fields_we_rely_on() {
        let body = r#"{"tag_name":"v0.6.0","name":"0.6.0","assets":[
            {"name":"gigabytectl-v0.6.0-x86_64-unknown-linux-gnu.tar.gz",
             "browser_download_url":"https://example/t.tar.gz","size":1}]}"#;
        let release: Release = serde_json::from_str(body).unwrap();
        assert_eq!(release.tag_name, "v0.6.0");
        assert_eq!(release.assets[0].browser_download_url, "https://example/t.tar.gz");
        // A release with no assets yet must parse rather than fail.
        let empty: Release = serde_json::from_str(r#"{"tag_name":"v0.6.0"}"#).unwrap();
        assert!(empty.assets.is_empty());
    }

    #[test]
    fn only_our_own_directories_are_removable() {
        let dir = std::env::temp_dir().join("gigabytectl-uninstall-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("keep")).unwrap();

        // Wrong name: left alone.
        remove_data_dir(&dir);
        assert!(dir.is_dir(), "a directory that is not ours must survive");

        let ours = dir.join(DATA_DIR_NAME);
        fs::create_dir_all(&ours).unwrap();
        remove_data_dir(&ours);
        assert!(!ours.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
