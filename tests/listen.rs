#![allow(dead_code, unused_imports)]

include!("support/common.rs");

#[cfg(unix)]
fn write_onboarded_config(
    config_path: &Path,
    config: impl AsRef<str>,
) -> Result<(), Box<dyn Error>> {
    fs::write(
        config_path,
        format!(
            "{}\n[onboarding]\ncompleted = true\n",
            config.as_ref().trim_end()
        ),
    )?;
    Ok(())
}

#[cfg(unix)]
fn write_listen_store_session(
    config_path: &Path,
    repo_root: &Path,
    sessions: Vec<serde_json::Value>,
) -> Result<PathBuf, Box<dyn Error>> {
    let store_dir = listen_project_store_dir(config_path, repo_root, None)?;
    let source_root = listen_source_root(repo_root)?;
    let metastack_root = source_root.join(".metastack").canonicalize()?;
    fs::create_dir_all(store_dir.join("logs"))?;
    fs::write(
        store_dir.join("project.json"),
        serde_json::to_vec_pretty(&json!({
            "version": 1,
            "project_key": store_dir
                .file_name()
                .expect("store dir should have a file name")
                .to_string_lossy(),
            "project_label": source_root
                .file_name()
                .expect("source root should have a file name")
                .to_string_lossy(),
            "source_root": source_root.display().to_string(),
            "metastack_root": metastack_root.display().to_string()
        }))?,
    )?;
    let state_path = store_dir.join("session.json");
    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&json!({
            "version": 1,
            "sessions": sessions
        }))?,
    )?;
    Ok(state_path)
}

#[cfg(unix)]
fn listen_session_json(
    issue_identifier: &str,
    phase: &str,
    updated_at_epoch_seconds: u64,
    pid: Option<u32>,
) -> serde_json::Value {
    json!({
        "issue_id": format!("{issue_identifier}-id"),
        "issue_identifier": issue_identifier,
        "issue_title": format!("{issue_identifier} title"),
        "project_name": "MetaStack CLI",
        "team_key": "MET",
        "issue_url": format!("https://linear.app/issues/{issue_identifier}"),
        "phase": phase,
        "summary": format!("{issue_identifier} summary"),
        "brief_path": format!(".metastack/agents/briefs/{issue_identifier}.md"),
        "workspace_path": format!("/tmp/{issue_identifier}"),
        "workpad_comment_id": format!("comment-{issue_identifier}"),
        "updated_at_epoch_seconds": updated_at_epoch_seconds,
        "pid": pid,
        "session_id": format!("session-{issue_identifier}"),
        "turns": 1,
        "tokens": {},
        "log_path": format!("logs/{issue_identifier}.log")
    })
}

#[test]
fn listen_requires_auth_when_not_in_demo_mode() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir().expect("tempdir should build");
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        temp.path(),
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  }
}
"#,
    )?;

    meta()
        .current_dir(temp.path())
        .env_remove("LINEAR_API_KEY")
        .env("METASTACK_CONFIG", &config_path)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .arg("listen")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("LINEAR_API_KEY")
                .or(predicate::str::contains("Linear profile")
                    .and(predicate::str::contains("is not configured"))),
        );

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_sessions_clear_issue_identifier_removes_only_matching_session()
-> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    init_repo_with_origin(&repo_root)?;

    let state_path = write_listen_store_session(
        &config_path,
        &repo_root,
        vec![
            listen_session_json("ENG-10163", "blocked", 200, None),
            listen_session_json("ENG-10164", "blocked", 300, None),
        ],
    )?;
    fs::write(
        listen_log_path(&config_path, &repo_root, "ENG-10163")?,
        "log 63\n",
    )?;
    fs::write(
        listen_log_path(&config_path, &repo_root, "ENG-10164")?,
        "log 64\n",
    )?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "clear",
            "ENG-10163",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Cleared 1 stored MetaListen session(s) matched by issue `ENG-10163`",
        ))
        .stdout(predicate::str::contains("ENG-10163 [Blocked]"));

    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    let sessions = state["sessions"]
        .as_array()
        .expect("sessions should remain an array");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["issue_identifier"], "ENG-10164");
    assert!(listen_log_path(&config_path, &repo_root, "ENG-10164")?.is_file());

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "inspect",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("ENG-10164 [Blocked]"))
        .stdout(predicate::str::contains("ENG-10163").not());

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_sessions_clear_refuses_live_pid_records() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    init_repo_with_origin(&repo_root)?;

    let mut child = ProcessCommand::new("sleep").arg("30").spawn()?;
    write_listen_store_session(
        &config_path,
        &repo_root,
        vec![listen_session_json(
            "ENG-10163",
            "running",
            300,
            Some(child.id()),
        )],
    )?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "clear",
            "--all",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "cannot clear live MetaListen session record(s)",
        ))
        .stderr(predicate::str::contains("ENG-10163"))
        .stderr(predicate::str::contains("pid"));

    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

#[cfg(unix)]
#[test]
fn agents_listen_sessions_clear_blocked_preserves_other_selector_states()
-> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    init_repo_with_origin(&repo_root)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    let state_path = write_listen_store_session(
        &config_path,
        &repo_root,
        vec![
            listen_session_json("ENG-10163", "blocked", now, None),
            listen_session_json("ENG-10164", "completed", now, None),
            listen_session_json("ENG-10165", "running", now, None),
        ],
    )?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "agents",
            "listen",
            "sessions",
            "clear",
            "--blocked",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Cleared 1 stored MetaListen session(s) matched by `--blocked`",
        ))
        .stdout(predicate::str::contains("ENG-10163 [Blocked]"));

    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    let sessions = state["sessions"]
        .as_array()
        .expect("sessions should remain an array");
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0]["issue_identifier"], "ENG-10164");
    assert_eq!(sessions[1]["issue_identifier"], "ENG-10165");

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "agents",
            "listen",
            "sessions",
            "inspect",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("ENG-10164 [Completed]"))
        .stdout(predicate::str::contains("ENG-10165 [Running]"))
        .stdout(predicate::str::contains("ENG-10163").not());

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_sessions_clear_completed_preserves_blocked_records() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    init_repo_with_origin(&repo_root)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    let state_path = write_listen_store_session(
        &config_path,
        &repo_root,
        vec![
            listen_session_json("ENG-10163", "completed", now, None),
            listen_session_json("ENG-10164", "blocked", now, None),
        ],
    )?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "clear",
            "--completed",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Cleared 1 stored MetaListen session(s) matched by `--completed`",
        ))
        .stdout(predicate::str::contains("ENG-10163 [Completed]"));

    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    let sessions = state["sessions"]
        .as_array()
        .expect("sessions should remain an array");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["issue_identifier"], "ENG-10164");

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_sessions_clear_stale_removes_only_dead_pid_records() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    init_repo_with_origin(&repo_root)?;

    let mut child = ProcessCommand::new("sleep").arg("30").spawn()?;
    let state_path = write_listen_store_session(
        &config_path,
        &repo_root,
        vec![
            listen_session_json("ENG-10163", "blocked", 100, Some(99_999)),
            listen_session_json("ENG-10164", "running", 200, Some(child.id())),
        ],
    )?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "clear",
            "--stale",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Cleared 1 stored MetaListen session(s) matched by `--stale`",
        ))
        .stdout(predicate::str::contains("ENG-10163 [Blocked]"));

    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    let sessions = state["sessions"]
        .as_array()
        .expect("sessions should remain an array");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["issue_identifier"], "ENG-10164");

    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_sessions_list_prunes_expired_completed_sessions_on_load() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    init_repo_with_origin(&repo_root)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let state_path = write_listen_store_session(
        &config_path,
        &repo_root,
        vec![
            listen_session_json("ENG-10163", "completed", now - (24 * 60 * 60) - 1, None),
            listen_session_json("ENG-10164", "blocked", now, None),
        ],
    )?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args(["listen", "sessions", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ENG-10164"))
        .stdout(predicate::str::contains("ENG-10163").not());

    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    let sessions = state["sessions"]
        .as_array()
        .expect("sessions should remain an array");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["issue_identifier"], "ENG-10164");
    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_sessions_inspect_prunes_expired_completed_sessions_on_load() -> Result<(), Box<dyn Error>>
{
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    init_repo_with_origin(&repo_root)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let state_path = write_listen_store_session(
        &config_path,
        &repo_root,
        vec![
            listen_session_json("ENG-10163", "completed", now - (24 * 60 * 60) - 1, None),
            listen_session_json("ENG-10164", "blocked", now, None),
        ],
    )?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "inspect",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Tracked sessions:"))
        .stdout(predicate::str::contains("ENG-10164 [Blocked]"))
        .stdout(predicate::str::contains("ENG-10163").not());

    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    let sessions = state["sessions"]
        .as_array()
        .expect("sessions should remain an array");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["issue_identifier"], "ENG-10164");
    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_sessions_inspect_reports_mixed_token_usage() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    init_repo_with_origin(&repo_root)?;

    write_listen_store_session(
        &config_path,
        &repo_root,
        vec![
            json!({
                "issue_id": "issue-10181",
                "issue_identifier": "ENG-10181",
                "issue_title": "Track built-in listen token usage",
                "project_name": "MetaStack CLI",
                "team_key": "MET",
                "issue_url": "https://linear.app/issues/ENG-10181",
                "phase": "running",
                "summary": "Token telemetry is flowing",
                "brief_path": null,
                "workspace_path": "/tmp/ENG-10181",
                "workpad_comment_id": "comment-10181",
                "updated_at_epoch_seconds": 1_773_575_100u64,
                "pid": 4242,
                "session_id": "session-10181",
                "turns": 2,
                "tokens": {
                    "input": 210,
                    "output": 34
                },
                "log_path": "logs/ENG-10181.log"
            }),
            json!({
                "issue_id": "issue-10182",
                "issue_identifier": "ENG-10182",
                "issue_title": "Fallback when usage is unavailable",
                "project_name": "MetaStack CLI",
                "team_key": "MET",
                "issue_url": "https://linear.app/issues/ENG-10182",
                "phase": "blocked",
                "summary": "Provider did not emit exact counts",
                "brief_path": null,
                "workspace_path": "/tmp/ENG-10182",
                "workpad_comment_id": "comment-10182",
                "updated_at_epoch_seconds": 1_773_575_000u64,
                "pid": null,
                "session_id": "session-10182",
                "turns": 1,
                "tokens": {},
                "log_path": "logs/ENG-10182.log"
            }),
        ],
    )?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "inspect",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Latest session:"))
        .stdout(predicate::str::contains("  - Tokens: in 210 | out 34 | total 244"))
        .stdout(predicate::str::contains(
            "  - ENG-10181 [Running] Token telemetry is flowing | tokens in 210 | out 34 | total 244",
        ))
        .stdout(predicate::str::contains(
            "  - ENG-10182 [Blocked] Provider did not emit exact counts | tokens n/a",
        ));

    Ok(())
}

#[test]
fn listen_once_demo_outputs_terminal_summary_without_browser_endpoints()
-> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--demo",
            "--once",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("meta listen"))
        .stdout(predicate::str::contains("Watching: all assignees"))
        .stdout(predicate::str::contains("Dashboard: terminal summary"))
        .stdout(predicate::str::contains("http://").not())
        .stdout(predicate::str::contains("127.0.0.1").not())
        .stdout(predicate::str::contains("localhost").not());

    Ok(())
}

