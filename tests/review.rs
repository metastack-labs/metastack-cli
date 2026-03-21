#![allow(dead_code, unused_imports)]

include!("support/common.rs");

// ---------------------------------------------------------------------------
// Help text surface
// ---------------------------------------------------------------------------

#[test]
fn agents_help_lists_review_subcommand() {
    cli()
        .args(["agents", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("review"));
}

#[test]
fn review_help_shows_options_and_pr_argument() {
    cli()
        .args(["agents", "review", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PR_NUMBER"))
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("--check"))
        .stdout(predicate::str::contains("--once"))
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--render-once"))
        .stdout(predicate::str::contains("--agent"))
        .stdout(predicate::str::contains("--model"))
        .stdout(predicate::str::contains("--reasoning"))
        .stdout(predicate::str::contains("--root"));
}

#[test]
fn review_help_shows_both_modes() {
    cli()
        .args(["agents", "review", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("one-shot"))
        .stdout(predicate::str::contains("listener"));
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

#[test]
fn review_rejects_invalid_pr_number() {
    cli()
        .args(["agents", "review", "not-a-number", "--root", "."])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
fn review_dry_run_conflicts_with_once() {
    cli()
        .args([
            "agents",
            "review",
            "42",
            "--dry-run",
            "--once",
            "--root",
            ".",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn review_dry_run_conflicts_with_check() {
    cli()
        .args([
            "agents",
            "review",
            "42",
            "--dry-run",
            "--check",
            "--root",
            ".",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn review_check_conflicts_with_once() {
    cli()
        .args(["agents", "review", "--check", "--once", "--root", "."])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

// ---------------------------------------------------------------------------
// One-shot review: gh auth failure
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn review_one_shot_fails_on_missing_gh_auth() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("meta.toml");
    fs::write(&config_path, "[onboarding]\ncompleted = true\n")?;

    let meta_dir = temp.path().join("repo").join(".metastack");
    fs::create_dir_all(&meta_dir)?;
    fs::write(
        meta_dir.join("meta.json"),
        r#"{"version":1,"project":{"name":"test"}}"#,
    )?;

    let repo = temp.path().join("repo");
    init_repo_with_origin(&repo)?;

    // Create a stub gh that fails auth status
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir)?;
    let gh_stub = bin_dir.join("gh");
    fs::write(&gh_stub, "#!/bin/sh\necho 'not logged in' >&2\nexit 1\n")?;
    fs::set_permissions(&gh_stub, fs::Permissions::from_mode(0o755))?;

    meta()
        .args(["agents", "review", "42", "--root", repo.to_str().unwrap()])
        .env("METASTACK_CONFIG", config_path.to_str().unwrap())
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .assert()
        .failure()
        .stderr(predicate::str::contains("gh auth status"));

    Ok(())
}

// ---------------------------------------------------------------------------
// Listener mode: check path
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn review_check_reports_gh_auth_ok_and_origin() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo = temp.path().join("repo");

    let config_path = temp.path().join("meta.toml");
    fs::write(
        &config_path,
        "[onboarding]\ncompleted = true\n\n[agents]\ndefault_agent = \"codex\"\ndefault_model = \"gpt-5.4\"\n",
    )?;

    let meta_dir = repo.join(".metastack");
    fs::create_dir_all(&meta_dir)?;
    fs::write(
        meta_dir.join("meta.json"),
        r#"{"version":1,"project":{"name":"test"}}"#,
    )?;

    init_repo_with_origin(&repo)?;

    // Create stubs that pass
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir)?;
    let gh_stub = bin_dir.join("gh");
    fs::write(
        &gh_stub,
        "#!/bin/sh\nif [ \"$1\" = \"auth\" ]; then echo 'Logged in'; exit 0; fi\nexit 1\n",
    )?;
    fs::set_permissions(&gh_stub, fs::Permissions::from_mode(0o755))?;

    let codex_stub = bin_dir.join("codex");
    fs::write(&codex_stub, "#!/bin/sh\nexit 0\n")?;
    fs::set_permissions(&codex_stub, fs::Permissions::from_mode(0o755))?;

    meta()
        .args([
            "agents",
            "review",
            "--check",
            "--root",
            repo.to_str().unwrap(),
        ])
        .env("METASTACK_CONFIG", config_path.to_str().unwrap())
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .assert()
        .success()
        .stdout(predicate::str::contains("gh auth: ok"))
        .stdout(predicate::str::contains("origin:"));

    Ok(())
}

// ---------------------------------------------------------------------------
// Route config
// ---------------------------------------------------------------------------

#[test]
fn review_route_key_appears_in_agents_examples() {
    cli()
        .args(["agents", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("meta agents review"));
}

// ---------------------------------------------------------------------------
// Dry-run output
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn review_dry_run_shows_pr_metadata_and_diagnostics() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo = temp.path().join("repo");
    let config_path = temp.path().join("meta.toml");
    fs::write(
        &config_path,
        "[onboarding]\ncompleted = true\n\n[agents]\ndefault_agent = \"codex\"\ndefault_model = \"gpt-5.4\"\n",
    )?;

    let meta_dir = repo.join(".metastack");
    fs::create_dir_all(&meta_dir)?;
    fs::write(
        meta_dir.join("meta.json"),
        r#"{"version":1,"project":{"name":"test"}}"#,
    )?;
    init_repo_with_origin(&repo)?;

    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir)?;
    let gh_stub = bin_dir.join("gh");
    // gh stub: auth succeeds, pr view returns JSON
    fs::write(
        &gh_stub,
        r#"#!/bin/sh
if [ "$1" = "auth" ]; then echo 'Logged in'; exit 0; fi
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  cat <<'JSON'
{"number":42,"title":"MET-99: Test PR","url":"https://github.com/test/repo/pull/42","body":"MET-99 body","author":{"login":"dev"},"headRefName":"met-99-feature","baseRefName":"main","changedFiles":3,"additions":50,"deletions":10,"state":"OPEN","labels":[{"name":"metastack"}],"reviewDecision":"REVIEW_REQUIRED"}
JSON
  exit 0
fi
exit 1
"#,
    )?;
    fs::set_permissions(&gh_stub, fs::Permissions::from_mode(0o755))?;

    let codex_stub = bin_dir.join("codex");
    fs::write(&codex_stub, "#!/bin/sh\nexit 0\n")?;
    fs::set_permissions(&codex_stub, fs::Permissions::from_mode(0o755))?;

    meta()
        .args([
            "agents",
            "review",
            "42",
            "--dry-run",
            "--root",
            repo.to_str().unwrap(),
        ])
        .env("METASTACK_CONFIG", config_path.to_str().unwrap())
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .assert()
        .success()
        .stdout(predicate::str::contains("dry-run"))
        .stdout(predicate::str::contains("PR: #42"))
        .stdout(predicate::str::contains("MET-99"))
        .stdout(predicate::str::contains("No mutations"));

    Ok(())
}

// ---------------------------------------------------------------------------
// Listener: --once with no eligible PRs (no-remediation path)
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn review_once_reports_zero_eligible_prs() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo = temp.path().join("repo");
    let config_path = temp.path().join("meta.toml");
    fs::write(
        &config_path,
        "[onboarding]\ncompleted = true\n\n[agents]\ndefault_agent = \"codex\"\ndefault_model = \"gpt-5.4\"\n",
    )?;

    let meta_dir = repo.join(".metastack");
    fs::create_dir_all(&meta_dir)?;
    fs::write(
        meta_dir.join("meta.json"),
        r#"{"version":1,"project":{"name":"test"}}"#,
    )?;
    init_repo_with_origin(&repo)?;

    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir)?;
    let gh_stub = bin_dir.join("gh");
    fs::write(
        &gh_stub,
        r#"#!/bin/sh
if [ "$1" = "auth" ]; then echo 'Logged in'; exit 0; fi
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then echo '[]'; exit 0; fi
exit 1
"#,
    )?;
    fs::set_permissions(&gh_stub, fs::Permissions::from_mode(0o755))?;

    meta()
        .args([
            "agents",
            "review",
            "--once",
            "--root",
            repo.to_str().unwrap(),
        ])
        .env("METASTACK_CONFIG", config_path.to_str().unwrap())
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .assert()
        .success()
        .stdout(predicate::str::contains("Eligible PRs: 0"))
        .stdout(predicate::str::contains("0 sessions"));

    Ok(())
}

// ---------------------------------------------------------------------------
// Listener: --once --json emits valid JSON
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn review_once_json_emits_valid_json() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo = temp.path().join("repo");
    let config_path = temp.path().join("meta.toml");
    fs::write(
        &config_path,
        "[onboarding]\ncompleted = true\n\n[agents]\ndefault_agent = \"codex\"\ndefault_model = \"gpt-5.4\"\n",
    )?;

    let meta_dir = repo.join(".metastack");
    fs::create_dir_all(&meta_dir)?;
    fs::write(
        meta_dir.join("meta.json"),
        r#"{"version":1,"project":{"name":"test"}}"#,
    )?;
    init_repo_with_origin(&repo)?;

    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir)?;
    let gh_stub = bin_dir.join("gh");
    fs::write(
        &gh_stub,
        r#"#!/bin/sh
if [ "$1" = "auth" ]; then echo 'Logged in'; exit 0; fi
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then echo '[]'; exit 0; fi
exit 1
"#,
    )?;
    fs::set_permissions(&gh_stub, fs::Permissions::from_mode(0o755))?;

    let output = meta()
        .args([
            "agents",
            "review",
            "--once",
            "--json",
            "--root",
            repo.to_str().unwrap(),
        ])
        .env("METASTACK_CONFIG", config_path.to_str().unwrap())
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: serde_json::Value = serde_json::from_slice(&output)?;
    assert_eq!(payload["eligible_prs"], 0);
    assert!(payload["scope"].is_string());
    assert!(payload["sessions"].is_array());
    assert!(payload["state_file"].is_string());

    Ok(())
}

// ---------------------------------------------------------------------------
// Dashboard snapshot rendering
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn review_render_once_produces_dashboard_snapshot() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo = temp.path().join("repo");
    let config_path = temp.path().join("meta.toml");
    fs::write(
        &config_path,
        "[onboarding]\ncompleted = true\n\n[agents]\ndefault_agent = \"codex\"\ndefault_model = \"gpt-5.4\"\n",
    )?;

    let meta_dir = repo.join(".metastack");
    fs::create_dir_all(&meta_dir)?;
    fs::write(
        meta_dir.join("meta.json"),
        r#"{"version":1,"project":{"name":"test"}}"#,
    )?;
    init_repo_with_origin(&repo)?;

    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir)?;
    let gh_stub = bin_dir.join("gh");
    fs::write(
        &gh_stub,
        r#"#!/bin/sh
if [ "$1" = "auth" ]; then echo 'Logged in'; exit 0; fi
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then echo '[]'; exit 0; fi
exit 1
"#,
    )?;
    fs::set_permissions(&gh_stub, fs::Permissions::from_mode(0o755))?;

    meta()
        .args([
            "agents",
            "review",
            "--render-once",
            "--root",
            repo.to_str().unwrap(),
        ])
        .env("METASTACK_CONFIG", config_path.to_str().unwrap())
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .assert()
        .success()
        .stdout(predicate::str::contains("Review Dashboard"))
        .stdout(predicate::str::contains("metastack"));

    Ok(())
}

// ---------------------------------------------------------------------------
// Dashboard snapshot with tab navigation
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn review_render_once_tab_switches_view() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo = temp.path().join("repo");
    let config_path = temp.path().join("meta.toml");
    fs::write(
        &config_path,
        "[onboarding]\ncompleted = true\n\n[agents]\ndefault_agent = \"codex\"\ndefault_model = \"gpt-5.4\"\n",
    )?;

    let meta_dir = repo.join(".metastack");
    fs::create_dir_all(&meta_dir)?;
    fs::write(
        meta_dir.join("meta.json"),
        r#"{"version":1,"project":{"name":"test"}}"#,
    )?;
    init_repo_with_origin(&repo)?;

    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir)?;
    let gh_stub = bin_dir.join("gh");
    fs::write(
        &gh_stub,
        r#"#!/bin/sh
if [ "$1" = "auth" ]; then echo 'Logged in'; exit 0; fi
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then echo '[]'; exit 0; fi
exit 1
"#,
    )?;
    fs::set_permissions(&gh_stub, fs::Permissions::from_mode(0o755))?;

    // Use --events to switch to Completed tab
    meta()
        .args([
            "agents",
            "review",
            "--render-once",
            "--events",
            "tab",
            "--root",
            repo.to_str().unwrap(),
        ])
        .env("METASTACK_CONFIG", config_path.to_str().unwrap())
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .assert()
        .success()
        .stdout(predicate::str::contains("Completed"));

    Ok(())
}

