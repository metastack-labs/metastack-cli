#![allow(dead_code, unused_imports)]

include!("support/common.rs");

#[test]
fn backlog_groom_report_mode_scaffolds_missing_packets_and_prints_findings()
-> Result<(), Box<dyn Error>> {
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
team = "MET"
"#
        ),
    )?;

    let issues_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "token")
            .body_includes("query Issues")
            .body_includes("\"project\":{\"id\":{\"eq\":\"project-1\"}}");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-11",
                        "identifier": "MET-11",
                        "title": "Tighten backlog ticket quality",
                        "description": "Short stub.",
                        "url": "https://linear.app/metastack-labs/issue/MET-11",
                        "priority": 2,
                        "estimate": null,
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
                        "assignee": null,
                        "labels": {
                            "nodes": []
                        },
                        "state": {
                            "id": "state-backlog",
                            "name": "Backlog",
                            "type": "backlog"
                        },
                        "attachments": {
                            "nodes": []
                        }
                    }],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        }));
    });

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .current_dir(&repo_root)
        .args(["backlog", "groom"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Backlog groom report for `project-1`"))
        .stdout(predicate::str::contains("Mode: report"))
        .stdout(predicate::str::contains("MET-11  Tighten backlog ticket quality"))
        .stdout(predicate::str::contains("refine:"))
        .stdout(predicate::str::contains("rescan-required:"))
        .stdout(predicate::str::contains("Local packets created: 1"));

    issues_mock.assert_calls(1);
    assert!(repo_root.join(".metastack/backlog/MET-11").is_dir());
    assert!(repo_root.join(".metastack/backlog/MET-11/.linear.json").is_file());
    assert_eq!(
        fs::read_to_string(repo_root.join(".metastack/backlog/MET-11/index.md"))?,
        "Short stub."
    );

    Ok(())
}
