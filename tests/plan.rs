#![allow(dead_code, unused_imports)]

include!("support/common.rs");

#[test]
fn plan_help_lists_non_interactive_inputs() {
    cli()
        .args(["plan", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Compatibility alias for `meta backlog plan`",
        ))
        .stdout(predicate::str::contains("[IDENTIFIER]"))
        .stdout(predicate::str::contains("--request <REQUEST>"))
        .stdout(predicate::str::contains("--answer <ANSWERS>"))
        .stdout(predicate::str::contains("--velocity"))
        .stdout(predicate::str::contains("--no-interactive"));
}

#[cfg(unix)]
fn write_reshape_agent_stub(
    bin_dir: &Path,
    stub_dir: &Path,
    response: &str,
) -> Result<(), Box<dyn Error>> {
    let stub_path = bin_dir.join("plan-agent-stub");
    fs::write(
        &stub_path,
        format!(
            r#"#!/bin/sh
count_file="$TEST_OUTPUT_DIR/count.txt"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
cat > "$TEST_OUTPUT_DIR/payload-$count.txt"
printf '%s' '{response}'
"#
        ),
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;
    let _ = stub_dir;
    Ok(())
}

#[cfg(unix)]
fn write_reshape_config(
    config_path: &Path,
    api_url: &str,
    include_agent: bool,
) -> Result<(), Box<dyn Error>> {
    let config = if include_agent {
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "stub"

[agents.commands.stub]
command = "plan-agent-stub"
transport = "stdin"
"#
        )
    } else {
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"
"#
        )
    };
    fs::write(config_path, config)?;
    Ok(())
}

#[cfg(unix)]
fn reshape_issue_list_node() -> serde_json::Value {
    json!({
        "id": "issue-reshape",
        "identifier": "ENG-10144",
        "title": "Technical: old reshape ticket",
        "description": "The current ticket body needs structure.",
        "url": "https://linear.app/issues/ENG-10144",
        "priority": 2,
        "updatedAt": "2026-03-19T12:00:00Z",
        "team": {
            "id": "team-1",
            "key": "ENG",
            "name": "Engineering"
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
    })
}

#[cfg(unix)]
fn reshape_issue_detail(existing_workpad: bool) -> serde_json::Value {
    let mut comments = vec![json!({
        "id": "comment-context",
        "body": "Need to preserve project and labels.",
        "createdAt": "2026-03-18T15:00:00Z",
        "user": {
            "name": "Reviewer"
        },
        "resolvedAt": null
    })];
    if existing_workpad {
        comments.push(json!({
            "id": "comment-active",
            "body": "## Codex Workpad\n\nExisting audit note",
            "createdAt": "2026-03-18T15:30:00Z",
            "user": {
                "name": "Harness"
            },
            "resolvedAt": null
        }));
    }

    json!({
        "id": "issue-reshape",
        "identifier": "ENG-10144",
        "title": "Technical: old reshape ticket",
        "description": "Current description.\n\nIt is too loose and missing acceptance criteria.",
        "url": "https://linear.app/issues/ENG-10144",
        "priority": 2,
        "updatedAt": "2026-03-19T12:00:00Z",
        "team": {
            "id": "team-1",
            "key": "ENG",
            "name": "Engineering"
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
                "name": "tech"
            }]
        },
        "comments": {
            "nodes": comments,
            "pageInfo": {
                "hasNextPage": false,
                "endCursor": null
            }
        },
        "state": {
            "id": "state-2",
            "name": "In Progress",
            "type": "started"
        },
        "attachments": {
            "nodes": [{
                "id": "attachment-1",
                "title": "current-screenshot.png",
                "url": "https://example.com/current-screenshot.png",
                "sourceType": "upload",
                "metadata": {
                    "kind": "image"
                }
            }]
        },
        "parent": null,
        "children": {
            "nodes": []
        }
    })
}

