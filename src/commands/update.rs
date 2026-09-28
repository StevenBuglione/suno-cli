//! Distribution-aware, verified updates.
//!
//! Package managers retain ownership of their binaries. A standalone binary is
//! replaced only after the exact platform archive has been downloaded over
//! HTTPS, SHA256-verified, safely extracted, and its reported version checked.

use flate2::read::GzDecoder;
use reqwest::blocking::Client;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::errors::CliError;
use crate::output::OutputFormat;

const BREW_FORMULA: &str = "paperfoot/tap/suno";
const RELEASES_URL: &str = "https://github.com/paperfoot/suno-cli/releases";
const LATEST_RELEASE_API: &str = "https://api.github.com/repos/paperfoot/suno-cli/releases/latest";
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Serialize)]
struct UpdateResult {
    current_version: &'static str,
    latest_version: Option<String>,
    status: &'static str,
    install_source: &'static str,
    update_mode: &'static str,
    upgrade_command: Option<String>,
    release_url: Option<String>,
    requires_skill_reinstall: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum InstallSource {
    Homebrew,
    Cargo,
    Standalone,
    Unknown,
}

impl InstallSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Homebrew => "homebrew",
            Self::Cargo => "cargo",
            Self::Standalone => "standalone",
            Self::Unknown => "unknown",
        }
    }

    fn update_mode(self) -> &'static str {
        match self {
            Self::Homebrew | Self::Cargo => "package_manager",
            Self::Standalone => "self_replace",
            Self::Unknown => "instructions_only",
        }
    }

    fn upgrade_command(self) -> Option<String> {
        match self {
            Self::Homebrew => Some(format!("brew upgrade {BREW_FORMULA}")),
            Self::Cargo => Some("cargo install --locked --force suno".to_string()),
            Self::Standalone | Self::Unknown => None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

struct ReleaseInfo {
    version: Version,
    url: String,
    asset_index: Option<usize>,
    release: GithubRelease,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ArchiveKind {
    TarGz,
    Zip,
}

struct PlatformAsset {
    archive_name: String,
    executable_name: &'static str,
    kind: ArchiveKind,
}

/// Explicit override first, then reliable package-manager/path evidence. A
/// development `target/` path is deliberately unknown rather than standalone.
fn detect_install_source() -> Result<InstallSource, CliError> {
    if let Ok(raw) = std::env::var("SUNO_INSTALL_SOURCE") {
        return match raw.trim().to_ascii_lowercase().as_str() {
            "homebrew" | "brew" => Ok(InstallSource::Homebrew),
            "cargo" => Ok(InstallSource::Cargo),
            "standalone" => Ok(InstallSource::Standalone),
            "unknown" => Ok(InstallSource::Unknown),
            other => Err(CliError::Config(format!(
                "invalid SUNO_INSTALL_SOURCE '{other}' (expected homebrew, cargo, standalone, or unknown)"
            ))),
        };
    }

    let exe = std::env::current_exe()?;
    let path = exe.to_string_lossy();
    if path.contains("/Cellar/") || path.starts_with("/opt/homebrew/bin/") {
        return Ok(InstallSource::Homebrew);
    }

    let cargo_bin = std::env::var_os("CARGO_HOME")
        .map(|p| Path::new(&p).join("bin"))
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cargo/bin")));
    if cargo_bin.as_ref().is_some_and(|dir| exe.starts_with(dir)) {
        return Ok(InstallSource::Cargo);
    }

    if exe.components().any(
        |component| matches!(component, Component::Normal(name) if name == OsStr::new("target")),
    ) {
        return Ok(InstallSource::Unknown);
    }
    let known_standalone = [
        Some(PathBuf::from("/usr/local/bin/suno")),
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/bin/suno")),
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("bin/suno")),
    ];
    if known_standalone
        .into_iter()
        .flatten()
        .any(|path| exe == path)
    {
        Ok(InstallSource::Standalone)
    } else {
        Ok(InstallSource::Unknown)
    }
}

fn http_client() -> Result<Client, CliError> {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if attempt.url().scheme() != "https" {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .user_agent(format!("suno/{} updater", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| CliError::Update(format!("could not create update client: {e}")))
}

fn require_https(url: &str) -> Result<reqwest::Url, CliError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| CliError::Update(format!("invalid release URL: {e}")))?;
    if parsed.scheme() != "https" {
        return Err(CliError::Update(format!(
            "refusing non-HTTPS release URL: {url}"
        )));
    }
    Ok(parsed)
}

fn download_limited(client: &Client, url: &str, max_bytes: u64) -> Result<Vec<u8>, CliError> {
    let url = require_https(url)?;
    let response = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|e| CliError::Update(format!("release download failed: {e}")))?;
    if response.url().scheme() != "https" {
        return Err(CliError::Update(
            "release download redirected to a non-HTTPS URL".into(),
        ));
    }
    if response
        .content_length()
        .is_some_and(|size| size > max_bytes)
    {
        return Err(CliError::Update(format!(
            "release download exceeds the {max_bytes} byte limit"
        )));
    }

    let mut bytes = Vec::new();
    response
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| CliError::Update(format!("could not read release download: {e}")))?;
    if bytes.len() as u64 > max_bytes {
        return Err(CliError::Update(format!(
            "release download exceeds the {max_bytes} byte limit"
        )));
    }
    Ok(bytes)
}

