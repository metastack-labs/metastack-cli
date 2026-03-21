use std::env;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use flate2::read::GzDecoder;
use reqwest::{
    Client,
    header::{ACCEPT, USER_AGENT},
};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tar::Archive;

use crate::cli::UpgradeArgs;

const INSTALLER_URL: &str = "https://raw.githubusercontent.com/metastack-systems/metastack-cli/main/scripts/install-meta.sh";

/// Resolve release metadata, verify the selected archive, and replace the running install when
/// this executable appears to come from a supported GitHub Release install. Returns an error when
/// release lookup, verification, or the staged replacement/rollback flow fails.
pub(crate) async fn run_upgrade(args: &UpgradeArgs) -> Result<()> {
    let executable_path = args
        .executable_path
        .clone()
        .unwrap_or(env::current_exe().context("failed to resolve the current `meta` executable")?);
    let install = InstalledBinary::inspect(&executable_path)?;
    let origin = InstallOrigin::detect(&executable_path);
    let platform = Platform::detect(args)?;
    let release_client = ReleaseClient::new(&args.github_api_url, &args.repository)?;
    let target_release = release_client.resolve_release(args).await?;
    let target_version = parse_tag_version(&target_release.tag_name)?;

    if !args.check {
        origin.ensure_supported(&executable_path)?;
        ensure_requested_upgrade_is_allowed(
            &install.version,
            &target_version,
            args.allow_downgrade,
        )?;
    }

    let asset_name = platform.asset_name(&target_version);
    let archive_asset = target_release.find_asset(&asset_name).ok_or_else(|| {
        anyhow!(
            "release '{}' is missing asset '{}'",
            target_release.tag_name,
            asset_name
        )
    })?;
    let checksum_asset = target_release.find_asset("SHA256SUMS").ok_or_else(|| {
        anyhow!(
            "release '{}' is missing asset 'SHA256SUMS'",
            target_release.tag_name
        )
    })?;

    if args.check {
        print_check_report(
            &install,
            origin,
            &platform,
            &target_release,
            &asset_name,
            &target_version,
        );
        return Ok(());
    }

    let helper = choose_elevation_helper(&executable_path);
    if args.dry_run {
        print_dry_run_report(
            &install,
            &platform,
            &target_release,
            &asset_name,
            &target_version,
            helper.as_ref(),
        );
        return Ok(());
    }

    if install.version == target_version {
        println!(
            "meta {} at {} is already up to date",
            install.version,
            executable_path.display()
        );
        return Ok(());
    }

    let staging = TempArtifactDir::new("upgrade")?;
    let archive_bytes = release_client
        .download_bytes(&archive_asset.browser_download_url)
        .await
        .with_context(|| format!("failed to download '{}'", archive_asset.name))?;
    let checksum_manifest = release_client
        .download_text(&checksum_asset.browser_download_url)
        .await
        .context("failed to download the release checksum manifest")?;
    let expected_checksum =
        manifest_checksum_for_asset(&checksum_manifest, &asset_name, &target_release.tag_name)?;
    let actual_checksum = sha256_hex(&archive_bytes);
    if actual_checksum != expected_checksum {
        bail!("checksum mismatch for '{}'", asset_name);
    }

    let staged_binary = staging.path().join("meta");
    extract_binary_from_archive(&archive_bytes, &staged_binary, &asset_name)?;
    verify_installed_binary_version(&staged_binary, &target_version)
        .context("staged release verification failed before installation")?;

    if install_target_is_writable(&executable_path)? {
        install_with_local_swap(&staged_binary, &executable_path, &target_version)?;
    } else if let Some(helper) = helper {
        install_with_elevation(&helper, &staged_binary, &executable_path, &target_version)?;
    } else {
        bail!(
            "install target '{}' is not writable and no supported elevation helper (`sudo` or `doas`) was found",
            executable_path.display()
        );
    }

    println!(
        "upgraded meta from {} to {} at {}",
        install.version,
        target_version,
        executable_path.display()
    );

    Ok(())
}

