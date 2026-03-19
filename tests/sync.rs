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
                "issue": {
                    "id": "issue-1",
                    "identifier": "MET-35",
                    "title": "Create the technical and sync commands",
                    "description": format!("# Pulled description\n\n![diagram]({})\n", server.url("/design.png")),
                    "url": "https://linear.app/issues/MET-35",
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
                    "labels": { "nodes": [] },
                    "comments": {
                        "nodes": [{
                            "id": "comment-1",
                            "body": format!("## Screenshot review\nPlease inspect ![comment]({})", server.url("/comment.png")),
                            "createdAt": "2026-03-15T12:00:00Z",
                            "user": {
                                "id": "user-1",
                                "name": "Taylor",
                                "email": "taylor@example.com"
                            },
                            "resolvedAt": null
                        }]
                    },
                    "state": {
                        "id": "state-1",
                        "name": "Todo",
                        "type": "unstarted"
                    },
                    "attachments": {
                        "nodes": [
                            {
                                "id": "attachment-1",
                                "title": "implementation.md",
                                "url": server.url("/downloads/implementation.md"),
                                "sourceType": "upload",
                                "metadata": {
                                    "managedBy": "metastack-cli",
                                    "relativePath": "implementation.md"
                                }
                            },
                            {
                                "id": "attachment-2",
                                "title": "external-link",
                                "url": "https://example.com/external",
                                "sourceType": "link",
                                "metadata": {}
                            }
                        ]
                    },
                    "parent": {
                        "id": "parent-1",
                        "identifier": "MET-01",
                        "title": "Program",
                        "url": "https://linear.app/issues/MET-01",
                        "description": format!("Parent screenshot ![parent]({})", server.url("/parent.png"))
                    },
                    "children": { "nodes": [] }
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(httpmock::Method::GET)
            .path("/downloads/implementation.md");
        then.status(200).body("# Downloaded implementation\n");
    });
    server.mock(|when, then| {
        when.method(httpmock::Method::GET).path("/design.png");
        then.status(200).body("design");
    });
    server.mock(|when, then| {
        when.method(httpmock::Method::GET).path("/parent.png");
        then.status(200).body("parent");
    });
    server.mock(|when, then| {
        when.method(httpmock::Method::GET).path("/comment.png");
        then.status(500).body("boom");
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
    let index = fs::read_to_string(issue_dir.join("index.md"))?;
    assert!(index.contains("Pulled description"));
    assert!(index.contains("![diagram](artifacts/design.png)"));
    assert!(fs::read_to_string(issue_dir.join("implementation.md"))?.contains("Downloaded"));
    let manifest = fs::read_to_string(issue_dir.join("artifacts/ticket-images.md"))?;
    assert!(manifest.contains("design.png"));
    assert!(manifest.contains("parent-parent.png"));
    assert!(manifest.contains("comment-1-comment.png"));
    let discussion = fs::read_to_string(issue_dir.join("context/ticket-discussion.md"))?;
    assert!(discussion.contains("### **Taylor** (2026-03-15)"));
    assert!(discussion.contains("Screenshot review"));
    assert_eq!(fs::read(issue_dir.join("artifacts/design.png"))?, b"design");
    assert_eq!(
        fs::read(issue_dir.join("artifacts/parent-parent.png"))?,
        b"parent"
    );
    assert!(!issue_dir.join("artifacts/comment-1-comment.png").exists());
    let metadata = fs::read_to_string(issue_dir.join(".linear.json"))?;
    assert!(metadata.contains("attachment-1"));
    assert!(metadata.contains("\"local_hash\":"));
    assert!(metadata.contains("\"remote_hash\":"));

    Ok(())
}

#[test]
fn sync_pull_localizes_ticket_images_and_writes_discussion_context() -> Result<(), Box<dyn Error>> {
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

    let detail_payload = serde_json::from_str::<serde_json::Value>(
        &json!({
            "data": {
                "issue": {
                    "id": "issue-1",
                    "identifier": "MET-35",
                    "title": "Create the technical and sync commands",
                    "description": "Pulled description\n\n![issue-shot](ISSUE_IMAGE)",
                    "url": "https://linear.app/issues/MET-35",
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
                    "labels": { "nodes": [] },
                    "comments": {
                        "nodes": [{
                            "id": "comment-1",
                            "body": "Need parent art\n\n![comment-shot](COMMENT_IMAGE)",
                            "createdAt": "2026-03-16T10:00:00Z",
                            "user": {
                                "name": "Alice"
                            },
                            "resolvedAt": null
                        }]
                    },
                    "state": {
                        "id": "state-2",
                        "name": "In Progress",
                        "type": "started"
                    },
                    "attachments": {
                        "nodes": [{
                            "id": "attachment-1",
                            "title": "implementation.md",
                            "url": "ATTACHMENT_URL",
                            "sourceType": "upload",
                            "metadata": {
                                "managedBy": "metastack-cli",
                                "relativePath": "implementation.md"
                            }
                        }]
                    },
                    "parent": {
                        "id": "parent-1",
                        "identifier": "MET-10",
                        "title": "Parent issue",
                        "url": "https://linear.app/issues/MET-10",
                        "description": "Parent issue context\n\n![parent-shot](PARENT_IMAGE)"
                    },
                    "children": {
                        "nodes": []
                    }
                }
            }
        })
        .to_string()
        .replace("ISSUE_IMAGE", &server.url("/images/issue-shot.png"))
        .replace("COMMENT_IMAGE", &server.url("/images/comment-shot.jpg"))
        .replace("PARENT_IMAGE", &server.url("/images/parent-shot.svg"))
        .replace(
            "ATTACHMENT_URL",
            &server.url("/downloads/implementation.md"),
        ),
    )?;

    server.mock(move |when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue")
            .body_includes("\"id\":\"issue-1\"");
        then.status(200).json_body(detail_payload);
    });

    server.mock(|when, then| {
        when.method(GET).path("/downloads/implementation.md");
        then.status(200).body("# Downloaded implementation\n");
    });

    for path in [
        "/images/issue-shot.png",
        "/images/comment-shot.jpg",
        "/images/parent-shot.svg",
    ] {
        server.mock(move |when, then| {
            when.method(GET).path(path);
            then.status(200).body("image-bytes");
        });
    }

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
    let index = fs::read_to_string(issue_dir.join("index.md"))?;
    let implementation = fs::read_to_string(issue_dir.join("implementation.md"))?;
    let ticket_images = fs::read_to_string(issue_dir.join("artifacts/ticket-images.md"))?;
    let ticket_discussion = fs::read_to_string(issue_dir.join("context/ticket-discussion.md"))?;

    assert!(index.contains("![issue-shot](artifacts/issue-shot.png)"));
    assert!(implementation.contains("Downloaded implementation"));
    assert!(ticket_images.contains("| `issue-shot.png` | issue-shot | Issue description |"));
    assert!(
        ticket_images.contains("| `parent-parent-shot.svg` | parent-shot | Parent description |")
    );
    assert!(
        ticket_images.contains("| `comment-1-comment-shot.jpg` | comment-shot | Need parent art |")
    );
    assert!(ticket_discussion.contains("### **Alice** (2026-03-16)"));
    assert!(ticket_discussion.contains("![comment-shot](artifacts/comment-1-comment-shot.jpg)"));
    assert!(issue_dir.join("artifacts/issue-shot.png").is_file());
    assert!(issue_dir.join("artifacts/parent-parent-shot.svg").is_file());
    assert!(
        issue_dir
            .join("artifacts/comment-1-comment-shot.jpg")
            .is_file()
    );

    Ok(())
}