fn parse_release(bytes: &[u8]) -> Result<GithubRelease, CliError> {
    let release: GithubRelease = serde_json::from_slice(bytes)
        .map_err(|e| CliError::Update(format!("invalid GitHub release metadata: {e}")))?;
    if release.draft || release.prerelease {
        return Err(CliError::Update(
            "GitHub latest release was not a stable published release".into(),
        ));
    }
    require_https(&release.html_url)?;
    Ok(release)
}

fn platform_asset() -> Option<PlatformAsset> {
    let (target, kind, executable_name) = if cfg!(all(target_os = "macos", target_arch = "x86_64"))
    {
        ("x86_64-apple-darwin", ArchiveKind::TarGz, "suno")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ("aarch64-apple-darwin", ArchiveKind::TarGz, "suno")
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu"
    )) {
        ("x86_64-unknown-linux-gnu", ArchiveKind::TarGz, "suno")
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "aarch64",
        target_env = "gnu"
    )) {
        ("aarch64-unknown-linux-gnu", ArchiveKind::TarGz, "suno")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        ("x86_64-pc-windows-msvc", ArchiveKind::Zip, "suno.exe")
    } else {
        return None;
    };

    let extension = match kind {
        ArchiveKind::TarGz => "tar.gz",
        ArchiveKind::Zip => "zip",
    };
    Some(PlatformAsset {
        archive_name: format!("suno-{target}.{extension}"),
        executable_name,
        kind,
    })
}

fn fetch_latest(
    client: &Client,
    platform: Option<&PlatformAsset>,
) -> Result<ReleaseInfo, CliError> {
    let release = parse_release(&download_limited(
        client,
        LATEST_RELEASE_API,
        MAX_METADATA_BYTES,
    )?)?;
    let raw_version = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    let version = Version::parse(raw_version).map_err(|e| {
        CliError::Update(format!(
            "invalid release version '{}': {e}",
            release.tag_name
        ))
    })?;
    if !version.pre.is_empty() {
        return Err(CliError::Update(
            "GitHub latest release has a prerelease version".into(),
        ));
    }
    let asset_index = platform.and_then(|wanted| {
        release
            .assets
            .iter()
            .position(|asset| asset.name == wanted.archive_name)
    });
    let url = release.html_url.clone();
    Ok(ReleaseInfo {
        version,
        url,
        asset_index,
        release,
    })
}