#[derive(Debug, Clone)]
struct InstalledBinary {
    path: PathBuf,
    version: Version,
}

impl InstalledBinary {
    fn inspect(path: &Path) -> Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            version: read_binary_version(path)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallOrigin {
    StandaloneRelease,
    CargoInstall,
    SourceCheckoutBuild,
}

impl InstallOrigin {
    fn detect(path: &Path) -> Self {
        if looks_like_cargo_install(path) {
            return Self::CargoInstall;
        }
        if looks_like_source_checkout_build(path) {
            return Self::SourceCheckoutBuild;
        }
        Self::StandaloneRelease
    }

    fn label(self) -> &'static str {
        match self {
            Self::StandaloneRelease => "github_release_install",
            Self::CargoInstall => "cargo_install",
            Self::SourceCheckoutBuild => "source_checkout_build",
        }
    }

    fn ensure_supported(self, executable_path: &Path) -> Result<()> {
        match self {
            Self::StandaloneRelease => Ok(()),
            Self::CargoInstall => bail!(
                "self-update only supports GitHub Release installs, but '{}' looks like a Cargo install.\nUpgrade this install with Cargo, or reinstall from GitHub Releases to enable `meta upgrade`:\n  curl -fsSL {} | sh",
                executable_path.display(),
                INSTALLER_URL
            ),
            Self::SourceCheckoutBuild => bail!(
                "self-update only supports GitHub Release installs, but '{}' looks like a source-checkout build under `target/`.\nRebuild from source in your checkout, or reinstall from GitHub Releases to enable `meta upgrade`:\n  curl -fsSL {} | sh",
                executable_path.display(),
                INSTALLER_URL
            ),
        }
    }
}

#[derive(Debug, Clone)]
struct Platform {
    os: String,
    arch: String,
}

impl Platform {
    fn detect(args: &UpgradeArgs) -> Result<Self> {
        let os = if let Some(os) = args.os.as_deref() {
            os.to_string()
        } else {
            match env::consts::OS {
                "macos" => "darwin".to_string(),
                "linux" => "linux".to_string(),
                other => bail!(
                    "unsupported operating system '{}' (supported: macOS, Linux)",
                    other
                ),
            }
        };
        let arch = if let Some(arch) = args.arch.as_deref() {
            arch.to_string()
        } else {
            match env::consts::ARCH {
                "aarch64" => "arm64".to_string(),
                "x86_64" => "x64".to_string(),
                other => bail!(
                    "unsupported architecture '{}' (supported: x86_64, arm64)",
                    other
                ),
            }
        };
        Ok(Self { os, arch })
    }

    fn slug(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }

    fn asset_name(&self, version: &Version) -> String {
        format!("metastack-cli-{}-{}.tar.gz", version, self.slug())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GithubReleaseAsset>,
}

impl GithubRelease {
    fn find_asset(&self, name: &str) -> Option<&GithubReleaseAsset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct GithubReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone)]
struct ReleaseClient {
    api_root: String,
    repository: String,
    http: Client,
}

impl ReleaseClient {
    fn new(api_root: &str, repository: &str) -> Result<Self> {
        Ok(Self {
            api_root: api_root.trim_end_matches('/').to_string(),
            repository: repository.to_string(),
            http: Client::builder()
                .build()
                .context("failed to initialize the HTTP client")?,
        })
    }

    async fn resolve_release(&self, args: &UpgradeArgs) -> Result<GithubRelease> {
        if let Some(version) = args.version.as_deref() {
            let normalized = normalize_version(version);
            let parsed = Version::parse(&normalized)
                .with_context(|| format!("failed to parse version '{}'", version))?;
            if !parsed.pre.is_empty() && !args.prerelease {
                bail!(
                    "version '{}' is a prerelease; rerun with `--prerelease` to opt in",
                    version
                );
            }
            return self.release_by_tag(&format!("v{normalized}")).await;
        }

        if args.prerelease {
            return self.latest_release_including_prereleases().await;
        }

        self.latest_stable_release().await
    }

