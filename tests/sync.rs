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
            .body_includes("query Issue($id: String!)")
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
fn sync_pull_rebuilds_discussion_context_downloads_images_with_auth_and_reuses_known_files()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    write_minimal_planning_context(
        temp.path(),
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "sync": {
    "discussion_file_char_limit": 500
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
                "issue": sync_issue_detail_node_with_comments(
                    "issue-1",
                    "MET-35",
                    "Create the technical and sync commands",
                    "# Pulled description\n",
                    vec![
                        sync_comment_node(
                            "comment-1",
                            "Taylor",
                            "2026-03-18T14:00:00Z",
                            &format!(
                                "Need the updated flow.\n\n![mockup]({})",
                                server.url("/uploads/mockup.png")
                            ),
                        ),
                        sync_comment_node(
                            "comment-2",
                            "Morgan",
                            "2026-03-18T15:00:00Z",
                            "Follow-up note.",
                        ),
                        sync_comment_node(
                            "comment-workpad",
                            "Codex",
                            "2026-03-18T15:30:00Z",
                            "## Codex Workpad\n\nIgnore this generated note.",
                        ),
                        sync_comment_node(
                            "comment-sync",
                            "Harness",
                            "2026-03-18T16:00:00Z",
                            "[harness-sync]\nIgnore this generated progress note.",
                        ),
                    ],
                    false,
                    None,
                    vec![],
                    None,
                )
            }
        }));
    });

    let image_mock = server.mock(|when, then| {
        when.method(httpmock::Method::GET)
            .path("/uploads/mockup.png");
        then.status(200).body("fake-image");
    });

    for _ in 0..2 {
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
            .stdout(predicate::str::contains("rebuilt discussion context"));
    }

    image_mock.assert_calls(1);

    let issue_dir = temp.path().join(".metastack/backlog/MET-35");
    let discussion = fs::read_to_string(issue_dir.join("context/ticket-discussion.md"))?;
    assert!(discussion.contains("### **Taylor** (2026-03-18)"));
    assert!(discussion.contains("### **Morgan** (2026-03-18)"));
    assert!(discussion.contains("![mockup](artifacts/comment-1-mockup.png)"));
    assert!(!discussion.contains("## Codex Workpad"));
    assert!(!discussion.contains("[harness-sync]"));

    let manifest = fs::read_to_string(issue_dir.join("artifacts/ticket-images.md"))?;
    assert!(manifest.contains("/uploads/mockup.png"));

    let metadata = fs::read_to_string(issue_dir.join(".linear.json"))?;
    assert!(metadata.contains("\"last_pulled_comment_ids\""));
    assert!(metadata.contains("\"comment-1\""));
    assert!(metadata.contains("\"comment-2\""));
    assert!(!metadata.contains("\"comment-workpad\""));
    assert!(!metadata.contains("\"comment-sync\""));
    assert_eq!(
        fs::read(issue_dir.join("artifacts/comment-1-mockup.png"))?,
        b"fake-image"
    );

    Ok(())
}

