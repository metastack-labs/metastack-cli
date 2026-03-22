#![allow(dead_code, unused_imports)]

include!("support/common.rs");

// ---------------------------------------------------------------------------
// Render-once snapshot tests (require gh stub)
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn improve_render_once_empty_state() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo = temp.path().join("repo");
    let config_path = temp.path().join("meta.toml");
    fs::write(&config_path, "[onboarding]\ncompleted = true\n")?;

    fs::create_dir_all(&repo)?;
    fs::write(repo.join("README.md"), "# test\n")?;
    init_repo_with_origin(&repo)?;

    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir)?;
    let gh_stub = bin_dir.join("gh");
    fs::write(
        &gh_stub,
        r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then echo '[]'; exit 0; fi
exit 1
"#,
    )?;
    fs::set_permissions(&gh_stub, fs::Permissions::from_mode(0o755))?;

    meta()
        .args([
            "agents",
            "improve",
            "--render-once",
            "--root",
            repo.to_str().unwrap(),
        ])
        .env("METASTACK_CONFIG", config_path.to_str().unwrap())
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .assert()
        .success()
        .stdout(predicate::str::contains("Improve:"))
        .stdout(predicate::str::contains("No open PRs"))
        .stdout(predicate::str::contains("No improve sessions"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn improve_render_once_with_open_prs() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo = temp.path().join("repo");
    let config_path = temp.path().join("meta.toml");
    fs::write(&config_path, "[onboarding]\ncompleted = true\n")?;

    fs::create_dir_all(&repo)?;
    fs::write(repo.join("README.md"), "# test\n")?;
    init_repo_with_origin(&repo)?;

    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir)?;
    let gh_stub = bin_dir.join("gh");
    fs::write(
        &gh_stub,
        r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
    echo '[{"number":42,"title":"MET-42: Add feature","url":"https://github.com/test/repo/pull/42","body":"PR body text","author":{"login":"alice"},"headRefName":"met-42-feature","baseRefName":"main"}]'
    exit 0
fi
exit 1
"#,
    )?;
    fs::set_permissions(&gh_stub, fs::Permissions::from_mode(0o755))?;

    meta()
        .args([
            "agents",
            "improve",
            "--render-once",
            "--root",
            repo.to_str().unwrap(),
        ])
        .env("METASTACK_CONFIG", config_path.to_str().unwrap())
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .assert()
        .success()
        .stdout(predicate::str::contains("#42"))
        .stdout(predicate::str::contains("MET-42"))
        .stdout(predicate::str::contains("alice"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn improve_render_once_tab_switches_to_sessions() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo = temp.path().join("repo");
    let config_path = temp.path().join("meta.toml");
    fs::write(&config_path, "[onboarding]\ncompleted = true\n")?;

    fs::create_dir_all(&repo)?;
    fs::write(repo.join("README.md"), "# test\n")?;
    init_repo_with_origin(&repo)?;

    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir)?;
    let gh_stub = bin_dir.join("gh");
    fs::write(
        &gh_stub,
        r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then echo '[]'; exit 0; fi
exit 1
"#,
    )?;
    fs::set_permissions(&gh_stub, fs::Permissions::from_mode(0o755))?;

    meta()
        .args([
            "agents",
            "improve",
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
        .stdout(predicate::str::contains("Sessions [active]"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn improve_render_once_enter_shows_pr_detail() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo = temp.path().join("repo");
    let config_path = temp.path().join("meta.toml");
    fs::write(&config_path, "[onboarding]\ncompleted = true\n")?;

    fs::create_dir_all(&repo)?;
    fs::write(repo.join("README.md"), "# test\n")?;
    init_repo_with_origin(&repo)?;

    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir)?;
    let gh_stub = bin_dir.join("gh");
    fs::write(
        &gh_stub,
        r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
    echo '[{"number":42,"title":"MET-42: Add feature","url":"https://github.com/test/repo/pull/42","body":"PR body text","author":{"login":"alice"},"headRefName":"met-42-feature","baseRefName":"main"}]'
    exit 0
fi
exit 1
"#,
    )?;
    fs::set_permissions(&gh_stub, fs::Permissions::from_mode(0o755))?;

    meta()
        .args([
            "agents",
            "improve",
            "--render-once",
            "--events",
            "enter",
            "--root",
            repo.to_str().unwrap(),
        ])
        .env("METASTACK_CONFIG", config_path.to_str().unwrap())
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .assert()
        .success()
        .stdout(predicate::str::contains("PR #42"))
        .stdout(predicate::str::contains("Author:"))
        .stdout(predicate::str::contains("alice"))
        .stdout(predicate::str::contains("met-42-feature"))
        .stdout(predicate::str::contains("Back to PRs"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn improve_render_once_enter_then_back_returns_to_list() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo = temp.path().join("repo");
    let config_path = temp.path().join("meta.toml");
    fs::write(&config_path, "[onboarding]\ncompleted = true\n")?;

    fs::create_dir_all(&repo)?;
    fs::write(repo.join("README.md"), "# test\n")?;
    init_repo_with_origin(&repo)?;

    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir)?;
    let gh_stub = bin_dir.join("gh");
    fs::write(
        &gh_stub,
        r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
    echo '[{"number":42,"title":"MET-42: Add feature","url":"https://github.com/test/repo/pull/42","body":"PR body text","author":{"login":"alice"},"headRefName":"met-42-feature","baseRefName":"main"}]'
    exit 0
fi
exit 1
"#,
    )?;
    fs::set_permissions(&gh_stub, fs::Permissions::from_mode(0o755))?;

    meta()
        .args([
            "agents",
            "improve",
            "--render-once",
            "--events",
            "enter,back",
            "--root",
            repo.to_str().unwrap(),
        ])
        .env("METASTACK_CONFIG", config_path.to_str().unwrap())
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .assert()
        .success()
        .stdout(predicate::str::contains("Open PRs [active]"))
        .stdout(predicate::str::contains("#42"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn improve_render_once_session_detail() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo = temp.path().join("repo");
    let config_path = temp.path().join("meta.toml");
    fs::write(&config_path, "[onboarding]\ncompleted = true\n")?;

    let meta_dir = repo.join(".metastack");
    let improve_sessions_dir = meta_dir.join("agents").join("improve").join("sessions");
    fs::create_dir_all(&improve_sessions_dir)?;
    fs::write(
        improve_sessions_dir.join("state.json"),
        r#"{
            "version": 1,
            "sessions": [{
                "session_id": "sess-test-1",
                "source_pr": {
                    "number": 99,
                    "title": "MET-99: Important fix",
                    "url": "https://github.com/test/repo/pull/99",
                    "author": "bob",
                    "head_branch": "met-99-fix",
                    "base_branch": "main"
                },
                "instructions": "Fix the flaky test",
                "phase": "running",
                "workspace_path": null,
                "improve_branch": "improve/met-99-fix",
                "stacked_pr_number": null,
                "stacked_pr_url": null,
                "error_summary": null,
                "created_at_epoch_seconds": 1000,
                "updated_at_epoch_seconds": 1000
            }]
        }"#,
    )?;

    init_repo_with_origin(&repo)?;

    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir)?;
    let gh_stub = bin_dir.join("gh");
    fs::write(
        &gh_stub,
        r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then echo '[]'; exit 0; fi
exit 1
"#,
    )?;
    fs::set_permissions(&gh_stub, fs::Permissions::from_mode(0o755))?;

    // Tab to sessions, then Enter to see session detail
    meta()
        .args([
            "agents",
            "improve",
            "--render-once",
            "--events",
            "tab,enter",
            "--root",
            repo.to_str().unwrap(),
        ])
        .env("METASTACK_CONFIG", config_path.to_str().unwrap())
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .assert()
        .success()
        .stdout(predicate::str::contains("Session:"))
        .stdout(predicate::str::contains("Session ID:"))
        .stdout(predicate::str::contains("sess-test-1"))
        .stdout(predicate::str::contains("Running"))
        .stdout(predicate::str::contains("Instructions:"))
        .stdout(predicate::str::contains("Fix the flaky test"))
        .stdout(predicate::str::contains("Back to Sessions"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn improve_render_once_with_persisted_session() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo = temp.path().join("repo");
    let config_path = temp.path().join("meta.toml");
    fs::write(&config_path, "[onboarding]\ncompleted = true\n")?;

    let meta_dir = repo.join(".metastack");
    let improve_sessions_dir = meta_dir.join("agents").join("improve").join("sessions");
    fs::create_dir_all(&improve_sessions_dir)?;
    fs::write(
        improve_sessions_dir.join("state.json"),
        r#"{
            "version": 1,
            "sessions": [{
                "session_id": "sess-test-1",
                "source_pr": {
                    "number": 99,
                    "title": "MET-99: Important fix",
                    "url": "https://github.com/test/repo/pull/99",
                    "author": "bob",
                    "head_branch": "met-99-fix",
                    "base_branch": "main"
                },
                "instructions": "Fix the flaky test",
                "phase": "completed",
                "workspace_path": null,
                "improve_branch": "improve/met-99-fix",
                "stacked_pr_number": 100,
                "stacked_pr_url": "https://github.com/test/repo/pull/100",
                "error_summary": null,
                "created_at_epoch_seconds": 1000,
                "updated_at_epoch_seconds": 1000
            }]
        }"#,
    )?;

    init_repo_with_origin(&repo)?;

    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir)?;
    let gh_stub = bin_dir.join("gh");
    fs::write(
        &gh_stub,
        r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then echo '[]'; exit 0; fi
exit 1
"#,
    )?;
    fs::set_permissions(&gh_stub, fs::Permissions::from_mode(0o755))?;

    meta()
        .args([
            "agents",
            "improve",
            "--render-once",
            "--root",
            repo.to_str().unwrap(),
        ])
        .env("METASTACK_CONFIG", config_path.to_str().unwrap())
        .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
        .assert()
        .success()
        .stdout(predicate::str::contains("#99"))
        .stdout(predicate::str::contains("Completed"))
        .stdout(predicate::str::contains("1 session"));

    Ok(())
}
