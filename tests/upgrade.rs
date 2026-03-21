#![allow(dead_code, unused_imports)]

include!("support/common.rs");
#[cfg(unix)]
use sha2::{Digest, Sha256};

#[cfg(unix)]
fn write_meta_stub(path: &Path, version: &str) -> Result<(), Box<dyn Error>> {
    fs::write(
        path,
        format!("#!/bin/sh\nset -eu\nprintf 'meta {}\\n'\n", version),
    )?;
    make_executable(path)?;
    Ok(())
}

#[cfg(unix)]
fn write_custom_meta_stub(path: &Path, body: &str) -> Result<(), Box<dyn Error>> {
    fs::write(path, body)?;
    make_executable(path)?;
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), Box<dyn Error>> {
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
fn read_meta_version(path: &Path) -> Result<String, Box<dyn Error>> {
    let output = ProcessCommand::new(path).arg("--version").output()?;
    if !output.status.success() {
        return Err(format!("{} --version failed", path.display()).into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(unix)]
fn archive_meta_binary(version_output: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let temp = tempdir()?;
    let stage_dir = temp.path().join("stage");
    let archive_path = temp.path().join("meta.tar.gz");
    fs::create_dir_all(&stage_dir)?;
    write_custom_meta_stub(
        &stage_dir.join("meta"),
        &format!("#!/bin/sh\nset -eu\nprintf 'meta {}\\n'\n", version_output),
    )?;

    let status = ProcessCommand::new("tar")
        .current_dir(&stage_dir)
        .arg("-czf")
        .arg(&archive_path)
        .arg("meta")
        .status()?;
    assert!(
        status.success(),
        "expected tar to create {}",
        archive_path.display()
    );

    Ok(fs::read(archive_path)?)
}

#[cfg(unix)]
fn archive_custom_meta_binary(body: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let temp = tempdir()?;
    let stage_dir = temp.path().join("stage");
    let archive_path = temp.path().join("meta.tar.gz");
    fs::create_dir_all(&stage_dir)?;
    write_custom_meta_stub(&stage_dir.join("meta"), body)?;

    let status = ProcessCommand::new("tar")
        .current_dir(&stage_dir)
        .arg("-czf")
        .arg(&archive_path)
        .arg("meta")
        .status()?;
    assert!(
        status.success(),
        "expected tar to create {}",
        archive_path.display()
    );

    Ok(fs::read(archive_path)?)
}

#[cfg(unix)]
fn sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(unix)]
fn mock_release(
    server: &httpmock::MockServer,
    version: &str,
    archive_bytes: &[u8],
    checksum_manifest: &str,
    include_asset: bool,
    prerelease: bool,
) {
    let asset_name = format!("metastack-cli-{version}-linux-x64.tar.gz");
    let asset_url = server.url(format!("/downloads/{asset_name}"));
    let checksum_url = server.url("/downloads/SHA256SUMS");
    let assets = if include_asset {
        vec![
            serde_json::json!({
                "name": asset_name,
                "browser_download_url": asset_url,
            }),
            serde_json::json!({
                "name": "SHA256SUMS",
                "browser_download_url": checksum_url,
            }),
        ]
    } else {
        vec![serde_json::json!({
            "name": "SHA256SUMS",
            "browser_download_url": checksum_url,
        })]
    };

    let release_body = serde_json::json!({
        "tag_name": format!("v{version}"),
        "prerelease": prerelease,
        "draft": false,
        "assets": assets,
    });

    server.mock(|when, then| {
        when.method(GET)
            .path("/api/repos/metastack-systems/metastack-cli/releases/latest");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(release_body.clone());
    });

    server.mock(|when, then| {
        when.method(GET).path(format!(
            "/api/repos/metastack-systems/metastack-cli/releases/tags/v{version}"
        ));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(release_body.clone());
    });

    server.mock(|when, then| {
        when.method(GET)
            .path("/api/repos/metastack-systems/metastack-cli/releases");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(vec![release_body]);
    });

    server.mock(|when, then| {
        when.method(GET).path("/downloads/SHA256SUMS");
        then.status(200).body(checksum_manifest);
    });

    if include_asset {
        server.mock(|when, then| {
            when.method(GET).path(format!("/downloads/{asset_name}"));
            then.status(200).body(archive_bytes);
        });
    }
}

#[cfg(unix)]
fn upgrade_command(server: &httpmock::MockServer, executable_path: &Path) -> assert_cmd::Command {
    let mut command = cli();
    command.args([
        "upgrade",
        "--github-api-url",
        server.url("/api").as_str(),
        "--repository",
        "metastack-systems/metastack-cli",
        "--executable-path",
        executable_path.to_string_lossy().as_ref(),
        "--os",
        "linux",
        "--arch",
        "x64",
    ]);
    command
}

#[test]
fn upgrade_help_describes_default_and_advanced_paths() {
    cli()
        .args(["upgrade", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Default latest-stable path:"))
        .stdout(predicate::str::contains("meta upgrade --check"))
        .stdout(predicate::str::contains("meta upgrade --dry-run"))
        .stdout(predicate::str::contains(
            "meta upgrade --version 0.3.0-rc.1 --prerelease",
        ))
        .stdout(predicate::str::contains("--allow-downgrade"));
}

#[cfg(unix)]
#[test]
fn upgrade_check_reports_latest_stable_release() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let install_path = temp.path().join("bin").join("meta");
    fs::create_dir_all(install_path.parent().expect("bin dir"))?;
    write_meta_stub(&install_path, "0.1.0")?;

    let server = httpmock::MockServer::start();
    let archive = archive_meta_binary("0.2.0")?;
    let asset_name = "metastack-cli-0.2.0-linux-x64.tar.gz";
    mock_release(
        &server,
        "0.2.0",
        &archive,
        &format!("{}  {}\n", sha256(&archive), asset_name),
        true,
        false,
    );

    upgrade_command(&server, &install_path)
        .arg("--check")
        .assert()
        .success()
        .stdout(predicate::str::contains("current version: 0.1.0"))
        .stdout(predicate::str::contains("target version: 0.2.0"))
        .stdout(predicate::str::contains("status: update available"));

    assert_eq!(read_meta_version(&install_path)?, "meta 0.1.0");
    Ok(())
}

#[cfg(unix)]
#[test]
fn upgrade_dry_run_reports_the_planned_replacement_without_mutation() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
    let install_path = temp.path().join("bin").join("meta");
    fs::create_dir_all(install_path.parent().expect("bin dir"))?;
    write_meta_stub(&install_path, "0.1.0")?;

    let server = httpmock::MockServer::start();
    let archive = archive_meta_binary("0.2.0")?;
    let asset_name = "metastack-cli-0.2.0-linux-x64.tar.gz";
    mock_release(
        &server,
        "0.2.0",
        &archive,
        &format!("{}  {}\n", sha256(&archive), asset_name),
        true,
        false,
    );

    upgrade_command(&server, &install_path)
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "status: would replace the installed binary",
        ))
        .stdout(predicate::str::contains(
            "asset: metastack-cli-0.2.0-linux-x64.tar.gz",
        ));

    assert_eq!(read_meta_version(&install_path)?, "meta 0.1.0");
    Ok(())
}