#[test]
fn listen_render_once_demo_outputs_dashboard_snapshot() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--demo",
            "--render-once",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Listen Status"))
        .stdout(predicate::str::contains("Watching: all assignees"))
        .stdout(predicate::str::contains("Runtime"))
        .stdout(predicate::str::contains("Agent Sessions"))
        .stdout(predicate::str::contains("http://").not())
        .stdout(predicate::str::contains("127.0.0.1").not())
        .stdout(predicate::str::contains("localhost").not())
        .stdout(predicate::str::contains("SESSION"))
        .stdout(predicate::str::contains("PROGRESS"))
        .stdout(predicate::str::contains("MET-13"));

    Ok(())
}

#[test]
fn agents_listen_help_omits_browser_dashboard_flags() {
    let _guard = listen_test_lock();
    meta()
        .args(["agents", "listen", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--all-assignees"))
        .stdout(predicate::str::contains("--dashboard-port").not())
        .stdout(predicate::str::contains("browser").not());
}

#[test]
fn legacy_listen_help_omits_browser_dashboard_flags() {
    let _guard = listen_test_lock();
    meta()
        .args(["listen", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("meta listen sessions list"))
        .stdout(predicate::str::contains(
            "meta listen sessions inspect --root . --project \"MetaStack API\"",
        ))
        .stdout(predicate::str::contains(
            "meta listen sessions clear --root . --project \"MetaStack API\"",
        ))
        .stdout(predicate::str::contains(
            "meta agents listen --team MET --project \"MetaStack API\"",
        ))
        .stdout(predicate::str::contains("--all-assignees"))
        .stdout(predicate::str::contains("--dashboard-port").not())
        .stdout(predicate::str::contains("browser").not());
}

#[cfg(unix)]
#[test]
fn listen_check_reports_codex_config_status_and_linear_api_validation() -> Result<(), Box<dyn Error>>
{
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let home_dir = temp.path().join("home");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(home_dir.join(".codex"))?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  },
  "agent": {
    "provider": "codex",
    "model": "gpt-5.4",
    "reasoning": "high"
  },
  "listen": {
    "assignment_scope": "viewer"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"
"#,
        ),
    )?;
    fs::write(
        home_dir.join(".codex/config.toml"),
        r#"approval_policy = "never"
sandbox_mode = "danger-full-access"

[mcp_servers.linear]
enabled = true
"#,
    )?;

    let codex_path = bin_dir.join("codex");
    fs::write(
        &codex_path,
        r#"#!/bin/sh
if [ "$1" = "--help" ]; then
  cat <<'EOF'
-a, --ask-for-approval <APPROVAL_POLICY>
-s, --sandbox <SANDBOX_MODE>
-C, --cd <DIR>
    --add-dir <DIR>
    --dangerously-bypass-approvals-and-sandbox
EOF
  exit 0
fi
if [ "$1" = "exec" ] && [ "$2" = "--help" ]; then
  cat <<'EOF'
-m, --model <MODEL>
-c, --config <key=value>
EOF
  exit 0
fi
exit 0
"#,
    )?;
    let mut permissions = fs::metadata(&codex_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&codex_path, permissions)?;

    init_repo_with_origin(&repo_root)?;
    let viewer_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Viewer");
        then.status(200).json_body(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Kames",
                    "email": "sudo@example.com"
                }
            }
        }));
    });

    let current_path = std::env::var("PATH")?;
    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("HOME", &home_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "agents",
            "listen",
            "--check",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Listen preflight passed for provider `codex`.",
        ))
        .stdout(predicate::str::contains("approval_policy = \"never\""))
        .stdout(predicate::str::contains(
            "sandbox_mode = \"danger-full-access\"",
        ))
        .stdout(predicate::str::contains("Linear API endpoint is reachable"))
        .stdout(predicate::str::contains(
            "Linear API authentication succeeded.",
        ))
        .stdout(predicate::str::contains(
            "Effective assignee filter: Kames + unassigned",
        ));
    assert!(viewer_mock.calls() >= 1);

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_check_reports_viewer_only_scope_in_preflight_summary() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let home_dir = temp.path().join("home");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(home_dir.join(".codex"))?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  },
  "agent": {
    "provider": "codex",
    "model": "gpt-5.4",
    "reasoning": "high"
  },
  "listen": {
    "assignment_scope": "viewer_only"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"
"#,
        ),
    )?;
    fs::write(
        home_dir.join(".codex/config.toml"),
        r#"approval_policy = "never"
sandbox_mode = "danger-full-access"

[mcp_servers.linear]
enabled = true
"#,
    )?;

    let codex_path = bin_dir.join("codex");
    fs::write(
        &codex_path,
        r#"#!/bin/sh
if [ "$1" = "--help" ]; then
  cat <<'EOF'
-a, --ask-for-approval <APPROVAL_POLICY>
-s, --sandbox <SANDBOX_MODE>
-C, --cd <DIR>
    --add-dir <DIR>
    --dangerously-bypass-approvals-and-sandbox
EOF
  exit 0
fi
if [ "$1" = "exec" ] && [ "$2" = "--help" ]; then
  cat <<'EOF'
-m, --model <MODEL>
-c, --config <key=value>
EOF
  exit 0
fi
exit 0
"#,
    )?;
    let mut permissions = fs::metadata(&codex_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&codex_path, permissions)?;

    init_repo_with_origin(&repo_root)?;
    let viewer_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Viewer");
        then.status(200).json_body(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Kames",
                    "email": "sudo@example.com"
                }
            }
        }));
    });

    let current_path = std::env::var("PATH")?;
    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("HOME", &home_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "agents",
            "listen",
            "--check",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Effective assignee filter: only Kames",
        ));
    assert!(viewer_mock.calls() >= 1);

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_once_fails_fast_on_codex_preflight_before_linear_auth() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let home_dir = temp.path().join("home");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&home_dir)?;
    write_onboarded_config(&config_path, "")?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  },
  "agent": {
    "provider": "codex",
    "model": "gpt-5.4"
  }
}
"#,
    )?;

    let codex_path = bin_dir.join("codex");
    fs::write(
        &codex_path,
        r#"#!/bin/sh
if [ "$1" = "--help" ]; then
  cat <<'EOF'
-a, --ask-for-approval <APPROVAL_POLICY>
-s, --sandbox <SANDBOX_MODE>
-C, --cd <DIR>
    --add-dir <DIR>
    --dangerously-bypass-approvals-and-sandbox
EOF
  exit 0
fi
if [ "$1" = "exec" ] && [ "$2" = "--help" ]; then
  cat <<'EOF'
-m, --model <MODEL>
-c, --config <key=value>
EOF
  exit 0
fi
exit 0
"#,
    )?;
    let mut permissions = fs::metadata(&codex_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&codex_path, permissions)?;

    init_repo_with_origin(&repo_root)?;

    let current_path = std::env::var("PATH")?;
    meta()
        .current_dir(&repo_root)
        .env_remove("LINEAR_API_KEY")
        .env("METASTACK_CONFIG", &config_path)
        .env("HOME", &home_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "listen",
            "--once",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("approval_policy = \"never\""))
        .stderr(predicate::str::contains(
            "sandbox_mode = \"danger-full-access\"",
        ))
        .stderr(predicate::str::contains("LINEAR_API_KEY").not());

    Ok(())
}

#[test]
fn agents_listen_matches_legacy_listen_output() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;

    let legacy = meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--demo",
            "--render-once",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .output()?;
    assert!(legacy.status.success());

    let preferred = meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "agents",
            "listen",
            "--demo",
            "--render-once",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .output()?;
    assert!(preferred.status.success());

    assert_eq!(
        String::from_utf8(legacy.stdout)?,
        String::from_utf8(preferred.stdout)?
    );
    Ok(())
}

#[test]
fn agents_listen_matches_legacy_once_output() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;

    let legacy = meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--demo",
            "--once",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .output()?;
    assert!(legacy.status.success());

    let preferred = meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "agents",
            "listen",
            "--demo",
            "--once",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .output()?;
    assert!(preferred.status.success());

    assert_eq!(
        String::from_utf8(legacy.stdout)?,
        String::from_utf8(preferred.stdout)?
    );
    Ok(())
}

#[test]
fn listen_uses_repo_configured_poll_interval_by_default() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  },
  "listen": {
    "poll_interval_seconds": 42
  }
}
"#,
    )?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--demo",
            "--once",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Terminal refresh: 1s"))
        .stdout(predicate::str::contains("Linear refresh: 42s"));

    Ok(())
}

#[test]
fn listen_cli_poll_interval_overrides_repo_default() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  },
  "listen": {
    "poll_interval_seconds": 42
  }
}
"#,
    )?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--demo",
            "--once",
            "--poll-interval",
            "9",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Terminal refresh: 1s"))
        .stdout(predicate::str::contains("Linear refresh: 9s"));

    Ok(())
}

#[test]
fn listen_once_uses_repo_selected_profile_and_project_over_conflicting_global_defaults()
-> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let right_server = MockServer::start();
    let wrong_server = MockServer::start();
    let right_api_url = right_server.url("/graphql");
    let wrong_api_url = wrong_server.url("/graphql");

    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "global-token"
api_url = "{wrong_api_url}"
team = "PER"
default_profile = "personal"

[linear.profiles.work]
api_key = "repo-token"
api_url = "{right_api_url}"
team = "MET"

