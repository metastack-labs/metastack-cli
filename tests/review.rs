#![allow(dead_code, unused_imports)]

include!("support/common.rs");

#[cfg(unix)]
#[test]
fn review_command_writes_critique_only_artifacts_without_mutating_linear()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("review-agent-stub");
    let output_dir = temp.path().join("agent-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");

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
    write_review_config(&config_path, &api_url, &stub_path)?;
    write_review_stub(
        &stub_path,
        r##"#!/bin/sh
cat > "$TEST_OUTPUT_DIR/payload-1.txt"
cat <<'JSON'
{"summary":"Tighten the backlog review structure.","findings":{"missing_requirements":["Name the exact review artifacts."],"unclear_scope":["Clarify the repo-scoped command path."],"validation_gaps":["Add command proofs for critique-only and apply-back."],"dependency_risks":["Agent output quality can drift across passes."],"follow_up_ideas":["Document how review fits after meta plan."]},"rewrite":"# Reviewed MET-35\n\n## Context\n\nClarify the review loop.\n\n## Validation\n\n- `cargo test`\n- `meta backlog review MET-35`\n"}
JSON
"##,
    )?;

    let issue = issue_node(
        "issue-35",
        "MET-35",
        "Review existing backlog tickets",
        "Current Linear description",
        "state-2",
        "In Progress",
    );
    mock_issue_lookup(&server, vec![issue.clone()]);
    mock_issue_detail(
        &server,
        "issue-35",
        issue_detail_node(
            "issue-35",
            "MET-35",
            "Review existing backlog tickets",
            "Current Linear description",
            Vec::new(),
            None,
        ),
    );
    let update_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": issue
                }
            }
        }));
    });

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "backlog",
            "review",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "MET-35",
            "--critique-only",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("critique-only"))
        .stdout(predicate::str::contains("MET-35"));

    let run_dir = latest_review_dir(&repo_root.join(".metastack/backlog/MET-35"))?;
    let original = fs::read_to_string(run_dir.join("original.md"))?;
    let findings = fs::read_to_string(run_dir.join("pass-01-findings.md"))?;
    let rewrite = fs::read_to_string(run_dir.join("final-proposed.md"))?;
    let summary = fs::read_to_string(run_dir.join("summary.json"))?;

    assert_eq!(original, "Current Linear description");
    assert!(findings.contains("Missing Requirements"));
    assert!(rewrite.contains("# Reviewed MET-35"));
    assert!(summary.contains("\"critique_only\": true"));
    assert!(summary.contains("\"requested\": false"));
    update_issue_mock.assert_calls(0);

    Ok(())
}

#[cfg(unix)]
#[test]
fn review_command_default_apply_updates_local_and_pushes_to_linear() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("review-agent-stub");
    let output_dir = temp.path().join("agent-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");

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
    write_review_config(&config_path, &api_url, &stub_path)?;
    write_review_stub(
        &stub_path,
        r##"#!/bin/sh
cat > "$TEST_OUTPUT_DIR/payload-1.txt"
printf '%s' '{"summary":"Ready to apply.","findings":{"missing_requirements":[],"unclear_scope":[],"validation_gaps":["Keep the apply proof."],"dependency_risks":[],"follow_up_ideas":[]},"rewrite":"# Applied Review Rewrite\n\n## Validation\n\n- `meta backlog review MET-36`\n"}'
"##,
    )?;

    let issue = issue_node(
        "issue-36",
        "MET-36",
        "Apply the review rewrite",
        "Remote description before apply",
        "state-2",
        "In Progress",
    );
    mock_issue_lookup(&server, vec![issue.clone()]);
    mock_issue_detail(
        &server,
        "issue-36",
        issue_detail_node(
            "issue-36",
            "MET-36",
            "Apply the review rewrite",
            "Remote description before apply",
            Vec::new(),
            None,
        ),
    );
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
            .body_includes("\"id\":\"issue-36\"")
            .body_includes("# Applied Review Rewrite");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": issue_node(
                        "issue-36",
                        "MET-36",
                        "Apply the review rewrite",
                        "# Applied Review Rewrite",
                        "state-2",
                        "In Progress",
                    )
                }
            }
        }));
    });

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "backlog",
            "review",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "MET-36",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("applied"));

    let issue_dir = repo_root.join(".metastack/backlog/MET-36");
    assert_eq!(
        fs::read_to_string(issue_dir.join("index.md"))?,
        "# Applied Review Rewrite\n\n## Validation\n\n- `meta backlog review MET-36`"
    );
    let run_dir = latest_review_dir(&issue_dir)?;
    let summary = fs::read_to_string(run_dir.join("summary.json"))?;
    assert!(summary.contains("\"requested\": true"));
    assert!(summary.contains("\"local_updated\": true"));
    assert!(summary.contains("\"remote_updated\": true"));
    assert_eq!(
        fs::read_to_string(run_dir.join("applied-remote-before.md"))?,
        "Remote description before apply"
    );
    update_issue_mock.assert_calls(1);

    Ok(())
}