// ---------------------------------------------------------------------------
// Listener: --once discovers eligible PRs and attempts review
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn review_once_discovers_eligible_prs_and_reports_sessions() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo = temp.path().join("repo");
    let config_path = temp.path().join("meta.toml");
    fs::write(
        &config_path,
        "[onboarding]\ncompleted = true\n\n[agents]\ndefault_agent = \"codex\"\ndefault_model = \"gpt-5.4\"\n",
    )?;

    let meta_dir = repo.join(".metastack");
    fs::create_dir_all(&meta_dir)?;
    fs::write(
        meta_dir.join("meta.json"),
        r#"{"version":1,"project":{"name":"test"}}"#,
    )?;
    init_repo_with_origin(&repo)?;

    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir)?;
    let gh_stub = bin_dir.join("gh");
    // gh stub: auth passes, pr list returns one eligible PR, pr view returns metadata,
    // pr diff returns a diff
    fs::write(
        &gh_stub,
        r#"#!/bin/sh
if [ "$1" = "auth" ]; then echo 'Logged in'; exit 0; fi
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
  echo '[{"number":99,"title":"MET-55: Fix widgets","url":"https://github.com/test/repo/pull/99","labels":[{"name":"metastack"}]}]'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  cat <<'JSON'
{"number":99,"title":"MET-55: Fix widgets","url":"https://github.com/test/repo/pull/99","body":"MET-55 body","author":{"login":"dev"},"headRefName":"met-55-fix-widgets","baseRefName":"main","changedFiles":2,"additions":20,"deletions":5,"state":"OPEN","labels":[{"name":"metastack"}],"reviewDecision":"REVIEW_REQUIRED"}
JSON
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "diff" ]; then
  echo '--- a/src/lib.rs'
  echo '+++ b/src/lib.rs'
  echo '@@ -1,3 +1,5 @@'
  echo '+// new line'
  echo ' fn main() {}'
  exit 0