[linear.profiles.personal]
api_key = "personal-token"
api_url = "{wrong_api_url}"
team = "PER"
"#
        ),
    )?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "profile": "work",
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "required_label": "agent"
  }
}
"#,
    )?;

    let issues_mock = right_server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "repo-token")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-selected",
                        "identifier": "MET-401",
                        "title": "Repo default listen issue",
                        "description": "Should be observed for this repo",
                        "url": "https://linear.app/issues/401",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "assignee": null,
                        "labels": {
                            "nodes": []
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "Repo Project"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }, {
                        "id": "issue-wrong-project",
                        "identifier": "MET-402",
                        "title": "Wrong project issue",
                        "description": "Should be filtered out by the repo project default",
                        "url": "https://linear.app/issues/402",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:01:00Z",
                        "assignee": null,
                        "labels": {
                            "nodes": []
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-2",
                            "name": "Wrong Project"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }, {
                        "id": "issue-wrong-team",
                        "identifier": "PER-403",
                        "title": "Wrong team issue",
                        "description": "Should be filtered out by the repo team default",
                        "url": "https://linear.app/issues/403",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:02:00Z",
                        "assignee": null,
                        "labels": {
                            "nodes": []
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-2",
                            "key": "PER",
                            "name": "Personal"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "Repo Project"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });

    meta()
        .current_dir(&repo_root)
        .env_remove("LINEAR_API_KEY")
        .env_remove("LINEAR_API_URL")
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--once",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Observed 1 Todo issue(s) from Linear.",
        ))
        .stdout(predicate::str::contains("Dashboard: terminal summary"))
        .stdout(predicate::str::contains(
            "Skipped MET-401: missing required label `agent`.",
        ))
        .stdout(predicate::str::contains("http://").not())
        .stdout(predicate::str::contains("127.0.0.1").not())
        .stdout(predicate::str::contains("localhost").not())
        .stdout(predicate::str::contains("MET-402").not())
        .stdout(predicate::str::contains("PER-403").not());

    issues_mock.assert();
    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_rejects_duplicate_active_listener_lock_for_same_project() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    init_repo_with_origin(&repo_root)?;

    let project_dir = listen_project_store_dir(&config_path, &repo_root, Some("MetaStack CLI"))?;
    fs::create_dir_all(&project_dir)?;
    fs::write(
        project_dir.join("active-listener.lock.json"),
        format!(
            r#"{{
  "pid": {},
  "acquired_at_epoch_seconds": 1773575600,
  "source_root": "{}",
  "metastack_root": "{}"
}}"#,
            std::process::id(),
            listen_source_root(&repo_root)?.display(),
            listen_source_root(&repo_root)?
                .join(".metastack")
                .canonicalize()?
                .display()
        ),
    )?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--demo",
            "--once",
            "--project",
            "MetaStack CLI",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already owns project"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_allows_active_listener_lock_for_different_project() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    init_repo_with_origin(&repo_root)?;

    let alpha_dir = listen_project_store_dir(&config_path, &repo_root, Some("Alpha"))?;
    fs::create_dir_all(&alpha_dir)?;
    fs::write(
        alpha_dir.join("active-listener.lock.json"),
        format!(
            r#"{{
  "pid": {},
  "acquired_at_epoch_seconds": 1773575600,
  "source_root": "{}",
  "metastack_root": "{}"
}}"#,
            std::process::id(),
            listen_source_root(&repo_root)?.display(),
            listen_source_root(&repo_root)?
                .join(".metastack")
                .canonicalize()?
                .display()
        ),
    )?;

    let beta_state_path =
        listen_project_store_dir(&config_path, &repo_root, Some("Beta"))?.join("session.json");
    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--demo",
            "--once",
            "--project",
            "Beta",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "State file: {}",
            beta_state_path.display()
        )))
        .stdout(predicate::str::contains(alpha_dir.display().to_string()).not());

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_recovers_stale_active_listener_lock() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    init_repo_with_origin(&repo_root)?;

    let project_dir = listen_project_store_dir(&config_path, &repo_root, None)?;
    fs::create_dir_all(&project_dir)?;
    let lock_path = project_dir.join("active-listener.lock.json");
    fs::write(
        &lock_path,
        format!(
            r#"{{
  "pid": 999999,
  "acquired_at_epoch_seconds": 1773575600,
  "source_root": "{}",
  "metastack_root": "{}"
}}"#,
            listen_source_root(&repo_root)?.display(),
            listen_source_root(&repo_root)?
                .join(".metastack")
                .canonicalize()?
                .display()
        ),
    )?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--demo",
            "--once",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("State file:"))
        .stdout(predicate::str::contains(
            listen_state_path(&config_path, &repo_root)?
                .to_string_lossy()
                .as_ref(),
        ));
    assert!(!lock_path.exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_omitted_project_uses_repo_default_project_identity() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-default"
  }
}
"#,
    )?;
    init_repo_with_origin(&repo_root)?;

    let default_project_dir =
        listen_project_store_dir(&config_path, &repo_root, Some("project-default"))?;
    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--demo",
            "--once",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "State file: {}",
            default_project_dir.join("session.json").display()
        )));

    let metadata = fs::read_to_string(default_project_dir.join("project.json"))?;
    assert!(metadata.contains("\"project_selector\": \"project-default\""));
    assert!(metadata.contains("\"project_label\": \"project-default\""));

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_omitted_project_uses_install_default_project_identity() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(
        &config_path,
        r#"[defaults.linear]
project_id = "project-install"
"#,
    )?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    init_repo_with_origin(&repo_root)?;

    let install_project_dir =
        listen_project_store_dir(&config_path, &repo_root, Some("project-install"))?;
    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--demo",
            "--once",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "State file: {}",
            install_project_dir.join("session.json").display()
        )));

    let metadata = fs::read_to_string(install_project_dir.join("project.json"))?;
    assert!(metadata.contains("\"project_selector\": \"project-install\""));
    assert!(metadata.contains("\"project_label\": \"project-install\""));

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_render_once_suppresses_pid_probe_output_across_refreshes() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    let server = MockServer::start();
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{}"
"#,
            server.url("/graphql"),
        ),
    )?;

    let issues_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-40",
                        "identifier": "MET-40",
                        "title": "Keep the running session clean",
                        "description": "Dashboard output should stay clean while the worker is alive",
                        "url": "https://linear.app/issues/40",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "assignee": {
                            "id": "viewer-1",
                            "name": "Kames",
                            "email": "sudo@example.com"
                        },
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "agent"
                            }]
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-2",
                            "name": "In Progress",
                            "type": "started"
                        }
                    }, {
                        "id": "issue-41",
                        "identifier": "MET-41",
                        "title": "Keep the resumed session clean",
                        "description": "No raw process output should enter the dashboard",
                        "url": "https://linear.app/issues/41",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:01:00Z",
                        "assignee": {
                            "id": "viewer-1",
                            "name": "Kames",
                            "email": "sudo@example.com"
                        },
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "agent"
                            }]
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-2",
                            "name": "In Progress",
                            "type": "started"
                        }
                    }]
                }
            }
        }));
    });
    let issue_40_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-40\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": listen_issue_detail_node(
                    "issue-40",
                    "MET-40",
                    "Keep the running session clean",
                    "Dashboard output should stay clean while the worker is alive",
                    "state-2",
                    "In Progress",
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
            }
        }));
    });
    let issue_41_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-41\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": listen_issue_detail_node(
                    "issue-41",
                    "MET-41",
                    "Keep the resumed session clean",
                    "No raw process output should enter the dashboard",
                    "state-2",
                    "In Progress",
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
            }
        }));
    });
    let update_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": null
                }
            }
        }));
    });

    let ps_path = bin_dir.join("ps");
    fs::write(
        &ps_path,
        r#"#!/bin/sh
count_file="$TEST_OUTPUT_DIR/ps-count.txt"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
printf '  PID TTY           TIME CMD\n'
printf '4242 ??         0:00.00 meta listen-worker --ticket MET-noise\n'
printf 'stderr-noise-from-ps\n' >&2
exit 0
"#,
    )?;
    let mut permissions = fs::metadata(&ps_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&ps_path, permissions)?;

    init_repo_with_origin(&repo_root)?;

    let state_path = listen_state_path(&config_path, &repo_root)?;
    fs::create_dir_all(
        state_path
            .parent()
            .expect("listen state path should have a parent"),
    )?;
    fs::write(
        &state_path,
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "sessions": [
                {
                    "issue_id": "issue-40",
                    "issue_identifier": "MET-40",
                    "issue_title": "Keep the running session clean",
                    "project_name": "MetaStack CLI",
                    "team_key": "MET",
                    "issue_url": "https://linear.app/issues/40",
                    "phase": "running",
                    "summary": "Progress text stays clean",
                    "brief_path": null,
                    "backlog_issue_identifier": null,
                    "backlog_issue_title": null,
                    "backlog_path": null,
                    "workspace_path": null,
                    "branch": "met-40-clean-session",
                    "workpad_comment_id": "comment-40",
                    "updated_at_epoch_seconds": 1_773_575_000u64,
                    "pid": 4242,
                    "session_id": "019cedb4-2293-7651-b0b4-dfac4af6a640-019cedb4-229b-7453-825e-3e3da4e1bf2a",
                    "turns": 3,
                    "tokens": {},
                    "log_path": ".metastack/agents/sessions/MET-40.log"
                },
                {
                    "issue_id": "issue-41",
                    "issue_identifier": "MET-41",
                    "issue_title": "Keep the resumed session clean",
                    "project_name": "MetaStack CLI",
                    "team_key": "MET",
                    "issue_url": "https://linear.app/issues/41",
                    "phase": "running",
                    "summary": "Second progress text stays clean",
                    "brief_path": null,
                    "backlog_issue_identifier": null,
                    "backlog_issue_title": null,
                    "backlog_path": null,
                    "workspace_path": null,
                    "branch": "met-41-clean-session",
                    "workpad_comment_id": "comment-41",
                    "updated_at_epoch_seconds": 1_773_574_900u64,
                    "pid": 4343,
                    "session_id": "019ceda5-0a41-7ef1-bf96-4f26683c1570-019ceda5-0a57-7820-b050-c05e112d66dd",
                    "turns": 4,
                    "tokens": {},
                    "log_path": ".metastack/agents/sessions/MET-41.log"
                }
            ]
        }))?,
    )?;

    let current_path = std::env::var("PATH")?;
    let run_render_once = || -> Result<(String, String), Box<dyn Error>> {
        let output = meta()
            .current_dir(&repo_root)
            .env("METASTACK_CONFIG", &config_path)
            .env("TEST_OUTPUT_DIR", &stub_dir)
            .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
            .args([
                "listen",
                "--root",
                repo_root.to_str().expect("temp path should be utf-8"),
                "--render-once",
                "--width",
                "140",
                "--height",
                "36",
            ])
            .assert()
            .success()
            .get_output()
            .clone();
        Ok((
            String::from_utf8(output.stdout)?,
            String::from_utf8(output.stderr)?,
        ))
    };

    let (first_stdout, first_stderr) = run_render_once()?;
    let (second_stdout, second_stderr) = run_render_once()?;

    for rendered in [&first_stdout, &second_stdout] {
        assert!(rendered.contains("Agent Sessions"));
        assert!(rendered.contains("terminal snapshot"));
        assert!(rendered.contains("MET-40"));
        assert!(rendered.contains("MET-41"));
        assert!(rendered.contains("Running"));
        assert!(rendered.contains("n/a"));
        assert!(rendered.contains("019c...e1bf2a"));
        assert!(rendered.contains("019c...2d66dd"));
        assert!(rendered.contains("Progress text stays clean"));
        assert!(rendered.contains("Second progress text stays clean"));
        assert!(!rendered.contains("http://"));
        assert!(!rendered.contains("127.0.0.1"));
        assert!(!rendered.contains("localhost"));
        assert!(!rendered.contains("PID TTY"));
        assert!(!rendered.contains("meta listen-worker --ticket MET-noise"));
    }
    for rendered in [&first_stderr, &second_stderr] {
        assert!(!rendered.contains("stderr-noise-from-ps"));
    }

    assert_eq!(
        fs::read_to_string(stub_dir.join("ps-count.txt"))?.trim(),
        "4"
    );

    let state = fs::read_to_string(&state_path)?;
    assert!(state.contains("\"issue_identifier\": \"MET-40\""));
    assert!(state.contains("\"issue_identifier\": \"MET-41\""));
    assert_eq!(state.matches("\"phase\": \"running\"").count(), 2);

    assert!(issues_mock.calls() >= 2);
    assert!(issue_40_mock.calls() >= 2);
    assert!(issue_41_mock.calls() >= 2);
    update_issue_mock.assert_calls(0);

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_uses_the_same_project_identity_for_repo_and_worktree_roots() -> Result<(), Box<dyn Error>>
{
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    init_repo_with_origin(&repo_root)?;
    let worktree_root = create_worktree_checkout(&repo_root, "feature/listen", "repo-worktree")?;

    let repo_store_dir = listen_project_store_dir(&config_path, &repo_root, None)?;
    let worktree_store_dir = listen_project_store_dir(&config_path, &worktree_root, None)?;
    assert_eq!(repo_store_dir, worktree_store_dir);

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--demo",
            "--once",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success();

    meta()
        .current_dir(&worktree_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "inspect",
            "--root",
            worktree_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "Source root: {}",
            repo_root.canonicalize()?.display()
        )))
        .stdout(predicate::str::contains(format!(
            "Lock file: {}",
            repo_store_dir.join("active-listener.lock.json").display()
        )));

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_once_bootstraps_workspace_clone_workpad_and_agent_session() -> Result<(), Box<dyn Error>>
{
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "required_label": "agent",
    "assignment_scope": "viewer",
    "instructions_path": "instructions/listen.md"
  }
}
"#,
    )?;
    fs::create_dir_all(repo_root.join("instructions"))?;
    fs::write(
        repo_root.join("instructions/listen.md"),
        "# Listener Instructions\nKeep the workpad current.\n",
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "stub"

[agents.commands.stub]
command = "agent-stub"
args = ["{{{{payload}}}}"]
transport = "arg"
"#,
        ),
    )?;
    let stub_path = bin_dir.join("agent-stub");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