#[test]
fn sync_pull_logs_nonfatal_ticket_image_download_failures() -> Result<(), Box<dyn Error>> {
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

    let detail_payload = serde_json::from_str::<serde_json::Value>(
        &json!({
            "data": {
                "issue": {
                    "id": "issue-1",
                    "identifier": "MET-35",
                    "title": "Create the technical and sync commands",
                    "description": "Pulled description\n\n![issue-shot](MISSING_IMAGE)",
                    "url": "https://linear.app/issues/MET-35",
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
                    "labels": { "nodes": [] },
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
        })
        .to_string()
        .replace("MISSING_IMAGE", &server.url("/images/missing.png")),
    )?;

    server.mock(move |when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue")
            .body_includes("\"id\":\"issue-1\"");
        then.status(200).json_body(detail_payload);
    });

    server.mock(|when, then| {
        when.method(GET).path("/images/missing.png");
        then.status(500).body("boom");
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
        .stderr(predicate::str::contains(
            "warning: failed to localize ticket image for MET-35",
        ));

    let issue_dir = temp.path().join(".metastack/backlog/MET-35");
    assert!(
        fs::read_to_string(issue_dir.join("artifacts/ticket-images.md"))?
            .contains("| `missing.png` | issue-shot | Issue description |")
    );
    assert!(!issue_dir.join("artifacts/missing.png").exists());

    Ok(())
}

#[test]
fn sync_push_leaves_issue_description_unchanged_by_default_and_replaces_managed_attachments()
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

    let update_issue_mock = server.mock(|when, then| {
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
        .stdout(predicate::str::contains("synced 1 managed attachment file"))
        .stdout(predicate::str::contains(
            "left the Linear issue description unchanged",
        ));

    let metadata = fs::read_to_string(issue_dir.join(".linear.json"))?;
    assert!(metadata.contains("\"attachment_id\": \"attachment-new\""));
    assert!(metadata.contains("\"local_hash\":"));
    assert!(metadata.contains("\"remote_hash\":"));
    update_issue_mock.assert_calls(0);

    Ok(())
}

#[test]
fn sync_push_updates_the_issue_description_only_with_opt_in_flag() -> Result<(), Box<dyn Error>> {
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

    let update_issue_mock = server.mock(|when, then| {
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
            "--update-description",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "updated Linear issue description from index.md",
        ));

    update_issue_mock.assert_calls(1);
    Ok(())
}

#[test]
fn sync_push_description_update_is_blocked_for_the_active_listen_issue()
-> Result<(), Box<dyn Error>> {
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
            "--update-description",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "is disabled during `meta agents listen` because it would overwrite the primary Linear issue description",
        ));

    Ok(())
}

#[test]
fn sync_pull_refuses_remote_ahead_overwrite_without_a_tty() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let baseline_server = MockServer::start();
    let baseline_api_url = baseline_server.url("/graphql");
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

    baseline_server.mock(|when, then| {
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

    baseline_server.mock(|when, then| {
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
                    "# Original description\n",
                    vec![],
                    None,
                )
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
            &baseline_api_url,
            "pull",
            "MET-35",
        ])
        .assert()
        .success();

    let issue_dir = temp.path().join(".metastack/backlog/MET-35");
    let metadata_before = fs::read_to_string(issue_dir.join(".linear.json"))?;

    let remote_server = MockServer::start();
    let remote_api_url = remote_server.url("/graphql");
    remote_server.mock(|when, then| {
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
    remote_server.mock(|when, then| {
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
                    "# Remote changed description\n",
                    vec![],
                    None,
                )
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
            &remote_api_url,
            "pull",
            "MET-35",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("sync state is `remote-ahead`"))
        .stderr(predicate::str::contains("rerun in a TTY"));

    assert_eq!(
        fs::read_to_string(issue_dir.join("index.md"))?,
        "# Original description\n"
    );
    assert_eq!(
        fs::read_to_string(issue_dir.join(".linear.json"))?,
        metadata_before
    );

    Ok(())
}

#[test]
fn sync_pull_refuses_diverged_overwrite_without_a_tty() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let baseline_server = MockServer::start();
    let baseline_api_url = baseline_server.url("/graphql");
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

    baseline_server.mock(|when, then| {
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

    baseline_server.mock(|when, then| {
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
                    "# Original description\n",
                    vec![],
                    None,
                )
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
            &baseline_api_url,
            "pull",
            "MET-35",
        ])
        .assert()
        .success();

    let issue_dir = temp.path().join(".metastack/backlog/MET-35");
    fs::write(issue_dir.join("index.md"), "# Local changed description\n")?;
    let metadata_before = fs::read_to_string(issue_dir.join(".linear.json"))?;

    let remote_server = MockServer::start();
    let remote_api_url = remote_server.url("/graphql");
    remote_server.mock(|when, then| {
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
    remote_server.mock(|when, then| {
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
                    "# Remote changed description\n",
                    vec![],
                    None,
                )
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
            &remote_api_url,
            "pull",
            "MET-35",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("sync state is `diverged`"))
        .stderr(predicate::str::contains("rerun in a TTY"));

    assert_eq!(
        fs::read_to_string(issue_dir.join("index.md"))?,
        "# Local changed description\n"
    );
    assert_eq!(
        fs::read_to_string(issue_dir.join(".linear.json"))?,
        metadata_before
    );

    Ok(())
}

#[test]
fn sync_push_with_update_description_refuses_remote_ahead_description_overwrite()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let baseline_server = MockServer::start();
    let baseline_api_url = baseline_server.url("/graphql");
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

    baseline_server.mock(|when, then| {
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

    baseline_server.mock(|when, then| {
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
                    "# Original description\n",
                    vec![],
                    None,
                )
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
            &baseline_api_url,
            "pull",
            "MET-35",
        ])
        .assert()
        .success();

    let remote_server = MockServer::start();
    let remote_api_url = remote_server.url("/graphql");
    remote_server.mock(|when, then| {
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

    remote_server.mock(|when, then| {
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
                    "# Remote changed description\n",
                    vec![],
                    None,
                )
            }
        }));
    });

    let update_issue_mock = remote_server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": issue_node(
                        "issue-1",
                        "MET-35",
                        "Create the technical and sync commands",
                        "# Original description\n",
                        "state-2",
                        "In Progress",
                    )
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
            &remote_api_url,
            "push",
            "MET-35",
            "--update-description",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("sync state is `remote-ahead`"))
        .stderr(predicate::str::contains("--update-description"));

    update_issue_mock.assert_calls(0);
    Ok(())
}

#[test]
fn sync_push_with_update_description_refuses_diverged_description_overwrite()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let baseline_server = MockServer::start();
    let baseline_api_url = baseline_server.url("/graphql");
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

    baseline_server.mock(|when, then| {
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

    baseline_server.mock(|when, then| {
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
                    "# Original description\n",
                    vec![],
                    None,
                )
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
            &baseline_api_url,
            "pull",
            "MET-35",
        ])
        .assert()
        .success();

    let issue_dir = temp.path().join(".metastack/backlog/MET-35");
    fs::write(issue_dir.join("index.md"), "# Local changed description\n")?;

    let remote_server = MockServer::start();
    let remote_api_url = remote_server.url("/graphql");
    remote_server.mock(|when, then| {
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

    remote_server.mock(|when, then| {
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
                    "# Remote changed description\n",
                    vec![],
                    None,
                )
            }
        }));
    });

    let update_issue_mock = remote_server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": issue_node(
                        "issue-1",
                        "MET-35",
                        "Create the technical and sync commands",
                        "# Local changed description\n",
                        "state-2",
                        "In Progress",
                    )
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
            &remote_api_url,
            "push",
            "MET-35",
            "--update-description",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("sync state is `diverged`"))
        .stderr(predicate::str::contains("--update-description"));

    update_issue_mock.assert_calls(0);
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
    fs::create_dir_all(temp.path().join(".metastack/backlog/MET-13"))?;
    fs::write(
        temp.path().join(".metastack/backlog/MET-13/.linear.json"),
        r#"{
  "issue_id": "issue-3",
  "identifier": "MET-13",
  "title": "Third issue",
  "url": "https://linear.app/issues/MET-13",
  "team_key": "MET",
  "managed_files": []
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
        .stdout(predicate::str::contains("Issue Search"))
        .stdout(predicate::str::contains("Third issue"))
        .stdout(predicate::str::contains("sync: unlinked"));

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

fn write_linked_metadata(
    issue_dir: &Path,
    identifier: &str,
    title: &str,
    local_hash: Option<&str>,
    remote_hash: Option<&str>,
    last_sync_at: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    fs::write(
        issue_dir.join(".linear.json"),
        serde_json::to_string_pretty(&json!({
            "issue_id": format!("issue-{identifier}"),
            "identifier": identifier,
            "title": title,
            "url": format!("https://linear.app/issues/{identifier}"),
            "team_key": "MET",
            "project_id": "project-1",
            "project_name": "Repo Project",
            "parent_id": null,
            "parent_identifier": null,
            "local_hash": local_hash,
            "remote_hash": remote_hash,
            "last_sync_at": last_sync_at,
            "managed_files": []
        }))?,
    )?;
    Ok(())
}

#[test]
fn sync_link_in_no_interactive_mode_creates_metadata_without_hashes() -> Result<(), Box<dyn Error>>
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

    let issue_dir = temp.path().join(".metastack/backlog/manual-entry");
    fs::create_dir_all(&issue_dir)?;
    fs::write(issue_dir.join("index.md"), "# Manual notes\n")?;

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
                    vec![],
                    None,
                )
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
            "--no-interactive",
            "link",
            "MET-35",
            "--entry",
            "manual-entry",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Linked .metastack/backlog/manual-entry to MET-35.",
        ));

    let metadata = fs::read_to_string(issue_dir.join(".linear.json"))?;
    assert!(metadata.contains("\"identifier\": \"MET-35\""));
    assert!(metadata.contains("\"local_hash\": null"));
    assert!(metadata.contains("\"remote_hash\": null"));
    assert!(metadata.contains("\"last_sync_at\": null"));
    assert_eq!(
        fs::read_to_string(issue_dir.join("index.md"))?,
        "# Manual notes\n"
    );

    Ok(())
}

