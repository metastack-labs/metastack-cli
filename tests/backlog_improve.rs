#![allow(dead_code, unused_imports)]

include!("support/common.rs");

#[cfg(unix)]
#[test]
fn backlog_improve_scans_repo_backlog_and_writes_proposal_artifacts() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("backlog-improve-stub");
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
    write_backlog_improve_config(&config_path, &api_url, &stub_path)?;
    write_backlog_improve_stub(
        &stub_path,
        r##"#!/bin/sh
cat > "$TEST_OUTPUT_DIR/payload-1.txt"
cat <<'JSON'
{"summary":"Needs a small cleanup before execution.","needs_improvement":true,"findings":{"title_gaps":["Title should say what gets improved."],"description_gaps":["Acceptance criteria are missing from the body."],"acceptance_criteria_gaps":["Add executable acceptance criteria."],"metadata_gaps":["Missing the planning label and estimate."],"structure_opportunities":[]},"proposal":{"title":"Improve backlog hygiene workflow","description":"# Improve backlog hygiene workflow\n\n## Acceptance Criteria\n\n- `meta backlog improve` scans repo backlog issues\n- Proposal artifacts are stored under `.metastack/backlog/MET-510/artifacts/improvement/`\n","priority":2,"estimate":3,"labels":["plan"," plan "],"acceptance_criteria":["`meta backlog improve` scans repo backlog issues","Proposal artifacts are stored under `.metastack/backlog/MET-510/artifacts/improvement/`"]}}
JSON
"##,
    )?;

    let issue_dir = repo_root.join(".metastack/backlog/MET-510");
    fs::create_dir_all(&issue_dir)?;
    fs::write(issue_dir.join("index.md"), "# Existing local packet\n")?;

    mock_issue_list(
        &server,
        vec![
            issue_node(
                "issue-510",
                "MET-510",
                "Backlog cleanup",
                "Current description",
                "state-backlog",
                "Backlog",
            ),
            issue_node(
                "issue-511",
                "MET-511",
                "Sibling backlog item",
                "Sibling description",
                "state-backlog",
                "Backlog",
            ),
        ],
    );
    let update_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": issue_node(
                        "issue-510",
                        "MET-510",
                        "Improve backlog hygiene workflow",
                        "Current description",
                        "state-backlog",
                        "Backlog",
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
            "improve",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "--limit",
            "5",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Improved 2 issue(s):"))
        .stdout(predicate::str::contains("MET-510: basic proposal only"))
        .stdout(predicate::str::contains("MET-511: basic proposal only"));

    let run_dir = latest_improvement_dir(&issue_dir)?;
    assert_eq!(
        fs::read_to_string(run_dir.join("original.md"))?,
        "Current description"
    );
    assert_eq!(
        fs::read_to_string(run_dir.join("local-index.md"))?,
        "# Existing local packet\n"
    );
    assert!(fs::read_to_string(run_dir.join("proposal.md"))?.contains("## Proposed Changes"));
    let summary = fs::read_to_string(run_dir.join("summary.json"))?;
    assert!(summary.contains("\"needs_improvement\": true"));
    assert!(summary.contains("\"requested\": false"));
    assert!(summary.contains("\"local_updated\": false"));
    assert!(summary.contains("\"remote_updated\": false"));
    assert_eq!(
        fs::read_to_string(issue_dir.join("index.md"))?,
        "# Existing local packet\n"
    );

    let payload = fs::read_to_string(output_dir.join("payload-1.txt"))?;
    assert!(payload.contains("Improvement mode: `basic`"));
    assert!(payload.contains("Current local backlog index snapshot:"));
    assert!(payload.contains("Related repo-scoped backlog issues:"));
    update_issue_mock.assert_calls(0);

    Ok(())
}

