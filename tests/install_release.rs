use std::error::Error;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command as ProcessCommand;

#[cfg(unix)]
use tempfile::tempdir;

#[cfg(unix)]
fn write_meta_stub(path: &Path, version: &str) -> Result<(), Box<dyn Error>> {
    fs::write(
        path,
        format!("#!/bin/sh\nset -eu\nprintf 'meta {}\\n'\n", version),
    )?;

    let metadata = fs::metadata(path)?;
    let mut permissions = metadata.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    {
        use std::os::unix::fs::PermissionsExt;

        permissions.set_mode(0o755);
    }
    fs::set_permissions(path, permissions)?;

    Ok(())
}

#[cfg(unix)]
fn sha256(path: &Path) -> Result<String, Box<dyn Error>> {
    let output = if ProcessCommand::new("shasum")
        .arg("-a")
        .arg("256")
        .arg(path)
        .output()
        .is_ok()
    {
        ProcessCommand::new("shasum")
            .arg("-a")
            .arg("256")
            .arg(path)
            .output()?
    } else {
        ProcessCommand::new("sha256sum").arg(path).output()?
    };

    if !output.status.success() {
        return Err(format!(
            "failed to compute sha256 for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string())
}

#[cfg(unix)]
fn create_mock_release(
    base: &Path,
    version: &str,
    os: &str,
    arch: &str,
    corrupt_checksum: bool,
) -> Result<(), Box<dyn Error>> {
    let release_dir = base
        .join("metastack-systems")
        .join("metastack-cli")
        .join("releases")
        .join("download")
        .join(format!("v{version}"));
    let stage_dir = base.join("stage");
    let meta_path = stage_dir.join("meta");
    let asset_name = format!("metastack-cli-{version}-{os}-{arch}.tar.gz");
    let asset_path = release_dir.join(&asset_name);

    fs::create_dir_all(&release_dir)?;
    fs::create_dir_all(&stage_dir)?;
    write_meta_stub(&meta_path, version)?;

    let status = ProcessCommand::new("tar")
        .current_dir(&stage_dir)
        .arg("-czf")
        .arg(&asset_path)
        .arg("meta")
        .status()?;
    if !status.success() {
        return Err(format!("failed to archive {}", asset_path.display()).into());
    }

    let checksum = if corrupt_checksum {
        "deadbeef".repeat(8)
    } else {
        sha256(&asset_path)?
    };
    fs::write(
        release_dir.join("SHA256SUMS"),
        format!("{checksum}  {asset_name}\n"),
    )?;

    Ok(())
}

#[cfg(unix)]
fn installer_command(
    script: &Path,
    home: &Path,
    base: &Path,
    os: &str,
    arch: &str,
) -> ProcessCommand {
    let mut command = ProcessCommand::new("sh");
    command
        .arg(script)
        .env("HOME", home)
        .env(
            "META_INSTALL_BASE_URL",
            format!("file://{}", base.display()),
        )
        .env("META_INSTALL_OS", os)
        .env("META_INSTALL_ARCH", arch);
    command
}

#[cfg(unix)]
#[test]
fn installer_installs_latest_release_into_default_bin_dir() -> Result<(), Box<dyn Error>> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = tempdir()?;
    let home = temp.path().join("home");
    let releases = temp.path().join("mock-github");

    fs::create_dir_all(&home)?;
    create_mock_release(&releases, "0.1.0", "linux", "x64", false)?;

    let output = installer_command(
        &repo_root.join("scripts/install-meta.sh"),
        &home,
        &releases,
        "linux",
        "x64",
    )
    .env("META_INSTALL_LATEST_VERSION", "0.1.0")
    .output()?;

    assert!(
        output.status.success(),
        "expected installer to succeed, stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let installed = home.join(".local/bin/meta");
    assert!(installed.exists(), "expected {}", installed.display());

    let version = ProcessCommand::new(&installed).arg("--version").output()?;
    assert!(version.status.success());
    assert_eq!(String::from_utf8_lossy(&version.stdout), "meta 0.1.0\n");

    Ok(())
}

#[cfg(unix)]
#[test]
fn installer_accepts_pinned_versions_and_custom_bin_dir() -> Result<(), Box<dyn Error>> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = tempdir()?;
    let home = temp.path().join("home");
    let releases = temp.path().join("mock-github");
    let custom_bin = temp.path().join("bin");

    fs::create_dir_all(&home)?;
    create_mock_release(&releases, "0.1.0", "darwin", "arm64", false)?;

    let output = installer_command(
        &repo_root.join("scripts/install-meta.sh"),
        &home,
        &releases,
        "darwin",
        "arm64",
    )
    .arg("--version")
    .arg("0.1.0")
    .arg("--bin-dir")
    .arg(&custom_bin)
    .output()?;

    assert!(
        output.status.success(),
        "expected installer to succeed, stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let installed = custom_bin.join("meta");
    assert!(installed.exists(), "expected {}", installed.display());

    let version = ProcessCommand::new(&installed).arg("--version").output()?;
    assert!(version.status.success());
    assert_eq!(String::from_utf8_lossy(&version.stdout), "meta 0.1.0\n");

    Ok(())
}

#[cfg(unix)]
#[test]
fn installer_fails_on_checksum_mismatch() -> Result<(), Box<dyn Error>> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = tempdir()?;
    let home = temp.path().join("home");
    let releases = temp.path().join("mock-github");

    fs::create_dir_all(&home)?;
    create_mock_release(&releases, "0.1.0", "linux", "arm64", true)?;

    let output = installer_command(
        &repo_root.join("scripts/install-meta.sh"),
        &home,
        &releases,
        "linux",
        "arm64",
    )
    .arg("--version")
    .arg("v0.1.0")
    .output()?;

    assert!(
        !output.status.success(),
        "expected checksum mismatch to fail"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("checksum mismatch"),
        "unexpected stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(())
}
