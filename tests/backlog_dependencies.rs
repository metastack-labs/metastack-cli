#![allow(dead_code, unused_imports)]

include!("support/common.rs");

fn write_onboarded_config(config_path: &Path) -> Result<(), Box<dyn Error>> {
    fs::write(config_path, "[onboarding]\ncompleted = true\n")?;
    Ok(())
}

fn write_backlog_item(
    repo_root: &Path,
    slug: &str,
    identifier: &str,
    title: &str,
    index_body: &str,
) -> Result<(), Box<dyn Error>> {
    let issue_dir = repo_root.join(".metastack/backlog").join(slug);
    fs::create_dir_all(&issue_dir)?;
    fs::write(
        issue_dir.join(".linear.json"),
        serde_json::to_string_pretty(&json!({
            "issue_id": format!("issue-{slug}"),
            "identifier": identifier,
            "title": title,
            "url": format!("https://linear.app/issues/{identifier}"),
            "team_key": "MET",
            "project_id": "project-1",
            "project_name": "MetaStack CLI",
            "managed_files": []
        }))?,
    )?;
    fs::write(
        issue_dir.join("index.md"),
        format!("# {title}\n\n{index_body}\n"),
    )?;
    Ok(())
}

#[test]
fn backlog_dependencies_json_is_deterministic_from_local_packets() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path)?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  }
}
"#,
    )?;

    write_backlog_item(
        &repo_root,
        "met-11",
        "MET-11",
        "Platform: foundation",
        "Ready to land independently.",
    )?;
    write_backlog_item(
        &repo_root,
        "met-12",
        "MET-12",
        "Platform: polish",
        "Related: MET-11",
    )?;
    write_backlog_item(
        &repo_root,
        "met-10",
        "MET-10",
        "Platform: command surface",
        "Blocked by MET-11\nRelated: MET-12",
    )?;

    let first = cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "backlog",
            "dependencies",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let second = cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "backlog",
            "dependencies",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(
        first, second,
        "identical inputs should produce identical output"
    );

    let payload: serde_json::Value = serde_json::from_slice(&first)?;
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["command"], "backlog.dependencies");
    assert_eq!(
        payload["result"]["rollout_order"][0],
        json!(["MET-11", "MET-12"])
    );
    assert_eq!(payload["result"]["rollout_order"][1], json!(["MET-10"]));
    assert!(
        payload["result"]["proposals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|proposal| {
                proposal["kind"] == "blockedBy"
                    && proposal["issue"] == "MET-10"
                    && proposal["related_issue"] == "MET-11"
            })
    );
    assert!(
        payload["result"]["proposals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|proposal| {
                proposal["kind"] == "related"
                    && proposal["issue"] == "MET-10"
                    && proposal["related_issue"] == "MET-12"
            })
    );

    Ok(())
}

#[test]
fn backlog_dependencies_ignores_legacy_self_parent_metadata() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path)?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  }
}
"#,
    )?;

    write_backlog_item(
        &repo_root,
        "met-7",
        "MET-7",
        "Dependency analysis root ticket",
        "Ready to land independently.",
    )?;

    let metadata_path = repo_root.join(".metastack/backlog/met-7/.linear.json");
    fs::write(
        metadata_path,
        serde_json::to_string_pretty(&json!({
            "issue_id": "issue-met-7",
            "identifier": "MET-7",
            "title": "Dependency analysis root ticket",
            "url": "https://linear.app/issues/MET-7",
            "team_key": "MET",
            "project_id": "project-1",
            "project_name": "MetaStack CLI",
            "parent_id": "issue-met-7",
            "parent_identifier": "MET-7",
            "managed_files": []
        }))?,
    )?;

    let output = cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "backlog",
            "dependencies",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: serde_json::Value = serde_json::from_slice(&output)?;
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["result"]["warnings"], json!([]));
    assert_eq!(
        payload["result"]["items"][0]["parent_identifier"],
        serde_json::Value::Null
    );

    Ok(())
}

