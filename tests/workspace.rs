#![allow(dead_code, unused_imports)]

include!("support/common.rs");

#[cfg(unix)]
fn prepend_path(bin_dir: &std::path::Path) -> Result<String, Box<dyn Error>> {
    let current_path = std::env::var("PATH")?;
    Ok(format!("{}:{}", bin_dir.display(), current_path))
}

#[cfg(unix)]
fn write_onboarded_config(config_path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    fs::write(config_path, "[onboarding]\ncompleted = true\n")?;
    Ok(())
}

#[cfg(unix)]
fn write_linear_config(config_path: &std::path::Path, api_url: &str) -> Result<(), Box<dyn Error>> {
    fs::write(
        config_path,
        format!(
            r#"[onboarding]
completed = true

[linear]
api_key = "token"
api_url = "{api_url}"
"#,
        ),
    )?;
    Ok(())
}

#[cfg(unix)]
fn mock_linear_issues(server: &MockServer, issues: serde_json::Value) {
    server.mock(move |when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(serde_json::json!({
            "data": {
                "issues": {
                    "nodes": issues,
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        }));
    });
}

#[cfg(unix)]
fn linear_issue(identifier: &str, state_name: &str, state_kind: &str) -> serde_json::Value {
    serde_json::json!({
        "id": format!("issue-{identifier}"),
        "identifier": identifier,
        "title": format!("Title for {identifier}"),
        "description": format!("Description for {identifier}"),
        "url": format!("https://linear.app/issue/{identifier}"),
        "priority": 2,
        "estimate": null,
        "updatedAt": "2026-03-19T15:00:00Z",
        "team": {
            "id": "team-1",
            "key": identifier.split('-').next().unwrap_or("MET"),
            "name": "MetaStack"
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
            "id": format!("state-{identifier}"),
            "name": state_name,
            "type": state_kind
        },
        "attachments": {
            "nodes": []
        }
    })
}

#[cfg(unix)]
fn write_gh_stub(path: &std::path::Path, prs_json: &str) -> Result<(), Box<dyn Error>> {
    fs::write(
        path,
        format!(
            r#"#!/bin/sh
set -eu
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
  printf '%s' '{prs_json}'
  exit 0
fi
printf 'unexpected gh invocation: %s\n' "$*" >&2
exit 1
"#
        ),
    )?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
fn write_gh_failure_stub(path: &std::path::Path, message: &str) -> Result<(), Box<dyn Error>> {
    fs::write(
        path,
        format!("#!/bin/sh\nprintf '%s' '{message}' >&2\nexit 1\n"),
    )?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
fn git(repo_root: &std::path::Path, args: &[&str]) -> Result<(), Box<dyn Error>> {
    let status = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .status()?;
    assert!(status.success(), "git {:?} failed", args);
    Ok(())
}

#[cfg(unix)]
fn create_workspace_ticket(
    repo_root: &std::path::Path,
    ticket: &str,
    branch: &str,
) -> Result<std::path::PathBuf, Box<dyn Error>> {
    let workspace =
        create_workspace_clone_checkout(repo_root, &format!("repo-workspace/{ticket}"))?;
    git(&workspace, &["checkout", "-B", branch, "origin/main"])?;
    Ok(workspace)
}

#[cfg(unix)]
fn commit_workspace_file(
    workspace: &std::path::Path,
    file: &str,
    contents: &str,
    message: &str,
) -> Result<(), Box<dyn Error>> {
    fs::write(workspace.join(file), contents)?;
    git(workspace, &["add", file])?;
    git(workspace, &["commit", "-m", message])?;
    Ok(())
}

#[cfg(unix)]
fn push_workspace_branch(workspace: &std::path::Path, branch: &str) -> Result<(), Box<dyn Error>> {
    git(workspace, &["push", "-u", "origin", branch])?;
    Ok(())
}

#[cfg(unix)]
fn seed_ticket_store_state(
    config_path: &std::path::Path,
    repo_root: &std::path::Path,
    tickets: &[&str],
) -> Result<(), Box<dyn Error>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let state_path = listen_state_path(config_path, repo_root)?;
    let parent = state_path
        .parent()
        .expect("state path should have a parent");
    fs::create_dir_all(parent)?;
    let sessions = tickets
        .iter()
        .map(|ticket| {
            serde_json::json!({
                "issue_id": format!("issue-{ticket}"),
                "issue_identifier": ticket,
                "issue_title": format!("Title for {ticket}"),
                "project_name": "MetaStack CLI",
                "team_key": "ENG",
                "issue_url": format!("https://linear.app/issue/{ticket}"),
                "phase": "completed",
                "summary": "Completed",
                "brief_path": null,
                "backlog_issue_identifier": ticket,
                "backlog_issue_title": format!("Backlog for {ticket}"),
                "backlog_path": format!(".metastack/backlog/{ticket}"),
                "workspace_path": null,
                "branch": null,
                "workpad_comment_id": format!("comment-{ticket}"),
                "updated_at_epoch_seconds": now,
                "pid": null,
                "session_id": null,
                "turns": null,
                "tokens": {
                    "input": null,
                    "output": null
                },
                "log_path": format!(".metastack/agents/sessions/{ticket}.log")
            })
        })
        .collect::<Vec<_>>();
    fs::write(
        &state_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "version": 1,
            "sessions": sessions
        }))?,
    )?;
    for ticket in tickets {
        let log_path = listen_log_path(config_path, repo_root, ticket)?;
        fs::create_dir_all(log_path.parent().expect("log path should have a parent"))?;
        fs::write(log_path, format!("log for {ticket}\n"))?;
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn workspace_list_reports_linear_and_pr_enrichment() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let server = MockServer::start();
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, r#"{ "linear": { "team": "ENG" } }"#)?;
    write_linear_config(&config_path, &server.url("/graphql"))?;

    let active = create_workspace_ticket(&repo_root, "ENG-10175", "eng-10175-branch")?;
    commit_workspace_file(&active, "active.txt", "active\n", "Active workspace")?;
    push_workspace_branch(&active, "eng-10175-branch")?;
    let done = create_workspace_ticket(&repo_root, "ENG-10176", "eng-10176-branch")?;
    commit_workspace_file(&done, "done.txt", "done\n", "Done workspace")?;
    push_workspace_branch(&done, "eng-10176-branch")?;

    mock_linear_issues(
        &server,
        serde_json::json!([
            linear_issue("ENG-10175", "In Progress", "started"),
            linear_issue("ENG-10176", "Done", "completed")
        ]),
    );
    write_gh_stub(
        &bin_dir.join("gh"),
        r#"[{"headRefName":"eng-10175-branch","state":"OPEN"},{"headRefName":"eng-10176-branch","state":"MERGED"}]"#,
    )?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "workspace",
            "list",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("ENG-10175"))
        .stdout(predicate::str::contains("eng-10175-branch"))
        .stdout(predicate::str::contains("In Progress"))
        .stdout(predicate::str::contains("open"))
        .stdout(predicate::str::contains("ENG-10176"))
        .stdout(predicate::str::contains("Done"))
        .stdout(predicate::str::contains("merged"))
        .stdout(predicate::str::contains("candidate"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn workspace_list_marks_github_data_unavailable_when_gh_fails() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let server = MockServer::start();
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, r#"{ "linear": { "team": "ENG" } }"#)?;
    write_linear_config(&config_path, &server.url("/graphql"))?;
    let workspace = create_workspace_ticket(&repo_root, "ENG-10175", "eng-10175-branch")?;
    fs::write(workspace.join("sample.txt"), "sample\n")?;

    mock_linear_issues(
        &server,
        serde_json::json!([linear_issue("ENG-10175", "Done", "completed")]),
    );
    write_gh_failure_stub(&bin_dir.join("gh"), "gh auth missing")?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "workspace",
            "list",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("ENG-10175"))
        .stdout(predicate::str::contains("unavailable"))
        .stdout(predicate::str::contains(
            "GitHub PR data unavailable: gh auth missing",
        ));

    Ok(())
}