#[cfg(unix)]
#[test]
fn review_command_linear_first_pulls_latest_description_before_review() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("review-agent-stub");
    let output_dir = temp.path().join("agent-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");

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
    write_review_config(&config_path, &api_url, &stub_path)?;
    write_review_stub(
        &stub_path,
        r##"#!/bin/sh
cat > "$TEST_OUTPUT_DIR/payload-1.txt"
printf '%s' '{"summary":"Review pass.","findings":{"missing_requirements":[],"unclear_scope":[],"validation_gaps":[],"dependency_risks":[],"follow_up_ideas":[]},"rewrite":"# Critique Only Rewrite"}'
"##,
    )?;

    // Pre-seed an old local description.
    let issue_dir = repo_root.join(".metastack/backlog/MET-37");
    fs::create_dir_all(&issue_dir)?;
    fs::write(issue_dir.join("index.md"), "# Old local description\n")?;

    let issue = issue_node(
        "issue-37",
        "MET-37",
        "Linear-first refresh",
        "# Fresh Linear description",
        "state-2",
        "In Progress",
    );
    mock_issue_lookup(&server, vec![issue.clone()]);
    mock_issue_detail(
        &server,
        "issue-37",
        issue_detail_node(
            "issue-37",
            "MET-37",
            "Linear-first refresh",
            "# Fresh Linear description",
            Vec::new(),
            None,
        ),
    );

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "backlog",
            "review",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "MET-37",
            "--critique-only",
        ])
        .assert()
        .success();

    // Verify the payload sent to the agent contains the fresh Linear description.
    let payload = fs::read_to_string(output_dir.join("payload-1.txt"))?;
    assert!(
        payload.contains("# Fresh Linear description"),
        "agent should receive the latest Linear description, not the old local one"
    );

    // After a critique-only run, the local index.md should still contain
    // the fresh Linear description pulled before the review (not the old one).
    let local_index = fs::read_to_string(issue_dir.join("index.md"))?;
    assert_eq!(
        local_index, "# Fresh Linear description",
        "local index.md should be refreshed from Linear before the review"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn review_command_reports_remote_sync_failure_with_nonzero_exit() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("review-agent-stub");
    let output_dir = temp.path().join("agent-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");

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
    write_review_config(&config_path, &api_url, &stub_path)?;
    write_review_stub(
        &stub_path,
        r##"#!/bin/sh
cat > "$TEST_OUTPUT_DIR/payload-1.txt"
printf '%s' '{"summary":"Ready to apply, but remote fails.","findings":{"missing_requirements":[],"unclear_scope":[],"validation_gaps":[],"dependency_risks":[],"follow_up_ideas":[]},"rewrite":"# Local Rewrite Survives\n"}'
"##,
    )?;

    let issue = issue_node(
        "issue-38",
        "MET-38",
        "Handle remote sync failure",
        "Remote description before failure",
        "state-2",
        "In Progress",
    );
    mock_issue_lookup(&server, vec![issue.clone()]);
    mock_issue_detail(
        &server,
        "issue-38",
        issue_detail_node(
            "issue-38",
            "MET-38",
            "Handle remote sync failure",
            "Remote description before failure",
            Vec::new(),
            None,
        ),
    );
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
            .body_includes("\"id\":\"issue-38\"");
        then.status(200).json_body(json!({
            "errors": [{
                "message": "description update rejected"
            }]
        }));
    });

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "backlog",
            "review",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "MET-38",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "failed to finish the apply-back flow",
        ))
        .stderr(predicate::str::contains("description update rejected"));

    let issue_dir = repo_root.join(".metastack/backlog/MET-38");
    assert_eq!(
        fs::read_to_string(issue_dir.join("index.md"))?,
        "# Local Rewrite Survives"
    );
    let run_dir = latest_review_dir(&issue_dir)?;
    let summary = fs::read_to_string(run_dir.join("summary.json"))?;
    assert!(summary.contains("\"requested\": true"));
    assert!(summary.contains("\"local_updated\": true"));
    assert!(summary.contains("\"remote_updated\": false"));
    assert!(summary.contains("description update rejected"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn review_command_fallback_agent_recorded_in_artifacts() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let primary_stub_path = temp.path().join("primary-agent-stub");
    let fallback_stub_path = temp.path().join("fallback-agent-stub");
    let output_dir = temp.path().join("agent-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");

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

    // Primary agent fails, fallback agent succeeds.
    write_review_stub(
        &primary_stub_path,
        r##"#!/bin/sh
exit 1
"##,
    )?;
    write_review_stub(
        &fallback_stub_path,
        r##"#!/bin/sh
cat > "$TEST_OUTPUT_DIR/fallback-payload.txt"
printf '%s' '{"summary":"Fallback agent handled this.","findings":{"missing_requirements":[],"unclear_scope":[],"validation_gaps":[],"dependency_risks":[],"follow_up_ideas":[]},"rewrite":"# Fallback Rewrite"}'
"##,
    )?;

    fs::write(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "primary-stub"

[agents.commands.primary-stub]
command = "{}"
transport = "stdin"

[agents.commands.fallback-stub]
command = "{}"
transport = "stdin"
"#,
            primary_stub_path.display(),
            fallback_stub_path.display()
        ),
    )?;

    let issue = issue_node(
        "issue-39",
        "MET-39",
        "Fallback agent scenario",
        "Current description",
        "state-2",
        "In Progress",
    );
    mock_issue_lookup(&server, vec![issue.clone()]);
    mock_issue_detail(
        &server,
        "issue-39",
        issue_detail_node(
            "issue-39",
            "MET-39",
            "Fallback agent scenario",
            "Current description",
            Vec::new(),
            None,
        ),
    );

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "backlog",
            "review",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "MET-39",
            "--critique-only",
            "--agents",
            "primary-stub,fallback-stub",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MET-39"));

    let run_dir = latest_review_dir(&repo_root.join(".metastack/backlog/MET-39"))?;
    let agent_record = fs::read_to_string(run_dir.join("pass-01-agent.json"))?;
    assert!(
        agent_record.contains("\"requested_agent\""),
        "agent record should document the requested agent"
    );
    assert!(
        agent_record.contains("\"actual_agent\": \"fallback-stub\""),
        "agent record should document the fallback agent that was actually used"
    );
    assert!(
        agent_record.contains("\"fallback_reason\""),
        "agent record should document the fallback reason"
    );

    assert!(output_dir.join("fallback-payload.txt").exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn review_config_serialization_roundtrips_through_toml() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    fs::write(
        &config_path,
        r#"[linear]
api_key = "token"

[agents]
default_agent = "codex"

[backlog_review]
agent_chain = "codex,claude,codex"
passes = 3
default_mode = "critique"
fallback_behavior = "next_agent"
"#,
    )?;

    meta()
        .env("METASTACK_CONFIG", &config_path)
        .args(["runtime", "config", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("backlog_review"))
        .stdout(predicate::str::contains("codex,claude,codex"))
        .stdout(predicate::str::contains("\"passes\": 3"))
        .stdout(predicate::str::contains("critique"))
        .stdout(predicate::str::contains("next_agent"));

    Ok(())
}

// ── helpers ──────────────────────────────────────────────────────────────────

#[cfg(unix)]
fn write_review_config(
    config_path: &Path,
    api_url: &str,
    stub_path: &Path,
) -> Result<(), Box<dyn Error>> {
    fs::write(
        config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[agents]
default_agent = "review-stub"

[agents.commands.review-stub]
command = "{}"
transport = "stdin"
"#,
            stub_path.display()
        ),
    )?;
    Ok(())
}

#[cfg(unix)]
fn write_review_stub(path: &Path, contents: &str) -> Result<(), Box<dyn Error>> {
    fs::write(path, contents)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
fn mock_issue_lookup(server: &MockServer, issues: Vec<serde_json::Value>) {
    server.mock(move |when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": issues
                }
            }
        }));
    });
}

#[cfg(unix)]
fn mock_issue_detail(server: &MockServer, issue_id: &str, issue: serde_json::Value) {
    server.mock(move |when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue")
            .body_includes(format!("\"id\":\"{issue_id}\""));
        then.status(200).json_body(json!({
            "data": {
                "issue": issue
            }
        }));
    });
}

#[cfg(unix)]
fn latest_review_dir(issue_dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let review_root = issue_dir.join("artifacts").join("review");
    let mut entries = fs::read_dir(&review_root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    entries
        .pop()
        .ok_or_else(|| format!("no review run found under `{}`", review_root.display()).into())
}