fn decode_sha256(raw: &str) -> Result<[u8; 32], CliError> {
    let digest = raw.strip_prefix("sha256:").unwrap_or(raw);
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(CliError::Update(
            "release SHA256 must contain exactly 64 hexadecimal characters".into(),
        ));
    }
    let mut decoded = [0_u8; 32];
    for (index, byte) in decoded.iter_mut().enumerate() {
        let pair = &digest.as_bytes()[index * 2..index * 2 + 2];
        let text = std::str::from_utf8(pair)
            .map_err(|e| CliError::Update(format!("invalid SHA256 encoding: {e}")))?;
        *byte = u8::from_str_radix(text, 16)
            .map_err(|e| CliError::Update(format!("invalid SHA256 encoding: {e}")))?;
    }
    Ok(decoded)
}

fn checksum_entry(contents: &str, asset_name: &str) -> Result<[u8; 32], CliError> {
    let mut matching = contents.lines().filter_map(|line| {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let mut fields = line.split_whitespace();
        let digest = fields.next()?;
        let name = fields.next()?;
        if fields.next().is_none() && name.trim_start_matches('*') == asset_name {
            Some(digest)
        } else {
            None
        }
    });
    let digest = matching.next().ok_or_else(|| {
        CliError::Update(format!(
            "checksums.txt has no exact SHA256 entry for {asset_name}"
        ))
    })?;
    if matching.next().is_some() {
        return Err(CliError::Update(format!(
            "checksums.txt has multiple entries for {asset_name}"
        )));
    }
    decode_sha256(digest)
}

fn trusted_checksum(
    client: &Client,
    release: &GithubRelease,
    asset: &GithubAsset,
) -> Result<[u8; 32], CliError> {
    if let Some(digest) = &asset.digest {
        let sha256 = digest.strip_prefix("sha256:").ok_or_else(|| {
            CliError::Update(format!(
                "GitHub returned an unsupported digest for {}",
                asset.name
            ))
        })?;
        return decode_sha256(sha256);
    }

    let checksums = release
        .assets
        .iter()
        .find(|candidate| candidate.name == "checksums.txt")
        .ok_or_else(|| {
            CliError::Update(format!(
                "release asset {} has no GitHub SHA256 digest or checksums.txt entry",
                asset.name
            ))
        })?;
    if checksums.size > MAX_METADATA_BYTES {
        return Err(CliError::Update(
            "checksums.txt exceeds the metadata size limit".into(),
        ));
    }
    let bytes = download_limited(client, &checksums.browser_download_url, MAX_METADATA_BYTES)?;
    let contents = std::str::from_utf8(&bytes)
        .map_err(|e| CliError::Update(format!("checksums.txt is not UTF-8: {e}")))?;
    checksum_entry(contents, &asset.name)
}

fn verify_sha256(bytes: &[u8], expected: &[u8; 32]) -> Result<(), CliError> {
    let actual = Sha256::digest(bytes);
    if actual.as_slice() != expected {
        return Err(CliError::Update(
            "release archive SHA256 verification failed".into(),
        ));
    }
    Ok(())
}

fn valid_archive_path(path: &Path, executable_name: &str) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Normal(name)) if name == OsStr::new(executable_name))
        && components.next().is_none()
}

fn copy_bounded<R: Read>(mut reader: R, destination: &mut File) -> Result<(), CliError> {
    let copied = std::io::copy(
        &mut reader.by_ref().take(MAX_ARCHIVE_BYTES + 1),
        destination,
    )
    .map_err(|e| CliError::Update(format!("could not extract release executable: {e}")))?;
    if copied > MAX_ARCHIVE_BYTES {
        return Err(CliError::Update(
            "extracted executable exceeds the 64 MiB limit".into(),
        ));
    }
    destination
        .flush()
        .map_err(|e| CliError::Update(format!("could not flush staged executable: {e}")))?;
    Ok(())
}

