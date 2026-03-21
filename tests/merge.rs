#![allow(dead_code, unused_imports)]

include!("support/common.rs");

#[cfg(unix)]
fn write_onboarded_config(
    config_path: &std::path::Path,
    config: impl AsRef<str>,
) -> Result<(), Box<dyn Error>> {
    let contents = format!(
        "{}\n[onboarding]\ncompleted = true\n",
        config.as_ref().trim_end()
    );
    fs::write(config_path, &contents)?;
    let home_config = isolated_home_dir().join(".config/metastack/config.toml");
    if let Some(parent) = home_config.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(home_config, contents)?;
    Ok(())
}

#[cfg(unix)]
fn prepend_path(bin_dir: &std::path::Path) -> Result<String, Box<dyn Error>> {
    let current_path = std::env::var("PATH")?;
    Ok(format!("{}:{}", bin_dir.display(), current_path))
}

#[cfg(unix)]
fn write_github_stub(
    path: &std::path::Path,
    pr_numbers: &[u64],
    create_url: &str,
) -> Result<(), Box<dyn Error>> {
    let pr_entries = pr_numbers
        .iter()
        .map(|number| {
            format!(
                r#"{{"number":{number},"title":"PR {number}","body":"Description for PR {number}","url":"https://github.com/metastack-systems/metastack-cli/pull/{number}","headRefName":"feature/{number}","baseRefName":"main","updatedAt":"2026-03-16T18:00:00Z","author":{{"login":"kames"}}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    fs::write(
        path,
        format!(
            r#"#!/bin/sh
set -eu
if [ "$1" = "repo" ] && [ "$2" = "view" ]; then
  printf '%s' '{{"nameWithOwner":"metastack-systems/metastack-cli","url":"https://github.com/metastack-systems/metastack-cli","defaultBranchRef":{{"name":"main"}}}}'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
  case " $* " in
    *" --head meta-merge/"*)
      printf '%s' '[]'
      ;;
    *)
      printf '%s' '[{pr_entries}]'
      ;;
  esac
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "create" ]; then
  printf '%s' '{{"number":999,"url":"{create_url}","isDraft":false}}'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "edit" ]; then
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
fn write_github_update_stub(
    path: &std::path::Path,
    pr_numbers: &[u64],
    existing_url: &str,
) -> Result<(), Box<dyn Error>> {
    let pr_entries = pr_numbers
        .iter()
        .map(|number| {
            format!(
                r#"{{"number":{number},"title":"PR {number}","body":"Description for PR {number}","url":"https://github.com/metastack-systems/metastack-cli/pull/{number}","headRefName":"feature/{number}","baseRefName":"main","updatedAt":"2026-03-16T18:00:00Z","author":{{"login":"kames"}}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    fs::write(
        path,
        format!(
            r#"#!/bin/sh
set -eu
if [ "$1" = "repo" ] && [ "$2" = "view" ]; then
  printf '%s' '{{"nameWithOwner":"metastack-systems/metastack-cli","url":"https://github.com/metastack-systems/metastack-cli","defaultBranchRef":{{"name":"main"}}}}'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
  case " $* " in
    *" --head meta-merge/"*)
      printf '%s' '[{{"number":999,"url":"{existing_url}"}}]'
      ;;
    *)
      printf '%s' '[{pr_entries}]'
      ;;
  esac
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "create" ]; then
  printf 'unexpected gh invocation: %s\n' "$*" >&2
  exit 1
fi
if [ "$1" = "pr" ] && [ "$2" = "edit" ]; then
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
fn write_github_retry_create_stub(
    path: &std::path::Path,
    pr_numbers: &[u64],
    create_url: &str,
) -> Result<(), Box<dyn Error>> {
    let pr_entries = pr_numbers
        .iter()
        .map(|number| {
            format!(
                r#"{{"number":{number},"title":"PR {number}","body":"Description for PR {number}","url":"https://github.com/metastack-systems/metastack-cli/pull/{number}","headRefName":"feature/{number}","baseRefName":"main","updatedAt":"2026-03-16T18:00:00Z","author":{{"login":"kames"}}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    fs::write(
        path,
        format!(
            r#"#!/bin/sh
set -eu
count_file="${{TEST_OUTPUT_DIR}}/gh-create-count.txt"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
if [ "$1" = "repo" ] && [ "$2" = "view" ]; then
  printf '%s' '{{"nameWithOwner":"metastack-systems/metastack-cli","url":"https://github.com/metastack-systems/metastack-cli","defaultBranchRef":{{"name":"main"}}}}'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
  case " $* " in
    *" --head meta-merge/"*)
      printf '%s' '[]'
      ;;
    *)
      printf '%s' '[{pr_entries}]'
      ;;
  esac
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "create" ]; then
  count=$((count + 1))
  printf '%s' "$count" > "$count_file"
  if [ "$count" -eq 1 ]; then
    printf '%s\n' 'temporary github failure' >&2
    exit 1
  fi
  printf '%s' '{{"number":999,"url":"{create_url}","isDraft":false}}'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "edit" ]; then
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
fn write_github_persistent_pr_stub(
    path: &std::path::Path,
    pr_numbers: &[u64],
    create_url: &str,
) -> Result<(), Box<dyn Error>> {
    let pr_entries = pr_numbers
        .iter()
        .map(|number| {
            format!(
                r#"{{"number":{number},"title":"PR {number}","body":"Description for PR {number}","url":"https://github.com/metastack-systems/metastack-cli/pull/{number}","headRefName":"feature/{number}","baseRefName":"main","updatedAt":"2026-03-16T18:00:00Z","author":{{"login":"kames"}}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    fs::write(
        path,
        format!(
            r#"#!/bin/sh
set -eu
count_file="${{TEST_OUTPUT_DIR}}/gh-persistent-count.txt"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
if [ "$1" = "repo" ] && [ "$2" = "view" ]; then
  printf '%s' '{{"nameWithOwner":"metastack-systems/metastack-cli","url":"https://github.com/metastack-systems/metastack-cli","defaultBranchRef":{{"name":"main"}}}}'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
  case " $* " in
    *" --head meta-merge/"*)
      if [ "$count" -ge 1 ]; then
        printf '%s' '[{{"number":999,"url":"{create_url}"}}]'
      else
        printf '%s' '[]'
      fi
      ;;
    *)
      printf '%s' '[{pr_entries}]'
      ;;
  esac
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "create" ]; then
  count=$((count + 1))
  printf '%s' "$count" > "$count_file"
  printf '%s' '{{"number":999,"url":"{create_url}","isDraft":false}}'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "edit" ]; then
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
fn write_agent_stub(path: &std::path::Path, planner_order: &[u64]) -> Result<(), Box<dyn Error>> {
    let order = planner_order
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    fs::write(
        path,
        format!(
            r#"#!/bin/sh
set -eu
case "$METASTACK_AGENT_PROMPT" in
  *"Return strict JSON"*)
    printf '%s' '{{"merge_order":[{order}],"conflict_hotspots":["shared.txt"],"summary":"Use the selected order."}}'
    ;;
  *"Repair a failing aggregate merge validation"*)
    if [ -n "${{TEST_VALIDATE_FIX_FILE:-}}" ]; then
      printf 'fixed\n' > "$TEST_VALIDATE_FIX_FILE"
    fi
    printf '%s' 'Repaired validation failure'
    ;;
  *)
    if [ -f shared.txt ]; then
      printf 'resolved\n' > shared.txt
    fi
    printf '%s' 'Resolved merge conflict in shared.txt'
    ;;
esac
"#
        ),
    )?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
fn write_flaky_validation_stub(path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    fs::write(
        path,
        r#"#!/bin/sh
set -eu
count_file="${TEST_OUTPUT_DIR}/validation-count.txt"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
if [ "$count" -eq 1 ]; then
  printf '%s\n' 'test result: FAILED. 12 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out' >&2
  printf '%s\n' 'error: test failed, to rerun pass `--test flaky`' >&2
  exit 1
fi
exit 0
"#,
    )?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
fn write_stuck_validation_stub(path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    fs::write(
        path,
        r#"#!/bin/sh
set -eu
count_file="${TEST_OUTPUT_DIR}/stuck-validation-count.txt"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
printf '%s\n' 'test plan_interactive_preserves_explicit_builtin_overrides_across_resumed_phases ... FAILED'
printf '%s\n' ''
printf '%s\n' 'failures:'
printf '%s\n' '---- plan_interactive_preserves_explicit_builtin_overrides_across_resumed_phases stdout ----'
printf '%s\n' 'Error: Os { code: 2, kind: NotFound, message: "No such file or directory" }'
printf '%s\n' ''
printf '%s\n' 'test result: FAILED. 12 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out'
printf '%s\n' 'error: test failed, to rerun pass `--test plan`' >&2
exit 1
"#,
    )?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_json_discovers_repo_and_open_pull_requests_via_gh() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    write_onboarded_config(&config_path, "")?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[101, 102],
        "https://github.com/example/pull/999",
    )?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args(["merge", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"name_with_owner\": \"metastack-systems/metastack-cli\"",
        ))
        .stdout(predicate::str::contains("\"number\": 101"))
        .stdout(predicate::str::contains("\"number\": 102"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_reports_repo_resolution_errors_from_gh() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    write_onboarded_config(&config_path, "")?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    fs::write(
        bin_dir.join("gh"),
        "#!/bin/sh\nprintf '%s' 'gh auth missing' >&2\nexit 1\n",
    )?;
    let mut permissions = fs::metadata(bin_dir.join("gh"))?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(bin_dir.join("gh"), permissions)?;

    let assert = cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args(["merge", "--json"])
        .assert()
        .failure();

    let payload: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout)?;
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["command"], "merge");
    assert_eq!(payload["error"]["code"], "invalid_input");
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("gh repo view --json nameWithOwner,url,defaultBranchRef failed")
    );
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("gh auth missing")
    );
    assert!(assert.get_output().stderr.is_empty());

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_rejects_conflicting_execution_modes() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    write_onboarded_config(&config_path, "")?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[101, 102],
        "https://github.com/example/pull/999",
    )?;

    let assert = cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args(["merge", "--json", "--render-once"])
        .assert()
        .failure();

    let payload: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout)?;
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["command"], "merge");
    assert_eq!(payload["error"]["code"], "invalid_input");
    assert_eq!(
        payload["error"]["message"],
        "the argument '--json' cannot be used with '--render-once'"
    );
    assert!(assert.get_output().stderr.is_empty());

    let assert = cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "merge",
            "--json",
            "--no-interactive",
            "--pull-request",
            "101",
        ])
        .assert()
        .failure();

    let payload: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout)?;
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["command"], "merge");
    assert_eq!(payload["error"]["code"], "invalid_input");
    assert_eq!(
        payload["error"]["message"],
        "the argument '--json' cannot be used with '--no-interactive'"
    );
    assert!(assert.get_output().stderr.is_empty());

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_render_once_shows_selected_batch_summary() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    write_onboarded_config(&config_path, "")?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[101, 102],
        "https://github.com/example/pull/999",
    )?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "merge",
            "--render-once",
            "--events",
            "space,down,space,enter",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("#101 PR 101"))
        .stdout(predicate::str::contains("#102 PR 102"))
        .stdout(predicate::str::contains(
            "2 pull request(s) will be handed to the merge agent",
        ));

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_render_once_handles_empty_discovery_state() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    write_onboarded_config(&config_path, "")?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[],
        "https://github.com/example/pull/999",
    )?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args(["merge", "--render-once"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "The GitHub repository currently has no open pull requests.",
        ))
        .stdout(predicate::str::contains(
            "No one-shot batch can be created until open pull requests",
        ));

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_requires_pull_request_selection_for_no_interactive_runs() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    write_onboarded_config(&config_path, "")?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[101, 102],
        "https://github.com/example/pull/999",
    )?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args(["merge", "--no-interactive"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "`meta merge --no-interactive` requires at least one `--pull-request <NUMBER>`",
        ));

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_rejects_duplicate_pull_request_selection() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    write_onboarded_config(&config_path, "")?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[101, 102],
        "https://github.com/example/pull/999",
    )?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "merge",
            "--no-interactive",
            "--pull-request",
            "101",
            "--pull-request",
            "101",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "pull request #101 was selected more than once",
        ));

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_requires_a_tty_for_interactive_dashboard_runs() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    write_onboarded_config(&config_path, "")?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[101, 102],
        "https://github.com/example/pull/999",
    )?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args(["merge"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "the interactive merge dashboard requires a TTY",
        ))
        .stderr(predicate::str::contains("meta merge --json"))
        .stderr(predicate::str::contains("meta merge --no-interactive"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_executes_clean_batch_and_writes_artifacts() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let agent_stub = temp.path().join("merge-agent-stub");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[11, 12],
        "https://github.com/example/pull/999",
    )?;
    write_agent_stub(&agent_stub, &[11, 12])?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            agent_stub.display()
        ),
    )?;

    commit_and_push_pull_ref(&repo_root, "feature/11", "one.txt", "one\n", 11)?;
    commit_and_push_pull_ref(&repo_root, "feature/12", "two.txt", "two\n", 12)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "merge",
            "--no-interactive",
            "--pull-request",
            "11",
            "--pull-request",
            "12",
            "--validate",
            "test -f one.txt",
            "--validate",
            "test -f two.txt",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Run artifacts:"))
        .stdout(predicate::str::contains(
            "Running phase 1/6: Workspace preparation",
        ))
        .stdout(predicate::str::contains(
            "Running phase 3/6: Merge application",
        ))
        .stdout(predicate::str::contains(
            "Merging pull request #11 (PR 11) onto the aggregate branch.",
        ))
        .stdout(predicate::str::contains("Pull request #12 merged cleanly."))
        .stdout(predicate::str::contains(
            "Running phase 6/6: PR publication",
        ))
        .stdout(predicate::str::contains(
            "Created aggregate PR https://github.com/example/pull/999",
        ));

    let run_root = repo_root.join(".metastack/merge-runs");
    let mut run_dirs = fs::read_dir(&run_root)?
        .map(|entry| entry.map(|item| item.path()))
        .collect::<Result<Vec<_>, _>>()?;
    run_dirs.sort();
    let run_dir = run_dirs.pop().expect("merge run should exist");

    assert!(run_dir.join("context.json").is_file());
    assert!(run_dir.join("plan.json").is_file());
    assert!(run_dir.join("progress.json").is_file());
    assert!(run_dir.join("merge-progress.json").is_file());
    assert!(run_dir.join("validation.json").is_file());
    assert!(run_dir.join("publication.json").is_file());
    assert!(fs::read_to_string(run_dir.join("publication.json"))?.contains("created"));
    let progress = fs::read_to_string(run_dir.join("progress.json"))?;
    assert!(progress.contains("\"status\": \"succeeded\""));
    assert!(progress.contains("\"current_phase_key\": \"publish_pr\""));
    assert!(progress.contains("\"pull_request\": 11"));
    assert!(progress.contains("\"pull_request\": 12"));
    let context = fs::read_to_string(run_dir.join("context.json"))?;
    assert!(context.contains("\"aggregate_branch\""));
    assert!(context.contains("-workspace/merge-runs/"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_persists_failed_progress_when_validation_repairs_are_exhausted()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let agent_stub = temp.path().join("merge-agent-stub");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[21],
        "https://github.com/example/pull/1001",
    )?;
    write_agent_stub(&agent_stub, &[21])?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[merge]
validation_repair_attempts = 2

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            agent_stub.display()
        ),
    )?;

    commit_and_push_pull_ref(&repo_root, "feature/21", "one.txt", "one\n", 21)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "merge",
            "--no-interactive",
            "--pull-request",
            "21",
            "--validate",
            "test -f repaired.txt",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Running phase 4/6: Validation"))
        .stdout(predicate::str::contains("validation still needs attention"));

    let run_root = repo_root.join(".metastack/merge-runs");
    let mut run_dirs = fs::read_dir(&run_root)?
        .map(|entry| entry.map(|item| item.path()))
        .collect::<Result<Vec<_>, _>>()?;
    run_dirs.sort();
    let run_dir = run_dirs.pop().expect("merge run should exist");

    assert!(run_dir.join("progress.json").is_file());
    assert!(run_dir.join("validation.json").is_file());
    assert!(run_dir.join("publication.json").is_file());

    let progress = fs::read_to_string(run_dir.join("progress.json"))?;
    assert!(progress.contains("\"status\": \"succeeded\""));
    assert!(progress.contains("\"current_phase_key\": \"publish_pr\""));
    assert!(progress.contains("Validation remains unresolved"));
    let merge_progress = fs::read_to_string(run_dir.join("merge-progress.json"))?;
    assert!(merge_progress.contains("\"status\": \"succeeded\""));
    assert!(merge_progress.contains("\"current_phase_key\": \"publish_pr\""));
    assert!(merge_progress.contains("\"pull_request\": 21"));
    assert!(merge_progress.contains("\"status\": \"merged\""));
    let validation = fs::read_to_string(run_dir.join("validation.json"))?;
    assert!(validation.contains("\"success\": false"));
    assert!(validation.contains("\"repair_attempts\": 1"));
    assert!(validation.contains("\"final_error\""));
    assert!(
        run_dir
            .join("validation-repair-prompt-attempt-1.md")
            .is_file()
    );
    assert!(
        run_dir
            .join("validation-repair-output-attempt-1.md")
            .is_file()
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_prefers_route_specific_agent_over_global_default() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let output_dir = temp.path().join("output");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&output_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[11, 12],
        "https://github.com/example/pull/999",
    )?;
    write_onboarded_config(
        &config_path,
        r#"[agents]
default_agent = "global-stub"

[agents.routing.commands."merge.run"]
provider = "route-stub"

[agents.commands.global-stub]
command = "global-stub"
args = ["{{payload}}"]
transport = "arg"

[agents.commands.route-stub]
command = "route-stub"
args = ["{{payload}}"]
transport = "arg"
"#,
    )?;

    fs::write(
        bin_dir.join("global-stub"),
        r#"#!/bin/sh
printf '%s' "$METASTACK_AGENT_NAME" > "$TEST_OUTPUT_DIR/global-agent.txt"
printf '%s' '{"merge_order":[11,12],"conflict_hotspots":[],"summary":"global"}'
"#,
    )?;
    let mut permissions = fs::metadata(bin_dir.join("global-stub"))?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(bin_dir.join("global-stub"), permissions)?;

    fs::write(
        bin_dir.join("route-stub"),
        r#"#!/bin/sh
printf '%s' "$METASTACK_AGENT_NAME" > "$TEST_OUTPUT_DIR/route-agent.txt"
printf '%s' '{"merge_order":[11,12],"conflict_hotspots":[],"summary":"route"}'
"#,
    )?;
    let mut permissions = fs::metadata(bin_dir.join("route-stub"))?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(bin_dir.join("route-stub"), permissions)?;

    commit_and_push_pull_ref(&repo_root, "feature/11", "one.txt", "one\n", 11)?;
    commit_and_push_pull_ref(&repo_root, "feature/12", "two.txt", "two\n", 12)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "merge",
            "--no-interactive",
            "--pull-request",
            "11",
            "--pull-request",
            "12",
            "--validate",
            "test -f one.txt",
            "--validate",
            "test -f two.txt",
        ])
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(output_dir.join("route-agent.txt"))?,
        "route-stub"
    );
    assert!(!output_dir.join("global-agent.txt").exists());
    let run_root = repo_root.join(".metastack/merge-runs");
    let mut run_dirs = fs::read_dir(&run_root)?
        .map(|entry| entry.map(|item| item.path()))
        .collect::<Result<Vec<_>, _>>()?;
    run_dirs.sort();
    let run_dir = run_dirs.pop().expect("merge run should exist");
    let context = fs::read_to_string(run_dir.join("context.json"))?;
    assert!(context.contains("\"agent_resolution\""));
    assert!(context.contains("\"provider\": \"route-stub\""));
    assert!(context.contains("\"route_key\": \"merge.run\""));
    assert!(context.contains("\"provider_source\": \"command_route:merge.run\""));

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_infers_make_quality_validation_when_omitted() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let agent_stub = temp.path().join("merge-agent-stub");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    fs::write(
        repo_root.join("Makefile"),
        ".PHONY: quality\nquality:\n\ttest -f one.txt\n\ttest -f two.txt\n",
    )?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[17, 18],
        "https://github.com/example/pull/2999",
    )?;
    write_agent_stub(&agent_stub, &[17, 18])?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            agent_stub.display()
        ),
    )?;

    commit_and_push_pull_ref(&repo_root, "feature/17", "one.txt", "one\n", 17)?;
    commit_and_push_pull_ref(&repo_root, "feature/18", "two.txt", "two\n", 18)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "merge",
            "--no-interactive",
            "--pull-request",
            "17",
            "--pull-request",
            "18",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created aggregate PR https://github.com/example/pull/2999",
        ));

    let run_root = repo_root.join(".metastack/merge-runs");
    let mut run_dirs = fs::read_dir(&run_root)?
        .map(|entry| entry.map(|item| item.path()))
        .collect::<Result<Vec<_>, _>>()?;
    run_dirs.sort();
    let run_dir = run_dirs.pop().expect("merge run should exist");

    let validation = fs::read_to_string(run_dir.join("validation.json"))?;
    assert!(validation.contains("\"command\": \"make quality\""));
    assert!(validation.contains("\"exit_code\": 0"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_infers_cargo_test_validation_when_makefile_is_missing() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let agent_stub = temp.path().join("merge-agent-stub");
    fs::create_dir_all(repo_root.join("src"))?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(
        repo_root.join("Cargo.toml"),
        r#"[package]
name = "merge-fallback-proof"
version = "0.1.0"
edition = "2021"
"#,
    )?;
    fs::write(
        repo_root.join("src/lib.rs"),
        r#"pub fn merge_validation_proof() -> &'static str {
    "ok"
}

#[cfg(test)]
mod tests {
    use super::merge_validation_proof;

    #[test]
    fn merge_validation_proof_passes() {
        assert_eq!(merge_validation_proof(), "ok");
    }
}
"#,
    )?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[19, 20],
        "https://github.com/example/pull/3999",
    )?;
    write_agent_stub(&agent_stub, &[19, 20])?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            agent_stub.display()
        ),
    )?;

    commit_and_push_pull_ref(&repo_root, "feature/19", "one.txt", "one\n", 19)?;
    commit_and_push_pull_ref(&repo_root, "feature/20", "two.txt", "two\n", 20)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "merge",
            "--no-interactive",
            "--pull-request",
            "19",
            "--pull-request",
            "20",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created aggregate PR https://github.com/example/pull/3999",
        ));

    let run_root = repo_root.join(".metastack/merge-runs");
    let mut run_dirs = fs::read_dir(&run_root)?
        .map(|entry| entry.map(|item| item.path()))
        .collect::<Result<Vec<_>, _>>()?;
    run_dirs.sort();
    let run_dir = run_dirs.pop().expect("merge run should exist");

    let validation = fs::read_to_string(run_dir.join("validation.json"))?;
    assert!(validation.contains("\"command\": \"cargo test\""));
    assert!(validation.contains("\"exit_code\": 0"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_repairs_validation_failures_and_publishes() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let agent_stub = temp.path().join("merge-agent-stub");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[13, 14],
        "https://github.com/example/pull/1999",
    )?;
    write_agent_stub(&agent_stub, &[13, 14])?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            agent_stub.display()
        ),
    )?;

    commit_and_push_pull_ref(&repo_root, "feature/13", "one.txt", "one\n", 13)?;
    commit_and_push_pull_ref(&repo_root, "feature/14", "two.txt", "two\n", 14)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .env("TEST_VALIDATE_FIX_FILE", "repaired.txt")
        .args([
            "merge",
            "--no-interactive",
            "--pull-request",
            "13",
            "--pull-request",
            "14",
            "--validate",
            "test -f one.txt",
            "--validate",
            "test -f repaired.txt",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created aggregate PR https://github.com/example/pull/1999",
        ));

    let run_root = repo_root.join(".metastack/merge-runs");
    let mut run_dirs = fs::read_dir(&run_root)?
        .map(|entry| entry.map(|item| item.path()))
        .collect::<Result<Vec<_>, _>>()?;
    run_dirs.sort();
    let run_dir = run_dirs.pop().expect("merge run should exist");

    assert!(run_dir.join("context.json").is_file());
    assert!(run_dir.join("plan.json").is_file());
    assert!(run_dir.join("merge-progress.json").is_file());
    assert!(run_dir.join("validation.json").is_file());
    assert!(run_dir.join("publication.json").is_file());
    assert!(
        run_dir
            .join("validation-repair-prompt-attempt-1.md")
            .is_file()
    );
    assert!(
        run_dir
            .join("validation-repair-output-attempt-1.md")
            .is_file()
    );
    let validation = fs::read_to_string(run_dir.join("validation.json"))?;
    assert!(validation.contains("\"success\": true"));
    assert!(validation.contains("\"repair_attempts\": 1"));
    assert!(validation.contains("\"command\": \"test -f repaired.txt\""));
    assert!(validation.contains("\"exit_code\": 1"));
    assert!(validation.contains("\"exit_code\": 0"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_retries_transient_validation_failures_without_consuming_repair_budget()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let agent_stub = temp.path().join("merge-agent-stub");
    let flaky_validation = temp.path().join("flaky-validation");
    let output_dir = temp.path().join("output");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&output_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[23, 24],
        "https://github.com/example/pull/2001",
    )?;
    write_agent_stub(&agent_stub, &[23, 24])?;
    write_flaky_validation_stub(&flaky_validation)?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            agent_stub.display()
        ),
    )?;

    commit_and_push_pull_ref(&repo_root, "feature/23", "one.txt", "one\n", 23)?;
    commit_and_push_pull_ref(&repo_root, "feature/24", "two.txt", "two\n", 24)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "merge",
            "--no-interactive",
            "--pull-request",
            "23",
            "--pull-request",
            "24",
            "--validate",
            flaky_validation
                .to_str()
                .expect("validation path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created aggregate PR https://github.com/example/pull/2001",
        ));

    let run_root = repo_root.join(".metastack/merge-runs");
    let mut run_dirs = fs::read_dir(&run_root)?
        .map(|entry| entry.map(|item| item.path()))
        .collect::<Result<Vec<_>, _>>()?;
    run_dirs.sort();
    let run_dir = run_dirs.pop().expect("merge run should exist");
    let validation = fs::read_to_string(run_dir.join("validation.json"))?;
    assert!(validation.contains("\"success\": true"));
    assert!(validation.contains("\"repair_attempts\": 0"));
    assert!(validation.contains("\"attempt\": 2"));
    assert!(
        !run_dir
            .join("validation-repair-output-attempt-1.md")
            .exists()
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("validation-count.txt"))?,
        "2"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_retries_aggregate_publication_after_transient_gh_failure() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let agent_stub = temp.path().join("merge-agent-stub");
    let output_dir = temp.path().join("output");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&output_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_retry_create_stub(
        &bin_dir.join("gh"),
        &[25, 26],
        "https://github.com/example/pull/2002",
    )?;
    write_agent_stub(&agent_stub, &[25, 26])?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            agent_stub.display()
        ),
    )?;

    commit_and_push_pull_ref(&repo_root, "feature/25", "one.txt", "one\n", 25)?;
    commit_and_push_pull_ref(&repo_root, "feature/26", "two.txt", "two\n", 26)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "merge",
            "--no-interactive",
            "--pull-request",
            "25",
            "--pull-request",
            "26",
            "--validate",
            "test -f one.txt",
            "--validate",
            "test -f two.txt",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created aggregate PR https://github.com/example/pull/2002",
        ));

    assert_eq!(
        fs::read_to_string(output_dir.join("gh-create-count.txt"))?,
        "3"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_resume_run_revalidates_and_updates_existing_aggregate_pr() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let agent_stub = temp.path().join("merge-agent-stub");
    let output_dir = temp.path().join("output");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&output_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_persistent_pr_stub(
        &bin_dir.join("gh"),
        &[28],
        "https://github.com/example/pull/2004",
    )?;
    write_agent_stub(&agent_stub, &[28])?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[merge]
validation_repair_attempts = 1

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            agent_stub.display()
        ),
    )?;

    commit_and_push_pull_ref(&repo_root, "feature/28", "one.txt", "one\n", 28)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "merge",
            "--no-interactive",
            "--pull-request",
            "28",
            "--validate",
            "test -f repaired.txt",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("validation still needs attention"));

    let run_root = repo_root.join(".metastack/merge-runs");
    let mut run_dirs = fs::read_dir(&run_root)?
        .map(|entry| entry.map(|item| item.path()))
        .collect::<Result<Vec<_>, _>>()?;
    run_dirs.sort();
    let run_dir = run_dirs.pop().expect("merge run should exist");
    let run_id = run_dir
        .file_name()
        .and_then(|value| value.to_str())
        .expect("run id should be utf-8")
        .to_string();

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .env("TEST_VALIDATE_FIX_FILE", "repaired.txt")
        .args([
            "merge",
            "--resume-run",
            &run_id,
            "--validate",
            "test -f repaired.txt",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Updated aggregate PR https://github.com/example/pull/2004",
        ));

    let validation = fs::read_to_string(run_dir.join("validation.json"))?;
    assert!(validation.contains("\"success\": true"));
    let publication = fs::read_to_string(run_dir.join("publication.json"))?;
    assert!(publication.contains("\"action\": \"updated\""));
    assert!(publication.contains("\"validation_success\": true"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_does_not_repeat_transient_retries_for_same_failure_signature() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let agent_stub = temp.path().join("merge-agent-stub");
    let stuck_validation = temp.path().join("stuck-validation");
    let output_dir = temp.path().join("output");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&output_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[27],
        "https://github.com/example/pull/2003",
    )?;
    write_agent_stub(&agent_stub, &[27])?;
    write_stuck_validation_stub(&stuck_validation)?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[merge]
validation_repair_attempts = 2

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            agent_stub.display()
        ),
    )?;

    commit_and_push_pull_ref(&repo_root, "feature/27", "one.txt", "one\n", 27)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "merge",
            "--no-interactive",
            "--pull-request",
            "27",
            "--validate",
            stuck_validation
                .to_str()
                .expect("validation path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("validation still needs attention"));

    let run_root = repo_root.join(".metastack/merge-runs");
    let mut run_dirs = fs::read_dir(&run_root)?
        .map(|entry| entry.map(|item| item.path()))
        .collect::<Result<Vec<_>, _>>()?;
    run_dirs.sort();
    let run_dir = run_dirs.pop().expect("merge run should exist");
    let validation = fs::read_to_string(run_dir.join("validation.json"))?;
    assert!(validation.contains("\"attempt\": 3"));
    assert!(!validation.contains("\"attempt\": 4"));
    assert_eq!(
        fs::read_to_string(output_dir.join("stuck-validation-count.txt"))?,
        "3"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_rejects_planner_output_that_omits_selected_pull_requests() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let agent_stub = temp.path().join("merge-agent-stub");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[15, 16],
        "https://github.com/example/pull/1998",
    )?;
    write_agent_stub(&agent_stub, &[15])?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            agent_stub.display()
        ),
    )?;

    commit_and_push_pull_ref(&repo_root, "feature/15", "one.txt", "one\n", 15)?;
    commit_and_push_pull_ref(&repo_root, "feature/16", "two.txt", "two\n", 16)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "merge",
            "--no-interactive",
            "--pull-request",
            "15",
            "--pull-request",
            "16",
            "--validate",
            "test -f one.txt",
            "--validate",
            "test -f two.txt",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "merge planner must return the full selected pull request set in merge_order",
        ));

    let run_root = repo_root.join(".metastack/merge-runs");
    let mut run_dirs = fs::read_dir(&run_root)?
        .map(|entry| entry.map(|item| item.path()))
        .collect::<Result<Vec<_>, _>>()?;
    run_dirs.sort();
    let run_dir = run_dirs.pop().expect("merge run should exist");

    assert!(run_dir.join("context.json").is_file());
    assert!(!run_dir.join("plan.json").exists());
    assert!(!run_dir.join("merge-progress.json").exists());
    assert!(!run_dir.join("publication.json").exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_resolves_conflicts_with_agent_help() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let agent_stub = temp.path().join("merge-agent-stub");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[21, 22],
        "https://github.com/example/pull/1000",
    )?;
    write_agent_stub(&agent_stub, &[21, 22])?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            agent_stub.display()
        ),
    )?;

    fs::write(repo_root.join("shared.txt"), "base\n")?;
    let status = std::process::Command::new("git")
        .args([
            "-C",
            repo_root.to_string_lossy().as_ref(),
            "add",
            "shared.txt",
        ])
        .status()?;
    assert!(status.success());
    let status = std::process::Command::new("git")
        .args([
            "-C",
            repo_root.to_string_lossy().as_ref(),
            "commit",
            "-m",
            "Add shared file",
        ])
        .status()?;
    assert!(status.success());
    let status = std::process::Command::new("git")
        .args([
            "-C",
            repo_root.to_string_lossy().as_ref(),
            "push",
            "origin",
            "main",
        ])
        .status()?;
    assert!(status.success());

    commit_and_push_pull_ref(&repo_root, "feature/21", "shared.txt", "from pr21\n", 21)?;
    commit_and_push_pull_ref(&repo_root, "feature/22", "shared.txt", "from pr22\n", 22)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "merge",
            "--no-interactive",
            "--pull-request",
            "21",
            "--pull-request",
            "22",
            "--validate",
            "grep -q resolved shared.txt",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created aggregate PR https://github.com/example/pull/1000",
        ));

    let run_root = repo_root.join(".metastack/merge-runs");
    let mut run_dirs = fs::read_dir(&run_root)?
        .map(|entry| entry.map(|item| item.path()))
        .collect::<Result<Vec<_>, _>>()?;
    run_dirs.sort();
    let run_dir = run_dirs.pop().expect("merge run should exist");

    assert!(run_dir.join("conflict-resolution-pr-22.md").is_file());
    let progress = fs::read_to_string(run_dir.join("progress.json"))?;
    assert!(progress.contains("\"pull_request\": 22"));
    assert!(progress.contains("Conflict assistance invoked for pull request #22"));
    assert!(progress.contains("agent-assisted conflict resolution"));
    assert!(fs::read_to_string(run_dir.join("merge-progress.json"))?.contains("conflict_resolved"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_allows_repeat_runs_without_reusing_run_directories() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let agent_stub = temp.path().join("merge-agent-stub");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_stub(
        &bin_dir.join("gh"),
        &[31, 32],
        "https://github.com/example/pull/1001",
    )?;
    write_agent_stub(&agent_stub, &[31, 32])?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            agent_stub.display()
        ),
    )?;

    commit_and_push_pull_ref(&repo_root, "feature/31", "one.txt", "one\n", 31)?;
    commit_and_push_pull_ref(&repo_root, "feature/32", "two.txt", "two\n", 32)?;

    let mut command = cli();
    command.current_dir(&repo_root);
    command.env("METASTACK_CONFIG", &config_path);
    command.env("PATH", prepend_path(&bin_dir)?);
    command.args([
        "merge",
        "--no-interactive",
        "--pull-request",
        "31",
        "--pull-request",
        "32",
        "--validate",
        "test -f one.txt",
        "--validate",
        "test -f two.txt",
    ]);
    command.assert().success();

    let mut repeat = cli();
    repeat.current_dir(&repo_root);
    repeat.env("METASTACK_CONFIG", &config_path);
    repeat.env("PATH", prepend_path(&bin_dir)?);
    repeat.args([
        "merge",
        "--no-interactive",
        "--pull-request",
        "31",
        "--pull-request",
        "32",
        "--validate",
        "test -f one.txt",
        "--validate",
        "test -f two.txt",
    ]);
    repeat.assert().success();

    let run_root = repo_root.join(".metastack/merge-runs");
    let mut run_dirs = fs::read_dir(&run_root)?
        .map(|entry| entry.map(|item| item.path()))
        .collect::<Result<Vec<_>, _>>()?;
    run_dirs.sort();
    assert_eq!(run_dirs.len(), 2);
    assert_ne!(run_dirs[0], run_dirs[1]);

    Ok(())
}