#[test]
fn sync_link_with_pull_hydrates_the_selected_backlog_entry() -> Result<(), Box<dyn Error>> {
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

    let issue_dir = temp.path().join(".metastack/backlog/manual-entry");
    fs::create_dir_all(&issue_dir)?;
    fs::write(issue_dir.join("index.md"), "# Manual notes\n")?;

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
                    "# Pulled after link\n",
                    vec![],
                    None,
                )
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
            "--no-interactive",
            "link",
            "MET-35",
            "--entry",
            "manual-entry",
            "--pull",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Pulled MET-35 into .metastack/backlog/manual-entry",
        ));

    let metadata = fs::read_to_string(issue_dir.join(".linear.json"))?;
    assert!(metadata.contains("\"identifier\": \"MET-35\""));
    assert!(metadata.contains("\"local_hash\": \""));
    assert!(metadata.contains("\"remote_hash\": \""));
    assert!(metadata.contains("\"last_sync_at\": \""));
    assert_eq!(
        fs::read_to_string(issue_dir.join("index.md"))?,
        "# Pulled after link\n"
    );

    Ok(())
}

#[test]
fn sync_link_does_not_write_metadata_when_the_issue_is_missing() -> Result<(), Box<dyn Error>> {
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

    let issue_dir = temp.path().join(".metastack/backlog/manual-entry");
    fs::create_dir_all(&issue_dir)?;
    fs::write(issue_dir.join("index.md"), "# Manual notes\n")?;

    server.mock(|when, then| {
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

    cli()
        .current_dir(temp.path())
        .args([
            "sync",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "--no-interactive",
            "link",
            "MET-35",
            "--entry",
            "manual-entry",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "issue `MET-35` was not found in Linear",
        ));

    assert!(!issue_dir.join(".linear.json").exists());
    assert_eq!(
        fs::read_to_string(issue_dir.join("index.md"))?,
        "# Manual notes\n"
    );

    Ok(())
}

#[test]
fn sync_status_reports_local_ahead_and_unlinked_entries_without_fetch() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
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

    let unlinked_dir = temp.path().join(".metastack/backlog/manual-entry");
    fs::create_dir_all(&unlinked_dir)?;
    fs::write(unlinked_dir.join("index.md"), "# Unlinked manual entry\n")?;

    let linked_dir = temp.path().join(".metastack/backlog/MET-35");
    fs::create_dir_all(&linked_dir)?;
    fs::write(linked_dir.join("index.md"), "# Local changes\n")?;
    write_linked_metadata(
        &linked_dir,
        "MET-35",
        "Linked ticket",
        Some("baseline"),
        Some("remote-hash"),
        Some("2026-03-18T10:15:00Z"),
    )?;

    cli()
        .current_dir(temp.path())
        .args(["sync", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Identifier"))
        .stdout(predicate::str::contains("manual-entry"))
        .stdout(predicate::str::contains("Unlinked manual entry"))
        .stdout(predicate::str::contains("unlinked"))
        .stdout(predicate::str::contains("MET-35"))
        .stdout(predicate::str::contains("Linked ticket"))
        .stdout(predicate::str::contains("local-ahead"))
        .stdout(predicate::str::contains("2026-03-18T10:15:00Z"));

    Ok(())
}

#[test]
fn sync_status_fetch_reports_remote_ahead_with_live_linear_state() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let baseline_server = MockServer::start();
    let baseline_api_url = baseline_server.url("/graphql");
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

    baseline_server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [issue_node(
                        "issue-1",
                        "MET-35",
                        "Initial linked title",
                        "Parent issue description",
                        "state-2",
                        "In Progress",
                    )]
                }
            }
        }));
    });

    baseline_server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue")
            .body_includes("\"id\":\"issue-1\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": issue_detail_node(
                    "issue-1",
                    "MET-35",
                    "Initial linked title",
                    "# Original description\n",
                    vec![],
                    None,
                )
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
            &baseline_api_url,
            "pull",
            "MET-35",
        ])
        .assert()
        .success();

    let remote_server = MockServer::start();
    let remote_api_url = remote_server.url("/graphql");
    remote_server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [issue_node(
                        "issue-1",
                        "MET-35",
                        "Fetched linked title",
                        "Parent issue description",
                        "state-2",
                        "In Progress",
                    )]
                }
            }
        }));
    });

    remote_server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue")
            .body_includes("\"id\":\"issue-1\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": issue_detail_node(
                    "issue-1",
                    "MET-35",
                    "Fetched linked title",
                    "# Remote changed description\n",
                    vec![],
                    None,
                )
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
            &remote_api_url,
            "status",
            "--fetch",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Fetched linked title"))
        .stdout(predicate::str::contains("remote-ahead"));

    Ok(())
}