fn extract_archive(
    bytes: &[u8],
    platform: &PlatformAsset,
    destination: &Path,
) -> Result<(), CliError> {
    match platform.kind {
        ArchiveKind::TarGz => {
            let decoder = GzDecoder::new(Cursor::new(bytes));
            let mut archive = tar::Archive::new(decoder);
            let mut entries = archive
                .entries()
                .map_err(|e| CliError::Update(format!("invalid tar archive: {e}")))?;
            let mut entry = entries
                .next()
                .ok_or_else(|| CliError::Update("release archive is empty".into()))?
                .map_err(|e| CliError::Update(format!("invalid tar entry: {e}")))?;
            let path = entry
                .path()
                .map_err(|e| CliError::Update(format!("invalid tar path: {e}")))?;
            if !valid_archive_path(&path, platform.executable_name)
                || !entry.header().entry_type().is_file()
            {
                return Err(CliError::Update(format!(
                    "release archive must contain only root {}",
                    platform.executable_name
                )));
            }
            if entry.size() > MAX_ARCHIVE_BYTES {
                return Err(CliError::Update(
                    "extracted executable exceeds the 64 MiB limit".into(),
                ));
            }
            let mut file = File::create(destination)?;
            copy_bounded(&mut entry, &mut file)?;
            drop(entry);
            if entries.next().is_some() {
                return Err(CliError::Update(format!(
                    "release archive must contain only root {}",
                    platform.executable_name
                )));
            }
        }
        ArchiveKind::Zip => {
            let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
                .map_err(|e| CliError::Update(format!("invalid zip archive: {e}")))?;
            if archive.len() != 1 {
                return Err(CliError::Update(format!(
                    "release archive must contain only root {}",
                    platform.executable_name
                )));
            }
            let mut entry = archive
                .by_index(0)
                .map_err(|e| CliError::Update(format!("invalid zip entry: {e}")))?;
            let path = entry.enclosed_name().ok_or_else(|| {
                CliError::Update("release archive contains an unsafe path".into())
            })?;
            if !valid_archive_path(&path, platform.executable_name)
                || entry.is_dir()
                || entry.size() > MAX_ARCHIVE_BYTES
            {
                return Err(CliError::Update(format!(
                    "release archive must contain only root {}",
                    platform.executable_name
                )));
            }
            let mut file = File::create(destination)?;
            copy_bounded(&mut entry, &mut file)?;
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn reported_version(text: &str) -> Option<Version> {
    fn parse_text(text: &str) -> Option<Version> {
        text.split_whitespace().find_map(|token| {
            Version::parse(
                token
                    .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '-')
                    .trim_start_matches('v'),
            )
            .ok()
        })
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        for pointer in ["/data/version", "/data/usage"] {
            if let Some(version) = value
                .pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .and_then(parse_text)
            {
                return Some(version);
            }
        }
    }
    parse_text(text)
}

fn verify_reported_version(text: &str, expected: &Version) -> Result<(), CliError> {
    let actual = reported_version(text).ok_or_else(|| {
        CliError::Update("staged executable did not report a parseable version".into())
    })?;
    if &actual != expected {
        return Err(CliError::Update(format!(
            "staged executable reports {actual}, expected {expected}"
        )));
    }
    Ok(())
}

fn validate_staged_version(path: &Path, expected: &Version) -> Result<(), CliError> {
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CliError::Update(format!("could not run staged executable: {e}")))?;
    let started = Instant::now();
    loop {
        match child
            .try_wait()
            .map_err(|e| CliError::Update(format!("could not validate staged executable: {e}")))?
        {
            Some(_) => break,
            None if started.elapsed() < VERSION_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20));
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CliError::Update(
                    "staged executable --version timed out after 10 seconds".into(),
                ));
            }
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| CliError::Update(format!("could not read staged --version output: {e}")))?;
    if !output.status.success() {
        return Err(CliError::Update(format!(
            "staged executable --version exited with {}",
            output.status
        )));
    }
    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|e| CliError::Update(format!("staged --version output is not UTF-8: {e}")))?;
    verify_reported_version(stdout, expected)
}

