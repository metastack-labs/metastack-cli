#![allow(dead_code, unused_imports)]

include!("support/common.rs");

#[cfg(unix)]
fn write_onboarded_config(
    config_path: &Path,
    config: impl AsRef<str>,
) -> Result<(), Box<dyn Error>> {
    let contents = format!(
        "{}\n[onboarding]\ncompleted = true\n",
        config.as_ref().trim_end()
    );
    fs::write(config_path, &contents)?;
    let home_config = isolated_home_dir().join(".config/metastack/config.toml");
    if let Some(parent) = home_config.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(home_config, contents)?;
    Ok(())
}

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
    write_onboarded_config(config_path, config)
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
    write_onboarded_config(
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

    teams_mock.assert_calls(4);
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
fn plan_builtin_codex_reuses_session_across_non_interactive_phases() -> Result<(), Box<dyn Error>> {
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
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "codex"
default_model = "gpt-5.4"
default_reasoning = "high"
"#,
        ),
    )?;

    let stub_path = bin_dir.join("codex");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
if [ "$1" = "--help" ]; then
  cat <<'EOF'
Usage: codex [OPTIONS] [PROMPT]
  --sandbox <SANDBOX_MODE>
  --ask-for-approval <APPROVAL_POLICY>
  --cd <DIR>
  --add-dir <DIR>
EOF
  exit 0
fi

if [ "$1" = "exec" ] && [ "$2" = "--help" ]; then
  cat <<'EOF'
Usage: codex exec [OPTIONS] [PROMPT]
  --model <MODEL>
  -c, --config <key=value>
  --json
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
for arg in "$@"; do
  printf '%s\n' "$arg"
  last="$arg"
done > "$TEST_OUTPUT_DIR/args-$count.txt"
printf '%s' "$last" > "$TEST_OUTPUT_DIR/payload-$count.txt"

if [ "$count" -eq 1 ]; then
  printf '%s\n' '{"type":"thread.started","thread_id":"thread-codex-1"}'
  printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"{\"questions\":[\"Which area should be prioritized?\"]}"}}'
  exit 0
fi