fi
exit 1
"#,
    )?;
    fs::set_permissions(&gh_stub, fs::Permissions::from_mode(0o755))?;

    // Agent stub that returns a no-remediation review
    let codex_stub = bin_dir.join("codex");
    fs::write(
        &codex_stub,
        r#"#!/bin/sh
echo '## PR Review: #99 — Fix widgets'
echo ''
echo '### Summary'
echo 'Fixes widget rendering.'
echo ''
echo '### Required Fixes'
echo 'No required fixes identified.'
echo ''
echo '### Remediation Required'
echo 'NO'
exit 0
"#,
    )?;
    fs::set_permissions(&codex_stub, fs::Permissions::from_mode(0o755))?;

    let output = meta()
        .args([
            "agents",
            "review",
            "--once",
            "--json",
            "--root",
            repo.to_str().unwrap(),
        ])
        .env("METASTACK_CONFIG", config_path.to_str().unwrap())
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: serde_json::Value = serde_json::from_slice(&output)?;
    assert_eq!(payload["eligible_prs"], 1);
    let sessions = payload["sessions"]
        .as_array()
        .expect("sessions should be an array");
    assert!(!sessions.is_empty(), "should have at least one session");
    let session = &sessions[0];
    assert_eq!(session["pr_number"], 99);

    Ok(())
}