printf '%s' "$PWD" > "$TEST_OUTPUT_DIR/cwd.txt"
printf '%s' "$1" > "$TEST_OUTPUT_DIR/payload.txt"
printf '%s' "$METASTACK_LINEAR_WORKPAD_COMMENT_ID" > "$TEST_OUTPUT_DIR/workpad.txt"
printf '%s' "$METASTACK_AGENT_INSTRUCTIONS" > "$TEST_OUTPUT_DIR/instructions.txt"
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;
    init_repo_with_origin(&repo_root)?;

    let viewer_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Viewer");
        then.status(200).json_body(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Kames",
                    "email": "sudo@example.com"
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-21",
                        "identifier": "MET-21",
                        "title": "Daemon pickup flow",
                        "description": "Claim Todo work and create agent briefs",
                        "url": "https://linear.app/issues/21",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "assignee": {
                            "id": "viewer-1",
                            "name": "Kames",
                            "email": "sudo@example.com"
                        },
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "agent"
                            }]
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }, {
                        "id": "issue-36",
                        "identifier": "MET-36",
                        "title": "Technical: Daemon pickup flow",
                        "description": "# Technical: Daemon pickup flow\n",
                        "url": "https://linear.app/issues/36",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:01:00Z",
                        "assignee": null,
                        "labels": {
                            "nodes": []
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-backlog",
                            "name": "Backlog",
                            "type": "backlog"
                        }
                    }, {
                        "id": "issue-22",
                        "identifier": "MET-22",
                        "title": "Other project work",
                        "description": "Should not be claimed by this repo default",
                        "url": "https://linear.app/issues/22",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:02:00Z",
                        "assignee": {
                            "id": "viewer-2",
                            "name": "Someone Else",
                            "email": "else@example.com"
                        },
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "agent"
                            }]
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-2",
                            "name": "Another Project"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });

    let teams_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(json!({
            "data": {
                "teams": {
                    "nodes": [{
                        "id": "team-1",
                        "key": "MET",
                        "name": "Metastack",
                        "states": {
                            "nodes": [
                                {
                                    "id": "state-backlog",
                                    "name": "Backlog",
                                    "type": "backlog"
                                },
                                {
                                    "id": "state-1",
                                    "name": "Todo",
                                    "type": "unstarted"
                                },
                                {
                                    "id": "state-2",
                                    "name": "In Progress",
                                    "type": "started"
                                }
                            ]
                        }
                    }]
                }
            }
        }));
    });
    let _projects_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Projects");
        then.status(200).json_body(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-1",
                        "name": "MetaStack CLI",
                        "description": null,
                        "url": "https://linear.app/projects/project-1",
                        "progress": 0.5,
                        "teams": {
                            "nodes": [{
                                "id": "team-1",
                                "key": "MET",
                                "name": "Metastack"
                            }]
                        }
                    }]
                }
            }
        }));
    });

    let issue_detail_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-21\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": listen_issue_detail_node(
                    "issue-21",
                    "MET-21",
                    "Daemon pickup flow",
                    "Claim Todo work and create agent briefs",
                    "state-2",
                    "In Progress",
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": {
                        "id": "issue-21",
                        "identifier": "MET-21",
                        "title": "Daemon pickup flow",
                        "description": "Claim Todo work and create agent briefs",
                        "url": "https://linear.app/issues/21",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:05:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-2",
                            "name": "In Progress",
                            "type": "started"
                        }
                    }
                }
            }
        }));
    });

    let create_backlog_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue");
        then.status(500);
    });

    let comment_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateComment")
            .body_includes("## Codex Workpad");
        then.status(200).json_body(json!({
            "data": {
                "commentCreate": {
                    "success": true,
                    "comment": {
                        "id": "comment-21",
                        "body": "## Codex Workpad",
                        "resolvedAt": null
                    }
                }
            }
        }));
    });

    let current_path = std::env::var("PATH")?;
    let state_path = listen_state_path(&config_path, &repo_root)?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "listen",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--once",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 claimed this cycle"))
        .stdout(predicate::str::contains("MET-21"))
        .stdout(predicate::str::contains(
            state_path.to_string_lossy().as_ref(),
        ));

    let workspace_root = temp.path().join("repo-workspace/MET-21");
    assert!(workspace_root.is_dir());
    assert_eq!(
        git_stdout(
            &workspace_root,
            &["rev-parse", "--path-format=absolute", "--git-dir"]
        )?,
        git_stdout(
            &workspace_root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        )?
    );
    assert!(
        !git_stdout(&repo_root, &["worktree", "list"])?
            .contains(workspace_root.to_string_lossy().as_ref())
    );

    let brief = fs::read_to_string(workspace_root.join(".metastack/agents/briefs/MET-21.md"))?;
    assert!(brief.contains("Daemon pickup flow"));
    assert!(brief.contains("Picked up automatically by `meta listen`."));

    wait_for_path(&stub_dir.join("payload.txt"))?;
    wait_for_path(&stub_dir.join("workpad.txt"))?;
    wait_for_path(&stub_dir.join("instructions.txt"))?;
    assert_eq!(
        fs::read_to_string(stub_dir.join("workpad.txt"))?,
        "comment-21"
    );
    let instructions = fs::read_to_string(stub_dir.join("instructions.txt"))?;
    assert!(instructions.contains("## Built-in Workflow Contract"));
    assert!(instructions.contains("No repo overlay files were found"));
    assert!(instructions.contains("`metastack` label is attached"));
    assert!(instructions.contains("Do not use the legacy `symphony` label."));
    let backlog_index_path = workspace_root.join(".metastack/backlog/MET-21/index.md");
    assert!(
        backlog_index_path.is_file(),
        "expected backlog index at {}\nstate: {:?}\nbacklog root: {}\nworkspace entries: {:?}",
        backlog_index_path.display(),
        listen_state_path(&config_path, &repo_root)
            .ok()
            .and_then(|path| fs::read_to_string(path).ok()),
        workspace_root.join(".metastack/backlog").display(),
        fs::read_dir(&workspace_root)
            .map(|entries| entries.count())
            .ok()
    );
    let backlog_index = fs::read_to_string(&backlog_index_path)?;
    assert!(backlog_index.contains("## Requirements"));
    assert!(backlog_index.contains("Claim Todo work and create agent briefs"));
    let validation_plan =
        fs::read_to_string(workspace_root.join(".metastack/backlog/MET-21/validation.md"))?;
    assert!(validation_plan.contains("must not overwrite the primary Linear issue description"));
    assert!(validation_plan.contains("Update the existing `## Codex Workpad` comment"));
    assert!(!validation_plan.contains("meta sync push MET-21"));
    assert!(
        workspace_root
            .join(".metastack/backlog/MET-21/.linear.json")
            .is_file()
    );

    assert!(viewer_mock.calls() >= 1);
    teams_mock.assert_calls(1);
    issue_detail_mock.assert_calls(1);
    create_backlog_mock.assert_calls(0);
    comment_mock.assert_calls(1);

    assert!(
        state_path.is_file(),
        "expected listen state at {}",
        state_path.display()
    );
    let state = fs::read_to_string(&state_path)?;
    assert!(state.contains("\"issue_identifier\": \"MET-21\""));
    assert!(
        state.contains("\"phase\": \"running\"")
            || state.contains("\"phase\": \"blocked\"")
            || state.contains("\"phase\": \"completed\""),
        "expected an active or finished worker phase in state: {state}"
    );
    assert!(state.contains("\"workpad_comment_id\": \"comment-21\""));
    assert!(state.contains("\"workspace_path\":"));
    assert!(state.contains("\"backlog_issue_identifier\": \"MET-21\""));
    assert!(!state.contains("\"backlog_issue_identifier\": \"MET-36\""));
    assert!(!state.contains("\"issue_identifier\": \"MET-22\""));
    assert!(
        !repo_root
            .join(".metastack/agents/sessions/listen-state.json")
            .exists()
    );

    let inspect = meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "inspect",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let inspect = String::from_utf8_lossy(&inspect);
    assert!(inspect.contains(state_path.to_string_lossy().as_ref()));
    assert!(inspect.contains("Tracked sessions:"));
    assert!(inspect.contains("MET-21"));

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args(["listen", "sessions", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Stored MetaListen project sessions",
        ))
        .stdout(predicate::str::contains("repo"));

    let project_key = state_path
        .parent()
        .and_then(|path| path.file_name())
        .and_then(|value| value.to_str())
        .expect("project key should be present")
        .to_string();
    meta()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "resume",
            "--project-key",
            &project_key,
            "--demo",
            "--once",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            state_path.to_string_lossy().as_ref(),
        ));

    wait_for_terminal_session_state(&state_path)?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "clear",
            "--all",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Cleared 1 stored MetaListen session(s) matched by `--all`",
        ));
    let cleared_state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    assert_eq!(
        cleared_state["sessions"]
            .as_array()
            .expect("sessions should remain an array")
            .len(),
        0
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_sessions_target_multiple_project_scopes_from_one_repo() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-default"
  }
}
"#,
    )?;
    init_repo_with_origin(&repo_root)?;

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--demo",
            "--once",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success();
    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--demo",
            "--once",
            "--project",
            "project-beta",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success();

    let default_state_path =
        listen_project_store_dir(&config_path, &repo_root, Some("project-default"))?
            .join("session.json");
    let beta_state_path = listen_project_store_dir(&config_path, &repo_root, Some("project-beta"))?
        .join("session.json");

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args(["listen", "sessions", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("project-default"))
        .stdout(predicate::str::contains("project-beta"))
        .stdout(predicate::str::contains(
            repo_root.canonicalize()?.display().to_string(),
        ));

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "inspect",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            default_state_path.display().to_string(),
        ))
        .stdout(predicate::str::contains("Project: project-default"));

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "inspect",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--project",
            "project-beta",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            beta_state_path.display().to_string(),
        ))
        .stdout(predicate::str::contains("Project: project-beta"));

    let beta_project_key = beta_state_path
        .parent()
        .and_then(|path| path.file_name())
        .and_then(|value| value.to_str())
        .expect("beta project key should be present")
        .to_string();

    meta()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "inspect",
            "--project-key",
            &beta_project_key,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            beta_state_path.display().to_string(),
        ))
        .stdout(predicate::str::contains("Project: project-beta"));

    meta()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "resume",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--project",
            "project-beta",
            "--demo",
            "--once",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            beta_state_path.display().to_string(),
        ));

    meta()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "sessions",
            "clear",
            "--project-key",
            &beta_project_key,
            "--all",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("project-beta"));

    assert!(default_state_path.parent().is_some_and(Path::exists));
    assert!(beta_state_path.parent().is_some_and(Path::exists));
    if beta_state_path.is_file() {
        let beta_state: serde_json::Value = serde_json::from_slice(&fs::read(&beta_state_path)?)?;
        assert_eq!(
            beta_state["sessions"]
                .as_array()
                .expect("sessions should remain an array")
                .len(),
            0
        );
    }

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_once_prefers_command_route_agent_over_global_default() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "required_label": "agent",
    "assignment_scope": "viewer"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "global-stub"

