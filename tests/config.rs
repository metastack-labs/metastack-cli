#![allow(dead_code, unused_imports)]

include!("support/common.rs");

fn write_onboarded_config(config_path: &Path, body: &str) -> Result<(), Box<dyn Error>> {
    let body = body.trim_start();
    let content = if body.is_empty() {
        "[onboarding]\ncompleted = true\n".to_string()
    } else {
        format!("[onboarding]\ncompleted = true\n\n{body}")
    };
    fs::write(config_path, content)?;
    Ok(())
}

#[test]
fn config_json_is_global_only_and_does_not_scaffold_repo_defaults() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"config_path\""))
        .stdout(predicate::str::contains("\"metastack_meta_path\"").not());

    assert!(!repo_root.join(".metastack/meta.json").exists());
    Ok(())
}

#[test]
fn config_direct_updates_persist_fast_plan_defaults() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--plan-default-mode",
            "fast",
            "--plan-fast-single-ticket",
            "false",
            "--plan-fast-questions",
            "2",
        ])
        .assert()
        .success();

    let parsed: toml::Value = toml::from_str(&fs::read_to_string(&config_path)?)?;
    assert_eq!(
        parsed["defaults"]["plan"]["default_mode"].as_str(),
        Some("fast")
    );
    assert_eq!(
        parsed["defaults"]["plan"]["fast_single_ticket"].as_bool(),
        Some(false)
    );
    assert_eq!(
        parsed["defaults"]["plan"]["fast_questions"].as_integer(),
        Some(2)
    );

    Ok(())
}

#[test]
fn config_json_updates_vim_mode_and_returns_effective_value() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

    let output = cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--json",
            "--vim-mode",
            "enabled",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let rendered: serde_json::Value = serde_json::from_slice(&output)?;
    assert_eq!(rendered["app"]["defaults"]["ui"]["vim_mode"], json!(true));

    let saved: toml::Value = toml::from_str(&fs::read_to_string(config_path)?)?;
    assert_eq!(saved["defaults"]["ui"]["vim_mode"].as_bool(), Some(true));

    Ok(())
}

#[test]
fn setup_json_scaffolds_repo_defaults() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "setup",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"metastack_meta_path\""));

    assert!(repo_root.join(".metastack/meta.json").is_file());
    Ok(())
}

#[test]
fn setup_direct_updates_persist_fast_plan_defaults() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "setup",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--plan-default-mode",
            "fast",
            "--plan-fast-single-ticket",
            "false",
            "--plan-fast-questions",
            "2",
        ])
        .assert()
        .success();

    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(repo_root.join(".metastack/meta.json"))?)?;
    assert_eq!(parsed["plan"]["default_mode"].as_str(), Some("fast"));
    assert_eq!(parsed["plan"]["fast_single_ticket"].as_bool(), Some(false));
    assert_eq!(parsed["plan"]["fast_questions"].as_u64(), Some(2));

    Ok(())
}

#[test]
fn setup_json_fails_when_backlog_template_conflicts_exist() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let conflicting_index = repo_root.join(".metastack/backlog/_TEMPLATE/index.md");
    fs::create_dir_all(
        conflicting_index
            .parent()
            .expect("template file should have a parent"),
    )?;
    fs::write(&conflicting_index, "# Local template change\n")?;
    write_onboarded_config(&config_path, "")?;

    let assert = cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "runtime",
            "setup",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--json",
        ])
        .assert()
        .failure();

    let payload: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout)?;
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["command"], "runtime.setup");
    assert_eq!(payload["error"]["code"], "invalid_input");
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(
                "repo setup found existing canonical backlog template files with local changes"
            )
    );
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(".metastack/backlog/_TEMPLATE/index.md")
    );
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("rerun `meta setup --root")
    );

    assert_eq!(
        fs::read_to_string(conflicting_index)?,
        "# Local template change\n"
    );

    Ok(())
}

#[test]
fn setup_updates_repo_defaults_with_flags_and_resolves_project_name() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_url = "{api_url}"
team = "MET"

