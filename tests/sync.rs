#![allow(dead_code, unused_imports)]

include!("support/common.rs");

#[test]
fn sync_pull_restores_issue_description_and_managed_attachment_files() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
    let server = MockServer::start();
    let api_url = server.url("/graphql");
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

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [issue_node(
                        "issue-1",
                        "MET-35",
                        "Create the technical and sync commands",
                        "Parent issue description",
                        "state-2",
                        "In Progress",
                    )]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue")
            .body_includes("\"id\":\"issue-1\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": issue_detail_node(
                    "issue-1",
                    "MET-35",
                    "Create the technical and sync commands",
                    "# Pulled description\n",
                    vec![
                        json!({
                            "id": "attachment-1",
                            "title": "implementation.md",
                            "url": server.url("/downloads/implementation.md"),
                            "sourceType": "upload",
                            "metadata": {
                                "managedBy": "metastack-cli",
                                "relativePath": "implementation.md"
                            }
                        }),
                        json!({
                            "id": "attachment-2",
                            "title": "external-link",
                            "url": "https://example.com/external",
                            "sourceType": "link",
                            "metadata": {}
                        })
                    ],
                    None,
                )
            }
        }));
    });

    server.mock(|when, then| {
        when.method(httpmock::Method::GET)
            .path("/downloads/implementation.md");
        then.status(200).body("# Downloaded implementation\n");
    });

    cli()
        .current_dir(temp.path())
        .args([
            "sync",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "pull",
            "MET-35",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Pulled MET-35"));

    let issue_dir = temp.path().join(".metastack/backlog/MET-35");
    assert!(fs::read_to_string(issue_dir.join("index.md"))?.contains("Pulled description"));
    assert!(fs::read_to_string(issue_dir.join("implementation.md"))?.contains("Downloaded"));
    assert!(fs::read_to_string(issue_dir.join(".linear.json"))?.contains("attachment-1"));

    Ok(())
}

#[test]
fn sync_push_updates_the_issue_description_and_replaces_managed_attachments()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let issue_dir = temp.path().join(".metastack/backlog/MET-35");
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

    fs::create_dir_all(&issue_dir)?;
    fs::write(issue_dir.join("index.md"), "# Updated description\n")?;
    fs::write(
        issue_dir.join("implementation.md"),
        "# Local implementation\n",
    )?;

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [issue_node(
                        "issue-1",
                        "MET-35",
                        "Create the technical and sync commands",
                        "Parent issue description",
                        "state-2",
                        "In Progress",
                    )]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue")
            .body_includes("\"id\":\"issue-1\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": issue_detail_node(
                    "issue-1",
                    "MET-35",
                    "Create the technical and sync commands",
                    "Parent issue description",
                    vec![
                        json!({
                            "id": "managed-attachment",
                            "title": "implementation.md",
                            "url": server.url("/assets/old-implementation.md"),
                            "sourceType": "upload",
                            "metadata": {
                                "managedBy": "metastack-cli",
                                "relativePath": "implementation.md"
                            }
                        }),
                        json!({
                            "id": "external-attachment",
                            "title": "external-link",
                            "url": "https://example.com/external",
                            "sourceType": "link",
                            "metadata": {}
                        })
                    ],
                    None,
                )
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
            .body_includes("mutation UpdateIssue")
            .body_includes("\"id\":\"issue-1\"");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": issue_node(
                        "issue-1",
                        "MET-35",
                        "Create the technical and sync commands",
                        "# Updated description\n",
                        "state-2",
                        "In Progress",
                    )
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation DeleteAttachment")
            .body_includes("\"id\":\"managed-attachment\"");
        then.status(200).json_body(json!({
            "data": {
                "attachmentDelete": {
                    "success": true
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UploadFile")
            .body_includes("\"filename\":\"implementation.md\"");
        then.status(200).json_body(json!({
            "data": {
                "fileUpload": {
                    "success": true,
                    "uploadFile": {
                        "uploadUrl": server.url("/uploads/implementation.md"),
                        "assetUrl": server.url("/assets/implementation.md"),
                        "headers": [{
                            "key": "x-goog-content-length-range",
                            "value": "1,100000"
                        }]
                    }
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(httpmock::Method::PUT)
            .path("/uploads/implementation.md");
        then.status(200);
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateAttachment")
            .body_includes("\"relativePath\":\"implementation.md\"");
        then.status(200).json_body(json!({
            "data": {
                "attachmentCreate": {
                    "success": true,
                    "attachment": {
                        "id": "attachment-new",
                        "title": "implementation.md",
                        "url": server.url("/assets/implementation.md"),
                        "sourceType": "upload",
                        "metadata": {
                            "managedBy": "metastack-cli",
                            "relativePath": "implementation.md"
                        }
                    }
                }
            }
        }));
    });

    cli()
        .current_dir(temp.path())
        .args([
            "sync",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "push",
            "MET-35",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Pushed MET-35"))
        .stdout(predicate::str::contains("synced 1 managed attachment file"));

    let metadata = fs::read_to_string(issue_dir.join(".linear.json"))?;
    assert!(metadata.contains("\"attachment_id\": \"attachment-new\""));

    Ok(())
}

#[test]
fn sync_push_is_blocked_for_the_active_listen_issue() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let issue_dir = temp.path().join(".metastack/backlog/MET-99");
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
    fs::create_dir_all(&issue_dir)?;
    fs::write(issue_dir.join("index.md"), "# Local backlog\n")?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_LISTEN_UNATTENDED", "1")
        .env("METASTACK_LINEAR_ISSUE_IDENTIFIER", "MET-99")
        .args([
            "sync",
            "--api-key",
            "token",
            "--api-url",
            "http://127.0.0.1:9/graphql",
            "push",
            "MET-99",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "is disabled during `meta agents listen` because it would overwrite the primary Linear issue description",
        ));

    Ok(())
}

#[test]
fn sync_render_once_uses_default_project_and_loads_paginated_issue_list()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let server = MockServer::start();
    let api_url = server.url("/graphql");

    fs::create_dir_all(temp.path().join(".metastack"))?;
    fs::write(
        temp.path().join(".metastack/meta.json"),
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "MetaStack CLI"
  }
}
"#,
    )?;

    let first_page = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "token")
            .body_includes("query Issues")
            .body_includes("\"after\":null");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [
                        issue_node(
                            "issue-1",
                            "MET-11",
                            "First issue",
                            "First page issue",
                            "state-2",
                            "In Progress",
                        ),
                        issue_node(
                            "issue-2",
                            "MET-12",
                            "Second issue",
                            "First page issue",
                            "state-1",
                            "Todo",
                        )
                    ],
                    "pageInfo": {
                        "hasNextPage": true,
                        "endCursor": "cursor-1"
                    }
                }
            }
        }));
    });

    let second_page = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "token")
            .body_includes("query Issues")
            .body_includes("\"after\":\"cursor-1\"");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [
                        issue_node(
                            "issue-3",
                            "MET-13",
                            "Third issue",
                            "Second page issue",
                            "state-2",
                            "In Progress",
                        )
                    ],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        }));
    });

    cli()
        .current_dir(temp.path())
        .args([
            "sync",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "--render-once",
            "--events",
            "down,down,enter,down,enter",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "hint: `meta sync` is a compatibility alias; prefer `meta backlog sync`.",
        ))
        .stdout(predicate::str::contains(
            "meta backlog sync (MetaStack CLI)",
        ))
        .stdout(predicate::str::contains("Ready to push MET-13"))
        .stdout(predicate::str::contains("Third issue"));

    first_page.assert();
    second_page.assert();

    Ok(())
}