printf '%s\n' '{"type":"thread.started","thread_id":"thread-codex-1"}'
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"{\"summary\":\"Create one ticket.\",\"issues\":[{\"title\":\"Reuse the planning session\",\"description\":\"Keep one Codex session alive across multi-phase planning.\",\"acceptance_criteria\":[\"The second planning phase resumes the first session\"],\"priority\":2}]}"}}'
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(team_payload());
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
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue")
            .body_includes("\"title\":\"Reuse the planning session\"")
            .body_includes("\"projectId\":\"project-1\"")
            .body_includes("\"stateId\":\"state-backlog\"")
            .body_includes("\"labelIds\":[\"label-plan\"]");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": {
                        "id": "issue-51",
                        "identifier": "MET-51",
                        "title": "Reuse the planning session",
                        "description": "Keep one Codex session alive across multi-phase planning.",
                        "url": "https://linear.app/issues/51",
                        "priority": 2,
                        "updatedAt": "2026-03-19T19:00:00Z",
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
            "--agent",
            "codex",
            "--no-interactive",
            "--request",
            "Plan the runtime session reuse work",
            "--answer",
            "Prioritize backlog planning first",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created 1 backlog issue(s):"))
        .stdout(predicate::str::contains("MET-51"));

    let first_args = fs::read_to_string(stub_dir.join("args-1.txt"))?;
    assert!(first_args.contains("exec"));
    assert!(first_args.contains("--json"));
    assert!(!first_args.contains("resume"));

    let second_args = fs::read_to_string(stub_dir.join("args-2.txt"))?;
    assert!(second_args.contains("exec"));
    assert!(second_args.contains("resume"));
    assert!(second_args.contains("thread-codex-1"));
    assert!(second_args.contains("--json"));

    let second_payload = fs::read_to_string(stub_dir.join("payload-2.txt"))?;
    assert!(second_payload.contains("Prioritize backlog planning first"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn plan_builtin_claude_retries_fresh_after_invalid_resume() -> Result<(), Box<dyn Error>> {
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
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "claude"
default_model = "haiku"
default_reasoning = "low"
"#,
        ),
    )?;

    let stub_path = bin_dir.join("claude");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
for arg in "$@"; do
  if [ "$arg" = "--help" ]; then
    cat <<'EOF'
Usage: claude [options] [command] [prompt]
  -p, --print
  --model <model>
  --effort <level>
  --output-format <format>
  --permission-mode <mode>
EOF
    exit 0
  fi
done

count_file="$TEST_OUTPUT_DIR/count.txt"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
has_resume=0
for arg in "$@"; do
  printf '%s\n' "$arg"
  last="$arg"
  if [ "$arg" = "--resume" ]; then
    has_resume=1
  fi
done > "$TEST_OUTPUT_DIR/args-$count.txt"
printf '%s' "$last" > "$TEST_OUTPUT_DIR/payload-$count.txt"

if [ "$count" -eq 1 ]; then
  printf '%s' '{"type":"result","subtype":"success","result":"{\"questions\":[\"Which ticket should land first?\"]}","session_id":"stale-session"}'
  exit 0
fi

if [ "$has_resume" -eq 1 ]; then
  printf '%s' 'No conversation found with session ID: stale-session' >&2
  exit 1
fi

printf '%s' '{"type":"result","subtype":"success","result":"{\"summary\":\"Create one ticket after retry.\",\"issues\":[{\"title\":\"Retry Claude planning fresh\",\"description\":\"Recover from an invalid resume target with one fresh retry.\",\"acceptance_criteria\":[\"A stale Claude resume handle retries once without failing the command\"],\"priority\":2}]}","session_id":"fresh-session"}'
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(team_payload());
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
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue")
            .body_includes("\"title\":\"Retry Claude planning fresh\"")
            .body_includes("\"projectId\":\"project-1\"")
            .body_includes("\"stateId\":\"state-backlog\"")
            .body_includes("\"labelIds\":[\"label-plan\"]");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": {
                        "id": "issue-52",
                        "identifier": "MET-52",
                        "title": "Retry Claude planning fresh",
                        "description": "Recover from an invalid resume target with one fresh retry.",
                        "url": "https://linear.app/issues/52",
                        "priority": 2,
                        "updatedAt": "2026-03-19T19:05:00Z",
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
            "--agent",
            "claude",
            "--no-interactive",
            "--request",
            "Plan the retry behavior for invalid resume targets",
            "--answer",
            "Handle the retry in the shared runtime layer",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created 1 backlog issue(s):"))
        .stdout(predicate::str::contains("MET-52"));

    let second_args = fs::read_to_string(stub_dir.join("args-2.txt"))?;
    assert!(second_args.contains("--resume"));
    assert!(second_args.contains("stale-session"));
    assert!(second_args.contains("--output-format=json"));

    let third_args = fs::read_to_string(stub_dir.join("args-3.txt"))?;
    assert!(!third_args.contains("--resume"));
    assert!(third_args.contains("--output-format=json"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn plan_interactive_preserves_explicit_builtin_overrides_across_resumed_phases()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
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
    write_onboarded_config(
        &config_path,
        r#"[linear]
api_key = "token"

[agents]
default_agent = "codex"
default_model = "gpt-5.4"
default_reasoning = "high"
"#,
    )?;

    let codex_stub = bin_dir.join("codex");
    fs::write(
        &codex_stub,
        r#"#!/bin/sh
count_file="$TEST_OUTPUT_DIR/codex-count.txt"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"

if [ "$1" = "--help" ]; then
  cat <<'EOF'
Usage: codex [OPTIONS] [PROMPT]
  --sandbox <SANDBOX_MODE>
  --ask-for-approval <APPROVAL_POLICY>
  --cd <DIR>
  --add-dir <DIR>
EOF
  exit 0
fi

if [ "$1" = "exec" ] && [ "$2" = "--help" ]; then
  cat <<'EOF'
Usage: codex exec [OPTIONS] [PROMPT]
  --model <MODEL>
  -c, --config <key=value>
  --json
EOF
  exit 0
fi

printf '%s' '{"type":"thread.started","thread_id":"unexpected-codex-session"}'
printf '%s' '{"type":"item.completed","item":{"type":"agent_message","text":"{\"questions\":[]}"}}'
"#,
    )?;
    let mut permissions = fs::metadata(&codex_stub)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&codex_stub, permissions)?;

    let claude_stub = bin_dir.join("claude");
    fs::write(
        &claude_stub,
        r#"#!/bin/sh
for arg in "$@"; do
  if [ "$arg" = "--help" ]; then
    cat <<'EOF'
Usage: claude [options] [command] [prompt]
  -p, --print
  --model <model>
  --effort <level>
  --output-format <format>
  --permission-mode <mode>
EOF
    exit 0
  fi
done

count_file="$TEST_OUTPUT_DIR/claude-count.txt"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"

for arg in "$@"; do
  printf '%s\n' "$arg"
done > "$TEST_OUTPUT_DIR/claude-args-$count.txt"

if [ "$count" -eq 1 ]; then
  printf '%s' '{"type":"result","subtype":"success","result":"{\"questions\":[]}","session_id":"interactive-session"}'
  exit 0
fi

printf '%s' '{"type":"result","subtype":"success","result":"{\"summary\":\"Keep one interactive ticket.\",\"issues\":[{\"title\":\"Preserve interactive overrides\",\"description\":\"Keep explicit builtin overrides active across interactive planning phases.\",\"acceptance_criteria\":[\"Interactive phase two resumes the explicit provider session\"],\"priority\":2}]}","session_id":"interactive-session"}'
"#,
    )?;
    let mut permissions = fs::metadata(&claude_stub)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&claude_stub, permissions)?;

    let meta_command = test_command();
    let meta_bin = meta_command.get_program().to_os_string();
    let current_path = std::env::var("PATH")?;

    let expect_script = format!(
        r#"
set timeout 10
spawn -noecho {meta_bin} plan --root {repo_root} --agent claude --model haiku --reasoning low
expect {{
  -re "Planning Request" {{}}
}}
send -- "Plan the interactive continuation flow\r"
expect {{
  -re "Generating suggested tickets" {{ exp_continue }}
  -re "Combination Plan" {{}}
}}
send -- "\033"
expect eof
"#,
        meta_bin = tcl_escape(&meta_bin.to_string_lossy()),
        repo_root = tcl_escape(&repo_root.display().to_string()),
    );

    let output = ProcessCommand::new("expect")
        .arg("-c")
        .arg(expect_script)
        .current_dir(&repo_root)
        .env("HOME", isolated_home_dir())
        .env("XDG_CONFIG_HOME", isolated_home_dir().join(".config"))
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()?;

    assert!(
        output.status.success(),
        "interactive plan failed: stdout=`{}` stderr=`{}`",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let first_args = fs::read_to_string(stub_dir.join("claude-args-1.txt"))?;
    assert!(first_args.contains("-p"));
    assert!(first_args.contains("--model=haiku"));
    assert!(first_args.contains("--effort=low"));
    assert!(first_args.contains("--output-format=json"));
    assert!(!first_args.contains("--resume"));

    let second_args = fs::read_to_string(stub_dir.join("claude-args-2.txt"))?;
    assert!(second_args.contains("-p"));
    assert!(second_args.contains("--model=haiku"));
    assert!(second_args.contains("--effort=low"));
    assert!(second_args.contains("--output-format=json"));
    assert!(second_args.contains("--resume"));
    assert!(second_args.contains("interactive-session"));

    assert!(!stub_dir.join("codex-count.txt").exists());

    Ok(())
}

#[cfg(unix)]
fn tcl_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('{', "\\{")
        .replace('}', "\\}")
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
    write_onboarded_config(
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

    teams_mock.assert_calls(2);
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
fn plan_no_interactive_uses_remembered_selection_velocity_defaults_and_additive_labels()
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
    "team": "REP",
    "project_id": "project-repo"
  },
  "backlog": {
    "default_priority": 3,
    "default_labels": ["repo-default"],
    "velocity_defaults": {
      "state": "Todo",
      "auto_assign": "viewer"
    }
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
team = "OPS"

[backlog]
default_priority = 4
default_labels = ["global-default"]

[agents]
default_agent = "stub"

[agents.commands.stub]
command = "plan-agent-stub"
transport = "stdin"
"#,
        ),
    )?;

    let canonical_repo_root = fs::canonicalize(&repo_root)?;
    let remembered_path = config_path
        .parent()
        .expect("config path should have a parent")
        .join("data")
        .join("backlog")
        .join("selections.json");
    fs::create_dir_all(
        remembered_path
            .parent()
            .expect("remembered selections path should have a parent"),
    )?;
    fs::write(
        &remembered_path,
        format!(
            r#"{{
  "version": 1,
  "repositories": {{
    "{}": {{
      "team": "MET",
      "project_id": "project-memory",
      "project_name": "Remembered Project"
    }}
  }}
}}
"#,
            canonical_repo_root.to_string_lossy()
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
  printf '%s' '{"summary":"Create one ticket.","issues":[{"title":"Remember velocity defaults","description":"Use remembered project/team defaults when running without prompts.","acceptance_criteria":["remembered defaults are reused in zero-prompt mode"]}]}'
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
                            }, {
                                "id": "state-todo",
                                "name": "Todo",
                                "type": "unstarted"
                            }]
                        }
                    }, {
                        "id": "team-2",
                        "key": "REP",
                        "name": "Repo Team",
                        "states": { "nodes": [] }
                    }, {
                        "id": "team-3",
                        "key": "OPS",
                        "name": "Ops",
                        "states": { "nodes": [] }
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
                        "id": "project-memory",
                        "name": "Remembered Project",
                        "description": null,
                        "url": "https://linear.app/projects/project-memory",
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
    let labels_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query IssueLabels")
            .body_includes("\"key\":{\"eq\":\"MET\"}");
        then.status(200).json_body(json!({
            "data": {
                "issueLabels": {
                    "nodes": [{
                        "id": "label-plan",
                        "name": "plan"
                    }, {
                        "id": "label-global",
                        "name": "global-default"
                    }, {
                        "id": "label-repo",
                        "name": "repo-default"
                    }, {
                        "id": "label-cli",
                        "name": "cli-extra"
                    }]
                }
            }
        }));
    });
    let viewer_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Viewer");
        then.status(200).json_body(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Viewer",
                    "email": "viewer@example.com"
                }
            }
        }));
    });
    let create_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue")
            .body_includes("\"projectId\":\"project-memory\"")
            .body_includes("\"stateId\":\"state-todo\"")
            .body_includes("\"priority\":3")
            .body_includes("\"assigneeId\":\"viewer-1\"")
            .body_includes("\"labelIds\":[\"label-plan\",\"label-global\",\"label-repo\",\"label-cli\"]");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": {
                        "id": "issue-61",
                        "identifier": "MET-61",
                        "title": "Remember velocity defaults",
                        "description": "Use remembered project/team defaults when running without prompts.",
                        "url": "https://linear.app/issues/61",
                        "priority": 3,
                        "updatedAt": "2026-03-14T18:20:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-memory",
                            "name": "Remembered Project"
                        },
                        "state": {
                            "id": "state-todo",
                            "name": "Todo",
                            "type": "unstarted"
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
            "Remember the last project and apply zero-prompt defaults",
            "--label",
            "cli-extra",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created 1 backlog issue(s):"))
        .stdout(predicate::str::contains("MET-61"));

    teams_mock.assert_calls(2);
    projects_mock.assert_calls(1);
    labels_mock.assert_calls(1);
    viewer_mock.assert_calls(1);
    create_issue_mock.assert_calls(1);

    let remembered: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(remembered_path)?)?;
    assert_eq!(
        remembered["repositories"][canonical_repo_root.to_string_lossy().as_ref()]["team"].as_str(),
        Some("MET")
    );
    assert_eq!(
        remembered["repositories"][canonical_repo_root.to_string_lossy().as_ref()]["project_id"]
            .as_str(),
        Some("project-memory")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn plan_no_interactive_uses_install_default_project_and_label() -> Result<(), Box<dyn Error>> {
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
api_url = "{api_url}"

[defaults.linear]
project_id = "project-install"

[defaults.issue_labels]
plan = "planning-install"

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
  printf '%s' '{"summary":"Create one ticket.","issues":[{"title":"Use install defaults","description":"Ensure install-scoped defaults apply when repo defaults are absent.","acceptance_criteria":["`meta plan` resolves install defaults"],"priority":2}]}'
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
                        "id": "project-install",
                        "name": "Install Project",
                        "description": null,
                        "url": "https://linear.app/projects/project-install",
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
                        "id": "label-plan-install",
                        "name": "planning-install"
                    }]
                }
            }
        }));
    });
    let create_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue")
            .body_includes("\"title\":\"Use install defaults\"")
            .body_includes("\"projectId\":\"project-install\"")
            .body_includes("\"labelIds\":[\"label-plan-install\"]");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": {
                        "id": "issue-52",
                        "identifier": "MET-52",
                        "title": "Use install defaults",
                        "description": "Ensure install-scoped defaults apply when repo defaults are absent.",
                        "url": "https://linear.app/issues/52",
                        "priority": 2,
                        "updatedAt": "2026-03-19T18:10:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-install",
                            "name": "Install Project"
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

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env(
            "PATH",
            format!("{}:{}", bin_dir.display(), std::env::var("PATH")?),
        )
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .args([
            "plan",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "--no-interactive",
            "--request",
            "Create an install-default proof",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created 1 backlog issue(s):"))
        .stdout(predicate::str::contains("MET-52"));

    teams_mock.assert_calls(2);
    projects_mock.assert();
    issue_labels_mock.assert();
    create_issue_mock.assert();
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
    write_onboarded_config(
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

    teams_mock.assert_calls(2);
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
--output-format <format>
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
  printf '%s' '{"type":"result","subtype":"success","result":"{\"questions\":[]}","session_id":"session-1"}'
else
  printf '%s' '{"type":"result","subtype":"success","result":"{\"summary\":\"Create one ticket.\",\"issues\":[{\"title\":\"Builtin repo defaults win\",\"description\":\"Ensure repo-scoped builtin provider defaults beat global builtin defaults.\",\"acceptance_criteria\":[\"`meta plan` resolves repo-scoped builtin provider defaults\"],\"priority\":2}]}","session_id":"session-1"}'
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

    teams_mock.assert_calls(2);
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
    write_onboarded_config(
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
    let config_dir = temp.path().join(".config/metastack");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&config_dir)?;
    write_onboarded_config(&config_dir.join("config.toml"), "")?;
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
