#![allow(dead_code, unused_imports)]

include!("support/common.rs");

#[cfg(unix)]
#[test]
fn refine_command_writes_critique_only_artifacts_without_mutating_linear()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("refine-agent-stub");
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
    write_refine_config(&config_path, &api_url, &stub_path)?;
    write_refine_stub(
        &stub_path,
        r##"#!/bin/sh
cat > "$TEST_OUTPUT_DIR/payload-1.txt"
cat <<'JSON'
{"summary":"Tighten the ticket structure.","findings":{"missing_requirements":["Name the exact refinement artifacts."],"unclear_scope":["Clarify the repo-scoped command path."],"validation_gaps":["Add command proofs for critique-only and apply-back."],"dependency_risks":["Agent output quality can drift across passes."],"follow_up_ideas":["Document how refinement fits after meta plan."]},"rewrite":"# Refined MET-148\n\n## Context\n\nClarify the refinement loop.\n\n## Validation\n\n- `cargo test`\n- `meta issues refine MET-148`\n"}
JSON
"##,
    )?;

    let issue = issue_node(
        "issue-1",
        "MET-148",
        "Refine existing backlog tickets",
        "Current Linear description",
        "state-2",
        "In Progress",
    );
    mock_issue_lookup(&server, vec![issue.clone()]);
    mock_issue_detail(
        &server,
        "issue-1",
        issue_detail_node(
            "issue-1",
            "MET-148",
            "Refine existing backlog tickets",
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

    let assert = meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "issues",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "refine",
            "MET-148",
            "--json",
        ])
        .assert()
        .success();

    let payload: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout)?;
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["command"], "linear.issues.refine");
    assert_eq!(
        payload["result"]["reports"][0]["issue_identifier"],
        "MET-148"
    );
    assert_eq!(payload["result"]["reports"][0]["apply_requested"], false);

    let run_dir = latest_refinement_dir(&repo_root.join(".metastack/backlog/MET-148"))?;
    let original = fs::read_to_string(run_dir.join("original.md"))?;
    let findings = fs::read_to_string(run_dir.join("pass-01-findings.md"))?;
    let rewrite = fs::read_to_string(run_dir.join("final-proposed.md"))?;
    let summary = fs::read_to_string(run_dir.join("summary.json"))?;

    assert_eq!(original, "Current Linear description");
    assert!(findings.contains("Missing Requirements"));
    assert!(rewrite.contains("# Refined MET-148"));
    assert!(summary.contains("\"critique_only\": true"));
    assert!(summary.contains("\"requested\": false"));
    assert!(
        !repo_root
            .join(".metastack/backlog/MET-148/index.md")
            .exists()
    );
    update_issue_mock.assert_calls(0);

    Ok(())
}

#[cfg(unix)]
#[test]
fn refine_command_supports_multi_pass_multi_issue_runs() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("refine-agent-stub");
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
    write_refine_config(&config_path, &api_url, &stub_path)?;
    write_refine_stub(
        &stub_path,
        r##"#!/bin/sh
count_file="$TEST_OUTPUT_DIR/count.txt"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
cat > "$TEST_OUTPUT_DIR/payload-$count.txt"
case "$count" in
  1)
    printf '%s' '{"summary":"Pass 1 summary for issue one","findings":{"missing_requirements":["Issue one pass one"],"unclear_scope":[],"validation_gaps":[],"dependency_risks":[],"follow_up_ideas":[]},"rewrite":"# Issue One Pass One"}'
    ;;
  2)
    printf '%s' '{"summary":"Pass 2 summary for issue one","findings":{"missing_requirements":[],"unclear_scope":["Issue one tightened"],"validation_gaps":[],"dependency_risks":[],"follow_up_ideas":[]},"rewrite":"# Issue One Pass Two"}'
    ;;
  3)
    printf '%s' '{"summary":"Pass 1 summary for issue two","findings":{"missing_requirements":["Issue two pass one"],"unclear_scope":[],"validation_gaps":[],"dependency_risks":[],"follow_up_ideas":[]},"rewrite":"# Issue Two Pass One"}'
    ;;
  *)
    printf '%s' '{"summary":"Pass 2 summary for issue two","findings":{"missing_requirements":[],"unclear_scope":["Issue two tightened"],"validation_gaps":[],"dependency_risks":[],"follow_up_ideas":[]},"rewrite":"# Issue Two Pass Two"}'
    ;;