[agents]
default_agent = "codex"
default_model = "gpt-5.4"
"#
        )
        .as_str(),
    )?;

    let projects_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "repo-token")
            .body_includes("query Projects");
        then.status(200).json_body(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-42",
                        "name": "MetaStack CLI",
                        "description": "Primary repo project",
                        "url": "https://linear.app/project/project-42",
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
    let teams_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "repo-token")
            .body_includes("query Teams");
        then.status(200).json_body(json!({
            "data": {
                "teams": {
                    "nodes": [{
                        "id": "team-1",
                        "key": "MET",
                        "name": "Metastack",
                        "states": { "nodes": [] }
                    }]
                }
            }
        }));
    });
    let labels_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "repo-token")
            .body_includes("query IssueLabels")
            .body_includes("\"key\":{\"eq\":\"MET\"}");
        then.status(200).json_body(json!({
            "data": {
                "issueLabels": {
                    "nodes": [],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        }));
    });
    let create_plan_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "repo-token")
            .body_includes("mutation CreateIssueLabel")
            .body_includes("\"name\":\"planning\"");
        then.status(200).json_body(json!({
            "data": {
                "issueLabelCreate": {
                    "success": true,
                    "issueLabel": { "id": "label-plan", "name": "planning" }
                }
            }
        }));
    });
    let create_technical_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "repo-token")
            .body_includes("mutation CreateIssueLabel")
            .body_includes("\"name\":\"engineering\"");
        then.status(200).json_body(json!({
            "data": {
                "issueLabelCreate": {
                    "success": true,
                    "issueLabel": { "id": "label-technical", "name": "engineering" }
                }
            }
        }));
    });
    let create_listen_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "repo-token")
            .body_includes("mutation CreateIssueLabel")
            .body_includes("\"name\":\"agent\"");
        then.status(200).json_body(json!({
            "data": {
                "issueLabelCreate": {
                    "success": true,
                    "issueLabel": { "id": "label-listen", "name": "agent" }
                }
            }
        }));
    });

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "setup",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--api-key",
            "repo-token",
            "--team",
            "MET",
            "--project",
            "MetaStack CLI",
            "--provider",
            "claude",
            "--model",
            "opus",
            "--reasoning",
            "medium",
            "--listen-label",
            "agent",
            "--assignment-scope",
            "viewer-only",
            "--instructions-path",
            "instructions/listen.md",
            "--listen-poll-interval",
            "45",
            "--interactive-plan-follow-up-question-limit",
            "4",
            "--plan-label",
            "planning",
            "--technical-label",
            "engineering",
            "--default-assignee",
            "viewer",
            "--default-state",
            "Todo",
            "--default-priority",
            "3",
            "--default-label",
            "platform",
            "--default-label",
            "cli",
            "--velocity-project",
            "MetaStack CLI",
            "--velocity-state",
            "Backlog",
            "--velocity-auto-assign",
            "viewer",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Repo setup saved."));

    let config = fs::read_to_string(&config_path)?;
    let planning_meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(repo_root.join(".metastack/meta.json"))?)?;
    let canonical_repo_root = fs::canonicalize(&repo_root)?;

    assert!(config.contains(&format!(
        "[linear.repo_auth.\"{}\"]",
        canonical_repo_root.to_string_lossy()
    )));
    assert!(config.contains("api_key = \"repo-token\""));
    assert!(config.contains("team = \"MET\""));
    assert!(!config.contains("project-42"));
    assert!(planning_meta["linear"]["api_key"].is_null());
    assert_eq!(planning_meta["linear"]["team"].as_str(), Some("MET"));
    assert_eq!(
        planning_meta["linear"]["project_id"].as_str(),
        Some("project-42")
    );
    assert_eq!(planning_meta["agent"]["provider"].as_str(), Some("claude"));
    assert_eq!(planning_meta["agent"]["model"].as_str(), Some("opus"));
    assert_eq!(planning_meta["agent"]["reasoning"].as_str(), Some("medium"));
    assert_eq!(
        planning_meta["listen"]["required_labels"]
            .as_array()
            .and_then(|labels| labels.first())
            .and_then(|label| label.as_str()),
        Some("agent")
    );
    assert_eq!(
        planning_meta["listen"]["assignment_scope"].as_str(),
        Some("viewer_only")
    );
    assert_eq!(
        planning_meta["listen"]["instructions_path"].as_str(),
        Some("instructions/listen.md")
    );
    assert_eq!(
        planning_meta["listen"]["poll_interval_seconds"].as_u64(),
        Some(45)
    );
    assert_eq!(
        planning_meta["plan"]["interactive_follow_up_questions"].as_u64(),
        Some(4)
    );
    assert_eq!(
        planning_meta["issue_labels"]["plan"].as_str(),
        Some("planning")
    );
    assert_eq!(
        planning_meta["issue_labels"]["technical"].as_str(),
        Some("engineering")
    );
    assert_eq!(
        planning_meta["backlog"]["default_assignee"].as_str(),
        Some("viewer")
    );
    assert_eq!(
        planning_meta["backlog"]["default_state"].as_str(),
        Some("Todo")
    );
    assert_eq!(
        planning_meta["backlog"]["default_priority"].as_u64(),
        Some(3)
    );
    assert_eq!(
        planning_meta["backlog"]["default_labels"],
        json!(["platform", "cli"])
    );
    assert_eq!(
        planning_meta["backlog"]["velocity_defaults"]["project"].as_str(),
        Some("MetaStack CLI")
    );
    assert_eq!(
        planning_meta["backlog"]["velocity_defaults"]["state"].as_str(),
        Some("Backlog")
    );
    assert_eq!(
        planning_meta["backlog"]["velocity_defaults"]["auto_assign"].as_str(),
        Some("viewer")
    );
    projects_mock.assert();
    teams_mock.assert();
    labels_mock.assert();
    create_plan_mock.assert();
    create_technical_mock.assert();
    create_listen_mock.assert();

    Ok(())
}