#[cfg(unix)]
fn reshape_team_payload() -> serde_json::Value {
    json!({
        "data": {
            "teams": {
                "nodes": [{
                    "id": "team-1",
                    "key": "ENG",
                    "name": "Engineering",
                    "states": {
                        "nodes": [{
                            "id": "state-backlog",
                            "name": "Backlog",
                            "type": "backlog"
                        }, {
                            "id": "state-2",
                            "name": "In Progress",
                            "type": "started"
                        }]
                    }
                }]
            }
        }
    })
}

#[cfg(unix)]
#[test]
fn plan_reshape_velocity_updates_existing_issue_and_workpad() -> Result<(), Box<dyn Error>> {
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
    "team": "ENG",
    "project_id": "project-1"
  }
}
"#,
    )?;
    write_reshape_config(&config_path, &api_url, true)?;
    write_reshape_agent_stub(
        &bin_dir,
        &stub_dir,
        r#"{"summary":"Rewrite the ticket in place with clearer scope and acceptance criteria.","title":"Plan reshape existing Linear tickets in place","description":"Improve the current planning ticket by preserving its intent, tightening the scope, and making the acceptance criteria explicit.","acceptance_criteria":["`meta backlog plan ENG-10144` updates the existing issue instead of creating a new one","Interactive runs preview the diff and `--velocity` auto-applies the reshape"]}"#,
    )?;

    let issues_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [reshape_issue_list_node()]
                }
            }
        }));
    });
    let issue_detail_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-reshape\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": reshape_issue_detail(true)
            }
        }));
    });
    let teams_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(reshape_team_payload());
    });
    let update_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue")
            .body_includes("\"id\":\"issue-reshape\"")
            .body_includes("\"title\":\"Plan reshape existing Linear tickets in place\"")
            .body_includes("## Acceptance Criteria")
            .body_includes("updates the existing issue instead of creating a new one");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": {
                        "id": "issue-reshape",
                        "identifier": "ENG-10144",
                        "title": "Plan reshape existing Linear tickets in place",
                        "description": "# Plan reshape existing Linear tickets in place\n\nImprove the current planning ticket by preserving its intent, tightening the scope, and making the acceptance criteria explicit.\n\n## Acceptance Criteria\n\n- `meta backlog plan ENG-10144` updates the existing issue instead of creating a new one\n- Interactive runs preview the diff and `--velocity` auto-applies the reshape",
                        "url": "https://linear.app/issues/ENG-10144",
                        "priority": 2,
                        "updatedAt": "2026-03-19T13:00:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "ENG",
                            "name": "Engineering"
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
    let update_comment_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateComment")
            .body_includes("\"id\":\"comment-active\"")
            .body_includes(
                "Rewrite the ticket in place with clearer scope and acceptance criteria.",
            )
            .body_includes("Local `.metastack/backlog/` files were not modified");
        then.status(200).json_body(json!({
            "data": {
                "commentUpdate": {
                    "success": true,
                    "comment": {
                        "id": "comment-active",
                        "body": "## Codex Workpad\n\nupdated",
                        "resolvedAt": null
                    }
                }
            }
        }));
    });
    let create_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": reshape_issue_list_node()
                }
            }
        }));
    });
    let create_comment_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateComment");
        then.status(200).json_body(json!({
            "data": {
                "commentCreate": {
                    "success": true,
                    "comment": {
                        "id": "comment-created",
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
            "plan",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--no-interactive",
            "--velocity",
            "ENG-10144",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Reshaped ENG-10144 in place"));

    issues_mock.assert_calls(2);
    issue_detail_mock.assert_calls(1);
    teams_mock.assert_calls(1);
    update_issue_mock.assert_calls(1);
    update_comment_mock.assert_calls(1);
    create_issue_mock.assert_calls(0);
    create_comment_mock.assert_calls(0);

    let payload = fs::read_to_string(stub_dir.join("payload-1.txt"))?;
    assert!(payload.contains("\"identifier\": \"ENG-10144\""));
    assert!(payload.contains("current-screenshot.png"));
    assert!(payload.contains("Need to preserve project and labels."));
    assert!(payload.contains("Preserve the issue's intent"));
    assert!(!repo_root.join(".metastack/backlog/ENG-10144").exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn plan_reshape_interactive_preview_requires_confirmation() -> Result<(), Box<dyn Error>> {
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
    "team": "ENG",
    "project_id": "project-1"
  }
}
"#,
    )?;
    write_reshape_config(&config_path, &api_url, true)?;
    write_reshape_agent_stub(
        &bin_dir,
        &stub_dir,
        r#"{"summary":"Interactive reshape proof.","title":"Plan reshape existing Linear tickets in place","description":"Add a deterministic diff preview before updating the existing ticket.","acceptance_criteria":["Interactive reshape previews the current and replacement ticket body"]}"#,
    )?;

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [reshape_issue_list_node()]
                }
            }
        }));
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-reshape\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": reshape_issue_detail(false)
            }
        }));
    });
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(reshape_team_payload());
    });
    let update_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue")
            .body_includes("\"id\":\"issue-reshape\"")
            .body_includes("\"title\":\"Plan reshape existing Linear tickets in place\"");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": {
                        "id": "issue-reshape",
                        "identifier": "ENG-10144",
                        "title": "Plan reshape existing Linear tickets in place",
                        "description": "# Plan reshape existing Linear tickets in place\n\nAdd a deterministic diff preview before updating the existing ticket.\n\n## Acceptance Criteria\n\n- Interactive reshape previews the current and replacement ticket body",
                        "url": "https://linear.app/issues/ENG-10144",
                        "priority": 2,
                        "updatedAt": "2026-03-19T13:05:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "ENG",
                            "name": "Engineering"
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
            .body_includes("Interactive reshape proof.");
        then.status(200).json_body(json!({
            "data": {
                "commentCreate": {
                    "success": true,
                    "comment": {
                        "id": "comment-created",
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
        .write_stdin("a\n")
        .args([
            "plan",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "ENG-10144",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Description diff:"))
        .stdout(predicate::str::contains("Choose [a]pply or [c]ancel"))
        .stdout(predicate::str::contains("--- linear/current-description"))
        .stdout(predicate::str::contains("Reshaped ENG-10144 in place"));

    update_issue_mock.assert_calls(1);
    create_comment_mock.assert_calls(1);

    Ok(())
}

#[cfg(unix)]
#[test]
fn plan_reshape_missing_issue_fails_without_creating_new_ticket() -> Result<(), Box<dyn Error>> {
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
    "team": "ENG",
    "project_id": "project-1"
  }
}
"#,
    )?;
    write_reshape_config(&config_path, &api_url, false)?;

    let issues_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": []
                }
            }
        }));
    });
    let create_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": reshape_issue_list_node()
                }
            }
        }));
    });

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "plan",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--no-interactive",
            "--velocity",
            "ENG-10144",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "issue `ENG-10144` was not found in Linear",
        ));

    issues_mock.assert_calls(1);
    create_issue_mock.assert_calls(0);

    Ok(())
}

