#![allow(dead_code, unused_imports)]

include!("support/common.rs");

fn onboarded_config_path() -> (tempfile::TempDir, PathBuf) {
    let config_dir = tempdir().unwrap();
    let config_path = config_dir.path().join("config.toml");
    fs::write(&config_path, "[onboarding]\ncompleted = true\n").unwrap();
    (config_dir, config_path)
}

/// Create a test CLI command with onboarding bypassed.
fn onboarded_cli(config_path: &Path) -> Command {
    let mut cmd = cli();
    cmd.env("METASTACK_CONFIG", config_path);
    cmd
}

/// Create a temporary git repo with `.metastack/` directory and an initial commit.
fn init_test_repo() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(tmp.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args(["config", "user.name", "MetaStack Tests"])
            .current_dir(tmp.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args(["config", "user.email", "tests@example.com"])
            .current_dir(tmp.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(tmp.path())
            .status()
            .unwrap()
            .success()
    );
    fs::create_dir_all(tmp.path().join(".metastack")).unwrap();
    tmp
}

// ---------------------------------------------------------------------------
// CLI surface tests
// ---------------------------------------------------------------------------

#[test]
fn orchestrate_help_shows_purpose_and_flags() {
    cli()
        .args(["agents", "orchestrate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Orchestrate backlog promotion"))
        .stdout(predicate::str::contains("--staging-branch"))
        .stdout(predicate::str::contains("--render-once"))
        .stdout(predicate::str::contains("--status"))
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--root"));
}

#[test]
fn agents_help_lists_orchestrate_subcommand() {
    cli()
        .args(["agents", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("orchestrate"));
}

#[test]
fn orchestrate_status_fails_without_metastack_dir() {
    let (_cfg_dir, config_path) = onboarded_config_path();
    let tmp = tempdir().unwrap();
    onboarded_cli(&config_path)
        .args(["agents", "orchestrate", "--status", "--root"])
        .arg(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("no .metastack/ directory found"));
}

#[test]
fn orchestrate_status_fails_without_active_session() {
    let (_cfg_dir, config_path) = onboarded_config_path();
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".metastack")).unwrap();
    onboarded_cli(&config_path)
        .args(["agents", "orchestrate", "--status", "--root"])
        .arg(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no active orchestrator session found",
        ));
}

#[test]
fn orchestrate_render_once_fails_without_active_session() {
    let (_cfg_dir, config_path) = onboarded_config_path();
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".metastack")).unwrap();
    onboarded_cli(&config_path)
        .args(["agents", "orchestrate", "--render-once", "--root"])
        .arg(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no active orchestrator session found",
        ));
}

// ---------------------------------------------------------------------------
// Daemon execution tests (single cycle)
// ---------------------------------------------------------------------------