#[test]
fn config_builtin_defaults_do_not_persist_builtin_command_override_entries()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--default-agent",
            "codex",
            "--default-model",
            "gpt-5.4",
            "--default-reasoning",
            "medium",
            "--default-assignee",
            "viewer",
            "--default-state",
            "Backlog",
            "--default-priority",
            "2",
            "--default-label",
            "platform",
            "--default-label",
            "cli",
            "--velocity-project",
            "MetaStack CLI",
            "--velocity-state",
            "Todo",
            "--velocity-auto-assign",
            "viewer",
        ])
        .assert()
        .success();

    let saved = fs::read_to_string(config_path)?;
    let parsed: toml::Value = toml::from_str(&saved)?;
    assert_eq!(parsed["agents"]["default_agent"].as_str(), Some("codex"));
    assert_eq!(
        parsed["agents"]["default_reasoning"].as_str(),
        Some("medium")
    );
    assert_eq!(
        parsed["backlog"]["default_assignee"].as_str(),
        Some("viewer")
    );
    assert_eq!(parsed["backlog"]["default_state"].as_str(), Some("Backlog"));
    assert_eq!(parsed["backlog"]["default_priority"].as_integer(), Some(2));
    assert_eq!(
        parsed["backlog"]["default_labels"]
            .as_array()
            .map(|labels| labels
                .iter()
                .filter_map(toml::Value::as_str)
                .collect::<Vec<_>>()),
        Some(vec!["platform", "cli"])
    );
    assert_eq!(
        parsed["backlog"]["velocity_defaults"]["project"].as_str(),
        Some("MetaStack CLI")
    );
    assert_eq!(
        parsed["backlog"]["velocity_defaults"]["state"].as_str(),
        Some("Todo")
    );
    assert_eq!(
        parsed["backlog"]["velocity_defaults"]["auto_assign"].as_str(),
        Some("viewer")
    );
    assert!(!saved.contains("[agents.commands.codex]"));

    Ok(())
}

#[test]
fn config_persists_merge_validation_repair_attempt_limit() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--merge-validation-repair-attempts",
            "8",
        ])
        .assert()
        .success();

    let saved = fs::read_to_string(config_path)?;
    assert!(saved.contains("[merge]"));
    assert!(saved.contains("validation_repair_attempts = 8"));

    Ok(())
}

#[test]
fn config_rejects_zero_merge_validation_repair_attempt_limit() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--merge-validation-repair-attempts",
            "0",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "merge validation repair attempt limit must be at least 1; got 0",
        ));

    Ok(())
}

#[test]
fn config_rejects_invalid_builtin_reasoning_for_selected_model() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    fs::write(
        &config_path,
        r#"[agents]
default_agent = "claude"
default_model = "haiku"
"#,
    )?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--default-reasoning",
            "xhigh",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "supported reasoning: low, medium, high, max",
        ));

    Ok(())
}

