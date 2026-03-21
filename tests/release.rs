#![allow(dead_code, unused_imports)]

include!("support/common.rs");

fn write_backlog_issue(
    repo_root: &Path,
    identifier: &str,
    title: &str,
    extra_note: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let issue_dir = repo_root.join(".metastack/backlog").join(identifier);
    fs::create_dir_all(&issue_dir)?;
    fs::write(
        issue_dir.join(".linear.json"),
        format!(
            r#"{{
  "issue_id": "issue-{identifier}",
  "identifier": "{identifier}",
  "title": "{title}",
  "url": "https://linear.app/issues/{identifier}",
  "team_key": "MET"
}}
"#
        ),
    )?;
    fs::write(
        issue_dir.join("index.md"),
        format!("# {title}\n\n{}\n", extra_note.unwrap_or("No extra notes.")),
    )?;
    fs::write(
        issue_dir.join("implementation.md"),
        extra_note.unwrap_or(""),
    )?;
    Ok(())
}

fn issues_payload() -> serde_json::Value {
    json!({
        "data": {
            "issues": {
                "nodes": [
                    {
                        "id": "issue-MET-10",
                        "identifier": "MET-10",
                        "title": "Lay release foundations",
                        "description": "Shared foundations.",
                        "url": "https://linear.app/issues/MET-10",
                        "priority": 3,
                        "estimate": 2.0,
                        "updatedAt": "2026-03-20T00:00:00Z",
                        "team": { "id": "team-1", "key": "MET", "name": "Metastack" },
                        "project": { "id": "project-1", "name": "MetaStack CLI" },
                        "assignee": null,
                        "labels": { "nodes": [] },
                        "state": { "id": "state-backlog", "name": "Backlog", "type": "backlog" },
                        "attachments": { "nodes": [] }
                    },
                    {
                        "id": "issue-MET-11",
                        "identifier": "MET-11",
                        "title": "Ship release batching UX",
                        "description": "Depends on MET-10 for shared foundations.",
                        "url": "https://linear.app/issues/MET-11",
                        "priority": 2,
                        "estimate": 3.0,
                        "updatedAt": "2026-03-20T00:00:00Z",
                        "team": { "id": "team-1", "key": "MET", "name": "Metastack" },
                        "project": { "id": "project-1", "name": "MetaStack CLI" },
                        "assignee": null,
                        "labels": { "nodes": [] },
                        "state": { "id": "state-backlog", "name": "Backlog", "type": "backlog" },
                        "attachments": { "nodes": [] }
                    },
                    {
                        "id": "issue-MET-12",
                        "identifier": "MET-12",
                        "title": "Add stretch automation",
                        "description": "Deferrable automation.",
                        "url": "https://linear.app/issues/MET-12",
                        "priority": 4,
                        "estimate": 1.0,
                        "updatedAt": "2026-03-20T00:00:00Z",
                        "team": { "id": "team-1", "key": "MET", "name": "Metastack" },
                        "project": { "id": "project-1", "name": "MetaStack CLI" },
                        "assignee": null,
                        "labels": { "nodes": [] },
                        "state": { "id": "state-backlog", "name": "Backlog", "type": "backlog" },
                        "attachments": { "nodes": [] }
                    }
                ],
                "pageInfo": {
                    "hasNextPage": false,
                    "endCursor": null
                }
            }
        }
    })
}

#[cfg(unix)]
#[test]
fn release_writes_local_packet_with_cut_line_and_ordering() -> Result<(), Box<dyn Error>> {
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
  }
}
"#,
    )?;
    write_backlog_issue(&repo_root, "MET-10", "Lay release foundations", None)?;
    write_backlog_issue(
        &repo_root,
        "MET-11",
        "Ship release batching UX",
        Some("This rollout depends on MET-10 before the UX can ship."),
    )?;
    write_backlog_issue(&repo_root, "MET-12", "Add stretch automation", None)?;

    fs::write(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"
"#,
        ),
    )?;

    let issues_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(issues_payload());
    });

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "backlog",
            "release",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--name",
            "sprint-1",
            "--batch-size",
            "2",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"sprint-1\""))
        .stdout(predicate::str::contains("\"included\""))
        .stdout(predicate::str::contains("\"identifier\": \"MET-10\""))
        .stdout(predicate::str::contains("\"identifier\": \"MET-11\""))
        .stdout(predicate::str::contains("\"identifier\": \"MET-12\""));

    let markdown = fs::read_to_string(
        repo_root
            .join(".metastack/releases/sprint-1")
            .join("index.md"),
    )?;
    let json = fs::read_to_string(
        repo_root
            .join(".metastack/releases/sprint-1")
            .join("plan.json"),
    )?;

    assert!(markdown.contains("## Recommended Batch"));
    assert!(markdown.contains("## Cut Line"));
    assert!(markdown.contains("after `MET-11`"));
    assert!(markdown.contains("`MET-10`"));
    assert!(markdown.contains("`MET-11`"));
    assert!(markdown.contains("`MET-12`"));
    assert!(json.contains("\"cut_line\": {"));
    assert!(json.contains("\"ends_after\": \"MET-11\""));
    assert!(json.contains("\"ordering\": [\n    \"MET-10\",\n    \"MET-11\",\n    \"MET-12\""));
    assert!(json.contains("\"above_cut_line\": true"));
    assert!(json.contains("\"above_cut_line\": false"));

    issues_mock.assert_calls(1);
    Ok(())
}