#[test]
fn sync_pull_all_reports_synced_and_skipped_summary() -> Result<(), Box<dyn Error>> {
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
                    "nodes": [
                        issue_node(
                            "issue-1",
                            "MET-35",
                            "Create the technical and sync commands",
                            "Parent issue description",
                            "state-2",
                            "In Progress",
                        ),
                        issue_node(
                            "issue-2",
                            "MET-36",
                            "Batch sync another entry",
                            "Parent issue description",
                            "state-2",
                            "In Progress",
                        )
                    ]
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
                    "# Existing description\n",
                    vec![],
                    None,
                )
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue")
            .body_includes("\"id\":\"issue-2\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": issue_detail_node(
                    "issue-2",
                    "MET-36",
                    "Batch sync another entry",
                    "# Fresh description\n",
                    vec![],
                    None,
                )
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
            "pull",
            "MET-35",
        ])
        .assert()
        .success();

    let manual_dir = temp.path().join(".metastack/backlog/manual-36");
    fs::create_dir_all(&manual_dir)?;
    write_linked_metadata(
        &manual_dir,
        "MET-36",
        "Batch sync another entry",
        None,
        None,
        None,
    )?;

    cli()
        .current_dir(temp.path())
        .args([
            "sync",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "pull",
            "--all",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Pull summary: 1 synced, 1 skipped, 0 errors.",
        ));

    assert_eq!(
        fs::read_to_string(manual_dir.join("index.md"))?,
        "# Fresh description\n"
    );

    Ok(())
}