#[test]
fn setup_rejects_ambiguous_project_names() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "linear-token"
api_url = "{api_url}"
team = "MET"
"#
        )
        .as_str(),
    )?;

    let projects_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "linear-token")
            .body_includes("query Projects");
        then.status(200).json_body(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-1",
                        "name": "MetaStack CLI",
                        "description": "Primary",
                        "url": "https://linear.app/project/project-1",
                        "progress": 0.5,
                        "teams": { "nodes": [{ "id": "team-1", "key": "MET", "name": "Metastack" }] }
                    }, {
                        "id": "project-2",
                        "name": "MetaStack CLI",
                        "description": "Duplicate",
                        "url": "https://linear.app/project/project-2",
                        "progress": 0.5,
                        "teams": { "nodes": [{ "id": "team-1", "key": "MET", "name": "Metastack" }] }
                    }]
                }
            }
        }));
    });

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "setup",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--team",
            "MET",
            "--project",
            "MetaStack CLI",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "matched multiple Linear projects for team `MET`",
        ))
        .stderr(predicate::str::contains("project-1"))
        .stderr(predicate::str::contains("project-2"));

    projects_mock.assert();
    Ok(())
}

#[test]
fn setup_rejects_missing_project_names() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "linear-token"
api_url = "{api_url}"
team = "MET"
"#
        )
        .as_str(),
    )?;

    let projects_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "linear-token")
            .body_includes("query Projects");
        then.status(200).json_body(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-99",
                        "name": "Other Project",
                        "description": "Other",
                        "url": "https://linear.app/project/project-99",
                        "progress": 0.1,
                        "teams": { "nodes": [{ "id": "team-1", "key": "MET", "name": "Metastack" }] }
                    }]
                }
            }
        }));
    });

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "setup",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--team",
            "MET",
            "--project",
            "MetaStack CLI",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "project `MetaStack CLI` was not found in Linear for team `MET`",
        ));

    projects_mock.assert();
    Ok(())
}

#[test]
fn repo_dependent_commands_redirect_into_onboarding_when_meta_is_missing()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    fs::create_dir_all(&repo_root)?;

    cli()
        .args([
            "plan",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--no-interactive",
            "--request",
            "Plan repo setup work",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MetaStack"))
        .stdout(predicate::str::contains("meta plan"));

    cli()
        .args(["sync", "--root", repo_root.to_string_lossy().as_ref()])
        .assert()
        .success()
        .stdout(predicate::str::contains("MetaStack"))
        .stdout(predicate::str::contains("onboarding"))
        .stdout(predicate::str::contains("meta sync"))
        .stderr(predicate::str::contains("requires repo setup").not());

    cli()
        .args(["technical", "--root", repo_root.to_string_lossy().as_ref()])
        .assert()
        .success()
        .stdout(predicate::str::contains("MetaStack"))
        .stdout(predicate::str::contains("meta technical"));

    cli()
        .args([
            "listen",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--demo",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MetaStack"))
        .stdout(predicate::str::contains("onboarding"))
        .stdout(predicate::str::contains("meta listen"))
        .stderr(predicate::str::contains("compatibility alias").not());

    Ok(())
}

#[test]
fn config_render_once_shows_global_only_dashboard() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    fs::write(
        &config_path,
        r#"[linear]
api_key = "linear-token"
team = "MET"

[agents]
default_agent = "codex"
default_model = "gpt-5.4"
default_reasoning = "medium"
"#,
    )?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--render-once",
            "--width",
            "110",
            "--height",
            "50",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Global configuration"))
        .stdout(predicate::str::contains("Meta Config"))
        .stdout(predicate::str::contains("Listen label"))
        .stdout(predicate::str::contains("Project ID"))
        .stdout(predicate::str::contains("Assignee scope"))
        .stdout(predicate::str::contains("Refresh policy"))
        .stdout(predicate::str::contains("Poll interval"))
        .stdout(predicate::str::contains("Plan follow-ups"))
        .stdout(predicate::str::contains("Plan mode"))
        .stdout(predicate::str::contains("Fast single-ticket"))
        .stdout(predicate::str::contains("Fast questions"))
        .stdout(predicate::str::contains("Plan label"))
        .stdout(predicate::str::contains("Tech label"));

    Ok(())
}