esac
"##,
    )?;

    let issue_one = issue_node(
        "issue-201",
        "MET-201",
        "First ticket",
        "Original description one",
        "state-2",
        "In Progress",
    );
    let issue_two = issue_node(
        "issue-202",
        "MET-202",
        "Second ticket",
        "Original description two",
        "state-2",
        "In Progress",
    );
    mock_issue_lookup(&server, vec![issue_one.clone(), issue_two.clone()]);
    mock_issue_detail(
        &server,
        "issue-201",
        issue_detail_node(
            "issue-201",
            "MET-201",
            "First ticket",
            "Original description one",
            Vec::new(),
            None,
        ),
    );
    mock_issue_detail(
        &server,
        "issue-202",
        issue_detail_node(
            "issue-202",
            "MET-202",
            "Second ticket",
            "Original description two",
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
                    "issue": issue_one
                }
            }
        }));
    });

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "issues",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "refine",
            "MET-201",
            "MET-202",
            "--passes",
            "2",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Refined 2 issue(s):"))
        .stdout(predicate::str::contains("MET-201"))
        .stdout(predicate::str::contains("MET-202"));

    let issue_one_run = latest_refinement_dir(&repo_root.join(".metastack/backlog/MET-201"))?;
    let issue_two_run = latest_refinement_dir(&repo_root.join(".metastack/backlog/MET-202"))?;
    assert!(issue_one_run.join("pass-01.json").exists());
    assert!(issue_one_run.join("pass-02.json").exists());
    assert!(issue_two_run.join("pass-01.json").exists());
    assert!(issue_two_run.join("pass-02.json").exists());
    assert!(
        fs::read_to_string(issue_one_run.join("summary.json"))?.contains("\"passes_requested\": 2")
    );
    assert!(
        fs::read_to_string(issue_two_run.join("summary.json"))?.contains("\"passes_requested\": 2")
    );

    let second_payload = fs::read_to_string(output_dir.join("payload-2.txt"))?;
    let fourth_payload = fs::read_to_string(output_dir.join("payload-4.txt"))?;
    assert!(second_payload.contains("Refinement pass: 2 of 2"));
    assert!(second_payload.contains("Pass 1 summary for issue one"));
    assert!(fourth_payload.contains("Refinement pass: 2 of 2"));
    assert!(fourth_payload.contains("Pass 1 summary for issue two"));
    update_issue_mock.assert_calls(0);

    Ok(())
}