#[cfg(unix)]
#[test]
fn workspace_list_reports_local_only_metadata_when_linear_issue_is_missing()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let server = MockServer::start();
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, r#"{ "linear": { "team": "ENG" } }"#)?;
    write_linear_config(&config_path, &server.url("/graphql"))?;

    let workspace = create_workspace_ticket(&repo_root, "ENG-10175", "eng-10175-branch")?;
    fs::write(workspace.join("sample.txt"), "sample\n")?;
    write_gh_stub(&bin_dir.join("gh"), "[]")?;
    mock_linear_issues(&server, serde_json::json!([]));

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "workspace",
            "list",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("ENG-10175"))
        .stdout(predicate::str::contains("eng-10175-branch"))
        .stdout(predicate::str::contains("dirty"))
        .stdout(predicate::str::contains("Missing"))
        .stdout(predicate::str::contains("none"))
        .stdout(predicate::str::contains("candidate").not());

    Ok(())
}

#[cfg(unix)]
#[test]
fn workspace_terminal_state_kind_drives_candidates_and_prune_preview() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let server = MockServer::start();
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, r#"{ "linear": { "team": "ENG" } }"#)?;
    write_linear_config(&config_path, &server.url("/graphql"))?;

    let workspace = create_workspace_ticket(&repo_root, "ENG-10175", "eng-10175-branch")?;
    commit_workspace_file(
        &workspace,
        "archived.txt",
        "archived\n",
        "Archived workspace",
    )?;
    push_workspace_branch(&workspace, "eng-10175-branch")?;

    mock_linear_issues(
        &server,
        serde_json::json!([linear_issue("ENG-10175", "Archived", "completed")]),
    );
    write_gh_stub(&bin_dir.join("gh"), "[]")?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "workspace",
            "list",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Archived"))
        .stdout(predicate::str::contains("candidate"));

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "workspace",
            "prune",
            "--dry-run",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("REMOVE  ENG-10175"))
        .stdout(predicate::str::contains(
            "ticket completed and no PR was found",
        ));

    Ok(())
}