#[test]
fn sync_uses_repo_selected_profile_and_project_over_conflicting_global_defaults()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let right_server = MockServer::start();
    let wrong_server = MockServer::start();
    let right_api_url = right_server.url("/graphql");
    let wrong_api_url = wrong_server.url("/graphql");

    fs::create_dir_all(repo_root.join(".metastack"))?;
    fs::write(
        repo_root.join(".metastack/meta.json"),
        r#"{
  "linear": {
    "profile": "work",
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
                        "identifier": "MET-210",
                        "title": "Repo default sync issue",
                        "description": "Selected from the repo-scoped Linear project",
                        "url": "https://linear.app/issues/MET-210",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
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
                        "identifier": "MET-211",
                        "title": "Wrong project sync issue",
                        "description": "Should be filtered out by the repo project default",
                        "url": "https://linear.app/issues/MET-211",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
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
                        "identifier": "PER-212",
                        "title": "Wrong team sync issue",
                        "description": "Should be filtered out by the repo team default",
                        "url": "https://linear.app/issues/PER-212",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
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

    cli()
        .current_dir(&repo_root)
        .env_remove("LINEAR_API_KEY")
        .env_remove("LINEAR_API_URL")
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "sync",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--render-once",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "hint: `meta sync` is a compatibility alias; prefer `meta backlog sync`.",
        ))
        .stdout(predicate::str::contains("meta backlog sync (project-1)"))
        .stdout(predicate::str::contains("Repo default sync issue"))
        .stdout(predicate::str::contains("Wrong project sync issue").not())
        .stdout(predicate::str::contains("Wrong team sync issue").not());

    issues_mock.assert();
    Ok(())
}

#[test]
fn sync_without_subcommand_requires_default_project_configuration() {
    let temp = tempdir().expect("tempdir should build");
    write_minimal_planning_context(
        temp.path(),
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )
    .expect("planning context should write");

    cli()
        .current_dir(temp.path())
        .args(["sync", "--api-key", "token", "--render-once"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "hint: `meta sync` is a compatibility alias; prefer `meta backlog sync`.",
        ))
        .stderr(predicate::str::contains(
            "`meta backlog sync` requires a repo default project",
        ));
}