#[cfg(unix)]
#[test]
fn upgrade_refuses_cargo_installs_with_remediation_text() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let install_path = temp.path().join("home/.cargo/bin/meta");
    fs::create_dir_all(install_path.parent().expect("cargo bin dir"))?;
    write_meta_stub(&install_path, "0.1.0")?;

    let server = httpmock::MockServer::start();
    let archive = archive_meta_binary("0.2.0")?;
    let asset_name = "metastack-cli-0.2.0-linux-x64.tar.gz";
    mock_release(
        &server,
        "0.2.0",
        &archive,
        &format!("{}  {}\n", sha256(&archive), asset_name),
        true,
        false,
    );

    upgrade_command(&server, &install_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("looks like a Cargo install"))
        .stderr(predicate::str::contains(
            "reinstall from GitHub Releases to enable `meta upgrade`",
        ));

    assert_eq!(read_meta_version(&install_path)?, "meta 0.1.0");
    Ok(())
}

#[cfg(unix)]
#[test]
fn upgrade_replaces_the_installed_binary_after_checksum_verification() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
    let install_path = temp.path().join("bin").join("meta");
    fs::create_dir_all(install_path.parent().expect("bin dir"))?;
    write_meta_stub(&install_path, "0.1.0")?;

    let server = httpmock::MockServer::start();
    let archive = archive_meta_binary("0.2.0")?;
    let asset_name = "metastack-cli-0.2.0-linux-x64.tar.gz";
    mock_release(
        &server,
        "0.2.0",
        &archive,
        &format!("{}  {}\n", sha256(&archive), asset_name),
        true,
        false,
    );

    upgrade_command(&server, &install_path)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "upgraded meta from 0.1.0 to 0.2.0",
        ));

    assert_eq!(read_meta_version(&install_path)?, "meta 0.2.0");
    Ok(())
}

#[cfg(unix)]
#[test]
fn upgrade_fails_when_the_release_checksum_does_not_match() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let install_path = temp.path().join("bin").join("meta");
    fs::create_dir_all(install_path.parent().expect("bin dir"))?;
    write_meta_stub(&install_path, "0.1.0")?;

    let server = httpmock::MockServer::start();
    let archive = archive_meta_binary("0.2.0")?;
    mock_release(
        &server,
        "0.2.0",
        &archive,
        &format!(
            "{}  {}\n",
            "deadbeef".repeat(8),
            "metastack-cli-0.2.0-linux-x64.tar.gz"
        ),
        true,
        false,
    );

    upgrade_command(&server, &install_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("checksum mismatch"));

    assert_eq!(read_meta_version(&install_path)?, "meta 0.1.0");
    Ok(())
}