#[test]
fn first_run_interception_redirects_normal_commands_into_onboarding() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "plan",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--no-interactive",
            "--request",
            "plan something",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MetaStack"))
        .stdout(predicate::str::contains("onboarding"))
        .stdout(predicate::str::contains("meta plan"))
        .stderr(predicate::str::contains("requires repo setup").not());

    Ok(())
}

#[test]
fn first_run_interception_redirects_setup_into_onboarding() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args(["setup", "--root", repo_root.to_string_lossy().as_ref()])
        .assert()
        .success()
        .stdout(predicate::str::contains("MetaStack"))
        .stdout(predicate::str::contains("onboarding"))
        .stdout(predicate::str::contains("meta setup"))
        .stdout(predicate::str::contains("Repo setup").not());

    Ok(())
}

#[test]
fn config_replay_onboarding_renders_shared_wizard() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--replay-onboarding",
            "--render-once",
            "--width",
            "110",
            "--height",
            "32",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MetaStack"))
        .stdout(predicate::str::contains("onboarding replay"))
        .stdout(predicate::str::contains("Global configuration").not());

    Ok(())
}

#[test]
fn config_render_once_wraps_summary_and_steps_at_narrow_width() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    fs::write(
        &config_path,
        r#"[linear]
api_key = "linear-token"
team = "MetaStack Team West"

[agents]
default_agent = "codex"
default_model = "gpt-5.4"
default_reasoning = "high"
"#,
    )?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--render-once",
            "--width",
            "92",
            "--height",
            "70",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Steps"))
        .stdout(predicate::str::contains("Default reasoning"))
        .stdout(predicate::str::contains("high"))
        .stdout(predicate::str::contains("MetaStack Team West"))
        .stdout(predicate::str::contains("Summary"));

    Ok(())
}

#[test]
fn setup_render_once_covers_long_summary_values() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(repo_root.join(".metastack"))?;
    write_onboarded_config(
        &config_path,
        r#"[linear]
api_key = "linear-token"
team = "MET"

[agents]
default_agent = "codex"
default_model = "gpt-5.4"
"#,
    )?;
    fs::write(
        repo_root.join(".metastack/meta.json"),
        r#"{
  "linear": {
    "profile": null,
    "team": "MET",
    "project_id": "project-very-long-1234567890"
  },
  "agent": {
    "provider": "claude",
    "model": "opus",
    "reasoning": "high"
  },
  "listen": {
    "required_label": "agent-ticket-needing-extra-room",
    "assignment_scope": "viewer",
    "refresh_policy": "recreate_from_origin_main",
    "instructions_path": "docs/instructions/with/a/very/long/path/listen.md",
    "poll_interval_seconds": 45
  },
  "plan": {
    "interactive_follow_up_questions": 6
  },
  "issue_labels": {
    "plan": "planning-with-a-long-label",
    "technical": "engineering-with-a-long-label"
  }
}
"#,
    )?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "setup",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--render-once",
            "--width",
            "120",
            "--height",
            "40",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Meta Setup"))
        .stdout(predicate::str::contains("agent-ticket-needing-extra-room"))
        .stdout(predicate::str::contains(
            "docs/instructions/with/a/very/long/path/li",
        ))
        .stdout(predicate::str::contains("sten.md"))
        .stdout(predicate::str::contains("planning-with-a-long-label"))
        .stdout(predicate::str::contains("engineering-with-a-long-label"));

    Ok(())
}