#[test]
fn sync_push_all_exits_non_zero_when_any_entry_errors() -> Result<(), Box<dyn Error>> {
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
                    "nodes": [
                        issue_node(
                            "issue-1",
                            "MET-35",
                            "Create the technical and sync commands",
                            "Parent issue description",
                            "state-2",
                            "In Progress",
                        ),
                        issue_node(
                            "issue-2",
                            "MET-36",
                            "Broken push entry",
                            "Parent issue description",
                            "state-2",
                            "In Progress",
                        )
                    ]
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
                    "# Existing description\n",
                    vec![],
                    None,
                )
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue")
            .body_includes("\"id\":\"issue-2\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": issue_detail_node(
                    "issue-2",
                    "MET-36",
                    "Broken push entry",
                    "# Remote description\n",
                    vec![],
                    None,
                )
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
            "pull",
            "MET-35",
        ])
        .assert()
        .success();

    let broken_dir = temp.path().join(".metastack/backlog/manual-36");
    fs::create_dir_all(&broken_dir)?;
    write_linked_metadata(&broken_dir, "MET-36", "Broken push entry", None, None, None)?;

    cli()
        .current_dir(temp.path())
        .args([
            "sync",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "push",
            "--all",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "Push summary: 0 synced, 1 skipped, 1 errors.",
        ))
        .stderr(predicate::str::contains(
            "`meta backlog sync push --all` completed with 1 error",
        ));

    Ok(())
}