#[test]
fn backlog_dependencies_warns_on_parent_cycles() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path)?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  }
}
"#,
    )?;

    write_backlog_item(
        &repo_root,
        "met-30",
        "MET-30",
        "Dependency: parent loop A",
        "Parent: MET-31",
    )?;
    write_backlog_item(
        &repo_root,
        "met-31",
        "MET-31",
        "Dependency: parent loop B",
        "Parent: MET-30",
    )?;

    let output = cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "backlog",
            "dependencies",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: serde_json::Value = serde_json::from_slice(&output)?;
    let warnings = payload["result"]["warnings"]
        .as_array()
        .ok_or("warnings should be an array")?;

    assert!(
        warnings.iter().any(|warning| {
            warning
                .as_str()
                .is_some_and(|value| value.contains("parent cycle: MET-30 -> MET-31 -> MET-30"))
        }),
        "expected parent cycle warning, got {warnings:?}"
    );
    assert_eq!(payload["result"]["changes"], json!([]));

    Ok(())
}

#[test]
fn backlog_dependencies_apply_creates_blocker_relation_in_linear() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path)?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  }
}
"#,
    )?;
    write_backlog_item(
        &repo_root,
        "met-20",
        "MET-20",
        "Dependency: core",
        "Ready first.",
    )?;
    write_backlog_item(
        &repo_root,
        "met-21",
        "MET-21",
        "Dependency: command",
        "Blocked by MET-20",
    )?;

    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let issue_20 = issue_node(
        "issue-20",
        "MET-20",
        "Dependency: core",
        "Ready first.",
        "state-1",
        "Todo",
    );
    let issue_21 = issue_node(
        "issue-21",
        "MET-21",
        "Dependency: command",
        "Blocked by MET-20",
        "state-1",
        "Todo",
    );

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues")
            .body_includes("MET");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [issue_21.clone(), issue_20.clone()],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query IssueDependencies")
            .body_includes("\"id\":\"issue-20\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": {
                    "id": "issue-20",
                    "identifier": "MET-20",
                    "title": "Dependency: core",
                    "description": "Ready first.",
                    "url": "https://linear.app/issues/MET-20",
                    "priority": 2,
                    "estimate": null,
                    "updatedAt": "2026-03-21T10:00:00Z",
                    "team": { "id": "team-1", "key": "MET", "name": "Metastack" },
                    "project": { "id": "project-1", "name": "MetaStack CLI" },
                    "assignee": null,
                    "labels": { "nodes": [] },
                    "comments": { "nodes": [] },
                    "state": { "id": "state-1", "name": "Todo", "type": "unstarted" },
                    "attachments": { "nodes": [] },
                    "parent": null,
                    "children": { "nodes": [] },
                    "relations": { "nodes": [] },
                    "inverseRelations": { "nodes": [] }
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query IssueDependencies")
            .body_includes("\"id\":\"issue-21\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": {
                    "id": "issue-21",
                    "identifier": "MET-21",
                    "title": "Dependency: command",
                    "description": "Blocked by MET-20",
                    "url": "https://linear.app/issues/MET-21",
                    "priority": 2,
                    "estimate": null,
                    "updatedAt": "2026-03-21T10:00:00Z",
                    "team": { "id": "team-1", "key": "MET", "name": "Metastack" },
                    "project": { "id": "project-1", "name": "MetaStack CLI" },
                    "assignee": null,
                    "labels": { "nodes": [] },
                    "comments": { "nodes": [] },
                    "state": { "id": "state-1", "name": "Todo", "type": "unstarted" },
                    "attachments": { "nodes": [] },
                    "parent": null,
                    "children": { "nodes": [] },
                    "relations": { "nodes": [] },
                    "inverseRelations": { "nodes": [] }
                }
            }
        }));
    });

    let relation_create = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssueRelation")
            .body_includes("\"type\":\"blocks\"")
            .body_includes("\"issueId\":\"issue-20\"")
            .body_includes("\"relatedIssueId\":\"issue-21\"");
        then.status(200).json_body(json!({
            "data": {
                "issueRelationCreate": {
                    "success": true,
                    "issueRelation": {
                        "id": "relation-1",
                        "type": "blocks",
                        "issue": {
                            "id": "issue-20",
                            "identifier": "MET-20",
                            "title": "Dependency: core",
                            "url": "https://linear.app/issues/MET-20",
                            "description": "Ready first."
                        },
                        "relatedIssue": {
                            "id": "issue-21",
                            "identifier": "MET-21",
                            "title": "Dependency: command",
                            "url": "https://linear.app/issues/MET-21",
                            "description": "Blocked by MET-20"
                        }
                    }
                }
            }
        }));
    });

    let output = cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "backlog",
            "dependencies",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--api-key",
            "test-token",
            "--api-url",
            api_url.as_str(),
            "--fetch",
            "--apply",
            "--yes",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    relation_create.assert();

    let payload: serde_json::Value = serde_json::from_slice(&output)?;
    assert_eq!(payload["status"], "ok");
    assert!(
        payload["result"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| {
                change["action"] == "create"
                    && change["kind"] == "blockedBy"
                    && change["issue"] == "MET-21"
                    && change["related_issue"] == "MET-20"
            })
    );

    Ok(())
}