// ---------------------------------------------------------------------------
// Unit-level: remediation output with required fixes triggers remediation
// ---------------------------------------------------------------------------

#[test]
fn review_remediation_required_detection_with_full_output() {
    // Tests the structured output contract parsing at the integration level
    let review_output = r#"## PR Review: #42 — Test PR

### Summary
Found issues in the implementation.

### Required Fixes
1. **Missing error handling**: The function does not handle the error case.
   - **Rationale**: This will cause a panic in production.
   - **Location**: src/lib.rs:42
   - **Suggested fix**: Add proper error handling with `?` operator.

### Optional Recommendations
1. Consider adding more tests.

### Remediation Required
YES
The remediation PR should add error handling to the identified function.
"#;

    let lower = review_output.to_lowercase();
    let pos = lower.find("### remediation required").unwrap();
    let rest = &lower[pos..];
    let found_yes = rest
        .lines()
        .skip(1)
        .take(3)
        .any(|l| l.trim().starts_with("yes"));
    assert!(
        found_yes,
        "should detect YES after remediation required header"
    );
}

// ---------------------------------------------------------------------------
// Unit-level: no-remediation output exits cleanly
// ---------------------------------------------------------------------------

#[test]
fn review_no_remediation_output_detected_correctly() {
    let review_output = r#"## PR Review: #42 — Test PR

### Summary
Clean implementation.

### Required Fixes
No required fixes identified.

### Optional Recommendations
No additional recommendations.

### Remediation Required
NO
"#;

    let lower = review_output.to_lowercase();
    let pos = lower.find("### remediation required").unwrap();
    let rest = &lower[pos..];
    let found_no = rest
        .lines()
        .skip(1)
        .take(3)
        .any(|l| l.trim().starts_with("no"));
    assert!(
        found_no,
        "should detect NO after remediation required header"
    );
    let found_yes = rest
        .lines()
        .skip(1)
        .take(3)
        .any(|l| l.trim().starts_with("yes"));
    assert!(!found_yes, "should not detect YES");
}