[agents.routing.commands."agents.listen"]
provider = "listen-stub"

[agents.commands.global-stub]
command = "global-stub"
args = ["{{{{payload}}}}"]
transport = "arg"

[agents.commands.listen-stub]
command = "listen-stub"
args = ["{{{{payload}}}}"]
transport = "arg"
"#,
        ),
    )?;

    let global_stub_path = bin_dir.join("global-stub");
    fs::write(
        &global_stub_path,
        r#"#!/bin/sh
printf '%s' "$1" > "$TEST_OUTPUT_DIR/global.txt"
"#,
    )?;
    let mut permissions = fs::metadata(&global_stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&global_stub_path, permissions)?;

    let listen_stub_path = bin_dir.join("listen-stub");
    fs::write(
        &listen_stub_path,
        r#"#!/bin/sh
printf '%s' "$1" > "$TEST_OUTPUT_DIR/listen.txt"
printf '%s' "$METASTACK_AGENT_PROVIDER_SOURCE" > "$TEST_OUTPUT_DIR/provider-source.txt"
printf '%s' "$METASTACK_AGENT_ROUTE_KEY" > "$TEST_OUTPUT_DIR/route-key.txt"
"#,
    )?;
    let mut permissions = fs::metadata(&listen_stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&listen_stub_path, permissions)?;
    init_repo_with_origin(&repo_root)?;

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Viewer");
        then.status(200).json_body(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Kames",
                    "email": "sudo@example.com"
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-63",
                        "identifier": "MET-63",
                        "title": "Route listen agent",
                        "description": "Verify listen routing",
                        "url": "https://linear.app/issues/63",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "assignee": {
                            "id": "viewer-1",
                            "name": "Kames",
                            "email": "sudo@example.com"
                        },
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "agent"
                            }]
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(json!({
            "data": {
                "teams": {
                    "nodes": [{
                        "id": "team-1",
                        "key": "MET",
                        "name": "Metastack",
                        "states": {
                            "nodes": [
                                {
                                    "id": "state-1",
                                    "name": "Todo",
                                    "type": "unstarted"
                                },
                                {
                                    "id": "state-2",
                                    "name": "In Progress",
                                    "type": "started"
                                }
                            ]
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Projects");
        then.status(200).json_body(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-1",
                        "name": "MetaStack CLI",
                        "description": null,
                        "url": "https://linear.app/projects/project-1",
                        "progress": 0.5,
                        "teams": {
                            "nodes": [{
                                "id": "team-1",
                                "key": "MET",
                                "name": "Metastack"
                            }]
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-63\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": listen_issue_detail_node(
                    "issue-63",
                    "MET-63",
                    "Route listen agent",
                    "Verify listen routing",
                    "state-2",
                    "In Progress",
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": {
                        "id": "issue-63",
                        "identifier": "MET-63",
                        "title": "Route listen agent",
                        "description": "Verify listen routing",
                        "url": "https://linear.app/issues/63",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:05:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-2",
                            "name": "In Progress",
                            "type": "started"
                        }
                    }
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue");
        then.status(500);
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateComment")
            .body_includes("## Codex Workpad");
        then.status(200).json_body(json!({
            "data": {
                "commentCreate": {
                    "success": true,
                    "comment": {
                        "id": "comment-63",
                        "body": "## Codex Workpad",
                        "resolvedAt": null
                    }
                }
            }
        }));
    });

    let current_path = std::env::var("PATH")?;
    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "listen",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--once",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MET-63"));

    wait_for_path(&stub_dir.join("listen.txt"))?;
    wait_for_path(&stub_dir.join("provider-source.txt"))?;
    wait_for_path(&stub_dir.join("route-key.txt"))?;
    assert!(stub_dir.join("listen.txt").exists());
    assert!(!stub_dir.join("global.txt").exists());
    assert_eq!(
        fs::read_to_string(stub_dir.join("provider-source.txt"))?,
        "command_route:agents.listen"
    );
    assert_eq!(
        fs::read_to_string(stub_dir.join("route-key.txt"))?,
        "agents.listen"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_once_downloads_issue_attachment_context_for_agent() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "required_label": "agent",
    "assignment_scope": "viewer"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "stub"

[agents.commands.stub]
command = "agent-stub"
args = ["{{{{payload}}}}"]
transport = "arg"
"#,
        ),
    )?;
    let stub_path = bin_dir.join("agent-stub");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
printf '%s' "$1" > "$TEST_OUTPUT_DIR/payload.txt"
printf '%s' "$METASTACK_AGENT_INSTRUCTIONS" > "$TEST_OUTPUT_DIR/instructions.txt"
printf '%s' "$METASTACK_LINEAR_ATTACHMENT_CONTEXT_PATH" > "$TEST_OUTPUT_DIR/attachment-context-path.txt"
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;
    init_repo_with_origin(&repo_root)?;

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Viewer");
        then.status(200).json_body(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Kames",
                    "email": "sudo@example.com"
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-24",
                        "identifier": "MET-24",
                        "title": "Attachment bootstrap",
                        "description": "Use uploaded docs as implementation context",
                        "url": "https://linear.app/issues/24",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "assignee": {
                            "id": "viewer-1",
                            "name": "Kames",
                            "email": "sudo@example.com"
                        },
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "agent"
                            }]
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(team_payload());
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-24\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": listen_issue_detail_node(
                    "issue-24",
                    "MET-24",
                    "Attachment bootstrap",
                    "Use uploaded docs as implementation context",
                    "state-2",
                    "In Progress",
                    Vec::new(),
                    vec![
                        json!({
                            "id": "attachment-1",
                            "title": "specification.md",
                            "url": server.url("/downloads/specification.md"),
                            "sourceType": "upload",
                            "metadata": {}
                        }),
                        json!({
                            "id": "attachment-2",
                            "title": "diagram.png",
                            "url": server.url("/downloads/diagram.png"),
                            "sourceType": "upload",
                            "metadata": {}
                        })
                    ],
                    Vec::new(),
                )
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": {
                        "id": "issue-24",
                        "identifier": "MET-24",
                        "title": "Attachment bootstrap",
                        "description": "Use uploaded docs as implementation context",
                        "url": "https://linear.app/issues/24",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:05:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-2",
                            "name": "In Progress",
                            "type": "started"
                        }
                    }
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(httpmock::Method::GET)
            .path("/downloads/specification.md");
        then.status(200).body("# Downloaded specification\n");
    });

    server.mock(|when, then| {
        when.method(httpmock::Method::GET)
            .path("/downloads/diagram.png");
        then.status(200).body("fake-png");
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateComment")
            .body_includes("## Codex Workpad");
        then.status(200).json_body(json!({
            "data": {
                "commentCreate": {
                    "success": true,
                    "comment": {
                        "id": "comment-24",
                        "body": "## Codex Workpad",
                        "resolvedAt": null
                    }
                }
            }
        }));
    });

    let current_path = std::env::var("PATH")?;
    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "listen",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--once",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MET-24"));

    wait_for_path(&stub_dir.join("attachment-context-path.txt"))?;
    let workspace_root = temp.path().join("repo-workspace/MET-24");
    let context_dir = workspace_root.join(".metastack/agents/issue-context/MET-24");
    let reported_context_dir = PathBuf::from(fs::read_to_string(
        stub_dir.join("attachment-context-path.txt"),
    )?);
    assert_eq!(
        reported_context_dir.canonicalize()?,
        context_dir.canonicalize()?
    );

    let manifest = fs::read_to_string(context_dir.join("README.md"))?;
    assert!(manifest.contains("Files downloaded: 2"));
    assert!(manifest.contains("files/01-specification.md"));
    assert!(manifest.contains("files/02-diagram.png"));
    assert_eq!(
        fs::read_to_string(context_dir.join("files/01-specification.md"))?,
        "# Downloaded specification\n"
    );
    assert_eq!(
        fs::read(context_dir.join("files/02-diagram.png"))?,
        b"fake-png"
    );

    let payload = fs::read_to_string(stub_dir.join("payload.txt"))?;
    let instructions = fs::read_to_string(stub_dir.join("instructions.txt"))?;
    assert!(payload.contains("Attachment context:"));
    assert!(payload.contains("Attachment manifest:"));
    assert!(instructions.contains("Additional Linear attachment context has been downloaded"));
    assert!(instructions.contains("## Repository Scope"));
    assert!(instructions.contains("Active workspace checkout:"));
    assert!(instructions.contains(
        "Keep implementation, validation, and local backlog updates anchored to the provided workspace checkout"
    ));
    assert!(!instructions.contains("MetaStack CLI"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_once_refreshes_existing_workspace_clone_and_reuses_backlog_and_workpad_comment()
-> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "required_label": "agent",
    "assignment_scope": "viewer",
    "instructions_path": "instructions/listen.md"
  }
}
"#,
    )?;
    fs::create_dir_all(repo_root.join("instructions"))?;
    fs::write(
        repo_root.join("instructions/listen.md"),
        "# Listener Instructions\nKeep the workpad current.\n",
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "stub"

[agents.commands.stub]
command = "agent-stub"
args = ["{{{{payload}}}}"]
transport = "arg"
"#,
        ),
    )?;
    let stub_path = bin_dir.join("agent-stub");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