#[test]
fn orchestrate_creates_session_state() {
    let (_cfg_dir, config_path) = onboarded_config_path();
    let tmp = init_test_repo();

    onboarded_cli(&config_path)
        .args(["agents", "orchestrate", "--root"])
        .arg(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Orchestrate Session:"))
        .stdout(predicate::str::contains("Completed"))
        .stdout(predicate::str::contains("Cycles:           1"))
        .stdout(predicate::str::contains("staging/orchestrate-"));

    // Verify state files exist.
    let orchestrate_dir = tmp.path().join(".metastack/orchestrate");
    assert!(
        orchestrate_dir.exists(),
        ".metastack/orchestrate/ should exist"
    );
    assert!(
        orchestrate_dir.join("current.json").exists(),
        "current.json should exist"
    );
    assert!(
        orchestrate_dir.join("sessions").exists(),
        "sessions/ should exist"
    );
}

#[test]
fn orchestrate_with_staging_branch_override() {
    let (_cfg_dir, config_path) = onboarded_config_path();
    let tmp = init_test_repo();

    onboarded_cli(&config_path)
        .args(["agents", "orchestrate", "--root"])
        .arg(tmp.path())
        .args(["--staging-branch", "staging/custom-branch"])
        .assert()
        .success()
        .stdout(predicate::str::contains("staging/custom-branch"));
}

#[test]
fn orchestrate_rejects_invalid_staging_branch_name() {
    let (_cfg_dir, config_path) = onboarded_config_path();
    let tmp = init_test_repo();

    onboarded_cli(&config_path)
        .args(["agents", "orchestrate", "--root"])
        .arg(tmp.path())
        .args(["--staging-branch", "bad..name"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must not contain '..'"));
}

#[test]
fn orchestrate_json_output_is_valid_json() {
    let (_cfg_dir, config_path) = onboarded_config_path();
    let tmp = init_test_repo();

    let output = onboarded_cli(&config_path)
        .args(["agents", "orchestrate", "--json", "--root"])
        .arg(tmp.path())
        .assert()
        .success();

    let stdout = output.get_output().stdout.clone();
    let text = String::from_utf8(stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["phase"], "completed");
    assert!(parsed["session_id"].is_string());
    assert!(
        parsed["staging_branch"]
            .as_str()
            .unwrap()
            .starts_with("staging/orchestrate-")
    );
}

#[test]
fn orchestrate_status_reads_previously_created_session() {
    let (_cfg_dir, config_path) = onboarded_config_path();
    let tmp = init_test_repo();

    // First run creates a session.
    onboarded_cli(&config_path)
        .args(["agents", "orchestrate", "--root"])
        .arg(tmp.path())
        .assert()
        .success();

    // Second run with --status reads the session.
    onboarded_cli(&config_path)
        .args(["agents", "orchestrate", "--status", "--root"])
        .arg(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Orchestrate Session:"))
        .stdout(predicate::str::contains("Completed"));
}

#[test]
fn orchestrate_status_json_reads_previously_created_session() {
    let (_cfg_dir, config_path) = onboarded_config_path();
    let tmp = init_test_repo();

    onboarded_cli(&config_path)
        .args(["agents", "orchestrate", "--root"])
        .arg(tmp.path())
        .assert()
        .success();

    let output = onboarded_cli(&config_path)
        .args(["agents", "orchestrate", "--status", "--json", "--root"])
        .arg(tmp.path())
        .assert()
        .success();

    let stdout = output.get_output().stdout.clone();
    let text = String::from_utf8(stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["phase"], "completed");
}

// ---------------------------------------------------------------------------
// Corrupted state tests
// ---------------------------------------------------------------------------

#[test]
fn orchestrate_status_detects_corrupted_current_pointer() {
    let (_cfg_dir, config_path) = onboarded_config_path();
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".metastack/orchestrate")).unwrap();
    fs::write(
        tmp.path().join(".metastack/orchestrate/current.json"),
        "not valid json",
    )
    .unwrap();

    onboarded_cli(&config_path)
        .args(["agents", "orchestrate", "--status", "--root"])
        .arg(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "corrupted current session pointer",
        ));
}

#[test]
fn orchestrate_status_detects_corrupted_session() {
    let (_cfg_dir, config_path) = onboarded_config_path();
    let tmp = tempdir().unwrap();
    let orch = tmp.path().join(".metastack/orchestrate");
    fs::create_dir_all(orch.join("sessions/bad-session")).unwrap();
    fs::write(
        orch.join("current.json"),
        r#"{"session_id":"bad-session","started_at":"2026-03-21T00:00:00Z"}"#,
    )
    .unwrap();
    fs::write(orch.join("sessions/bad-session/session.json"), "broken").unwrap();

    onboarded_cli(&config_path)
        .args(["agents", "orchestrate", "--status", "--root"])
        .arg(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "failed to load orchestrator session",
        ));
}

// ---------------------------------------------------------------------------
// Event log tests
// ---------------------------------------------------------------------------

#[test]
fn orchestrate_writes_events_log() {
    let (_cfg_dir, config_path) = onboarded_config_path();
    let tmp = init_test_repo();

    onboarded_cli(&config_path)
        .args(["agents", "orchestrate", "--root"])
        .arg(tmp.path())
        .assert()
        .success();

    // Find the session directory.
    let sessions_dir = tmp.path().join(".metastack/orchestrate/sessions");
    let sessions: Vec<_> = fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(sessions.len(), 1);

    let events_path = sessions[0].path().join("events.log");
    assert!(events_path.exists());
    let content = fs::read_to_string(&events_path).unwrap();
    assert!(content.contains("session_start"));
    assert!(content.contains("session_complete"));
}

// ---------------------------------------------------------------------------
// Machine output command inference
// ---------------------------------------------------------------------------

#[test]
fn orchestrate_json_parse_failure_returns_machine_error() {
    let (_cfg_dir, config_path) = onboarded_config_path();
    let tmp = tempdir().unwrap();
    // No .metastack/ dir so orchestrate will fail.
    onboarded_cli(&config_path)
        .args(["agents", "orchestrate", "--json", "--root"])
        .arg(tmp.path())
        .assert()
        .failure()
        .stdout(predicate::str::contains("\"status\": \"error\""))
        .stdout(predicate::str::contains(
            "\"command\": \"agents.orchestrate\"",
        ));
}