#[cfg(unix)]
#[test]
fn backlog_improve_apply_updates_local_packet_and_linear_issue() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("backlog-improve-stub");
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
    write_backlog_improve_config(&config_path, &api_url, &stub_path)?;
    write_backlog_improve_stub(
        &stub_path,
        r##"#!/bin/sh
cat > "$TEST_OUTPUT_DIR/payload-1.txt"
printf '%s' '{"summary":"Ready to apply.","needs_improvement":true,"findings":{"title_gaps":[],"description_gaps":[],"acceptance_criteria_gaps":[],"metadata_gaps":["Set an estimate before execution."],"structure_opportunities":["Parenting is already fine."]},"proposal":{"title":"Applied backlog improvement","description":"# Applied backlog improvement\n\n## Acceptance Criteria\n\n- `meta backlog improve MET-610 --mode advanced --apply` updates the local packet before Linear\n","priority":1,"estimate":5,"acceptance_criteria":["`meta backlog improve MET-610 --mode advanced --apply` updates the local packet before Linear"]}}'
"##,
    )?;

    let issue_dir = repo_root.join(".metastack/backlog/MET-610");
    fs::create_dir_all(&issue_dir)?;
    fs::write(issue_dir.join("index.md"), "# Previous local packet\n")?;

    let issue = issue_node(
        "issue-610",
        "MET-610",
        "Old backlog title",
        "Remote description before apply",
        "state-backlog",
        "Backlog",
    );
    mock_issue_list(&server, vec![issue.clone()]);
    mock_issue_detail(
        &server,
        "issue-610",
        issue_detail_node(
            "issue-610",
            "MET-610",
            "Old backlog title",
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
            .body_includes("\"id\":\"issue-610\"")
            .body_includes("Applied backlog improvement")
            .body_includes("\"priority\":1")
            .body_includes("\"estimate\":5.0");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": issue_node(
                        "issue-610",
                        "MET-610",
                        "Applied backlog improvement",
                        "# Applied backlog improvement",
                        "state-backlog",
                        "Backlog",
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
            "improve",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "MET-610",
            "--mode",
            "advanced",
            "--apply",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MET-610: advanced applied"));

    assert_eq!(
        fs::read_to_string(issue_dir.join("index.md"))?,
        "# Applied backlog improvement\n\n## Acceptance Criteria\n\n- `meta backlog improve MET-610 --mode advanced --apply` updates the local packet before Linear"
    );
    let run_dir = latest_improvement_dir(&issue_dir)?;
    assert_eq!(
        fs::read_to_string(run_dir.join("applied-local-before.md"))?,
        "# Previous local packet\n"
    );
    assert_eq!(
        fs::read_to_string(run_dir.join("applied-remote-before.md"))?,
        "Remote description before apply"
    );
    let summary = fs::read_to_string(run_dir.join("summary.json"))?;
    assert!(summary.contains("\"requested\": true"));
    assert!(summary.contains("\"local_updated\": true"));
    assert!(summary.contains("\"remote_updated\": true"));
    assert!(summary.contains("\"mode\": \"advanced\""));
    update_issue_mock.assert_calls(1);

    Ok(())
}

#[cfg(unix)]
fn write_backlog_improve_config(
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
default_agent = "backlog-improve-stub"

[agents.commands.backlog-improve-stub]
command = "{}"
transport = "stdin"
"#,
            stub_path.display()
        ),
    )?;
    Ok(())
}

#[cfg(unix)]
fn write_backlog_improve_stub(path: &Path, contents: &str) -> Result<(), Box<dyn Error>> {
    fs::write(path, contents)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
fn mock_issue_list(server: &MockServer, issues: Vec<serde_json::Value>) {
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
fn latest_improvement_dir(issue_dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let improvement_root = issue_dir.join("artifacts").join("improvement");
    let mut entries = fs::read_dir(&improvement_root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    entries.pop().ok_or_else(|| {
        format!(
            "no improvement run found under `{}`",
            improvement_root.display()
        )
        .into()
    })
}