printf '%s' "$1" > "$TEST_OUTPUT_DIR/payload.txt"
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;
    init_repo_with_origin(&repo_root)?;

    let workspace_root = create_workspace_clone_checkout(&repo_root, "repo-workspace/MET-50")?;
    let status = ProcessCommand::new("git")
        .args([
            "-C",
            workspace_root.to_string_lossy().as_ref(),
            "checkout",
            "-b",
            "scratch-local",
        ])
        .status()?;
    assert!(status.success());
    fs::write(workspace_root.join("stale.txt"), "stale\n")?;
    for args in [
        vec![
            "-C",
            workspace_root.to_string_lossy().as_ref(),
            "add",
            "stale.txt",
        ],
        vec![
            "-C",
            workspace_root.to_string_lossy().as_ref(),
            "commit",
            "-m",
            "stale workspace commit",
        ],
    ] {
        let status = ProcessCommand::new("git").args(args).status()?;
        assert!(status.success());
    }
    let backlog_dir = workspace_root.join(".metastack/backlog/MET-50");
    fs::create_dir_all(&backlog_dir)?;
    fs::write(
        backlog_dir.join("index.md"),
        "# Existing Technical Backlog\n\nDo not overwrite me.\n",
    )?;

    let viewer_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Viewer");
        then.status(200).json_body(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Kames",
                    "email": "sudo@example.com"
                }
            }
        }));
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-50",
                        "identifier": "MET-50",
                        "title": "Reuse existing listener workspace",
                        "description": "Resume the current backlog inside the existing workspace clone",
                        "url": "https://linear.app/issues/50",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "assignee": {
                            "id": "viewer-1",
                            "name": "Kames",
                            "email": "sudo@example.com"
                        },
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "agent"
                            }]
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }, {
                        "id": "issue-51",
                        "identifier": "MET-51",
                        "title": "Technical: Reuse existing listener workspace",
                        "description": "# Existing Technical Backlog\n\nDo not overwrite me.\n",
                        "url": "https://linear.app/issues/51",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:01:00Z",
                        "assignee": null,
                        "labels": {
                            "nodes": []
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-backlog",
                            "name": "Backlog",
                            "type": "backlog"
                        }
                    }]
                }
            }
        }));
    });
    let teams_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(team_payload());
    });
    let _projects_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Projects");
        then.status(200).json_body(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-1",
                        "name": "MetaStack CLI",
                        "description": "CLI platform work",
                        "url": "https://linear.app/projects/1",
                        "progress": 0.42,
                        "teams": {
                            "nodes": [{
                                "id": "team-1",
                                "key": "MET",
                                "name": "Metastack"
                            }]
                        }
                    }]
                }
            }
        }));
    });
    let update_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": {
                        "id": "issue-50",
                        "identifier": "MET-50",
                        "title": "Reuse existing listener workspace",
                        "description": "Resume the current backlog inside the existing workspace clone",
                        "url": "https://linear.app/issues/50",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:05:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-2",
                            "name": "In Progress",
                            "type": "started"
                        }
                    }
                }
            }
        }));
    });
    let parent_detail_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-50\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": listen_issue_detail_node(
                    "issue-50",
                    "MET-50",
                    "Reuse existing listener workspace",
                    "Resume the current backlog inside the existing workspace clone",
                    "state-2",
                    "In Progress",
                    vec![json!({
                        "id": "comment-50",
                        "body": "## Codex Workpad\n",
                        "resolvedAt": null
                    })],
                    Vec::new(),
                    vec![json!({
                        "id": "issue-51",
                        "identifier": "MET-51",
                        "title": "Technical: Reuse existing listener workspace",
                        "url": "https://linear.app/issues/51"
                    })],
                )
            }
        }));
    });
    let update_comment_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateComment")
            .body_includes("\"id\":\"comment-50\"")
            .body_includes("## Codex Workpad");
        then.status(200).json_body(json!({
            "data": {
                "commentUpdate": {
                    "success": true,
                    "comment": {
                        "id": "comment-50",
                        "body": "## Codex Workpad",
                        "resolvedAt": null
                    }
                }
            }
        }));
    });
    let create_backlog_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue");
        then.status(500);
    });
    let create_comment_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateComment");
        then.status(500);
    });

    let current_path = std::env::var("PATH")?;
    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "listen",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--once",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MET-50"));

    assert!(viewer_mock.calls() >= 1);
    teams_mock.assert_calls(1);
    update_issue_mock.assert_calls(1);
    parent_detail_mock.assert_calls(1);
    update_comment_mock.assert_calls(0);
    create_backlog_mock.assert_calls(0);
    create_comment_mock.assert_calls(0);

    wait_for_path(&stub_dir.join("payload.txt"))?;
    assert_eq!(
        fs::read_to_string(backlog_dir.join("index.md"))?,
        "# Existing Technical Backlog\n\nDo not overwrite me.\n"
    );
    assert!(!workspace_root.join("stale.txt").exists());
    assert_eq!(
        git_stdout(&workspace_root, &["rev-parse", "--abbrev-ref", "HEAD"])?,
        "met-50-reuse-existing-listener-workspace"
    );
    assert_eq!(
        git_stdout(
            &workspace_root,
            &["rev-parse", "--path-format=absolute", "--git-dir"]
        )?,
        git_stdout(
            &workspace_root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        )?
    );
    assert!(
        fs::read_to_string(stub_dir.join("payload.txt"))?.contains("Backlog identifier: MET-50")
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_once_executes_repo_selected_builtin_claude_provider() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "agent": {
    "provider": "claude",
    "model": "sonnet",
    "reasoning": "high"
  },
  "listen": {
    "required_label": "agent",
    "assignment_scope": "viewer"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "codex"
default_model = "gpt-5.4"
default_reasoning = "low"
"#,
        ),
    )?;

    let claude_path = bin_dir.join("claude");
    fs::write(
        &claude_path,
        r#"#!/bin/sh
if [ "$1" = "-p" ] && [ "$2" = "--help" ]; then
  cat <<'EOF'
-p, --print
--model <model>
--effort <level>
--output-format <format>
--permission-mode <mode>
EOF
  exit 0
fi
printf '%s\n' "$@" > "$TEST_OUTPUT_DIR/claude-args.txt"
printf '%s' "$METASTACK_AGENT_NAME" > "$TEST_OUTPUT_DIR/agent.txt"
printf '%s' "$METASTACK_AGENT_MODEL" > "$TEST_OUTPUT_DIR/model.txt"
printf '%s' "$METASTACK_AGENT_REASONING" > "$TEST_OUTPUT_DIR/reasoning.txt"
printf '%s' "$METASTACK_AGENT_PROVIDER_SOURCE" > "$TEST_OUTPUT_DIR/provider-source.txt"
printf '%s' "$METASTACK_AGENT_ROUTE_KEY" > "$TEST_OUTPUT_DIR/route-key.txt"
printf '%s' '{"type":"result","subtype":"success","result":"claude listen ok","session_id":"listen-session-1"}'
"#,
    )?;
    let mut permissions = fs::metadata(&claude_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&claude_path, permissions)?;

    let codex_path = bin_dir.join("codex");
    fs::write(
        &codex_path,
        r#"#!/bin/sh
if [ "$1" = "--help" ]; then
  cat <<'EOF'
-a, --ask-for-approval <APPROVAL_POLICY>
-s, --sandbox <SANDBOX_MODE>
-C, --cd <DIR>
    --add-dir <DIR>
    --dangerously-bypass-approvals-and-sandbox
EOF
  exit 0
fi
if [ "$1" = "exec" ] && [ "$2" = "--help" ]; then
  cat <<'EOF'
-m, --model <MODEL>
-c, --config <key=value>
EOF
  exit 0
fi
printf 'codex fallback invoked' > "$TEST_OUTPUT_DIR/codex.txt"
exit 99
"#,
    )?;
    let mut permissions = fs::metadata(&codex_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&codex_path, permissions)?;

    init_repo_with_origin(&repo_root)?;

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Viewer");
        then.status(200).json_body(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Kames",
                    "email": "sudo@example.com"
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-64",
                        "identifier": "MET-64",
                        "title": "Builtin Claude listen agent",
                        "description": "Verify repo-selected builtin provider resolution",
                        "url": "https://linear.app/issues/64",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "assignee": {
                            "id": "viewer-1",
                            "name": "Kames",
                            "email": "sudo@example.com"
                        },
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "agent"
                            }]
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(json!({
            "data": {
                "teams": {
                    "nodes": [{
                        "id": "team-1",
                        "key": "MET",
                        "name": "Metastack",
                        "states": {
                            "nodes": [
                                {
                                    "id": "state-1",
                                    "name": "Todo",
                                    "type": "unstarted"
                                },
                                {
                                    "id": "state-2",
                                    "name": "In Progress",
                                    "type": "started"
                                }
                            ]
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Projects");
        then.status(200).json_body(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-1",
                        "name": "MetaStack CLI",
                        "description": null,
                        "url": "https://linear.app/projects/project-1",
                        "progress": 0.5,
                        "teams": {
                            "nodes": [{
                                "id": "team-1",
                                "key": "MET",
                                "name": "Metastack"
                            }]
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-64\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": listen_issue_detail_node(
                    "issue-64",
                    "MET-64",
                    "Builtin Claude listen agent",
                    "Verify repo-selected builtin provider resolution",
                    "state-2",
                    "In Progress",
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": {
                        "id": "issue-64",
                        "identifier": "MET-64",
                        "title": "Builtin Claude listen agent",
                        "description": "Verify repo-selected builtin provider resolution",
                        "url": "https://linear.app/issues/64",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:05:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-2",
                            "name": "In Progress",
                            "type": "started"
                        }
                    }
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue");
        then.status(500);
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateComment")
            .body_includes("## Codex Workpad");
        then.status(200).json_body(json!({
            "data": {
                "commentCreate": {
                    "success": true,
                    "comment": {
                        "id": "comment-64",
                        "body": "## Codex Workpad",
                        "resolvedAt": null
                    }
                }
            }
        }));
    });

    let current_path = std::env::var("PATH")?;
    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "listen",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--once",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MET-64"));

    wait_for_path(&stub_dir.join("claude-args.txt"))?;
    wait_for_path(&stub_dir.join("provider-source.txt"))?;
    assert!(!stub_dir.join("codex.txt").exists());

    let args = fs::read_to_string(stub_dir.join("claude-args.txt"))?;
    assert!(args.contains("--permission-mode=bypassPermissions"));
    assert!(args.contains("-p"));
    assert!(args.contains("--model=sonnet"));
    assert!(args.contains("--effort=high"));
    assert!(args.contains("--output-format=json"));
    assert!(!args.contains("--reasoning="));
    assert_eq!(fs::read_to_string(stub_dir.join("agent.txt"))?, "claude");
    assert_eq!(fs::read_to_string(stub_dir.join("model.txt"))?, "sonnet");
    assert_eq!(fs::read_to_string(stub_dir.join("reasoning.txt"))?, "high");
    assert_eq!(
        fs::read_to_string(stub_dir.join("provider-source.txt"))?,
        "repo_default"
    );
    assert_eq!(
        fs::read_to_string(stub_dir.join("route-key.txt"))?,
        "agents.listen"
    );

    let listen_log = fs::read_to_string(listen_log_path(&config_path, &repo_root, "MET-64")?)?;
    assert!(listen_log.contains("Resolved provider: claude"));
    assert!(listen_log.contains("Resolved model: sonnet"));
    assert!(listen_log.contains("Resolved reasoning: high"));
    assert!(listen_log.contains("Provider source: repo_default"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_once_recreates_existing_workspace_clone_when_configured() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "required_label": "agent",
    "assignment_scope": "viewer",
    "refresh_policy": "recreate_from_origin_main"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "stub"

[agents.commands.stub]
command = "agent-stub"
args = ["{{{{payload}}}}"]
transport = "arg"
"#,
        ),
    )?;
    let stub_path = bin_dir.join("agent-stub");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
printf '%s' "$1" > "$TEST_OUTPUT_DIR/payload.txt"
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;
    init_repo_with_origin(&repo_root)?;

    let workspace_root = create_workspace_clone_checkout(&repo_root, "repo-workspace/MET-52")?;
    fs::write(workspace_root.join("stale.txt"), "remove me\n")?;
    let old_backlog_dir = workspace_root.join(".metastack/backlog/MET-52");
    fs::create_dir_all(&old_backlog_dir)?;
    fs::write(
        old_backlog_dir.join("index.md"),
        "# Old Backlog\n\nRemove me.\n",
    )?;

    let viewer_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Viewer");
        then.status(200).json_body(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Kames",
                    "email": "sudo@example.com"
                }
            }
        }));
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-52",
                        "identifier": "MET-52",
                        "title": "Recreate existing listener workspace",
                        "description": "Recreate the local ticket workspace from origin/main",
                        "url": "https://linear.app/issues/52",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "assignee": {
                            "id": "viewer-1",
                            "name": "Kames",
                            "email": "sudo@example.com"
                        },
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "agent"
                            }]
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });
    let teams_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(team_payload());
    });
    let issue_detail_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-52\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": listen_issue_detail_node(
                    "issue-52",
                    "MET-52",
                    "Recreate existing listener workspace",
                    "Recreate the local ticket workspace from origin/main",
                    "state-2",
                    "In Progress",
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
            }
        }));
    });
    let update_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": {
                        "id": "issue-52",
                        "identifier": "MET-52",
                        "title": "Recreate existing listener workspace",
                        "description": "Recreate the local ticket workspace from origin/main",
                        "url": "https://linear.app/issues/52",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:05:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-2",
                            "name": "In Progress",
                            "type": "started"
                        }
                    }
                }
            }
        }));
    });
    let create_comment_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateComment")
            .body_includes("## Codex Workpad");
        then.status(200).json_body(json!({
            "data": {
                "commentCreate": {
                    "success": true,
                    "comment": {
                        "id": "comment-52",
                        "body": "## Codex Workpad",
                        "resolvedAt": null
                    }
                }
            }
        }));
    });

    let current_path = std::env::var("PATH")?;
    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "listen",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--once",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MET-52"));

    assert!(viewer_mock.calls() >= 1);
    teams_mock.assert_calls(1);
    issue_detail_mock.assert_calls(1);
    update_issue_mock.assert_calls(1);
    create_comment_mock.assert_calls(1);

    wait_for_path(&stub_dir.join("payload.txt"))?;
    assert!(!workspace_root.join("stale.txt").exists());
    let recreated_backlog =
        fs::read_to_string(workspace_root.join(".metastack/backlog/MET-52/index.md"))?;
    assert!(recreated_backlog.contains("## Requirements"));
    assert!(recreated_backlog.contains("Recreate the local ticket workspace from origin/main"));
    assert_eq!(
        git_stdout(&workspace_root, &["rev-parse", "--abbrev-ref", "HEAD"])?,
        "met-52-recreate-existing-listener-workspace"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_once_relaunches_agent_until_issue_leaves_active_states() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    let server = DynamicLinearServer::start()?;
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "required_label": "agent",
    "assignment_scope": "viewer",
    "instructions_path": "instructions/listen.md"
  }
}
"#,
    )?;
    fs::create_dir_all(repo_root.join("instructions"))?;
    fs::write(
        repo_root.join("instructions/listen.md"),
        "# Listener Instructions\nKeep the workpad current.\n",
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "stub"

[agents.commands.stub]
command = "agent-stub"
args = ["{{{{payload}}}}"]
transport = "arg"
"#,
            api_url = server.url.as_str(),
        ),
    )?;
    let stub_path = bin_dir.join("agent-stub");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
