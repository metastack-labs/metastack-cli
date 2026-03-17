use std::error::Error;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn meta() -> Command {
    Command::cargo_bin("meta").expect("meta binary should build for tests")
}

#[test]
fn version_flag_reports_package_version() {
    meta()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::diff(format!(
            "meta {}\n",
            env!("CARGO_PKG_VERSION")
        )));
}

#[cfg(unix)]
fn write_build_stub(path: &Path) -> Result<(), Box<dyn Error>> {
    fs::write(
        path,
        r#"#!/bin/sh
set -eu

[ "${1:-}" = "build" ] || {
  printf '%s\n' "unexpected invocation: $*" >&2
  exit 1
}

target=
target_dir=

while [ "$#" -gt 0 ]; do
  case "$1" in
    --target)
      shift
      target=$1
      ;;
    --target-dir)
      shift
      target_dir=$1
      ;;
  esac
  shift
done

[ -n "$target" ] || {
  printf '%s\n' "missing --target" >&2
  exit 1
}

[ -n "$target_dir" ] || {
  printf '%s\n' "missing --target-dir" >&2
  exit 1
}

output_dir=$target_dir/$target/release
mkdir -p "$output_dir"
cat > "$output_dir/meta" <<EOF
#!/bin/sh
printf 'meta %s\n' "${META_RELEASE_STUB_VERSION:?}"
EOF
chmod +x "$output_dir/meta"
"#,
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
#[test]
fn release_script_packages_supported_targets_and_writes_checksums() -> Result<(), Box<dyn Error>> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = tempdir()?;
    let stub_dir = temp.path().join("stub-bin");
    let target_dir = temp.path().join("target");
    let output_root = temp.path().join("artifacts");

    fs::create_dir_all(&stub_dir)?;

    let cargo_stub = stub_dir.join("cargo");
    let cross_stub = stub_dir.join("cross");
    write_build_stub(&cargo_stub)?;
    write_build_stub(&cross_stub)?;

    let output = ProcessCommand::new(repo_root.join("scripts/release-artifacts.sh"))
        .current_dir(&repo_root)
        .arg("--output-dir")
        .arg(&output_root)
        .env("META_RELEASE_TARGET_DIR", &target_dir)
        .env("META_RELEASE_CARGO", &cargo_stub)
        .env("META_RELEASE_CROSS", &cross_stub)
        .env("META_RELEASE_STUB_VERSION", env!("CARGO_PKG_VERSION"))
        .env("META_RELEASE_VERIFY_ALL_TARGETS", "1")
        .output()?;

    assert!(
        output.status.success(),
        "expected release script to succeed, stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let version_dir = output_root.join(env!("CARGO_PKG_VERSION"));
    let darwin_archive = version_dir.join(format!(
        "metastack-cli-{}-darwin-arm64.tar.gz",
        env!("CARGO_PKG_VERSION")
    ));
    let darwin_x64_archive = version_dir.join(format!(
        "metastack-cli-{}-darwin-x64.tar.gz",
        env!("CARGO_PKG_VERSION")
    ));
    let linux_x64_archive = version_dir.join(format!(
        "metastack-cli-{}-linux-x64.tar.gz",
        env!("CARGO_PKG_VERSION")
    ));
    let linux_archive = version_dir.join(format!(
        "metastack-cli-{}-linux-arm64.tar.gz",
        env!("CARGO_PKG_VERSION")
    ));
    let checksum_manifest = version_dir.join("SHA256SUMS");

    for artifact in [
        &darwin_archive,
        &darwin_x64_archive,
        &linux_x64_archive,
        &linux_archive,
        &checksum_manifest,
    ] {
        assert!(
            artifact.exists(),
            "expected artifact at {}",
            artifact.display()
        );
    }

    let manifest = fs::read_to_string(&checksum_manifest)?;
    assert!(manifest.contains(&format!(
        "metastack-cli-{}-darwin-arm64.tar.gz",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(manifest.contains(&format!(
        "metastack-cli-{}-darwin-x64.tar.gz",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(manifest.contains(&format!(
        "metastack-cli-{}-linux-x64.tar.gz",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(manifest.contains(&format!(
        "metastack-cli-{}-linux-arm64.tar.gz",
        env!("CARGO_PKG_VERSION")
    )));

    let extract_dir = temp.path().join("extract");
    fs::create_dir_all(&extract_dir)?;
    let status = ProcessCommand::new("tar")
        .current_dir(&extract_dir)
        .arg("-xzf")
        .arg(&darwin_archive)
        .status()?;
    assert!(status.success(), "expected tar extract to succeed");

    let version_output = ProcessCommand::new(extract_dir.join("meta"))
        .arg("--version")
        .output()?;
    assert!(version_output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&version_output.stdout),
        format!("meta {}\n", env!("CARGO_PKG_VERSION"))
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn release_script_honors_explicit_target_subset() -> Result<(), Box<dyn Error>> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = tempdir()?;
    let stub_dir = temp.path().join("stub-bin");
    let target_dir = temp.path().join("target");
    let output_root = temp.path().join("artifacts");

    fs::create_dir_all(&stub_dir)?;

    let cargo_stub = stub_dir.join("cargo");
    let cross_stub = stub_dir.join("cross");
    write_build_stub(&cargo_stub)?;
    write_build_stub(&cross_stub)?;

    let output = ProcessCommand::new(repo_root.join("scripts/release-artifacts.sh"))
        .current_dir(&repo_root)
        .arg("--output-dir")
        .arg(&output_root)
        .arg("--target")
        .arg("x86_64-unknown-linux-musl")
        .env("META_RELEASE_TARGET_DIR", &target_dir)
        .env("META_RELEASE_CARGO", &cargo_stub)
        .env("META_RELEASE_CROSS", &cross_stub)
        .env("META_RELEASE_STUB_VERSION", env!("CARGO_PKG_VERSION"))
        .env("META_RELEASE_VERIFY_ALL_TARGETS", "1")
        .output()?;

    assert!(
        output.status.success(),
        "expected release script to succeed, stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let version_dir = output_root.join(env!("CARGO_PKG_VERSION"));
    assert!(
        version_dir
            .join(format!(
                "metastack-cli-{}-linux-x64.tar.gz",
                env!("CARGO_PKG_VERSION")
            ))
            .exists()
    );
    assert!(
        !version_dir
            .join(format!(
                "metastack-cli-{}-darwin-arm64.tar.gz",
                env!("CARGO_PKG_VERSION")
            ))
            .exists()
    );

    let manifest = fs::read_to_string(version_dir.join("SHA256SUMS"))?;
    assert!(manifest.contains(&format!(
        "metastack-cli-{}-linux-x64.tar.gz",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(!manifest.contains("darwin-arm64"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn release_script_fails_fast_when_cross_is_missing() -> Result<(), Box<dyn Error>> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = tempdir()?;
    let stub_dir = temp.path().join("stub-bin");
    let target_dir = temp.path().join("target");
    let output_root = temp.path().join("artifacts");

    fs::create_dir_all(&stub_dir)?;

    let cargo_stub = stub_dir.join("cargo");
    write_build_stub(&cargo_stub)?;

    let output = ProcessCommand::new(repo_root.join("scripts/release-artifacts.sh"))
        .current_dir(&repo_root)
        .arg("--output-dir")
        .arg(&output_root)
        .env("META_RELEASE_TARGET_DIR", &target_dir)
        .env("META_RELEASE_CARGO", &cargo_stub)
        .env("META_RELEASE_CROSS", temp.path().join("missing-cross"))
        .env("META_RELEASE_STUB_VERSION", env!("CARGO_PKG_VERSION"))
        .output()?;

    assert!(!output.status.success(), "expected missing cross to fail");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("missing required command"),
        "unexpected stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn release_script_rejects_version_mismatch() -> Result<(), Box<dyn Error>> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = ProcessCommand::new(repo_root.join("scripts/release-artifacts.sh"))
        .current_dir(&repo_root)
        .arg("--expect-version")
        .arg("999.0.0")
        .output()?;

    assert!(
        !output.status.success(),
        "expected version mismatch to fail"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("expected Cargo.toml version '999.0.0'"),
        "unexpected stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(())
}