#[cfg(unix)]
#[test]
fn upgrade_fails_when_the_release_is_missing_the_platform_asset() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let install_path = temp.path().join("bin").join("meta");
    fs::create_dir_all(install_path.parent().expect("bin dir"))?;
    write_meta_stub(&install_path, "0.1.0")?;

    let server = httpmock::MockServer::start();
    let archive = archive_meta_binary("0.2.0")?;
    mock_release(&server, "0.2.0", &archive, "", false, false);

    upgrade_command(&server, &install_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "is missing asset 'metastack-cli-0.2.0-linux-x64.tar.gz'",
        ));

    assert_eq!(read_meta_version(&install_path)?, "meta 0.1.0");
    Ok(())
}

#[cfg(unix)]
#[test]
fn upgrade_fails_when_the_checksum_manifest_is_missing_the_platform_entry()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let install_path = temp.path().join("bin").join("meta");
    fs::create_dir_all(install_path.parent().expect("bin dir"))?;
    write_meta_stub(&install_path, "0.1.0")?;

    let server = httpmock::MockServer::start();
    let archive = archive_meta_binary("0.2.0")?;
    mock_release(
        &server,
        "0.2.0",
        &archive,
        "deadbeef  some-other-asset.tar.gz\n",
        true,
        false,
    );

    upgrade_command(&server, &install_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "is missing a checksum entry for 'metastack-cli-0.2.0-linux-x64.tar.gz'",
        ));

    assert_eq!(read_meta_version(&install_path)?, "meta 0.1.0");
    Ok(())
}

#[cfg(unix)]
#[test]
fn upgrade_rolls_back_when_post_install_verification_fails() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let install_path = temp.path().join("bin").join("meta");
    fs::create_dir_all(install_path.parent().expect("bin dir"))?;
    write_meta_stub(&install_path, "0.1.0")?;

    let server = httpmock::MockServer::start();
    let archive = archive_custom_meta_binary(
        r#"#!/bin/sh
set -eu
case "$0" in
  *metastack-upgrade-*)
    printf 'meta 0.2.0\n'
    ;;
  *)
    printf 'meta 0.9.9\n'
    ;;
esac
"#,
    )?;
    let asset_name = "metastack-cli-0.2.0-linux-x64.tar.gz";
    mock_release(
        &server,
        "0.2.0",
        &archive,
        &format!("{}  {}\n", sha256(&archive), asset_name),
        true,
        false,
    );

    upgrade_command(&server, &install_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("post-install verification failed"));

    assert_eq!(read_meta_version(&install_path)?, "meta 0.1.0");
    Ok(())
}

#[cfg(unix)]
#[test]
fn upgrade_uses_a_supported_elevation_helper_for_non_writable_installs()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let install_dir = temp.path().join("protected-bin");
    let install_path = install_dir.join("meta");
    let helper_dir = temp.path().join("helper-bin");
    let helper_path = helper_dir.join("sudo");
    let marker_path = temp.path().join("sudo-used");
    fs::create_dir_all(&install_dir)?;
    fs::create_dir_all(&helper_dir)?;
    write_meta_stub(&install_path, "0.1.0")?;

    write_custom_meta_stub(
        &helper_path,
        r#"#!/bin/sh
set -eu
[ "${1:-}" = "-n" ] && shift
[ "${1:-}" = "sh" ] || {
  printf '%s\n' "unexpected helper invocation: $*" >&2
  exit 1
}
script=$2
shift 2
target=$2
chmod 755 "$(dirname "$target")"
: > "${META_TEST_SUDO_MARKER:?}"
exec sh "$script" "$@"
"#,
    )?;

    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&install_dir, fs::Permissions::from_mode(0o555))?;
    }

    let server = httpmock::MockServer::start();
    let archive = archive_meta_binary("0.2.0")?;
    let asset_name = "metastack-cli-0.2.0-linux-x64.tar.gz";
    mock_release(
        &server,
        "0.2.0",
        &archive,
        &format!("{}  {}\n", sha256(&archive), asset_name),
        true,
        false,
    );

    let original_path = std::env::var_os("PATH").unwrap_or_default();
    let prefixed_path = std::env::join_paths(
        std::iter::once(helper_dir.clone()).chain(std::env::split_paths(&original_path)),
    )?;

    upgrade_command(&server, &install_path)
        .env("PATH", &prefixed_path)
        .env("META_TEST_SUDO_MARKER", &marker_path)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "upgraded meta from 0.1.0 to 0.2.0",
        ));

    assert!(
        marker_path.is_file(),
        "expected helper marker to be created"
    );
    assert_eq!(read_meta_version(&install_path)?, "meta 0.2.0");
    Ok(())
}