#[test]
fn backlog_dependencies_apply_yes_prints_preview_before_mutating() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path)?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  }
}
"#,
    )?;
    write_backlog_item(
        &repo_root,
        "met-40",
        "MET-40",
        "Dependency: core",
        "Ready first.",
    )?;
    write_backlog_item(
        &repo_root,
        "met-41",
        "MET-41",
        "Dependency: command",
        "Blocked by MET-40",
    )?;

    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let issue_40 = issue_node(
        "issue-40",
        "MET-40",
        "Dependency: core",
        "Ready first.",
        "state-1",
        "Todo",
    );
    let issue_41 = issue_node(
        "issue-41",
        "MET-41",
        "Dependency: command",
        "Blocked by MET-40",
        "state-1",
        "Todo",
    );

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues")
            .body_includes("MET");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [issue_41.clone(), issue_40.clone()],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query IssueDependencies")
            .body_includes("\"id\":\"issue-40\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": {
                    "id": "issue-40",
                    "identifier": "MET-40",
                    "title": "Dependency: core",
                    "description": "Ready first.",
                    "url": "https://linear.app/issues/MET-40",
                    "priority": 2,
                    "estimate": null,
                    "updatedAt": "2026-03-21T10:00:00Z",
                    "team": { "id": "team-1", "key": "MET", "name": "Metastack" },
                    "project": { "id": "project-1", "name": "MetaStack CLI" },
                    "assignee": null,
                    "labels": { "nodes": [] },
                    "comments": { "nodes": [] },
                    "state": { "id": "state-1", "name": "Todo", "type": "unstarted" },
                    "attachments": { "nodes": [] },
                    "parent": null,
                    "children": { "nodes": [] },
                    "relations": { "nodes": [] },
                    "inverseRelations": { "nodes": [] }
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query IssueDependencies")
            .body_includes("\"id\":\"issue-41\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": {
                    "id": "issue-41",
                    "identifier": "MET-41",
                    "title": "Dependency: command",
                    "description": "Blocked by MET-40",
                    "url": "https://linear.app/issues/MET-41",
                    "priority": 2,
                    "estimate": null,
                    "updatedAt": "2026-03-21T10:00:00Z",
                    "team": { "id": "team-1", "key": "MET", "name": "Metastack" },
                    "project": { "id": "project-1", "name": "MetaStack CLI" },
                    "assignee": null,
                    "labels": { "nodes": [] },
                    "comments": { "nodes": [] },
                    "state": { "id": "state-1", "name": "Todo", "type": "unstarted" },
                    "attachments": { "nodes": [] },
                    "parent": null,
                    "children": { "nodes": [] },
                    "relations": { "nodes": [] },
                    "inverseRelations": { "nodes": [] }
                }
            }
        }));
    });

    let relation_create = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssueRelation")
            .body_includes("\"type\":\"blocks\"")
            .body_includes("\"issueId\":\"issue-40\"")
            .body_includes("\"relatedIssueId\":\"issue-41\"");
        then.status(200).json_body(json!({
            "data": {
                "issueRelationCreate": {
                    "success": true,
                    "issueRelation": {
                        "id": "relation-1",
                        "type": "blocks",
                        "issue": {
                            "id": "issue-40",
                            "identifier": "MET-40",
                            "title": "Dependency: core",
                            "url": "https://linear.app/issues/MET-40",
                            "description": "Ready first."
                        },
                        "relatedIssue": {
                            "id": "issue-41",
                            "identifier": "MET-41",
                            "title": "Dependency: command",
                            "url": "https://linear.app/issues/MET-41",
                            "description": "Blocked by MET-40"
                        }
                    }
                }
            }
        }));
    });

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "backlog",
            "dependencies",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--api-key",
            "test-token",
            "--api-url",
            api_url.as_str(),
            "--fetch",
            "--apply",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "meta backlog dependencies apply preview",
        ))
        .stdout(predicate::str::contains(
            "Applying relationship changes because `--yes` was provided.",
        ))
        .stdout(predicate::str::contains("Applied relationship changes:"));

    relation_create.assert();

    Ok(())
}