#[cfg(unix)]
#[test]
fn workspace_clean_force_removes_clone_and_ticket_scoped_store_artifacts()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_onboarded_config(&config_path)?;

    let workspace = create_workspace_ticket(&repo_root, "ENG-10175", "eng-10175-branch")?;
    let other = create_workspace_ticket(&repo_root, "ENG-10176", "eng-10176-branch")?;
    commit_workspace_file(&workspace, "remove.txt", "remove\n", "Remove workspace")?;
    push_workspace_branch(&workspace, "eng-10175-branch")?;
    commit_workspace_file(&other, "keep.txt", "keep\n", "Keep workspace")?;
    push_workspace_branch(&other, "eng-10176-branch")?;
    seed_ticket_store_state(&config_path, &repo_root, &["ENG-10175", "ENG-10176"])?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "workspace",
            "clean",
            "ENG-10175",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--force",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Workspace `ENG-10175` safety: clean",
        ))
        .stdout(predicate::str::contains("Removed workspace `ENG-10175`"));

    assert!(!workspace.exists());
    assert!(other.exists());
    let state = fs::read_to_string(listen_state_path(&config_path, &repo_root)?)?;
    assert!(!state.contains("ENG-10175"));
    assert!(state.contains("ENG-10176"));
    assert!(!listen_log_path(&config_path, &repo_root, "ENG-10175")?.exists());
    assert!(listen_log_path(&config_path, &repo_root, "ENG-10176")?.exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn workspace_clean_prompts_and_reports_dirty_and_ahead_warnings() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_onboarded_config(&config_path)?;

    let workspace = create_workspace_ticket(&repo_root, "ENG-10175", "eng-10175-branch")?;
    fs::write(workspace.join("committed.txt"), "committed\n")?;
    git(&workspace, &["add", "committed.txt"])?;
    git(&workspace, &["commit", "-m", "Workspace commit"])?;
    fs::write(workspace.join("dirty.txt"), "dirty\n")?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .write_stdin("y\n")
        .args([
            "workspace",
            "clean",
            "ENG-10175",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Workspace `ENG-10175` safety: dirty+ahead",
        ))
        .stdout(predicate::str::contains(
            "Warning: uncommitted changes will be deleted.",
        ))
        .stdout(predicate::str::contains(
            "Warning: unpushed commits were detected.",
        ))
        .stderr(predicate::str::contains("Delete workspace `ENG-10175`"));

    assert!(!workspace.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn workspace_clean_target_only_removes_target_directories() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_onboarded_config(&config_path)?;

    let workspace = create_workspace_ticket(&repo_root, "ENG-10175", "eng-10175-branch")?;
    let nested_target = workspace.join("nested/target");
    fs::create_dir_all(&nested_target)?;
    fs::write(nested_target.join("artifact"), "artifact\n")?;
    let other = create_workspace_ticket(&repo_root, "ENG-10176", "eng-10176-branch")?;
    let root_target = other.join("target");
    fs::create_dir_all(&root_target)?;
    fs::write(root_target.join("artifact"), "artifact\n")?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "workspace",
            "clean",
            "--target-only",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ENG-10175: removed 1 target directory",
        ))
        .stdout(predicate::str::contains(
            "ENG-10176: removed 1 target directory",
        ));

    assert!(!nested_target.exists());
    assert!(!root_target.exists());
    assert!(workspace.exists());
    assert!(other.exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn workspace_clean_target_only_can_limit_cleanup_to_one_ticket() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_onboarded_config(&config_path)?;

    let workspace = create_workspace_ticket(&repo_root, "ENG-10175", "eng-10175-branch")?;
    let selected_target = workspace.join("target");
    fs::create_dir_all(&selected_target)?;
    fs::write(selected_target.join("artifact"), "artifact\n")?;
    let other = create_workspace_ticket(&repo_root, "ENG-10176", "eng-10176-branch")?;
    let preserved_target = other.join("target");
    fs::create_dir_all(&preserved_target)?;
    fs::write(preserved_target.join("artifact"), "artifact\n")?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "workspace",
            "clean",
            "--target-only",
            "ENG-10175",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ENG-10175: removed 1 target directory",
        ))
        .stdout(predicate::str::contains("ENG-10176").not());

    assert!(!selected_target.exists());
    assert!(preserved_target.exists());
    assert!(workspace.exists());
    assert!(other.exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn workspace_prune_dry_run_previews_actions_and_skips_open_or_ahead_clones()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let server = MockServer::start();
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, r#"{ "linear": { "team": "ENG" } }"#)?;
    write_linear_config(&config_path, &server.url("/graphql"))?;

    let removable = create_workspace_ticket(&repo_root, "ENG-10175", "eng-10175-branch")?;
    commit_workspace_file(&removable, "remove.txt", "remove\n", "Removable workspace")?;
    push_workspace_branch(&removable, "eng-10175-branch")?;
    let open_pr = create_workspace_ticket(&repo_root, "ENG-10176", "eng-10176-branch")?;
    commit_workspace_file(&open_pr, "open.txt", "open\n", "Open PR workspace")?;
    push_workspace_branch(&open_pr, "eng-10176-branch")?;
    let ahead = create_workspace_ticket(&repo_root, "ENG-10177", "eng-10177-branch")?;
    commit_workspace_file(&ahead, "ahead.txt", "ahead\n", "Ahead commit")?;

    mock_linear_issues(
        &server,
        serde_json::json!([
            linear_issue("ENG-10175", "Done", "completed"),
            linear_issue("ENG-10176", "Cancelled", "canceled"),
            linear_issue("ENG-10177", "Done", "completed")
        ]),
    );
    write_gh_stub(
        &bin_dir.join("gh"),
        r#"[{"headRefName":"eng-10175-branch","state":"MERGED"},{"headRefName":"eng-10176-branch","state":"OPEN"},{"headRefName":"eng-10177-branch","state":"CLOSED"}]"#,
    )?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "workspace",
            "prune",
            "--dry-run",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("REMOVE  ENG-10175"))
        .stdout(predicate::str::contains(
            "ticket completed and PR is merged",
        ))
        .stdout(predicate::str::contains("KEEP  ENG-10176"))
        .stdout(predicate::str::contains(
            "branch pull request is still open",
        ))
        .stdout(predicate::str::contains("KEEP  ENG-10177"))
        .stdout(predicate::str::contains("unpushed commits detected"))
        .stdout(predicate::str::contains("Removed 1 clones, freed"))
        .stdout(predicate::str::contains("Kept 2 clones."));

    assert!(removable.exists());
    assert!(open_pr.exists());
    assert!(ahead.exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn workspace_prune_removes_completed_clones_without_github_auth() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let server = MockServer::start();
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, r#"{ "linear": { "team": "ENG" } }"#)?;
    write_linear_config(&config_path, &server.url("/graphql"))?;

    let removable = create_workspace_ticket(&repo_root, "ENG-10175", "eng-10175-branch")?;
    commit_workspace_file(&removable, "remove.txt", "remove\n", "Removable workspace")?;
    push_workspace_branch(&removable, "eng-10175-branch")?;
    let keep = create_workspace_ticket(&repo_root, "ENG-10176", "eng-10176-branch")?;
    commit_workspace_file(&keep, "keep.txt", "keep\n", "Keep workspace")?;
    push_workspace_branch(&keep, "eng-10176-branch")?;
    seed_ticket_store_state(&config_path, &repo_root, &["ENG-10175", "ENG-10176"])?;

    mock_linear_issues(
        &server,
        serde_json::json!([
            linear_issue("ENG-10175", "Done", "completed"),
            linear_issue("ENG-10176", "In Progress", "started")
        ]),
    );
    write_gh_failure_stub(&bin_dir.join("gh"), "gh auth missing")?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "workspace",
            "prune",
            "--force",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "GitHub PR data unavailable; using Linear completion state only: gh auth missing",
        ))
        .stdout(predicate::str::contains("Removed 1 clones, freed"))
        .stdout(predicate::str::contains("Kept 1 clones."));

    assert!(!removable.exists());
    assert!(keep.exists());
    let state = fs::read_to_string(listen_state_path(&config_path, &repo_root)?)?;
    assert!(!state.contains("ENG-10175"));
    assert!(state.contains("ENG-10176"));

    Ok(())
}