#[cfg(unix)]
#[test]
fn plan_no_interactive_creates_multiple_backlog_issues_from_agent_output()
-> Result<(), Box<dyn Error>> {
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
  }
}
"#,
    )?;
    fs::write(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "stub"

[agents.commands.stub]
command = "plan-agent-stub"
transport = "stdin"
"#,
        ),
    )?;

    let stub_path = bin_dir.join("plan-agent-stub");
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
cat > "$TEST_OUTPUT_DIR/payload-$count.txt"
if [ "$count" -eq 1 ]; then
  printf '%s' '{"questions":["Which repo areas are in scope?","Should this ship as one ticket or multiple?"]}'
else
  printf '%s' '{"summary":"Split the work into command wiring and dashboard flow.","issues":[{"title":"Add the meta plan command","description":"Introduce the top-level command and the deterministic non-interactive planning path.","acceptance_criteria":["`meta plan --help` works","Non-interactive planning can create backlog issues"],"priority":2},{"title":"Build the planning dashboard","description":"Capture the request, follow-up answers, and ticket review in ratatui.","acceptance_criteria":["TTY planning runs show request, questions, and review states","The dashboard confirms multi-issue creation before writing to Linear"],"priority":3}]}'
fi
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

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
                                    "id": "state-todo",
                                    "name": "Todo",
                                    "type": "unstarted"
                                }
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
    let issue_labels_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query IssueLabels");
        then.status(200).json_body(json!({
            "data": {
                "issueLabels": {
                    "nodes": [{
                        "id": "label-plan",
                        "name": "plan"
                    }]
                }
            }
        }));
    });
    let create_command_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue")
            .body_includes("\"title\":\"Add the meta plan command\"")
            .body_includes("\"projectId\":\"project-1\"")
            .body_includes("\"stateId\":\"state-backlog\"")
            .body_includes("\"labelIds\":[\"label-plan\"]");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": {
                        "id": "issue-41",
                        "identifier": "MET-41",
                        "title": "Add the meta plan command",
                        "description": "Introduce the top-level command and the deterministic non-interactive planning path.",
                        "url": "https://linear.app/issues/41",
                        "priority": 2,
                        "updatedAt": "2026-03-14T18:00:00Z",
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
                    }
                }
            }
        }));
    });

    let create_dashboard_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue")
            .body_includes("\"title\":\"Build the planning dashboard\"")
            .body_includes("\"projectId\":\"project-1\"")
            .body_includes("\"stateId\":\"state-backlog\"")
            .body_includes("\"labelIds\":[\"label-plan\"]");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": {
                        "id": "issue-42",
                        "identifier": "MET-42",
                        "title": "Build the planning dashboard",
                        "description": "Capture the request, follow-up answers, and ticket review in ratatui.",
                        "url": "https://linear.app/issues/42",
                        "priority": 3,
                        "updatedAt": "2026-03-14T18:01:00Z",
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
            "plan",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--no-interactive",
            "--request",
            "Plan the new meta plan workflow for backlog automation",
            "--answer",
            "Command wiring and Linear ticket creation",
            "--answer",
            "Split it into multiple tickets",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created 2 backlog issue(s):"))
        .stdout(predicate::str::contains("MET-41"))
        .stdout(predicate::str::contains("MET-42"));

    teams_mock.assert_calls(2);
    projects_mock.assert_calls(2);
    issue_labels_mock.assert_calls(2);
    create_command_mock.assert_calls(1);
    create_dashboard_mock.assert_calls(1);

    let first_payload = fs::read_to_string(stub_dir.join("payload-1.txt"))?;
    assert!(first_payload.contains("Plan the new meta plan workflow"));
    assert!(first_payload.contains("Target repository:"));
    assert!(first_payload.contains("Default scope: the full repository rooted at"));
    assert!(first_payload.contains("Ask at most 3 concise follow-up questions"));
    assert!(!first_payload.contains("You are helping plan backlog work for the MetaStack CLI"));

    let second_payload = fs::read_to_string(stub_dir.join("payload-2.txt"))?;
    assert!(second_payload.contains("Follow-up answers"));
    assert!(second_payload.contains("Command wiring and Linear ticket creation"));
    assert!(second_payload.contains("Split it into multiple tickets"));
    assert!(second_payload.contains("Linear backlog issues for this repository directory only"));
    assert!(!second_payload.contains("revising a backlog ticket plan for the MetaStack CLI"));

    let first_issue_dir = repo_root.join(".metastack/backlog/MET-41");
    let first_index = fs::read_to_string(first_issue_dir.join("index.md"))?;
    let first_readme = fs::read_to_string(first_issue_dir.join("README.md"))?;
    let first_checklist = fs::read_to_string(first_issue_dir.join("checklist.md"))?;
    let first_proposed_prs = fs::read_to_string(first_issue_dir.join("proposed-prs.md"))?;
    assert!(first_issue_dir.is_dir());
    assert!(first_issue_dir.join(".linear.json").is_file());
    assert!(first_issue_dir.join("README.md").is_file());
    assert!(first_issue_dir.join("checklist.md").is_file());
    assert!(first_issue_dir.join("contacts.md").is_file());
    assert!(first_issue_dir.join("decisions.md").is_file());
    assert!(first_issue_dir.join("implementation.md").is_file());
    assert!(first_issue_dir.join("proposed-prs.md").is_file());
    assert!(first_issue_dir.join("risks.md").is_file());
    assert!(first_issue_dir.join("specification.md").is_file());
    assert!(first_issue_dir.join("validation.md").is_file());
    assert!(first_issue_dir.join("context/README.md").is_file());
    assert!(
        first_issue_dir
            .join("context/context-note-template.md")
            .is_file()
    );
    assert!(first_issue_dir.join("tasks/README.md").is_file());
    assert!(
        first_issue_dir
            .join("tasks/workstream-template.md")
            .is_file()
    );
    assert!(first_issue_dir.join("artifacts/README.md").is_file());
    assert!(
        first_issue_dir
            .join("artifacts/artifact-template.md")
            .is_file()
    );
    assert!(first_index.contains("# Add the meta plan command"));
    assert!(first_index.contains("Introduce the top-level command"));
    assert!(first_index.contains("## Acceptance Criteria"));
    assert!(first_index.contains("Non-interactive planning can create backlog issues"));
    assert!(!first_index.contains("## Parent Issue"));
    assert!(!first_index.contains("Standalone backlog item"));
    assert!(!first_index.contains("## Context"));
    assert!(!first_index.contains("_Generated by `meta plan`._"));
    assert!(first_readme.contains("Add the meta plan command"));
    assert!(!first_readme.contains("{{BACKLOG_TITLE}}"));
    assert!(first_checklist.contains("Last updated: "));
    assert!(!first_checklist.contains("{{TODAY}}"));
    assert!(first_proposed_prs.contains("add-the-meta-plan-command-01"));
    assert!(!first_proposed_prs.contains("{{BACKLOG_SLUG}}"));

    let second_issue_dir = repo_root.join(".metastack/backlog/MET-42");
    let second_index = fs::read_to_string(second_issue_dir.join("index.md"))?;
    assert!(second_issue_dir.is_dir());
    assert!(second_issue_dir.join(".linear.json").is_file());
    assert!(
        second_issue_dir
            .join("tasks/workstream-template.md")
            .is_file()
    );
    assert!(second_index.contains("# Build the planning dashboard"));
    assert!(second_index.contains("Capture the request, follow-up answers, and ticket review"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn plan_no_interactive_resolves_repo_meta_project_name_to_project_id() -> Result<(), Box<dyn Error>>
{
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
    "project_id": "MetaStack CLI"
  }
}
"#,
    )?;
    fs::write(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "stub"

[agents.commands.stub]
command = "plan-agent-stub"
transport = "stdin"
"#,
        ),
    )?;

    let stub_path = bin_dir.join("plan-agent-stub");
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
cat > "$TEST_OUTPUT_DIR/payload-$count.txt"
if [ "$count" -eq 1 ]; then
  printf '%s' '{"questions":[]}'