#[test]
fn setup_render_once_keeps_sidebar_content_readable_at_narrow_width() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(repo_root.join(".metastack"))?;
    write_onboarded_config(
        &config_path,
        r#"[linear]
api_key = "linear-token"
team = "MET"

[agents]
default_agent = "codex"
default_model = "gpt-5.4"
"#,
    )?;
    fs::write(
        repo_root.join(".metastack/meta.json"),
        r#"{
  "linear": {
    "profile": "ops profile west",
    "team": "MET",
    "project_id": "project-very-long-1234567890"
  },
  "agent": {
    "provider": "codex",
    "model": "gpt-5.4",
    "reasoning": "high"
  },
  "listen": {
    "required_label": "agent ticket needing extra room",
    "assignment_scope": "viewer",
    "refresh_policy": "recreate_from_origin_main",
    "instructions_path": "docs/instructions/with/a/very/long/path/listen.md",
    "poll_interval_seconds": 45
  },
  "plan": {
    "interactive_follow_up_questions": 6
  },
  "issue_labels": {
    "plan": "planning with a long label",
    "technical": "engineering with a long label"
  }
}
"#,
    )?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "setup",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--render-once",
            "--width",
            "96",
            "--height",
            "40",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Questions"))
        .stdout(predicate::str::contains("Summary"))
        .stdout(predicate::str::contains("agent ticket needing extra room"))
        .stdout(predicate::str::contains("17. Plan label"))
        .stdout(predicate::str::contains("18. Tech label"))
        .stdout(predicate::str::contains("19. Save"));

    Ok(())
}

#[test]
fn config_json_includes_advanced_agent_routing_map() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    fs::write(
        &config_path,
        r#"[agents]
default_agent = "codex"
default_model = "gpt-5.4"

[agents.routing.families.backlog]
provider = "claude"
model = "opus"
reasoning = "high"

[agents.routing.commands."backlog.plan"]
provider = "codex"
model = "gpt-5.3-codex"
"#,
    )?;

    let output = cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&output)?;

    assert_eq!(
        parsed["app"]["agents"]["routing"]["families"]["backlog"]["provider"].as_str(),
        Some("claude")
    );
    assert_eq!(
        parsed["app"]["agents"]["routing"]["commands"]["backlog.plan"]["model"].as_str(),
        Some("gpt-5.3-codex")
    );

    Ok(())
}

#[test]
fn config_direct_route_updates_can_set_and_clear_family_and_command_overrides()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--default-agent",
            "codex",
            "--default-model",
            "gpt-5.4",
        ])
        .assert()
        .success();

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--route",
            "backlog",
            "--route-agent",
            "claude",
            "--route-model",
            "opus",
            "--route-reasoning",
            "high",
        ])
        .assert()
        .success();

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--route",
            "backlog.plan",
            "--route-agent",
            "codex",
            "--route-model",
            "gpt-5.3-codex",
        ])
        .assert()
        .success();

    let config = fs::read_to_string(&config_path)?;
    assert!(config.contains("[agents.routing.families.backlog]"));
    assert!(config.contains("[agents.routing.commands.\"backlog.plan\"]"));

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--clear-route",
            "backlog.plan",
        ])
        .assert()
        .success();

    let output = cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&output)?;
    assert_eq!(
        parsed["app"]["agents"]["routing"]["families"]["backlog"]["provider"].as_str(),
        Some("claude")
    );
    assert!(
        parsed["app"]["agents"]["routing"]["commands"]
            .get("backlog.plan")
            .is_none()
    );

    Ok(())
}

#[test]
fn config_render_once_can_show_dedicated_advanced_routing_dashboard() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    fs::write(
        &config_path,
        r#"[agents]
default_agent = "codex"
default_model = "gpt-5.4"

[agents.routing.families.backlog]
provider = "claude"
model = "opus"
"#,
    )?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--advanced-routing",
            "--render-once",
            "--width",
            "140",
            "--height",
            "40",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Advanced Agent Routing"))
        .stdout(predicate::str::contains("Family: backlog"))
        .stdout(predicate::str::contains("Command: backlog.plan"))
        .stdout(predicate::str::contains("Effective routes"));

    Ok(())
}

#[test]
fn config_json_rejects_invalid_route_keys_from_manual_toml_edits() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    fs::write(
        &config_path,
        r#"[agents]
default_agent = "codex"

[agents.routing.commands.backlogoops]
provider = "claude"
"#,
    )?;

    let assert = cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "runtime",
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--json",
        ])
        .assert()
        .failure();

    let payload: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout)?;
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["command"], "runtime.config");
    assert_eq!(payload["error"]["code"], "configuration_error");
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("invalid")
    );
    assert_eq!(
        payload["error"]["context"][0],
        "unknown agent command route key `backlogoops`; supported keys: backlog.plan, backlog.improve, backlog.split, context.scan, context.reload, linear.issues.refine, agents.listen, agents.workflows.run, runtime.cron.prompt, merge.run"
    );

    Ok(())
}