#[cfg(unix)]
#[test]
fn refine_command_prefers_command_route_agent_over_global_default() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let global_stub_path = temp.path().join("refine-global-stub");
    let route_stub_path = temp.path().join("refine-route-stub");
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
    fs::write(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"

[onboarding]
completed = true

[agents]
default_agent = "global-stub"

[agents.routing.commands."linear.issues.refine"]
provider = "route-stub"

[agents.commands.global-stub]
command = "{}"
transport = "stdin"

[agents.commands.route-stub]
command = "{}"
transport = "stdin"
"#,
            global_stub_path.display(),
            route_stub_path.display()
        ),
    )?;
    write_refine_stub(
        &global_stub_path,
        r##"#!/bin/sh
cat > "$TEST_OUTPUT_DIR/global-payload.txt"
cat <<'JSON'
{"summary":"global","findings":{"missing_requirements":[],"unclear_scope":[],"validation_gaps":[],"dependency_risks":[],"follow_up_ideas":[]},"rewrite":"# Global"}
JSON
"##,
    )?;
    write_refine_stub(
        &route_stub_path,
        r##"#!/bin/sh
cat > "$TEST_OUTPUT_DIR/route-payload.txt"
cat <<'JSON'
{"summary":"route","findings":{"missing_requirements":[],"unclear_scope":[],"validation_gaps":[],"dependency_risks":[],"follow_up_ideas":[]},"rewrite":"# Route"}
JSON
"##,
    )?;

    let issue = issue_node(
        "issue-263",
        "MET-263",
        "Route refine agent",
        "Current Linear description",
        "state-2",
        "In Progress",
    );
    mock_issue_lookup(&server, vec![issue.clone()]);
    mock_issue_detail(
        &server,
        "issue-263",
        issue_detail_node(
            "issue-263",
            "MET-263",
            "Route refine agent",
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
            "issues",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "refine",
            "MET-263",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MET-263"));

    assert!(output_dir.join("route-payload.txt").exists());
    assert!(!output_dir.join("global-payload.txt").exists());
    update_issue_mock.assert_calls(0);

    Ok(())
}

#[cfg(unix)]
#[test]
fn refine_command_apply_updates_local_backlog_and_linear_description() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("refine-agent-stub");
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
    write_refine_config(&config_path, &api_url, &stub_path)?;
    write_refine_stub(
        &stub_path,
        r##"#!/bin/sh
cat > "$TEST_OUTPUT_DIR/payload-1.txt"
printf '%s' '{"summary":"Ready to apply.","findings":{"missing_requirements":[],"unclear_scope":[],"validation_gaps":["Keep the apply proof."],"dependency_risks":[],"follow_up_ideas":[]},"rewrite":"# Applied Rewrite\n\n## Validation\n\n- `meta issues refine MET-301 --apply`\n"}'
"##,
    )?;

    let issue_dir = repo_root.join(".metastack/backlog/MET-301");
    fs::create_dir_all(&issue_dir)?;
    fs::write(issue_dir.join("index.md"), "# Old Local Index\n")?;

    let issue = issue_node(
        "issue-301",
        "MET-301",
        "Apply the rewrite",
        "Remote description before apply",
        "state-2",
        "In Progress",
    );
    mock_issue_lookup(&server, vec![issue.clone()]);
    mock_issue_detail(
        &server,
        "issue-301",
        issue_detail_node(
            "issue-301",
            "MET-301",
            "Apply the rewrite",
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
            .body_includes("\"id\":\"issue-301\"")
            .body_includes("# Applied Rewrite");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": issue_node(
                        "issue-301",
                        "MET-301",
                        "Apply the rewrite",
                        "# Applied Rewrite",
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
            "issues",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "refine",
            "MET-301",
            "--apply",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("applied"));

    assert_eq!(
        fs::read_to_string(issue_dir.join("index.md"))?,
        "# Applied Rewrite\n\n## Validation\n\n- `meta issues refine MET-301 --apply`"
    );
    let run_dir = latest_refinement_dir(&issue_dir)?;
    let summary = fs::read_to_string(run_dir.join("summary.json"))?;
    assert!(summary.contains("\"requested\": true"));
    assert!(summary.contains("\"local_updated\": true"));
    assert!(summary.contains("\"remote_updated\": true"));
    assert_eq!(
        fs::read_to_string(run_dir.join("applied-local-before.md"))?,
        "# Old Local Index\n"
    );
    assert_eq!(
        fs::read_to_string(run_dir.join("applied-remote-before.md"))?,
        "Remote description before apply"
    );
    update_issue_mock.assert_calls(1);

    Ok(())
}

#[cfg(unix)]
#[test]
fn refine_command_apply_persists_local_update_when_linear_update_fails()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("refine-agent-stub");
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
    write_refine_config(&config_path, &api_url, &stub_path)?;
    write_refine_stub(
        &stub_path,
        r##"#!/bin/sh
cat > "$TEST_OUTPUT_DIR/payload-1.txt"
printf '%s' '{"summary":"Ready to apply, but remote update fails.","findings":{"missing_requirements":[],"unclear_scope":[],"validation_gaps":[],"dependency_risks":["Remote mutations can fail after the local packet is updated."],"follow_up_ideas":[]},"rewrite":"# Local Rewrite Survives\n"}'
"##,
    )?;

    let issue_dir = repo_root.join(".metastack/backlog/MET-302");
    fs::create_dir_all(&issue_dir)?;
    fs::write(issue_dir.join("index.md"), "# Previous Local Index\n")?;

    let issue = issue_node(
        "issue-302",
        "MET-302",
        "Handle apply failures",
        "Remote description before failure",
        "state-2",
        "In Progress",
    );
    mock_issue_lookup(&server, vec![issue.clone()]);
    mock_issue_detail(
        &server,
        "issue-302",
        issue_detail_node(
            "issue-302",
            "MET-302",
            "Handle apply failures",
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
    let update_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue")
            .body_includes("\"id\":\"issue-302\"")
            .body_includes("# Local Rewrite Survives");
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
            "issues",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "refine",
            "MET-302",
            "--apply",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "failed to finish the apply-back flow",
        ))
        .stderr(predicate::str::contains("description update rejected"));

    assert_eq!(
        fs::read_to_string(issue_dir.join("index.md"))?,
        "# Local Rewrite Survives"
    );
    let run_dir = latest_refinement_dir(&issue_dir)?;
    let summary = fs::read_to_string(run_dir.join("summary.json"))?;
    assert!(summary.contains("\"requested\": true"));
    assert!(summary.contains("\"local_updated\": true"));
    assert!(summary.contains("\"remote_updated\": false"));
    assert!(summary.contains("description update rejected"));
    update_issue_mock.assert_calls(1);

    Ok(())
}

