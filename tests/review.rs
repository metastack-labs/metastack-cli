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