#[cfg(unix)]
#[test]
fn release_apply_updates_only_issues_above_cut_line() -> Result<(), Box<dyn Error>> {
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
  }
}
"#,
    )?;
    write_backlog_issue(&repo_root, "MET-10", "Lay release foundations", None)?;
    write_backlog_issue(
        &repo_root,
        "MET-11",
        "Ship release batching UX",
        Some("blocked by MET-10"),
    )?;
    write_backlog_issue(&repo_root, "MET-12", "Add stretch automation", None)?;

    fs::write(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"
"#,
        ),
    )?;

    let issues_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(issues_payload());
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
                                { "id": "state-backlog", "name": "Backlog", "type": "backlog" },
                                { "id": "state-todo", "name": "Todo", "type": "unstarted" }
                            ]
                        }
                    }]
                }
            }
        }));
    });
    let projects_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Projects");
        then.status(200).json_body(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-2",
                        "name": "Sprint 1",
                        "description": null,
                        "url": "https://linear.app/projects/project-2",
                        "progress": 0.2,
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
    let update_met_10 = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue")
            .body_includes("\"id\":\"issue-MET-10\"")
            .body_includes("\"projectId\":\"project-2\"")
            .body_includes("\"stateId\":\"state-todo\"");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": {
                        "id": "issue-MET-10",
                        "identifier": "MET-10",
                        "title": "Lay release foundations",
                        "description": "Shared foundations.",
                        "url": "https://linear.app/issues/MET-10",
                        "priority": 3,
                        "updatedAt": "2026-03-20T00:00:00Z",
                        "team": { "id": "team-1", "key": "MET", "name": "Metastack" },
                        "project": { "id": "project-2", "name": "Sprint 1" },
                        "labels": { "nodes": [] },
                        "comments": { "nodes": [] },
                        "state": { "id": "state-todo", "name": "Todo", "type": "unstarted" }
                    }
                }
            }
        }));
    });
    let update_met_11 = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue")
            .body_includes("\"id\":\"issue-MET-11\"")
            .body_includes("\"projectId\":\"project-2\"")
            .body_includes("\"stateId\":\"state-todo\"");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": {
                        "id": "issue-MET-11",
                        "identifier": "MET-11",
                        "title": "Ship release batching UX",
                        "description": "Depends on MET-10 for shared foundations.",
                        "url": "https://linear.app/issues/MET-11",
                        "priority": 2,
                        "updatedAt": "2026-03-20T00:00:00Z",
                        "team": { "id": "team-1", "key": "MET", "name": "Metastack" },
                        "project": { "id": "project-2", "name": "Sprint 1" },
                        "labels": { "nodes": [] },
                        "comments": { "nodes": [] },
                        "state": { "id": "state-todo", "name": "Todo", "type": "unstarted" }
                    }
                }
            }
        }));
    });

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "backlog",
            "release",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--name",
            "apply-sprint-1",
            "--batch-size",
            "2",
            "--apply",
            "--project",
            "Sprint 1",
            "--state",
            "Todo",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Applied Linear metadata to 2 issue(s).",
        ));

    let markdown = fs::read_to_string(
        repo_root
            .join(".metastack/releases/apply-sprint-1")
            .join("index.md"),
    )?;
    assert!(markdown.contains("## Applied Linear Metadata"));
    assert!(markdown.contains("MET-10, MET-11"));

    issues_mock.assert_calls(3);
    teams_mock.assert_calls(2);
    projects_mock.assert_calls(2);
    update_met_10.assert_calls(1);
    update_met_11.assert_calls(1);
    Ok(())
}

#[cfg(unix)]
#[test]
fn release_requires_at_least_two_backlog_items() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

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
    write_backlog_issue(&repo_root, "MET-10", "Lay release foundations", None)?;
    fs::write(
        &config_path,
        r#"[linear]
api_key = "token"
api_url = "http://127.0.0.1:9/graphql"
"#,
    )?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "backlog",
            "release",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--name",
            "too-small",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "not enough backlog items to build a release packet; found 1 item",
        ));

    Ok(())
}

#[cfg(unix)]
#[test]
fn release_reports_cyclic_dependency_signals_as_risks() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

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
    write_backlog_issue(
        &repo_root,
        "MET-10",
        "Lay release foundations",
        Some("blocked by MET-11"),
    )?;
    write_backlog_issue(
        &repo_root,
        "MET-11",
        "Ship release batching UX",
        Some("blocked by MET-10"),
    )?;
    fs::write(&config_path, "")?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "backlog",
            "release",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--name",
            "cycle-risk",
            "--batch-size",
            "1",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Cyclic dependency signals detected",
        ))
        .stdout(predicate::str::contains("\"ends_after\": \"MET-10\""));

    let markdown = fs::read_to_string(
        repo_root
            .join(".metastack/releases/cycle-risk")
            .join("index.md"),
    )?;
    assert!(markdown.contains("Cyclic dependency signals detected: MET-10 -> MET-11 -> MET-10."));

    Ok(())
}