#[test]
fn sync_pull_fetches_paginated_issue_comments() -> Result<(), Box<dyn Error>> {
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

    let first_page_comments = (1..=50)
        .map(|index| {
            sync_comment_node(
                &format!("comment-{index}"),
                "Taylor",
                &format!("2026-03-18T14:{:02}:00Z", index % 60),
                &format!("Comment {index}"),
            )
        })
        .collect::<Vec<_>>();

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

    server.mock(move |when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-1\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": sync_issue_detail_node_with_comments(
                    "issue-1",
                    "MET-35",
                    "Create the technical and sync commands",
                    "# Pulled description\n",
                    first_page_comments.clone(),
                    true,
                    Some("cursor-1"),
                    vec![],
                    None,
                )
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query IssueComments")
            .body_includes("\"after\":\"cursor-1\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": {
                    "comments": {
                        "nodes": [
                            sync_comment_node(
                                "comment-51",
                                "Morgan",
                                "2026-03-18T16:00:00Z",
                                "Comment 51"
                            )
                        ],
                        "pageInfo": {
                            "hasNextPage": false,
                            "endCursor": null
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
            "pull",
            "MET-35",
        ])
        .assert()
        .success();

    let discussion = fs::read_to_string(
        temp.path()
            .join(".metastack/backlog/MET-35/context/ticket-discussion.md"),
    )?;
    assert!(discussion.contains("Comment 1"));
    assert!(discussion.contains("Comment 50"));
    assert!(discussion.contains("Comment 51"));

    Ok(())
}

#[test]
fn sync_push_updates_a_single_harness_sync_comment_and_skips_generated_discussion_artifacts()
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

    fs::create_dir_all(issue_dir.join("context"))?;
    fs::create_dir_all(issue_dir.join("artifacts/ticket-images"))?;
    fs::write(issue_dir.join("index.md"), "# Updated description\n")?;
    fs::write(
        issue_dir.join("checklist.md"),
        "# Checklist\n\n## Milestone A\n- [x] done\n- [ ] todo\n\n## Milestone B\n- [x] done\n- [x] done\n- [ ] todo\n",
    )?;
    fs::write(
        issue_dir.join("implementation.md"),
        "# Local implementation\n",
    )?;
    fs::write(
        issue_dir.join("context/ticket-discussion.md"),
        "# Machine owned discussion file\n",
    )?;
    fs::write(
        issue_dir.join("artifacts/ticket-images.md"),
        "# Machine owned image manifest\n",
    )?;
    fs::write(
        issue_dir.join("artifacts/ticket-images/comment-1-01.png"),
        "fake-image",
    )?;
    fs::write(
        issue_dir.join("artifacts/issue-shot.png"),
        "localized-image",
    )?;
    fs::write(
        issue_dir.join(".ticket-context.json"),
        serde_json::to_string_pretty(&json!({
            "ignored_paths": [
                "context/ticket-discussion.md",
                "artifacts/ticket-images.md",
                "artifacts/issue-shot.png"
            ]
        }))?,
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
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-1\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": sync_issue_detail_node_with_comments(
                    "issue-1",
                    "MET-35",
                    "Create the technical and sync commands",
                    "Parent issue description",
                    vec![sync_comment_node(
                        "comment-sync",
                        "Harness",
                        "2026-03-18T17:00:00Z",
                        "[harness-sync]\nold progress"
                    )],
                    false,
                    None,
                    vec![json!({
                        "id": "managed-attachment",
                        "title": "implementation.md",
                        "url": server.url("/assets/old-implementation.md"),
                        "sourceType": "upload",
                        "metadata": {
                            "managedBy": "metastack-cli",
                            "relativePath": "implementation.md"
                        }
                    })],
                    None,
                )
            }
        }));
    });

    let update_comment_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateComment")
            .body_includes("\"id\":\"comment-sync\"")
            .body_includes("Milestone A -- 1/2 complete")
            .body_includes("Milestone B -- 2/3 complete")
            .body_includes("Overall: 60% (3/5)");
        then.status(200).json_body(json!({
            "data": {
                "commentUpdate": {
                    "success": true,
                    "comment": {
                        "id": "comment-sync",
                        "body": "[harness-sync]\nupdated progress",
                        "resolvedAt": null
                    }
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

    let upload_implementation_mock = server.mock(|when, then| {
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

    let upload_checklist_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UploadFile")
            .body_includes("\"filename\":\"checklist.md\"");
        then.status(200).json_body(json!({
            "data": {
                "fileUpload": {
                    "success": true,
                    "uploadFile": {
                        "uploadUrl": server.url("/uploads/checklist.md"),
                        "assetUrl": server.url("/assets/checklist.md"),
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
        when.method(httpmock::Method::PUT)
            .path("/uploads/checklist.md");
        then.status(200);
    });

    let create_implementation_attachment_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateAttachment")
            .body_includes("\"relativePath\":\"implementation.md\"");
        then.status(200).json_body(json!({
            "data": {
                "attachmentCreate": {
                    "success": true,
                    "attachment": {
                        "id": "attachment-implementation",
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

    let create_checklist_attachment_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateAttachment")
            .body_includes("\"relativePath\":\"checklist.md\"");
        then.status(200).json_body(json!({
            "data": {
                "attachmentCreate": {
                    "success": true,
                    "attachment": {
                        "id": "attachment-checklist",
                        "title": "checklist.md",
                        "url": server.url("/assets/checklist.md"),
                        "sourceType": "upload",
                        "metadata": {
                            "managedBy": "metastack-cli",
                            "relativePath": "checklist.md"
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
        .stdout(predicate::str::contains(
            "synced 2 managed attachment files",
        ))
        .stdout(predicate::str::contains(
            "updated [harness-sync] progress comment",
        ));

    update_comment_mock.assert_calls(1);
    upload_implementation_mock.assert_calls(1);
    upload_checklist_mock.assert_calls(1);
    create_implementation_attachment_mock.assert_calls(1);
    create_checklist_attachment_mock.assert_calls(1);

    Ok(())
}

#[test]
fn sync_push_groups_checklist_progress_by_nested_markdown_headings() -> Result<(), Box<dyn Error>> {
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
        issue_dir.join("checklist.md"),
        "# Checklist\n\n## Implementation\n\n### Comment sync\n- [x] pull comments\n- [ ] rewrite images\n\n### Progress sync\n- [x] parse checklist\n- [x] update comment\n",
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
                "issue": sync_issue_detail_node_with_comments(
                    "issue-1",
                    "MET-35",
                    "Create the technical and sync commands",
                    "Parent issue description",
                    vec![sync_comment_node(
                        "comment-sync",
                        "Harness",
                        "2026-03-18T17:00:00Z",
                        "[harness-sync]\nold progress"
                    )],
                    false,
                    None,
                    vec![],
                    None,
                )
            }
        }));
    });

    let update_comment_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateComment")
            .body_includes("\"id\":\"comment-sync\"")
            .body_includes("Comment sync -- 1/2 complete")
            .body_includes("Progress sync -- 2/2 complete")
            .body_includes("Overall: 75% (3/4)");
        then.status(200).json_body(json!({
            "data": {
                "commentUpdate": {
                    "success": true,
                    "comment": {
                        "id": "comment-sync",
                        "body": "[harness-sync]\nupdated progress",
                        "resolvedAt": null
                    }
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UploadFile")
            .body_includes("\"filename\":\"checklist.md\"");
        then.status(200).json_body(json!({
            "data": {
                "fileUpload": {
                    "success": true,
                    "uploadFile": {
                        "uploadUrl": server.url("/uploads/checklist.md"),
                        "assetUrl": server.url("/assets/checklist.md"),
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
            .path("/uploads/checklist.md");
        then.status(200);
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateAttachment")
            .body_includes("\"relativePath\":\"checklist.md\"");
        then.status(200).json_body(json!({
            "data": {
                "attachmentCreate": {
                    "success": true,
                    "attachment": {
                        "id": "attachment-checklist",
                        "title": "checklist.md",
                        "url": server.url("/assets/checklist.md"),
                        "sourceType": "upload",
                        "metadata": {
                            "managedBy": "metastack-cli",
                            "relativePath": "checklist.md"
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
        .success();

    update_comment_mock.assert_calls(1);

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn sync_issue_detail_node_with_comments(
    id: &str,
    identifier: &str,
    title: &str,
    description: &str,
    comments: Vec<serde_json::Value>,
    has_next_page: bool,
    end_cursor: Option<&str>,
    attachments: Vec<serde_json::Value>,
    parent: Option<serde_json::Value>,
) -> serde_json::Value {
    json!({
        "id": id,
        "identifier": identifier,
        "title": title,
        "description": description,
        "url": format!("https://linear.app/issues/{identifier}"),
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
            "nodes": comments,
            "pageInfo": {
                "hasNextPage": has_next_page,
                "endCursor": end_cursor
            }
        },
        "state": {
            "id": "state-1",
            "name": "Todo",
            "type": "unstarted"
        },
        "attachments": { "nodes": attachments },
        "parent": parent,
        "children": { "nodes": [] }
    })
}

fn sync_comment_node(
    id: &str,
    author_name: &str,
    created_at: &str,
    body: &str,
) -> serde_json::Value {
    json!({
        "id": id,
        "body": body,
        "createdAt": created_at,
        "user": {
            "id": format!("user-{id}"),
            "name": author_name,
            "email": format!("{author_name}@example.com")
        },
        "resolvedAt": null
    })
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
fn sync_push_identifier_stays_linked_entry_driven_outside_dashboard() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let issue_dir = temp.path().join(".metastack/backlog/generated-child");
    write_minimal_planning_context(
        temp.path(),
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;

    fs::create_dir_all(&issue_dir)?;
    fs::write(issue_dir.join("index.md"), "# Local child backlog\n")?;
    write_linked_metadata(
        &issue_dir,
        "MET-77",
        "Linked child backlog issue",
        None,
        None,
        None,
    )?;

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [issue_node(
                        "issue-77",
                        "MET-77",
                        "Linked child backlog issue",
                        "Remote child issue",
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
            .body_includes("\"id\":\"issue-77\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": issue_detail_node(
                    "issue-77",
                    "MET-77",
                    "Linked child backlog issue",
                    "Remote child issue",
                    Vec::new(),
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
            "push",
            "MET-77",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Pushed MET-77 from .metastack/backlog/generated-child",
        ))
        .stdout(predicate::str::contains(
            "synced 0 managed attachment files",
        ));

    assert!(!temp.path().join(".metastack/backlog/MET-77").exists());
    let metadata = fs::read_to_string(issue_dir.join(".linear.json"))?;
    assert!(metadata.contains("\"identifier\": \"MET-77\""));
    assert!(metadata.contains("\"issue_id\": \"issue-77\""));
    assert!(metadata.contains("\"local_hash\":"));
    assert!(metadata.contains("\"remote_hash\":"));

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
fn sync_render_once_uses_local_backlog_entries_and_only_hydrates_linked_rows()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let server = MockServer::start();
    let api_url = server.url("/graphql");

    write_minimal_planning_context(
        temp.path(),
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    let linked_dir = temp.path().join(".metastack/backlog/linked-entry");
    fs::create_dir_all(&linked_dir)?;
    fs::write(linked_dir.join("index.md"), "# Linked entry\n")?;
    fs::write(
        linked_dir.join(".linear.json"),
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
    let unlinked_dir = temp.path().join(".metastack/backlog/manual-entry");
    fs::create_dir_all(&unlinked_dir)?;
    fs::write(unlinked_dir.join("index.md"), "# Manual entry\n")?;

    let linked_issue_lookup = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "token")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [issue_node(
                        "issue-3",
                        "MET-13",
                        "Third issue",
                        "Second page issue",
                        "state-2",
                        "In Progress",
                    )],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        }));
    });

    let linked_issue_detail = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "token")
            .body_includes("query Issue")
            .body_includes("\"id\":\"issue-3\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": issue_detail_node(
                    "issue-3",
                    "MET-13",
                    "Third issue",
                    "Second page issue",
                    Vec::new(),
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
            "--render-once",
            "--events",
            "enter,down,enter",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "hint: `meta sync` is a compatibility alias; prefer `meta backlog sync`.",
        ))
        .stdout(predicate::str::contains("meta backlog sync"))
        .stdout(predicate::str::contains("Ready to push MET-13"))
        .stdout(predicate::str::contains("Backlog Search"))
        .stdout(predicate::str::contains("Third issue"))
        .stdout(predicate::str::contains("manual-entry"))
        .stdout(predicate::str::contains("sync: unlinked"));

    linked_issue_lookup.assert();
    linked_issue_detail.assert();

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
    let linked_dir = repo_root.join(".metastack/backlog/MET-210");
    fs::create_dir_all(&linked_dir)?;
    write_linked_metadata(
        &linked_dir,
        "MET-210",
        "Repo default sync issue",
        None,
        None,
        None,
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
    let issue_dir = repo_root.join(".metastack/backlog/MET-210");
    fs::create_dir_all(&issue_dir)?;
    fs::write(issue_dir.join("index.md"), "# Repo default sync issue\n")?;
    write_linked_metadata(
        &issue_dir,
        "MET-210",
        "Repo default sync issue",
        Some("baseline"),
        Some("remote-baseline"),
        Some("2026-03-18T10:15:00Z"),
    )?;

    let issues_mock = right_server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "repo-token")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [issue_node(
                        "issue-selected",
                        "MET-210",
                        "Repo default sync issue",
                        "Selected from the repo-scoped Linear project",
                        "state-1",
                        "Todo",
                    )]
                }
            }
        }));
    });
    let issue_detail_mock = right_server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "repo-token")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"issue-selected\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": issue_detail_node(
                    "issue-selected",
                    "MET-210",
                    "Repo default sync issue",
                    "Selected from the repo-scoped Linear project",
                    Vec::new(),
                    None,
                )
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
        .stdout(predicate::str::contains("Backlog Search"));

    issues_mock.assert();
    issue_detail_mock.assert();
    Ok(())
}

#[test]
fn sync_render_once_prefers_linked_backlog_children_over_project_rows() -> Result<(), Box<dyn Error>>
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

    let issue_dir = temp.path().join(".metastack/backlog/MET-36");
    fs::create_dir_all(&issue_dir)?;
    fs::write(issue_dir.join("index.md"), "# Child backlog docs\n")?;
    write_linked_metadata(
        &issue_dir,
        "MET-36",
        "Technical child sync issue",
        Some("baseline"),
        Some("remote-baseline"),
        Some("2026-03-18T10:15:00Z"),
    )?;

    let issues_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [
                        issue_node(
                            "parent-1",
                            "MET-35",
                            "Parent issue",
                            "Parent issue description",
                            "state-2",
                            "In Progress",
                        ),
                        issue_node(
                            "child-1",
                            "MET-36",
                            "Technical child sync issue",
                            "Technical child description",
                            "state-1",
                            "Todo",
                        )
                    ]
                }
            }
        }));
    });
    let child_detail_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)")
            .body_includes("\"id\":\"child-1\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": issue_detail_node(
                    "child-1",
                    "MET-36",
                    "Technical child sync issue",
                    "Technical child description",
                    Vec::new(),
                    Some(json!({
                        "id": "parent-1",
                        "identifier": "MET-35",
                        "title": "Parent issue",
                        "url": "https://linear.app/issues/MET-35"
                    })),
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
            "--render-once",
            "--events",
            "enter,down,enter",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "hint: `meta sync` is a compatibility alias; prefer `meta backlog sync`.",
        ))
        .stdout(predicate::str::contains("Ready to push MET-36"))
        .stdout(predicate::str::contains("Backlog Entries (1/1)"))
        .stdout(predicate::str::contains("Parent issue").not());

    issues_mock.assert();
    child_detail_mock.assert();
    Ok(())
}

#[test]
fn sync_without_subcommand_renders_local_backlog_without_default_project_configuration()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
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
    let issue_dir = temp.path().join(".metastack/backlog/manual-entry");
    fs::create_dir_all(&issue_dir)?;
    fs::write(issue_dir.join("index.md"), "# Manual entry\n")?;

    cli()
        .current_dir(temp.path())
        .args([
            "sync",
            "--api-key",
            "token",
            "--render-once",
            "--events",
            "enter,down,enter",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "hint: `meta sync` is a compatibility alias; prefer `meta backlog sync`.",
        ))
        .stdout(predicate::str::contains("manual-entry"))
        .stdout(predicate::str::contains("state: Unlinked"))
        .stdout(predicate::str::contains("link required"))
        .stdout(predicate::str::contains("meta backlog sync link"))
        .stdout(predicate::str::contains("--entry manual-entry"))
        .stdout(predicate::str::contains("Ready to push").not());

    Ok(())
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