else
  printf '%s' '{"summary":"Create one ticket.","issues":[{"title":"Fix the meta plan command","description":"Resolve repo-scoped project defaults before creating backlog issues.","acceptance_criteria":["`meta plan` resolves repo defaults stored as project names"],"priority":2}]}'
fi
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

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
                            "nodes": [{
                                "id": "state-backlog",
                                "name": "Backlog",
                                "type": "backlog"
                            }]
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
    let issue_labels_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query IssueLabels");
        then.status(200).json_body(json!({
            "data": {
                "issueLabels": {
                    "nodes": [{
                        "id": "label-plan",
                        "name": "plan"
                    }]
                }
            }
        }));
    });
    let create_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue")
            .body_includes("\"title\":\"Fix the meta plan command\"")
            .body_includes("\"projectId\":\"project-1\"")
            .body_includes("\"stateId\":\"state-backlog\"")
            .body_includes("\"labelIds\":[\"label-plan\"]");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": {
                        "id": "issue-42",
                        "identifier": "MET-42",
                        "title": "Fix the meta plan command",
                        "description": "Resolve repo-scoped project defaults before creating backlog issues.",
                        "url": "https://linear.app/issues/42",
                        "priority": 2,
                        "updatedAt": "2026-03-14T18:10:00Z",
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
            "plan",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--no-interactive",
            "--request",
            "Fix the meta plan command so it can create backlog tickets",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created 1 backlog issue(s):"))
        .stdout(predicate::str::contains("MET-42"));

    teams_mock.assert_calls(1);
    projects_mock.assert_calls(1);
    issue_labels_mock.assert_calls(1);
    create_issue_mock.assert_calls(1);

    let first_payload = fs::read_to_string(stub_dir.join("payload-1.txt"))?;
    assert!(first_payload.contains("Fix the meta plan command"));
    assert!(first_payload.contains("Ask at most 3 concise follow-up questions"));

    let second_payload = fs::read_to_string(stub_dir.join("payload-2.txt"))?;
    assert!(second_payload.contains("Fix the meta plan command"));
    assert!(second_payload.contains("Follow-up answers"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn repo_agent_defaults_apply_when_cli_overrides_are_absent() -> Result<(), Box<dyn Error>> {
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
    "provider": "repo-stub",
    "model": "repo-model",
    "reasoning": "high"
  }
}
"#,
    )?;
    fs::write(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "global-stub"
default_model = "global-model"
default_reasoning = "low"

[agents.commands.global-stub]
command = "plan-agent-stub"
transport = "stdin"

[agents.commands.repo-stub]
command = "plan-agent-stub"
transport = "stdin"
"#
        ),
    )?;

    let stub_path = bin_dir.join("plan-agent-stub");
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
printf '%s' "$METASTACK_AGENT_NAME" > "$TEST_OUTPUT_DIR/agent-$count.txt"
printf '%s' "$METASTACK_AGENT_MODEL" > "$TEST_OUTPUT_DIR/model-$count.txt"
printf '%s' "$METASTACK_AGENT_REASONING" > "$TEST_OUTPUT_DIR/reasoning-$count.txt"
cat > "$TEST_OUTPUT_DIR/payload-$count.txt"
if [ "$count" -eq 1 ]; then
  printf '%s' '{"questions":[]}'