    async fn latest_stable_release(&self) -> Result<GithubRelease> {
        self.get_json(&format!(
            "{}/repos/{}/releases/latest",
            self.api_root, self.repository
        ))
        .await
        .context("failed to resolve the latest stable release")
    }

    async fn latest_release_including_prereleases(&self) -> Result<GithubRelease> {
        let releases: Vec<GithubRelease> = self
            .get_json(&format!(
                "{}/repos/{}/releases",
                self.api_root, self.repository
            ))
            .await
            .context("failed to list GitHub releases")?;
        releases
            .into_iter()
            .find(|release| !release.draft)
            .ok_or_else(|| anyhow!("no published releases were found"))
    }

    async fn release_by_tag(&self, tag: &str) -> Result<GithubRelease> {
        self.get_json(&format!(
            "{}/repos/{}/releases/tags/{}",
            self.api_root, self.repository, tag
        ))
        .await
        .with_context(|| format!("failed to resolve release '{}'", tag))
    }

    async fn get_json<T>(&self, url: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let response = self
            .http
            .get(url)
            .header(
                USER_AGENT,
                format!("metastack-cli/{}", env!("CARGO_PKG_VERSION")),
            )
            .header(ACCEPT, "application/vnd.github+json")
            .send()
            .await
            .with_context(|| format!("failed to reach '{}'", url))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read the GitHub response body")?;
        if !status.is_success() {
            bail!("GitHub request failed with status {status}: {body}");
        }
        serde_json::from_str(&body).context("failed to decode the GitHub release payload")
    }

    async fn download_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let response = self
            .http
            .get(url)
            .header(
                USER_AGENT,
                format!("metastack-cli/{}", env!("CARGO_PKG_VERSION")),
            )
            .send()
            .await
            .with_context(|| format!("failed to reach '{}'", url))?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .context("failed to read the download response body")?;
            bail!("download failed with status {status}: {body}");
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .context("failed to read the downloaded release asset")
    }

    async fn download_text(&self, url: &str) -> Result<String> {
        let bytes = self.download_bytes(url).await?;
        String::from_utf8(bytes).context("downloaded text asset was not valid UTF-8")
    }
}

#[derive(Debug, Clone)]
struct ElevationHelper {
    command: String,
}

impl ElevationHelper {
    fn name(&self) -> &str {
        &self.command
    }
}

#[derive(Debug)]
struct TempArtifactDir {
    path: PathBuf,
}

