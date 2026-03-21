#![allow(dead_code, unused_imports)]

include!("support/common.rs");

#[test]
fn backlog_groom_report_creates_missing_packets_and_reports_findings() -> Result<(), Box<dyn Error>>
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
            .body_includes("query Issues")
            .body_includes("\"project\":{\"id\":{\"eq\":\"project-1\"}}");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [
                        {
                            "id": "issue-1",
                            "identifier": "MET-51",
                            "title": "Improve backlog sync status output",
                            "description": "Short note",
                            "url": "https://linear.app/issues/MET-51",
                            "priority": 2,
                            "updatedAt": "2026-03-19T16:00:00Z",
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
                                "id": "state-1",
                                "name": "Todo",
                                "type": "unstarted"
                            },
                            "attachments": { "nodes": [] },
                            "parent": null,
                            "children": { "nodes": [] }
                        },
                        {
                            "id": "issue-2",
                            "identifier": "MET-52",
                            "title": "Improve backlog sync status outputs",
                            "description": "# Acceptance Criteria\n\n- [ ] Preserve current layout\n- [ ] Keep snapshots stable\n",
                            "url": "https://linear.app/issues/MET-52",
                            "priority": 2,
                            "updatedAt": "2026-03-18T16:00:00Z",
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
                    ]
                }
            }
        }));
    });

    let update_issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": issue_node(
                        "issue-1",
                        "MET-51",
                        "Improve backlog sync status output",
                        "Short note",
                        "state-1",
                        "Todo",
                    )
                }
            }
        }));
    });

    cli()
        .current_dir(temp.path())
        .args([
            "backlog",
            "groom",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Backlog grooming report"))
        .stdout(predicate::str::contains("MET-51"))
        .stdout(predicate::str::contains("[refine]"))
        .stdout(predicate::str::contains("[rescan-required]"))
        .stdout(predicate::str::contains("MET-52"))
        .stdout(predicate::str::contains("[merge]"))
        .stdout(predicate::str::contains(
            "created 2 missing local packet(s)",
        ));

    assert!(
        temp.path()
            .join(".metastack/backlog/MET-51/index.md")
            .is_file()
    );
    assert!(
        temp.path()
            .join(".metastack/backlog/MET-51/.linear.json")
            .is_file()
    );
    assert!(
        temp.path()
            .join(".metastack/backlog/MET-52/index.md")
            .is_file()
    );
    update_issue_mock.assert_calls(0);

    Ok(())
}