else
  printf '%s' '{"summary":"Create one ticket.","issues":[{"title":"Use repo agent defaults","description":"Ensure repo-scoped agent defaults are applied.","acceptance_criteria":["`meta plan` resolves repo-scoped provider defaults"],"priority":2}]}'
fi
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

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
                            "nodes": [{
                                "id": "state-backlog",
                                "name": "Backlog",
                                "type": "backlog"
                            }]
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
    let issue_labels_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query IssueLabels");
        then.status(200).json_body(json!({
            "data": {
                "issueLabels": {
                    "nodes": [{
                        "id": "label-plan",
                        "name": "plan"
                    }]
                }
            }
        }));
    });
    let create_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue")
            .body_includes("\"title\":\"Use repo agent defaults\"")
            .body_includes("\"stateId\":\"state-backlog\"")
            .body_includes("\"labelIds\":[\"label-plan\"]");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": {
                        "id": "issue-77",
                        "identifier": "MET-77",
                        "title": "Use repo agent defaults",
                        "description": "Ensure repo-scoped agent defaults are applied.",
                        "url": "https://linear.app/issues/77",
                        "priority": 2,
                        "updatedAt": "2026-03-15T01:00:00Z",
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
            "plan",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--no-interactive",
            "--request",
            "Use the repo-scoped agent defaults",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MET-77"));

    teams_mock.assert_calls(1);
    projects_mock.assert_calls(1);
    issue_labels_mock.assert_calls(1);
    create_issue_mock.assert_calls(1);
    assert_eq!(
        fs::read_to_string(stub_dir.join("agent-1.txt"))?,
        "repo-stub"
    );
    assert_eq!(
        fs::read_to_string(stub_dir.join("model-1.txt"))?,
        "repo-model"
    );
    assert_eq!(
        fs::read_to_string(stub_dir.join("reasoning-1.txt"))?,
        "high"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn builtin_repo_provider_defaults_override_global_builtin_defaults() -> Result<(), Box<dyn Error>> {
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
    "reasoning": "medium"
  }
}
"#,
    )?;
    fs::write(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "codex"
default_model = "gpt-5.4"
default_reasoning = "low"
"#
        ),
    )?;

    for name in ["claude", "codex"] {
        let stub_path = bin_dir.join(name);
        fs::write(
            &stub_path,
            r##"#!/bin/sh
if [ "__NAME__" = "claude" ] && [ "$1" = "-p" ] && [ "$2" = "--help" ]; then
  cat <<'EOF'
-p, --print
--model <model>
--effort <level>
--permission-mode <mode>
EOF
  exit 0
fi
if [ "__NAME__" = "codex" ] && [ "$1" = "--help" ]; then
  cat <<'EOF'
-a, --ask-for-approval <APPROVAL_POLICY>
-s, --sandbox <SANDBOX_MODE>
-C, --cd <DIR>
    --add-dir <DIR>
    --dangerously-bypass-approvals-and-sandbox
EOF
  exit 0
fi
if [ "__NAME__" = "codex" ] && [ "$1" = "exec" ] && [ "$2" = "--help" ]; then
  cat <<'EOF'
-m, --model <MODEL>
-c, --config <key=value>
EOF
  exit 0
fi
count_file="$TEST_OUTPUT_DIR/count.txt"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
printf '%s' "__NAME__" > "$TEST_OUTPUT_DIR/bin-$count.txt"
printf '%s' "$METASTACK_AGENT_NAME" > "$TEST_OUTPUT_DIR/agent-$count.txt"
printf '%s' "$METASTACK_AGENT_MODEL" > "$TEST_OUTPUT_DIR/model-$count.txt"
printf '%s' "$METASTACK_AGENT_REASONING" > "$TEST_OUTPUT_DIR/reasoning-$count.txt"
printf '%s' "$METASTACK_AGENT_PROVIDER_SOURCE" > "$TEST_OUTPUT_DIR/provider-source-$count.txt"
if [ "$count" -eq 1 ]; then
  printf '%s' '{"questions":[]}'
else
  printf '%s' '{"summary":"Create one ticket.","issues":[{"title":"Builtin repo defaults win","description":"Ensure repo-scoped builtin provider defaults beat global builtin defaults.","acceptance_criteria":["`meta plan` resolves repo-scoped builtin provider defaults"],"priority":2}]}'
fi
"##
            .replace("__NAME__", name),
        )?;
        let mut permissions = fs::metadata(&stub_path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&stub_path, permissions)?;
    }

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
                            "nodes": [{
                                "id": "state-backlog",
                                "name": "Backlog",
                                "type": "backlog"
                            }]
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
    let issue_labels_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query IssueLabels");
        then.status(200).json_body(json!({
            "data": {
                "issueLabels": {
                    "nodes": [{
                        "id": "label-plan",
                        "name": "plan"
                    }]
                }
            }
        }));
    });
    let create_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue")
            .body_includes("\"title\":\"Builtin repo defaults win\"")
            .body_includes("\"stateId\":\"state-backlog\"")
            .body_includes("\"labelIds\":[\"label-plan\"]");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": {
                        "id": "issue-79",
                        "identifier": "MET-79",
                        "title": "Builtin repo defaults win",
                        "description": "Ensure repo-scoped builtin provider defaults beat global builtin defaults.",
                        "url": "https://linear.app/issues/79",
                        "priority": 2,
                        "updatedAt": "2026-03-16T01:10:00Z",
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
            "plan",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--no-interactive",
            "--request",
            "Use the repo-scoped builtin provider defaults",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MET-79"));

    teams_mock.assert_calls(1);
    projects_mock.assert_calls(1);
    issue_labels_mock.assert_calls(1);
    create_issue_mock.assert_calls(1);
    assert_eq!(fs::read_to_string(stub_dir.join("bin-1.txt"))?, "claude");
    assert_eq!(fs::read_to_string(stub_dir.join("agent-1.txt"))?, "claude");
    assert_eq!(fs::read_to_string(stub_dir.join("model-1.txt"))?, "sonnet");
    assert_eq!(
        fs::read_to_string(stub_dir.join("reasoning-1.txt"))?,
        "medium"
    );
    assert_eq!(
        fs::read_to_string(stub_dir.join("provider-source-1.txt"))?,
        "repo_default"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn cli_agent_overrides_beat_repo_and_global_defaults() -> Result<(), Box<dyn Error>> {
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
    "provider": "repo-stub",
    "model": "repo-model",
    "reasoning": "medium"
  }
}
"#,
    )?;
    fs::write(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "global-stub"
default_model = "global-model"
default_reasoning = "low"

[agents.commands.global-stub]
command = "plan-agent-stub"
transport = "stdin"

[agents.commands.repo-stub]
command = "plan-agent-stub"
transport = "stdin"

[agents.commands.cli-stub]
command = "plan-agent-stub"
transport = "stdin"
"#
        ),
    )?;

    let stub_path = bin_dir.join("plan-agent-stub");
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
printf '%s' "$METASTACK_AGENT_NAME" > "$TEST_OUTPUT_DIR/agent-$count.txt"
printf '%s' "$METASTACK_AGENT_MODEL" > "$TEST_OUTPUT_DIR/model-$count.txt"
printf '%s' "$METASTACK_AGENT_REASONING" > "$TEST_OUTPUT_DIR/reasoning-$count.txt"
cat > "$TEST_OUTPUT_DIR/payload-$count.txt"
if [ "$count" -eq 1 ]; then
  printf '%s' '{"questions":[]}'