impl TempArtifactDir {
    fn new(prefix: &str) -> Result<Self> {
        let base = env::temp_dir();
        for attempt in 0..32 {
            let candidate = base.join(format!(
                "metastack-{prefix}-{}-{}-{attempt}",
                std::process::id(),
                unique_suffix()
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => return Ok(Self { path: candidate }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to create '{}'", candidate.display()));
                }
            }
        }

        bail!("failed to allocate a temporary staging directory")
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempArtifactDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn print_check_report(
    install: &InstalledBinary,
    origin: InstallOrigin,
    platform: &Platform,
    release: &GithubRelease,
    asset_name: &str,
    target_version: &Version,
) {
    let status = if install.version == *target_version {
        "up to date"
    } else if install.version < *target_version {
        "update available"
    } else {
        "current install is newer than the latest resolved release"
    };

    println!("current version: {}", install.version);
    println!("target version: {}", target_version);
    println!("release tag: {}", release.tag_name);
    println!(
        "release channel: {}",
        if release.prerelease {
            "prerelease"
        } else {
            "stable"
        }
    );
    println!("install origin: {}", origin.label());
    println!("platform: {}", platform.slug());
    println!("asset: {}", asset_name);
    println!("executable: {}", install.path.display());
    println!("status: {}", status);
    if origin != InstallOrigin::StandaloneRelease {
        println!(
            "remediation: reinstall from GitHub Releases to enable `meta upgrade`: curl -fsSL {} | sh",
            INSTALLER_URL
        );
    }
}

fn print_dry_run_report(
    install: &InstalledBinary,
    platform: &Platform,
    release: &GithubRelease,
    asset_name: &str,
    target_version: &Version,
    helper: Option<&ElevationHelper>,
) {
    println!("current version: {}", install.version);
    println!("target version: {}", target_version);
    println!("release tag: {}", release.tag_name);
    println!(
        "release channel: {}",
        if release.prerelease {
            "prerelease"
        } else {
            "stable"
        }
    );
    println!("platform: {}", platform.slug());
    println!("asset: {}", asset_name);
    println!("executable: {}", install.path.display());
    if install.version == *target_version {
        println!("status: already up to date");
        return;
    }
    println!("status: would replace the installed binary");
    match helper {
        Some(helper) => println!("replacement path: elevation via {}", helper.name()),
        None => println!("replacement path: direct in-place swap"),
    }
}

fn ensure_requested_upgrade_is_allowed(
    current_version: &Version,
    target_version: &Version,
    allow_downgrade: bool,
) -> Result<()> {
    if target_version < current_version && !allow_downgrade {
        bail!(
            "refusing to replace {} with older release {}; rerun with `--allow-downgrade` to proceed",
            current_version,
            target_version
        );
    }
    Ok(())
}

fn normalize_version(version: &str) -> String {
    version.trim().trim_start_matches('v').to_string()
}

fn parse_tag_version(tag: &str) -> Result<Version> {
    Version::parse(&normalize_version(tag))
        .with_context(|| format!("failed to parse release tag '{}'", tag))
}

fn manifest_checksum_for_asset(manifest: &str, asset_name: &str, tag: &str) -> Result<String> {
    manifest
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let checksum = parts.next()?;
            let filename = parts.next()?;
            (filename == asset_name).then(|| checksum.to_string())
        })
        .ok_or_else(|| {
            anyhow!(
                "release '{}' is missing a checksum entry for '{}'",
                tag,
                asset_name
            )
        })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn extract_binary_from_archive(
    archive_bytes: &[u8],
    destination: &Path,
    asset_name: &str,
) -> Result<()> {
    let decoder = GzDecoder::new(Cursor::new(archive_bytes));
    let mut archive = Archive::new(decoder);
    let mut found = false;

    for entry in archive
        .entries()
        .with_context(|| format!("failed to open '{}'", asset_name))?
    {
        let mut entry = entry.with_context(|| format!("failed to read '{}'", asset_name))?;
        let path = entry
            .path()
            .with_context(|| format!("failed to inspect '{}'", asset_name))?;
        if entry.header().entry_type().is_dir() {
            continue;
        }
        if !archive_entry_is_root_meta(&path) {
            bail!(
                "release archive '{}' contained unexpected entry '{}'",
                asset_name,
                path.display()
            );
        }
        if found {
            bail!(
                "release archive '{}' contained multiple 'meta' binaries",
                asset_name
            );
        }

        let mut output = fs::File::create(destination)
            .with_context(|| format!("failed to create '{}'", destination.display()))?;
        std::io::copy(&mut entry, &mut output)
            .with_context(|| format!("failed to extract '{}'", asset_name))?;
        output
            .flush()
            .with_context(|| format!("failed to flush '{}'", destination.display()))?;
        set_executable_permissions(destination)?;
        found = true;
    }

    if !found {
        bail!(
            "release archive '{}' did not contain a 'meta' binary",
            asset_name
        );
    }

    Ok(())
}

fn archive_entry_is_root_meta(path: &Path) -> bool {
    let mut components = path.components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(name)), None) if name == OsStr::new("meta")
    )
}