count_file="$TEST_OUTPUT_DIR/count.txt"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
printf '%s' "$1" > "$TEST_OUTPUT_DIR/payload-$count.txt"
printf '%s' "$METASTACK_AGENT_INSTRUCTIONS" > "$TEST_OUTPUT_DIR/instructions-$count.txt"
mkdir -p src
printf '// turn %s\n' "$count" > "src/turn-$count.rs"
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;
    init_repo_with_origin(&repo_root)?;

    let current_path = std::env::var("PATH")?;
    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "listen",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--once",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 claimed this cycle"))
        .stdout(predicate::str::contains("MET-32"));

    wait_for_path_with_timeout(&stub_dir.join("payload-2.txt"), Duration::from_secs(120))?;
    wait_for_path_with_timeout(
        &stub_dir.join("instructions-2.txt"),
        Duration::from_secs(120),
    )?;
    let turn_count = fs::read_to_string(stub_dir.join("count.txt"))?
        .trim()
        .parse::<u32>()?;
    assert!(
        turn_count >= 2,
        "expected at least two agent turns, observed {turn_count}"
    );

    let first_payload = fs::read_to_string(stub_dir.join("payload-1.txt"))?;
    let second_payload = fs::read_to_string(stub_dir.join("payload-2.txt"))?;
    let second_instructions = fs::read_to_string(stub_dir.join("instructions-2.txt"))?;
    assert!(!first_payload.contains("continuation turn #2 of 20"));
    assert!(
        second_payload.contains("continuation turn #2 of 20")
            || second_payload.contains("continuation turn 2 of 20"),
        "unexpected second payload: {}",
        second_payload
    );
    assert!(second_instructions.contains("continuation turn 2 of 20"));

    let state_path = listen_state_path(&config_path, &repo_root)?;
    wait_for_file_substring_with_timeout(
        &state_path,
        "\"phase\": \"completed\"",
        Duration::from_secs(120),
    )?;
    let state = fs::read_to_string(state_path)?;
    assert!(state.contains("\"issue_identifier\": \"MET-32\""));
    assert!(state.contains("\"phase\": \"completed\""));
    assert!(state.contains("Human Review"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_once_blocks_after_repeated_noop_turns() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    let server = DynamicLinearServer::start_with_completion_after_refreshes(1_000_000)?;
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "required_label": "agent",
    "assignment_scope": "viewer",
    "instructions_path": "instructions/listen.md"
  }
}
"#,
    )?;
    fs::create_dir_all(repo_root.join("instructions"))?;
    fs::write(
        repo_root.join("instructions/listen.md"),
        "# Listener Instructions\nKeep the workpad current.\n",
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "stub"

[agents.commands.stub]
command = "agent-stub"
args = ["{{{{payload}}}}"]
transport = "arg"
"#,
            api_url = server.url.as_str(),
        ),
    )?;
    let stub_path = bin_dir.join("agent-stub");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
count_file="$TEST_OUTPUT_DIR/count.txt"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
printf '%s' "$1" > "$TEST_OUTPUT_DIR/payload-$count.txt"
printf '%s' "$METASTACK_AGENT_INSTRUCTIONS" > "$TEST_OUTPUT_DIR/instructions-$count.txt"
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;
    init_repo_with_origin(&repo_root)?;
    let workspace = create_workspace_clone_checkout(&repo_root, "repo-workspace/MET-32")?;
    let backlog_dir = workspace.join(".metastack/backlog/MET-32");
    fs::create_dir_all(&backlog_dir)?;
    fs::write(
        backlog_dir.join("index.md"),
        "# MET-32\n\n## Tasks\n\n- [ ] Keep working\n",
    )?;

    let current_path = std::env::var("PATH")?;
    meta()
        .current_dir(&workspace)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "listen-worker",
            "--source-root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--workspace",
            workspace.to_str().expect("workspace path should be utf-8"),
            "--issue",
            "MET-32",
            "--workpad-comment-id",
            "comment-32",
            "--backlog-issue",
            "MET-32",
        ])
        .assert()
        .success();

    let state_path = listen_state_path(&config_path, &repo_root)?;
    wait_for_file_substring(&state_path, "\"phase\": \"blocked\"")?;
    let turn_count = fs::read_to_string(stub_dir.join("count.txt"))?
        .trim()
        .parse::<u32>()?;
    assert!(
        turn_count >= 2,
        "expected at least two agent turns before the worker stalled, observed {turn_count}"
    );
    let state = fs::read_to_string(state_path)?;
    assert!(state.contains("\"issue_identifier\": \"MET-32\""));
    assert!(state.contains("\"phase\": \"blocked\""));
    assert!(state.contains("Blocked | stalled after 2 turn(s)"));
    assert!(state.contains("\"phase\": \"blocked\""));

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_once_skips_ineligible_issue_and_records_the_reason() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "required_label": "agent",
    "assignment_scope": "viewer"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"
"#,
        ),
    )?;
    init_repo_with_origin(&repo_root)?;

    let viewer_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Viewer");
        then.status(200).json_body(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Kames",
                    "email": "sudo@example.com"
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-31",
                        "identifier": "MET-31",
                        "title": "Ignored work",
                        "description": "Should not be claimed",
                        "url": "https://linear.app/issues/31",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "assignee": {
                            "id": "viewer-2",
                            "name": "Someone Else",
                            "email": "else@example.com"
                        },
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "manual"
                            }]
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });

    let update_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true
                }
            }
        }));
    });

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--once",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Watching: Kames + unassigned"))
        .stdout(predicate::str::contains("MET-31").not());

    assert!(viewer_mock.calls() >= 1);
    update_mock.assert_calls(0);
    let state = fs::read_to_string(listen_state_path(&config_path, &repo_root)?)?;
    assert!(!state.contains("MET-31"));
    assert!(!temp.path().join("repo-workspace/MET-31").exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_once_claims_viewer_assigned_issue_in_viewer_only_scope() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "required_label": "agent",
    "assignment_scope": "viewer_only"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "stub"

[agents.commands.stub]
command = "agent-stub"
args = ["{{{{payload}}}}"]
transport = "arg"
"#,
        ),
    )?;
    let stub_path = bin_dir.join("agent-stub");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
