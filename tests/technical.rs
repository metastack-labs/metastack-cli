#![allow(dead_code, unused_imports)]

include!("support/common.rs");

#[cfg(unix)]
#[test]
fn technical_command_creates_a_child_issue_and_local_backlog_files() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("technical-agent-stub");
    let output_dir = temp.path().join("agent-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let parent_issue = issue_node(
        "parent-1",
        "MET-35",
        "Create the technical and sync commands",
        "Context\n\nTechnical workflow.\n\n## Acceptance Criteria\n- Generate backlog docs from the template\n- Keep sync safe for the child ticket",
        "state-2",
        "In Progress",
    );
    let child_issue = issue_node(
        "child-1",
        "MET-36",
        "Technical: Create the technical and sync commands",
        "Technical child description",
        "state-1",
        "Todo",
    );

    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&output_dir)?;
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
    fs::create_dir_all(repo_root.join(".metastack/backlog/_TEMPLATE"))?;
    fs::write(
        repo_root.join(".metastack/backlog/_TEMPLATE/index.md"),
        "# {{BACKLOG_TITLE}}\n\nLast updated: {{TODAY}}\n\nParent: {{parent_identifier}}\n",
    )?;
    fs::write(
        repo_root.join(".metastack/backlog/_TEMPLATE/specification.md"),
        "# Specification: {{BACKLOG_TITLE}}\n\nSlug: {{BACKLOG_SLUG}}\n",
    )?;
    fs::write(
        repo_root.join(".metastack/backlog/_TEMPLATE/implementation.md"),
        "# Implementation\n\n- Fill in the workstream for {{BACKLOG_TITLE}}\n",
    )?;
    fs::write(
        repo_root.join(".metastack/backlog/_TEMPLATE/validation.md"),
        "# Validation\n\n- `meta backlog tech {{parent_identifier}}`\n- Generated on {{TODAY}}\n",
    )?;
    fs::write(
        &config_path,
        format!(
            r#"[agents]
default_agent = "technical-stub"

[agents.commands.technical-stub]
command = "{}"
transport = "stdin"
"#,
            stub_path.display()
        ),
    )?;
    fs::write(
        &stub_path,
        r##"#!/bin/sh
cat > "$TEST_OUTPUT_DIR/payload.txt"
cat <<'JSON'
{"files":[
  {"path":"index.md","contents":"# Technical: Create the technical and sync commands\n\nAgent-generated technical backlog for parent `MET-35`.\n"},
  {"path":"README.md","contents":"# Backlog Item Template\n\nThis directory is the canonical backlog for the technical child ticket.\n"},
  {"path":"checklist.md","contents":"# Checklist: Technical: Create the technical and sync commands\n\nLast updated: 2026-03-15\n"},
  {"path":"contacts.md","contents":"# Contacts: Technical: Create the technical and sync commands\n\nLast updated: 2026-03-15\n"},
  {"path":"decisions.md","contents":"# Decisions: Technical: Create the technical and sync commands\n\nLast updated: 2026-03-15\n"},
  {"path":"specification.md","contents":"# Specification: Technical: Create the technical and sync commands\n\nSlug: technical-create-the-technical-and-sync-commands\n\n## Functional Requirements\n1. Inspect the parent Linear issue before creating the child.\n2. Generate backlog docs through the configured agent.\n"},
  {"path":"implementation.md","contents":"# Implementation\n\n- Generate backlog docs from `.metastack/backlog/_TEMPLATE` through the configured agent.\n- Sync the generated docs back to Linear attachments.\n"},
  {"path":"proposed-prs.md","contents":"# Proposed PRs: Technical: Create the technical and sync commands\n\nLast updated: 2026-03-15\n\n1. `technical-create-the-technical-and-sync-commands-01`\n"},
  {"path":"risks.md","contents":"# Risks: Technical: Create the technical and sync commands\n\nLast updated: 2026-03-15\n"},
  {"path":"validation.md","contents":"# Validation\n\n- `meta backlog tech MET-35`\n- `cargo test technical_command_creates_a_child_issue_and_local_backlog_files`\n"},
  {"path":"context/README.md","contents":"# Context Index: Technical: Create the technical and sync commands\n\nLast updated: 2026-03-15\n"},
  {"path":"context/context-note-template.md","contents":"# Context Note: Parent issue snapshot\n\nLast updated: 2026-03-15\n"},
  {"path":"tasks/README.md","contents":"# Workstreams: Technical: Create the technical and sync commands\n\nLast updated: 2026-03-15\n"},
  {"path":"tasks/workstream-template.md","contents":"# Workstream: sync-surface\n\nLast updated: 2026-03-15\n"},
  {"path":"artifacts/README.md","contents":"# Artifact Index: Technical: Create the technical and sync commands\n\nLast updated: 2026-03-15\n"},
  {"path":"artifacts/artifact-template.md","contents":"# Artifact: generated-proof\n\nLast updated: 2026-03-15\n"}
]}
JSON
"##,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [parent_issue.clone(), child_issue.clone()]
                }
            }
        }));
    });

    server.mock(|when, then| {
        let parent_issue_detail = serde_json::from_str::<serde_json::Value>(
            &json!({
                "data": {
                    "issue": {
                        "id": "parent-1",
                        "identifier": "MET-35",
                        "title": "Create the technical and sync commands",
                        "description": "Context\n\nTechnical workflow.\n\n![diagram](http://127.0.0.1:0/images/issue-diagram.png)\n\n## Acceptance Criteria\n- Generate backlog docs from the template\n- Keep sync safe for the child ticket",
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
                                "body": "Need parent art\n\n![comment-shot](REPLACE_COMMENT_IMAGE)",
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
                        "attachments": { "nodes": [] },
                        "parent": {
                            "id": "meta-parent-1",
                            "identifier": "MET-10",
                            "title": "Parent context issue",
                            "url": "https://linear.app/issues/MET-10",
                            "description": "Parent issue context\n\n![parent-reference](REPLACE_PARENT_IMAGE)"
                        },
                        "children": { "nodes": [] }
                    }
                }
            })
            .to_string()
            .replace(
                "http://127.0.0.1:0/images/issue-diagram.png",
                &server.url("/images/issue-diagram.png"),
            )
            .replace("REPLACE_COMMENT_IMAGE", &server.url("/images/comment-shot.jpg"))
            .replace("REPLACE_PARENT_IMAGE", &server.url("/images/parent-reference.svg")),
        )
        .expect("parent issue detail should be valid json");
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue")
            .body_includes("\"id\":\"parent-1\"");
        then.status(200).json_body(parent_issue_detail);
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue")
            .body_includes("\"id\":\"child-1\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": issue_detail_node(
                    "child-1",
                    "MET-36",
                    "Technical: Create the technical and sync commands",
                    "Technical child description",
                    Vec::new(),
                    Some(json!({
                        "id": "parent-1",
                        "identifier": "MET-35",
                        "title": "Create the technical and sync commands",
                        "url": "https://linear.app/issues/MET-35"
                    })),
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
    let issue_labels_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query IssueLabels");
        then.status(200).json_body(json!({
            "data": {
                "issueLabels": {
                    "nodes": [{
                        "id": "label-technical",
                        "name": "technical"
                    }]
                }
            }
        }));
    });

    let create_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue")
            .body_includes("\"parentId\":\"parent-1\"")
            .body_includes("\"labelIds\":[\"label-technical\"]")
            .body_includes("Agent-generated technical backlog");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": child_issue
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue")
            .body_includes("\"id\":\"child-1\"")
            .body_includes("Agent-generated technical backlog");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": issue_node(
                        "child-1",
                        "MET-36",
                        "Technical: Create the technical and sync commands",
                        "Technical child description",
                        "state-1",
                        "Todo",
                    )
                }
            }
        }));
    });

    for path in [
        "/images/issue-diagram.png",
        "/images/comment-shot.jpg",
        "/images/parent-reference.svg",
    ] {
        server.mock(move |when, then| {
            when.method(GET).path(path);
            then.status(200).body("image-bytes");
        });
    }

    for (index, file_name) in [
        "README.md",
        "checklist.md",
        "contacts.md",
        "decisions.md",
        "implementation.md",
        "proposed-prs.md",
        "risks.md",
        "specification.md",
        "validation.md",
        "context/README.md",
        "context/context-note-template.md",
        "context/ticket-discussion.md",
        "tasks/README.md",
        "tasks/workstream-template.md",
        "artifacts/README.md",
        "artifacts/artifact-template.md",
        "artifacts/comment-1-comment-shot.jpg",
        "artifacts/issue-diagram.png",
        "artifacts/parent-parent-reference.svg",
        "artifacts/ticket-images.md",
    ]
    .into_iter()
    .enumerate()
    {
        let upload_name = std::path::Path::new(file_name)
            .file_name()
            .and_then(|value| value.to_str())
            .expect("attachment path should have a file name")
            .to_string();
        let upload_path = format!("/uploads/{}", upload_name.replace('/', "__"));
        let attachment_id = format!("attachment-{}", index + 1);
        let upload_url = server.url(&upload_path);
        let asset_url = server.url(format!("/assets/{}", upload_name.replace('/', "__")));
        let upload_asset_url = asset_url.clone();

        server.mock(move |when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("mutation UploadFile")
                .body_includes(format!("\"filename\":\"{upload_name}\""));
            then.status(200).json_body(json!({
                "data": {
                    "fileUpload": {
                        "success": true,
                        "uploadFile": {
                            "uploadUrl": upload_url,
                            "assetUrl": upload_asset_url,
                            "headers": [{
                                "key": "x-goog-content-length-range",
                                "value": "1,100000"
                            }]
                        }
                    }
                }
            }));
        });

        server.mock(move |when, then| {
            when.method(httpmock::Method::PUT).path(upload_path);
            then.status(200);
        });

        server.mock(move |when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("mutation CreateAttachment")
                .body_includes(format!("\"relativePath\":\"{file_name}\""));
            then.status(200).json_body(json!({
                "data": {
                    "attachmentCreate": {
                        "success": true,
                        "attachment": {
                            "id": attachment_id,
                            "title": file_name,
                            "url": asset_url,
                            "sourceType": "upload",
                            "metadata": {
                                "managedBy": "metastack-cli",
                                "relativePath": file_name
                            }
                        }
                    }
                }
            }));
        });
    }

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "technical",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "MET-35",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Pushed MET-36"))
        .stdout(predicate::str::contains(
            "Created technical sub-issue MET-36 under MET-35",
        ));

    let issue_dir = repo_root.join(".metastack/backlog/MET-36");
    let index = fs::read_to_string(issue_dir.join("index.md"))?;
    let readme = fs::read_to_string(issue_dir.join("README.md"))?;
    let checklist = fs::read_to_string(issue_dir.join("checklist.md"))?;
    let implementation = fs::read_to_string(issue_dir.join("implementation.md"))?;
    let proposed_prs = fs::read_to_string(issue_dir.join("proposed-prs.md"))?;
    let specification = fs::read_to_string(issue_dir.join("specification.md"))?;
    let validation = fs::read_to_string(issue_dir.join("validation.md"))?;
    let metadata = fs::read_to_string(issue_dir.join(".linear.json"))?;
    let payload = fs::read_to_string(output_dir.join("payload.txt"))?;
    let ticket_images = fs::read_to_string(issue_dir.join("artifacts/ticket-images.md"))?;
    let ticket_discussion = fs::read_to_string(issue_dir.join("context/ticket-discussion.md"))?;

    assert!(index.contains("Agent-generated technical backlog"));
    assert!(!index.contains("{{BACKLOG_TITLE}}"));
    assert!(readme.contains("canonical backlog for the technical child"));
    assert!(checklist.contains("Last updated: 2026-03-15"));
    assert!(implementation.contains("configured agent"));
    assert!(proposed_prs.contains("technical-create-the-technical-and-sync-commands-01"));
    assert!(specification.contains("Slug: technical-create-the-technical-and-sync-commands"));
    assert!(!specification.contains("{{BACKLOG_SLUG}}"));
    assert!(validation.contains("meta backlog tech MET-35"));
    assert!(metadata.contains("\"identifier\": \"MET-36\""));
    assert!(metadata.contains("\"parent_identifier\": \"MET-35\""));
    assert!(ticket_images.contains("| `issue-diagram.png` | diagram | Issue description |"));
    assert!(
        ticket_images
            .contains("| `parent-parent-reference.svg` | parent-reference | Parent description |")
    );
    assert!(
        ticket_images.contains("| `comment-1-comment-shot.jpg` | comment-shot | Need parent art |")
    );
    assert!(ticket_discussion.contains("### **Alice** (2026-03-16)"));
    assert!(ticket_discussion.contains("![comment-shot](artifacts/comment-1-comment-shot.jpg)"));
    assert!(payload.contains("Parent Linear issue"));
    assert!(payload.contains("Create the technical and sync commands"));
    assert!(payload.contains("Injected workflow contract:"));
    assert!(payload.contains("## Built-in Workflow Contract"));
    assert!(
        payload.contains("`BACKLOG_TITLE`: Technical: Create the technical and sync commands",)
    );
    assert!(payload.contains("`BACKLOG_SLUG`: technical-create-the-technical-and-sync-commands",));
    assert!(payload.contains("`TODAY`:"));
    assert!(payload.contains("context/context-note-template.md"));
    assert!(payload.contains("tasks/workstream-template.md"));
    assert!(payload.contains("artifacts/artifact-template.md"));
    assert!(payload.contains("## SCAN.md"));
    assert!(payload.contains("Selected acceptance criteria for this technical sub-ticket"));
    assert!(payload.contains("- Generate backlog docs from the template"));
    assert!(payload.contains("Parent issue context:"));
    assert!(payload.contains("Ticket discussion context:"));
    assert!(payload.contains("Localized ticket images:"));
    assert!(payload.contains("artifacts/issue-diagram.png"));
    assert!(payload.contains("artifacts/parent-parent-reference.svg"));
    assert!(payload.contains("Need parent art"));
    assert!(payload.contains("Repository directory snapshot"));
    issue_labels_mock.assert_calls(1);
    create_issue_mock.assert_calls(1);

    Ok(())
}