fn install_target_is_writable(executable_path: &Path) -> Result<bool> {
    let parent = executable_path.parent().ok_or_else(|| {
        anyhow!(
            "failed to resolve the parent directory for '{}'",
            executable_path.display()
        )
    })?;
    let probe = parent.join(format!(".meta-upgrade-write-test-{}", unique_suffix()));
    match OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            fs::remove_file(&probe)
                .with_context(|| format!("failed to remove '{}'", probe.display()))?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("failed to probe writability for '{}'", parent.display())),
    }
}

fn choose_elevation_helper(executable_path: &Path) -> Option<ElevationHelper> {
    if install_target_is_writable(executable_path).ok()? {
        return None;
    }

    ["sudo", "doas"]
        .into_iter()
        .find(|candidate| command_exists(candidate))
        .map(|command| ElevationHelper {
            command: command.to_string(),
        })
}

fn install_with_local_swap(
    staged_binary: &Path,
    executable_path: &Path,
    target_version: &Version,
) -> Result<()> {
    let swap = SwapPaths::new(executable_path)?;
    fs::copy(staged_binary, &swap.pending)
        .with_context(|| format!("failed to stage '{}'", swap.pending.display()))?;
    set_executable_permissions(&swap.pending)?;

    fs::rename(executable_path, &swap.backup).with_context(|| {
        format!(
            "failed to move '{}' to backup '{}'",
            executable_path.display(),
            swap.backup.display()
        )
    })?;

    if let Err(error) = fs::rename(&swap.pending, executable_path) {
        let rollback = fs::rename(&swap.backup, executable_path);
        return match rollback {
            Ok(()) => Err(error).with_context(|| {
                format!(
                    "failed to replace '{}' with the verified release binary",
                    executable_path.display()
                )
            }),
            Err(rollback_error) => Err(anyhow!(
                "failed to replace '{}': {error}; rollback also failed: {rollback_error}",
                executable_path.display()
            )),
        };
    }

    if let Err(error) = verify_installed_binary_version(executable_path, target_version) {
        rollback_after_failed_verification(executable_path, &swap.backup)
            .context("failed to roll back after post-install verification failed")?;
        return Err(error).context("post-install verification failed after replacing the binary");
    }

    fs::remove_file(&swap.backup)
        .with_context(|| format!("failed to remove '{}'", swap.backup.display()))?;
    Ok(())
}

fn install_with_elevation(
    helper: &ElevationHelper,
    staged_binary: &Path,
    executable_path: &Path,
    target_version: &Version,
) -> Result<()> {
    let swap = SwapPaths::new(executable_path)?;
    let script_dir = TempArtifactDir::new("upgrade-helper")?;
    let script_path = script_dir.path().join("replace.sh");
    fs::write(&script_path, elevation_script()).with_context(|| {
        format!(
            "failed to write the elevation helper script '{}'",
            script_path.display()
        )
    })?;

    let output = Command::new(&helper.command)
        .arg("-n")
        .arg("sh")
        .arg(&script_path)
        .arg(staged_binary)
        .arg(executable_path)
        .arg(&swap.pending)
        .arg(&swap.backup)
        .arg(target_version.to_string())
        .output()
        .with_context(|| format!("failed to launch '{}'", helper.command))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    if detail.is_empty() {
        bail!(
            "{} failed while replacing '{}'",
            helper.command,
            executable_path.display()
        );
    }
    bail!(
        "{} failed while replacing '{}': {}",
        helper.command,
        executable_path.display(),
        detail
    )
}

fn elevation_script() -> &'static str {
    r#"#!/bin/sh
set -eu

staged=$1
target=$2
pending=$3
backup=$4
expected=$5

cleanup() {
  rm -f "$pending"
}
trap cleanup EXIT INT TERM

cp "$staged" "$pending"
chmod 755 "$pending"
mv "$target" "$backup"
if ! mv "$pending" "$target"; then
  mv "$backup" "$target"
  printf '%s\n' "failed to place the upgraded binary at '$target'" >&2
  exit 1
fi

if "$target" --version | grep -F "meta $expected" >/dev/null 2>&1; then
  rm -f "$backup"
  exit 0
fi