else
  printf '%s' '{"summary":"Create one ticket.","issues":[{"title":"CLI overrides win","description":"Ensure direct agent overrides beat repo and global defaults.","acceptance_criteria":["CLI agent overrides have highest precedence"],"priority":2}]}'
fi
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

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
                            "nodes": [{
                                "id": "state-backlog",
                                "name": "Backlog",
                                "type": "backlog"
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
    let issue_labels_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query IssueLabels");
        then.status(200).json_body(json!({
            "data": {
                "issueLabels": {
                    "nodes": [{
                        "id": "label-plan",
                        "name": "plan"
                    }]
                }
            }
        }));
    });
    let create_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue")
            .body_includes("\"title\":\"CLI overrides win\"")
            .body_includes("\"stateId\":\"state-backlog\"")
            .body_includes("\"labelIds\":[\"label-plan\"]");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": {
                        "id": "issue-78",
                        "identifier": "MET-78",
                        "title": "CLI overrides win",
                        "description": "Ensure direct agent overrides beat repo and global defaults.",
                        "url": "https://linear.app/issues/78",
                        "priority": 2,
                        "updatedAt": "2026-03-15T01:10:00Z",
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
            "plan",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--no-interactive",
            "--request",
            "Use direct agent overrides",
            "--agent",
            "cli-stub",
            "--model",
            "cli-model",
            "--reasoning",
            "max",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MET-78"));

    issue_labels_mock.assert_calls(1);
    create_issue_mock.assert_calls(1);

    assert_eq!(
        fs::read_to_string(stub_dir.join("agent-1.txt"))?,
        "cli-stub"
    );
    assert_eq!(
        fs::read_to_string(stub_dir.join("model-1.txt"))?,
        "cli-model"
    );
    assert_eq!(fs::read_to_string(stub_dir.join("reasoning-1.txt"))?, "max");

    Ok(())
}

#[test]
fn plan_requires_linear_auth_for_non_interactive_runs() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
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

    meta()
        .current_dir(&repo_root)
        .env_remove("LINEAR_API_KEY")
        .env_remove("LINEAR_API_URL")
        .env_remove("LINEAR_TEAM")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("METASTACK_CONFIG")
        .env("HOME", temp.path())
        .args([
            "plan",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--no-interactive",
            "--request",
            "Plan a new backlog workflow",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Linear auth is required"));

    Ok(())
}