fn print_result(result: &UpdateResult, fmt: OutputFormat, quiet: bool) -> Result<(), CliError> {
    match fmt {
        OutputFormat::Json => crate::output::json::success(result)?,
        OutputFormat::Table if quiet => {}
        OutputFormat::Table => match result.status {
            "managed_install" | "not_checked" => {
                eprintln!("Installed via {}", result.install_source);
                if let Some(command) = &result.upgrade_command {
                    eprintln!("Update with: {command}");
                } else {
                    eprintln!("See {RELEASES_URL} for verified installation instructions");
                }
            }
            "unsupported_platform" => {
                eprintln!("No standalone release asset is available for this platform");
                eprintln!("See {RELEASES_URL}");
            }
            "up_to_date" => eprintln!("Up to date (v{})", result.current_version),
            "update_available" => {
                let latest = result.latest_version.as_deref().unwrap_or("unknown");
                eprintln!("Update available: v{} -> v{latest}", result.current_version);
                if let Some(command) = &result.upgrade_command {
                    eprintln!("Update with: {command}");
                } else if result.update_mode == "self_replace" {
                    eprintln!("Run `suno update` to install");
                } else {
                    eprintln!("See {RELEASES_URL} for installation instructions");
                }
            }
            "updated" => {
                let latest = result.latest_version.as_deref().unwrap_or("unknown");
                eprintln!("Updated: v{} -> v{latest}", result.current_version);
                eprintln!("Run `suno skill install` to refresh the agent skill");
            }
            _ => {}
        },
    }
    Ok(())
}

fn unchecked_result(source: InstallSource) -> UpdateResult {
    UpdateResult {
        current_version: env!("CARGO_PKG_VERSION"),
        latest_version: None,
        status: if source == InstallSource::Unknown {
            "not_checked"
        } else {
            "managed_install"
        },
        install_source: source.as_str(),
        update_mode: source.update_mode(),
        upgrade_command: source.upgrade_command(),
        release_url: Some(RELEASES_URL.to_string()),
        requires_skill_reinstall: false,
    }
}