#[cfg(unix)]
#[test]
fn refine_command_rejects_issues_outside_the_configured_repo_scope() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("refine-agent-stub");
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
    write_refine_config(&config_path, &api_url, &stub_path)?;
    write_refine_stub(
        &stub_path,
        r##"#!/bin/sh
cat > "$TEST_OUTPUT_DIR/payload-1.txt"
printf '%s' '{"summary":"This should never run.","findings":{"missing_requirements":[],"unclear_scope":[],"validation_gaps":[],"dependency_risks":[],"follow_up_ideas":[]},"rewrite":"# Unexpected"}'
"##,
    )?;

    let lookup_issue = issue_node(
        "issue-401",
        "MET-401",
        "Outside the configured scope",
        "Original description",
        "state-2",
        "In Progress",
    );
    mock_issue_lookup(&server, vec![lookup_issue]);
    mock_issue_detail(
        &server,
        "issue-401",
        json!({
            "id": "issue-401",
            "identifier": "MET-401",
            "title": "Outside the configured scope",
            "description": "Original description",
            "url": "https://linear.app/issues/MET-401",
            "priority": 2,
            "updatedAt": "2026-03-14T16:00:00Z",
            "team": {
                "id": "team-2",
                "key": "OPS",
                "name": "Operations"
            },
            "project": {
                "id": "project-2",
                "name": "Other Project"
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
        }),
    );

    meta()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "issues",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "refine",
            "MET-401",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "outside the configured repo team scope `MET`",
        ));

    assert!(!repo_root.join(".metastack/backlog/MET-401").exists());
    assert!(!output_dir.join("payload-1.txt").exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn refine_command_apply_is_blocked_for_the_active_issue_during_meta_listen()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("refine-agent-stub");
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
    write_refine_config(&config_path, &api_url, &stub_path)?;
    write_refine_stub(
        &stub_path,
        r##"#!/bin/sh
cat > "$TEST_OUTPUT_DIR/payload-1.txt"
printf '%s' '{"summary":"Rewrite blocked from apply during listen.","findings":{"missing_requirements":[],"unclear_scope":[],"validation_gaps":["Keep the active-issue guard."],"dependency_risks":[],"follow_up_ideas":[]},"rewrite":"# Listen Guard Rewrite\n"}'
"##,
    )?;

    let issue = issue_node(
        "issue-402",
        "MET-402",
        "Guard active issue apply",
        "Remote description before guard",
        "state-2",
        "In Progress",
    );
    mock_issue_lookup(&server, vec![issue.clone()]);
    mock_issue_detail(
        &server,
        "issue-402",
        issue_detail_node(
            "issue-402",
            "MET-402",
            "Guard active issue apply",
            "Remote description before guard",
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
        .env("METASTACK_LISTEN_UNATTENDED", "1")
        .env("METASTACK_LINEAR_ISSUE_IDENTIFIER", "MET-402")
        .args([
            "issues",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "refine",
            "MET-402",
            "--apply",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "disabled during `meta listen` because it would overwrite the primary Linear issue description",
        ));

    let issue_dir = repo_root.join(".metastack/backlog/MET-402");
    let run_dir = latest_refinement_dir(&issue_dir)?;
    let summary = fs::read_to_string(run_dir.join("summary.json"))?;

    assert!(!issue_dir.join("index.md").exists());
    assert!(summary.contains("\"requested\": true"));
    assert!(summary.contains("\"local_updated\": false"));
    assert!(summary.contains("\"remote_updated\": false"));
    assert!(summary.contains("disabled during `meta listen`"));
    update_issue_mock.assert_calls(0);

    Ok(())
}

#[cfg(unix)]
fn write_refine_config(
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

[onboarding]
completed = true

[agents]
default_agent = "refine-stub"

[agents.commands.refine-stub]
command = "{}"
transport = "stdin"
"#,
            stub_path.display()
        ),
    )?;
    Ok(())
}

#[cfg(unix)]
fn write_refine_stub(path: &Path, contents: &str) -> Result<(), Box<dyn Error>> {
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
fn latest_refinement_dir(issue_dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let refinement_root = issue_dir.join("artifacts").join("refinement");
    let mut entries = fs::read_dir(&refinement_root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    entries.pop().ok_or_else(|| {
        format!(
            "no refinement run found under `{}`",
            refinement_root.display()
        )
        .into()
    })
}