#[cfg(unix)]
#[test]
fn technical_command_requires_an_agent_to_generate_backlog_content() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let empty_bin = temp.path().join("empty-bin");
    let missing_config = temp.path().join("missing-metastack.toml");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let parent_issue = issue_node(
        "parent-1",
        "MET-35",
        "Create the technical and sync commands",
        "Parent issue description",
        "state-2",
        "In Progress",
    );

    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&empty_bin)?;
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
    fs::create_dir_all(repo_root.join(".metastack/backlog/_TEMPLATE"))?;
    fs::write(
        repo_root.join(".metastack/backlog/_TEMPLATE/index.md"),
        "# {{BACKLOG_TITLE}}\n",
    )?;

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [parent_issue.clone()]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue")
            .body_includes("\"id\":\"parent-1\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": issue_detail_node(
                    "parent-1",
                    "MET-35",
                    "Create the technical and sync commands",
                    "Parent issue description",
                    Vec::new(),
                    None,
                )
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
                    "issue": issue_node(
                        "child-1",
                        "MET-36",
                        "Technical: Create the technical and sync commands",
                        "Technical child description",
                        "state-1",
                        "Todo",
                    )
                }
            }
        }));
    });

    cli()
        .current_dir(&repo_root)
        .env("PATH", &empty_bin)
        .env("METASTACK_CONFIG", &missing_config)
        .args([
            "technical",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "MET-35",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires a configured local agent to generate backlog content",
        ))
        .stderr(predicate::str::contains("no agent was selected"));

    create_issue_mock.assert_calls(0);

    Ok(())
}