pub fn run(check: bool, force: bool, fmt: OutputFormat, quiet: bool) -> Result<(), CliError> {
    let current_text = env!("CARGO_PKG_VERSION");
    let current = Version::parse(current_text)
        .map_err(|e| CliError::Update(format!("invalid installed version: {e}")))?;
    let source = detect_install_source()?;

    if !check && source != InstallSource::Standalone {
        return print_result(&unchecked_result(source), fmt, quiet);
    }

    let client = http_client()?;
    let platform = platform_asset();
    let release = fetch_latest(&client, platform.as_ref())?;
    let latest_text = release.version.to_string();
    let available = release.version > current;

    if platform.is_none() || release.asset_index.is_none() {
        let result = UpdateResult {
            current_version: current_text,
            latest_version: Some(latest_text),
            status: "unsupported_platform",
            install_source: source.as_str(),
            update_mode: source.update_mode(),
            upgrade_command: source.upgrade_command(),
            release_url: Some(release.url),
            requires_skill_reinstall: false,
        };
        return print_result(&result, fmt, quiet);
    }

    if check || source != InstallSource::Standalone || !available {
        let result = UpdateResult {
            current_version: current_text,
            latest_version: Some(latest_text),
            status: if available {
                "update_available"
            } else {
                "up_to_date"
            },
            install_source: source.as_str(),
            update_mode: source.update_mode(),
            upgrade_command: source.upgrade_command(),
            release_url: Some(release.url),
            requires_skill_reinstall: available,
        };
        return print_result(&result, fmt, quiet);
    }

    let mut guard = crate::guard::DuplicateGuard::new(&crate::config::data_dir(), "update");
    guard.acquire(force)?;

    let platform = platform.expect("checked above");
    let asset_index = release.asset_index.expect("checked above");
    let asset = &release.release.assets[asset_index];
    if asset.size > MAX_ARCHIVE_BYTES {
        return Err(CliError::Update(format!(
            "release asset {} exceeds the 64 MiB limit",
            asset.name
        )));
    }
    require_https(&asset.browser_download_url)?;
    let expected_sha256 = trusted_checksum(&client, &release.release, asset)?;
    let archive = download_limited(&client, &asset.browser_download_url, MAX_ARCHIVE_BYTES)?;
    verify_sha256(&archive, &expected_sha256)?;

    let current_exe = std::env::current_exe()?;
    let exe_parent = current_exe
        .parent()
        .ok_or_else(|| CliError::Update("installed executable has no parent directory".into()))?;
    let staging = tempfile::tempdir_in(exe_parent)
        .map_err(|e| CliError::Update(format!("could not create update staging directory: {e}")))?;
    let staged_exe: PathBuf = staging.path().join(platform.executable_name);
    extract_archive(&archive, &platform, &staged_exe)?;
    validate_staged_version(&staged_exe, &release.version)?;

    self_replace::self_replace(&staged_exe)
        .map_err(|e| CliError::Update(format!("could not replace installed executable: {e}")))?;

    let result = UpdateResult {
        current_version: current_text,
        latest_version: Some(latest_text),
        status: "updated",
        install_source: source.as_str(),
        update_mode: source.update_mode(),
        upgrade_command: None,
        release_url: Some(release.url),
        requires_skill_reinstall: true,
    };
    print_result(&result, fmt, quiet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut compressed = Vec::new();
        {
            let encoder =
                flate2::write::GzEncoder::new(&mut compressed, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            for (name, body) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder.append_data(&mut header, name, *body).unwrap();
            }
            builder.finish().unwrap();
        }
        compressed
    }

    fn tar_platform() -> PlatformAsset {
        PlatformAsset {
            archive_name: "suno-test.tar.gz".into(),
            executable_name: "suno",
            kind: ArchiveKind::TarGz,
        }
    }

    #[test]
    fn sha256_accepts_exact_hash_and_rejects_mismatch() {
        let expected =
            decode_sha256("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
                .unwrap();
        verify_sha256(b"abc", &expected).unwrap();
        assert!(verify_sha256(b"abd", &expected).is_err());
        assert!(decode_sha256("1234").is_err());
    }

    #[test]
    fn checksum_requires_one_exact_asset_entry() {
        let hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(checksum_entry(&format!("{hash}  suno-test.tar.gz\n"), "suno-test.tar.gz").is_ok());
        assert!(
            checksum_entry(
                &format!("{hash}  prefix-suno-test.tar.gz\n"),
                "suno-test.tar.gz"
            )
            .is_err()
        );
        assert!(
            checksum_entry(
                &format!("{hash}  suno-test.tar.gz extra\n"),
                "suno-test.tar.gz"
            )
            .is_err()
        );
    }

    #[test]
    fn archive_path_allows_only_expected_root_file() {
        assert!(valid_archive_path(Path::new("suno"), "suno"));
        assert!(!valid_archive_path(Path::new("bin/suno"), "suno"));
        assert!(!valid_archive_path(Path::new("../suno"), "suno"));
        assert!(!valid_archive_path(Path::new("suno.exe"), "suno"));
    }

    #[test]
    fn extraction_accepts_single_root_executable() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("staged-suno");
        extract_archive(
            &tar_gz(&[("suno", b"binary")]),
            &tar_platform(),
            &destination,
        )
        .unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), b"binary");
    }

    #[test]
    fn extraction_rejects_unexpected_or_multiple_paths() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("staged-suno");
        assert!(
            extract_archive(
                &tar_gz(&[("bin/suno", b"binary")]),
                &tar_platform(),
                &destination
            )
            .is_err()
        );
        assert!(
            extract_archive(
                &tar_gz(&[("suno", b"binary"), ("README", b"text")]),
                &tar_platform(),
                &destination
            )
            .is_err()
        );
    }

    #[test]
    fn version_parser_accepts_plain_and_json_usage() {
        let expected = Version::parse("1.2.3").unwrap();
        assert_eq!(reported_version("suno 1.2.3\n"), Some(expected.clone()));
        assert_eq!(
            reported_version(r#"{"version":"1","status":"success","data":{"usage":"suno 1.2.3"}}"#),
            Some(expected.clone())
        );
        assert_eq!(reported_version("not-a-version"), None);
        verify_reported_version("suno 1.2.3", &expected).unwrap();
        assert!(verify_reported_version("suno 1.2.4", &expected).is_err());
        assert!(verify_reported_version("garbage", &expected).is_err());
    }
}