#[cfg(unix)]
#[test]
fn merge_updates_existing_aggregate_pull_request() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let agent_stub = temp.path().join("merge-agent-stub");
    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&bin_dir)?;
    fs::write(repo_root.join("README.md"), "# repo\n")?;
    init_repo_with_origin(&repo_root)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_github_update_stub(
        &bin_dir.join("gh"),
        &[41, 42],
        "https://github.com/example/pull/2000",
    )?;
    write_agent_stub(&agent_stub, &[41, 42])?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            agent_stub.display()
        ),
    )?;

    commit_and_push_pull_ref(&repo_root, "feature/41", "one.txt", "one\n", 41)?;
    commit_and_push_pull_ref(&repo_root, "feature/42", "two.txt", "two\n", 42)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", prepend_path(&bin_dir)?)
        .args([
            "merge",
            "--no-interactive",
            "--pull-request",
            "41",
            "--pull-request",
            "42",
            "--validate",
            "test -f one.txt",
            "--validate",
            "test -f two.txt",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Updated aggregate PR https://github.com/example/pull/2000",
        ));

    let run_root = repo_root.join(".metastack/merge-runs");
    let mut run_dirs = fs::read_dir(&run_root)?
        .map(|entry| entry.map(|item| item.path()))
        .collect::<Result<Vec<_>, _>>()?;
    run_dirs.sort();
    let run_dir = run_dirs.pop().expect("merge run should exist");

    assert!(fs::read_to_string(run_dir.join("publication.json"))?.contains("updated"));

    Ok(())
}