rm -f "$target"
mv "$backup" "$target"
printf '%s\n' "post-install verification failed for '$target'" >&2
exit 1
"#
}

fn rollback_after_failed_verification(executable_path: &Path, backup_path: &Path) -> Result<()> {
    let failed =
        executable_path.with_file_name(format!(".meta-upgrade-failed-{}", unique_suffix()));
    if executable_path.exists() {
        fs::rename(executable_path, &failed).with_context(|| {
            format!(
                "failed to move the rejected binary '{}' out of the way",
                executable_path.display()
            )
        })?;
    }
    fs::rename(backup_path, executable_path).with_context(|| {
        format!(
            "failed to restore '{}' from '{}'",
            executable_path.display(),
            backup_path.display()
        )
    })?;
    if failed.exists() {
        fs::remove_file(&failed)
            .with_context(|| format!("failed to remove '{}'", failed.display()))?;
    }
    Ok(())
}

#[derive(Debug)]
struct SwapPaths {
    pending: PathBuf,
    backup: PathBuf,
}

impl SwapPaths {
    fn new(executable_path: &Path) -> Result<Self> {
        let parent = executable_path.parent().ok_or_else(|| {
            anyhow!(
                "failed to resolve the parent directory for '{}'",
                executable_path.display()
            )
        })?;
        let suffix = unique_suffix();
        Ok(Self {
            pending: parent.join(format!(".meta-upgrade-{suffix}.new")),
            backup: parent.join(format!(".meta-upgrade-{suffix}.bak")),
        })
    }
}

fn verify_installed_binary_version(binary_path: &Path, expected_version: &Version) -> Result<()> {
    let actual = read_binary_version(binary_path)?;
    if actual != *expected_version {
        bail!(
            "expected '{}' to report version {}, found {}",
            binary_path.display(),
            expected_version,
            actual
        );
    }
    Ok(())
}

fn read_binary_version(binary_path: &Path) -> Result<Version> {
    let output = Command::new(binary_path)
        .arg("--version")
        .output()
        .with_context(|| format!("failed to execute '{} --version'", binary_path.display()))?;
    if !output.status.success() {
        bail!(
            "'{} --version' exited with status {}",
            binary_path.display(),
            output.status
        );
    }
    let stdout = String::from_utf8(output.stdout).with_context(|| {
        format!(
            "'{} --version' produced non UTF-8 output",
            binary_path.display()
        )
    })?;
    let version_text = stdout.split_whitespace().last().ok_or_else(|| {
        anyhow!(
            "'{} --version' did not print a version",
            binary_path.display()
        )
    })?;
    Version::parse(version_text).with_context(|| {
        format!(
            "failed to parse version output from '{}'",
            binary_path.display()
        )
    })
}

fn command_exists(command: &str) -> bool {
    env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).any(|path| path.join(command).is_file()))
        .unwrap_or(false)
}

fn looks_like_cargo_install(path: &Path) -> bool {
    if let Some(cargo_home) = env::var_os("CARGO_HOME").map(PathBuf::from)
        && path == cargo_home.join("bin").join("meta")
    {
        return true;
    }

    let parent = match path.parent() {
        Some(parent) => parent,
        None => return false,
    };
    let grandparent = match parent.parent() {
        Some(parent) => parent,
        None => return false,
    };

    parent.file_name() == Some(OsStr::new("bin"))
        && grandparent.file_name() == Some(OsStr::new(".cargo"))
        && path.file_name() == Some(OsStr::new("meta"))
}

fn looks_like_source_checkout_build(path: &Path) -> bool {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();

    components
        .windows(2)
        .any(|window| window == ["target", "debug"] || window == ["target", "release"])
        || components
            .windows(3)
            .any(|window| window[0] == "target" && (window[2] == "debug" || window[2] == "release"))
}

fn unique_suffix() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{timestamp:x}")
}

fn set_executable_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).with_context(|| {
            format!(
                "failed to set executable permissions on '{}'",
                path.display()
            )
        })
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}