printf '%s' "$1" > "$TEST_OUTPUT_DIR/payload.txt"
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;
    init_repo_with_origin(&repo_root)?;

    let viewer_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Viewer");
        then.status(200).json_body(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Kames",
                    "email": "sudo@example.com"
                }
            }
        }));
    });
    let issues_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-54",
                        "identifier": "MET-54",
                        "title": "Claim viewer assigned work",
                        "description": "Viewer-only scope should still claim viewer work",
                        "url": "https://linear.app/issues/54",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "assignee": {
                            "id": "viewer-1",
                            "name": "Kames",
                            "email": "sudo@example.com"
                        },
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "agent"
                            }]
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(team_payload());
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-54\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": {
                    "id": "issue-54",
                    "identifier": "MET-54",
                    "title": "Claim viewer assigned work",
                    "description": "Viewer-only scope should still claim viewer work",
                    "url": "https://linear.app/issues/MET-54",
                    "priority": 2,
                    "updatedAt": "2026-03-14T16:00:00Z",
                    "team": {
                        "id": "team-1",
                        "key": "MET",
                        "name": "Metastack"
                    },
                    "project": {
                        "id": "project-1",
                        "name": "MetaStack CLI"
                    },
                    "assignee": {
                        "id": "viewer-1",
                        "name": "Kames",
                        "email": "sudo@example.com"
                    },
                    "labels": {
                        "nodes": [{
                            "id": "label-1",
                            "name": "agent"
                        }]
                    },
                    "comments": { "nodes": [] },
                    "state": {
                        "id": "state-2",
                        "name": "In Progress",
                        "type": "started"
                    },
                    "attachments": { "nodes": [] },
                    "parent": null,
                    "children": { "nodes": [] }
                }
            }
        }));
    });
    let update_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": {
                        "id": "issue-54",
                        "identifier": "MET-54",
                        "title": "Claim viewer assigned work",
                        "description": "Viewer-only scope should still claim viewer work",
                        "url": "https://linear.app/issues/54",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:05:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-2",
                            "name": "In Progress",
                            "type": "started"
                        }
                    }
                }
            }
        }));
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue");
        then.status(500);
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateComment")
            .body_includes("## Codex Workpad");
        then.status(200).json_body(json!({
            "data": {
                "commentCreate": {
                    "success": true,
                    "comment": {
                        "id": "comment-54",
                        "body": "## Codex Workpad",
                        "resolvedAt": null
                    }
                }
            }
        }));
    });

    let current_path = std::env::var("PATH")?;
    let state_path = listen_state_path(&config_path, &repo_root)?;
    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "listen",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--once",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Watching: only Kames"))
        .stdout(predicate::str::contains("1 claimed this cycle"))
        .stdout(predicate::str::contains("MET-54"));

    assert!(viewer_mock.calls() >= 1);
    assert!(issues_mock.calls() >= 3);
    update_mock.assert_calls(1);
    assert!(temp.path().join("repo-workspace/MET-54").is_dir());
    let state = fs::read_to_string(state_path)?;
    assert!(state.contains("\"issue_identifier\": \"MET-54\""));

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_once_skips_unassigned_issue_in_viewer_only_scope() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "required_label": "agent",
    "assignment_scope": "viewer_only"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"
"#,
        ),
    )?;
    init_repo_with_origin(&repo_root)?;

    let viewer_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Viewer");
        then.status(200).json_body(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Kames",
                    "email": "sudo@example.com"
                }
            }
        }));
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-55",
                        "identifier": "MET-55",
                        "title": "Ignore unassigned strict-mode work",
                        "description": "Viewer-only scope should skip unassigned work",
                        "url": "https://linear.app/issues/55",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "assignee": null,
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "agent"
                            }]
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });
    let update_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true
                }
            }
        }));
    });

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--once",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Watching: only Kames"))
        .stdout(predicate::str::contains("MET-55").not());

    assert!(viewer_mock.calls() >= 1);
    update_mock.assert_calls(0);
    let state = fs::read_to_string(listen_state_path(&config_path, &repo_root)?)?;
    assert!(!state.contains("MET-55"));
    assert!(!temp.path().join("repo-workspace/MET-55").exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_once_skips_foreign_assigned_issue_in_viewer_only_scope() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "required_label": "agent",
    "assignment_scope": "viewer_only"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"
"#,
        ),
    )?;
    init_repo_with_origin(&repo_root)?;

    let viewer_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Viewer");
        then.status(200).json_body(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Kames",
                    "email": "sudo@example.com"
                }
            }
        }));
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-56",
                        "identifier": "MET-56",
                        "title": "Ignore foreign strict-mode work",
                        "description": "Viewer-only scope should skip someone else's work",
                        "url": "https://linear.app/issues/56",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "assignee": {
                            "id": "viewer-2",
                            "name": "Someone Else",
                            "email": "else@example.com"
                        },
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "agent"
                            }]
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });
    let update_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true
                }
            }
        }));
    });

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "listen",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--once",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Watching: only Kames"))
        .stdout(predicate::str::contains("MET-56").not());

    assert!(viewer_mock.calls() >= 1);
    update_mock.assert_calls(0);
    let state = fs::read_to_string(listen_state_path(&config_path, &repo_root)?)?;
    assert!(!state.contains("MET-56"));
    assert!(!temp.path().join("repo-workspace/MET-56").exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_once_claims_unassigned_issue_in_viewer_scope() -> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "required_label": "agent",
    "assignment_scope": "viewer"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "stub"

[agents.commands.stub]
command = "agent-stub"
args = ["{{{{payload}}}}"]
transport = "arg"
"#,
        ),
    )?;
    let stub_path = bin_dir.join("agent-stub");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
printf '%s' "$1" > "$TEST_OUTPUT_DIR/payload.txt"
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;
    init_repo_with_origin(&repo_root)?;

    let viewer_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Viewer");
        then.status(200).json_body(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Kames",
                    "email": "sudo@example.com"
                }
            }
        }));
    });
    let issues_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-52",
                        "identifier": "MET-52",
                        "title": "Claim unassigned listen work",
                        "description": "Unassigned issues should now be eligible",
                        "url": "https://linear.app/issues/52",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "assignee": null,
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "agent"
                            }]
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(team_payload());
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-52\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": {
                    "id": "issue-52",
                    "identifier": "MET-52",
                    "title": "Claim unassigned listen work",
                    "description": "Unassigned issues should now be eligible",
                    "url": "https://linear.app/issues/MET-52",
                    "priority": 2,
                    "updatedAt": "2026-03-14T16:00:00Z",
                    "team": {
                        "id": "team-1",
                        "key": "MET",
                        "name": "Metastack"
                    },
                    "project": {
                        "id": "project-1",
                        "name": "MetaStack CLI"
                    },
                    "assignee": null,
                    "labels": {
                        "nodes": [{
                            "id": "label-1",
                            "name": "agent"
                        }]
                    },
                    "comments": { "nodes": [] },
                    "state": {
                        "id": "state-2",
                        "name": "In Progress",
                        "type": "started"
                    },
                    "attachments": { "nodes": [] },
                    "parent": null,
                    "children": { "nodes": [] }
                }
            }
        }));
    });
    let update_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": {
                        "id": "issue-52",
                        "identifier": "MET-52",
                        "title": "Claim unassigned listen work",
                        "description": "Unassigned issues should now be eligible",
                        "url": "https://linear.app/issues/52",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:05:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-2",
                            "name": "In Progress",
                            "type": "started"
                        }
                    }
                }
            }
        }));
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue");
        then.status(500);
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateComment")
            .body_includes("## Codex Workpad");
        then.status(200).json_body(json!({
            "data": {
                "commentCreate": {
                    "success": true,
                    "comment": {
                        "id": "comment-52",
                        "body": "## Codex Workpad",
                        "resolvedAt": null
                    }
                }
            }
        }));
    });

    let current_path = std::env::var("PATH")?;
    let state_path = listen_state_path(&config_path, &repo_root)?;
    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "listen",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--once",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Watching: Kames + unassigned"))
        .stdout(predicate::str::contains("1 claimed this cycle"))
        .stdout(predicate::str::contains("MET-52"));

    assert!(viewer_mock.calls() >= 1);
    assert!(issues_mock.calls() >= 3);
    update_mock.assert_calls(1);
    assert!(temp.path().join("repo-workspace/MET-52").is_dir());
    let state = fs::read_to_string(state_path)?;
    assert!(state.contains("\"issue_identifier\": \"MET-52\""));

    Ok(())
}

#[cfg(unix)]
#[test]
fn listen_once_all_assignees_override_claims_foreign_assigned_issue_without_changing_repo_scope()
-> Result<(), Box<dyn Error>> {
    let _guard = listen_test_lock();
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "required_label": "agent",
    "assignment_scope": "viewer"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "stub"

[agents.commands.stub]
command = "agent-stub"
args = ["{{{{payload}}}}"]
transport = "arg"
"#,
        ),
    )?;
    let stub_path = bin_dir.join("agent-stub");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
printf '%s' "$1" > "$TEST_OUTPUT_DIR/payload.txt"
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;
    init_repo_with_origin(&repo_root)?;

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Viewer");
        then.status(200).json_body(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Kames",
                    "email": "sudo@example.com"
                }
            }
        }));
    });
    let issues_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-53",
                        "identifier": "MET-53",
                        "title": "Claim foreign assigned work with override",
                        "description": "Run-scoped override should allow pickup",
                        "url": "https://linear.app/issues/53",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "assignee": {
                            "id": "viewer-2",
                            "name": "Someone Else",
                            "email": "else@example.com"
                        },
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "agent"
                            }]
                        },
                        "comments": {
                            "nodes": []
                        },
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(team_payload());
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-53\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": {
                    "id": "issue-53",
                    "identifier": "MET-53",
                    "title": "Claim foreign assigned work with override",
                    "description": "Run-scoped override should allow pickup",
                    "url": "https://linear.app/issues/MET-53",
                    "priority": 2,
                    "updatedAt": "2026-03-14T16:00:00Z",
                    "team": {
                        "id": "team-1",
                        "key": "MET",
                        "name": "Metastack"
                    },
                    "project": {
                        "id": "project-1",
                        "name": "MetaStack CLI"
                    },
                    "assignee": {
                        "id": "viewer-2",
                        "name": "Someone Else",
                        "email": "else@example.com"
                    },
                    "labels": {
                        "nodes": [{
                            "id": "label-1",
                            "name": "agent"
                        }]
                    },
                    "comments": { "nodes": [] },
                    "state": {
                        "id": "state-2",
                        "name": "In Progress",
                        "type": "started"
                    },
                    "attachments": { "nodes": [] },
                    "parent": null,
                    "children": { "nodes": [] }
                }
            }
        }));
    });
    let update_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": {
                        "id": "issue-53",
                        "identifier": "MET-53",
                        "title": "Claim foreign assigned work with override",
                        "description": "Run-scoped override should allow pickup",
                        "url": "https://linear.app/issues/53",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:05:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-2",
                            "name": "In Progress",
                            "type": "started"
                        }
                    }
                }
            }
        }));
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue");
        then.status(500);
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateComment")
            .body_includes("## Codex Workpad");
        then.status(200).json_body(json!({
            "data": {
                "commentCreate": {
                    "success": true,
                    "comment": {
                        "id": "comment-53",
                        "body": "## Codex Workpad",
                        "resolvedAt": null
                    }
                }
            }
        }));
    });

    let current_path = std::env::var("PATH")?;
    let state_path = listen_state_path(&config_path, &repo_root)?;
    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "listen",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--once",
            "--all-assignees",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Watching: all assignees"))
        .stdout(predicate::str::contains("1 claimed this cycle"))
        .stdout(predicate::str::contains("MET-53"));

    assert!(issues_mock.calls() >= 3);
    update_mock.assert_calls(1);
    assert!(temp.path().join("repo-workspace/MET-53").is_dir());
    let state = fs::read_to_string(state_path)?;
    assert!(state.contains("\"issue_identifier\": \"MET-53\""));
    assert!(
        fs::read_to_string(repo_root.join(".metastack/meta.json"))?
            .contains("\"assignment_scope\": \"viewer\"")
    );

    Ok(())
}
