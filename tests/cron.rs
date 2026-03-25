#![allow(dead_code, unused_imports)]

include!("support/common.rs");

use metastack_cli::branding;

#[cfg(unix)]
fn write_onboarded_config(
    config_path: &std::path::Path,
    config: impl AsRef<str>,
) -> Result<(), Box<dyn Error>> {
    fs::write(
        config_path,
        format!(
            "{}\n[onboarding]\ncompleted = true\n",
            config.as_ref().trim_end()
        ),
    )?;
    Ok(())
}

#[cfg(unix)]
fn prepend_path(bin_dir: &std::path::Path) -> Result<String, Box<dyn Error>> {
    let current_path = std::env::var("PATH")?;
    Ok(format!("{}:{}", bin_dir.display(), current_path))
}

#[cfg(unix)]
fn run_state_files(root: &std::path::Path) -> Result<Vec<std::path::PathBuf>, Box<dyn Error>> {
    let mut paths =
        fs::read_dir(root.join(format!("{}/cron/.runtime/runs", branding::PROJECT_DIR)))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

#[cfg(unix)]
#[test]
fn cron_init_creates_a_markdown_job_template() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "")?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "cron",
            "init",
            "nightly",
            "--schedule",
            "0 * * * *",
            "--command",
            "echo hello",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "Created cron job template at {}/cron/nightly.md",
            branding::PROJECT_DIR
        )));

    let contents = fs::read_to_string(
        temp.path()
            .join(format!("{}/cron/nightly.md", branding::PROJECT_DIR)),
    )?;
    assert!(
        temp.path()
            .join(format!("{}/cron/README.md", branding::PROJECT_DIR))
            .is_file()
    );
    assert!(contents.contains("schedule: 0 * * * *"));
    assert!(contents.contains("command: echo hello"));
    assert!(!contents.contains("## Runbook"));

    Ok(())
}

#[test]
fn cron_init_render_once_shows_dashboard_fields() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "")?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["cron", "init", "--render-once", "--width", "220"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Cron Init Dashboard"))
        .stdout(predicate::str::contains("Schedule preset: Every N minutes"))
        .stdout(predicate::str::contains("Agent prompt: <blank>"))
        .stdout(predicate::str::contains("Save: Create cron job"))
        .stdout(predicate::str::contains("Execution contract:"))
        .stdout(predicate::str::contains("Prompt preview"))
        .stdout(predicate::str::contains("mouse wheel scrolls the editor."))
        .stdout(predicate::str::contains("Ctrl+V pastes text,"));

    Ok(())
}

#[test]
fn cron_init_render_once_prefills_existing_job_values() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(temp.path().join(format!("{}/cron", branding::PROJECT_DIR)))?;
    write_onboarded_config(&config_path, "")?;
    fs::write(
        temp.path()
            .join(format!("{}/cron/nightly.md", branding::PROJECT_DIR)),
        r#"---
schedule: "0 * * * *"
command: "echo old"
agent: "codex"
shell: "/bin/bash"
working_directory: "apps/api"
timeout_seconds: 42
enabled: false
---

Review old output
"#,
    )?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["cron", "init", "nightly", "--render-once", "--width", "220"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Name: nightly"))
        .stdout(predicate::str::contains("Command: echo old"))
        .stdout(predicate::str::contains("Shell: /bin/bash"))
        .stdout(predicate::str::contains("Working directory: apps/api"))
        .stdout(predicate::str::contains("Timeout seconds: 42"))
        .stdout(predicate::str::contains("Enabled: Disabled"));

    Ok(())
}

#[test]
fn cron_init_render_once_prefills_prompt_and_rejection_help() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(temp.path().join(format!("{}/cron", branding::PROJECT_DIR)))?;
    write_onboarded_config(&config_path, "")?;
    fs::write(
        temp.path()
            .join(format!("{}/cron/nightly.md", branding::PROJECT_DIR)),
        r#"---
schedule: "0 * * * *"
agent: "codex"
---

Review old output
"#,
    )?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["cron", "init", "nightly", "--render-once", "--width", "220"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Agent prompt: Review old output"))
        .stdout(predicate::str::contains("Prompt preview"))
        .stdout(predicate::str::contains("Ctrl+V pastes text,"));

    Ok(())
}

#[test]
fn cron_init_no_interactive_writes_agent_prompt_fields() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "")?;

    let assert = cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "runtime",
            "cron",
            "init",
            "nightly",
            "--no-interactive",
            "--schedule",
            "0 * * * *",
            "--command",
            "echo hello",
            "--agent",
            "codex",
            "--prompt",
            "Review the command output",
        ])
        .assert()
        .success();

    let payload: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout)?;
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["command"], "runtime.cron.init");
    assert_eq!(payload["result"]["status"], "created");
    assert_eq!(payload["result"]["name"], "nightly");
    assert_eq!(
        payload["result"]["path"],
        format!("{}/cron/nightly.md", branding::PROJECT_DIR)
    );
    assert_eq!(payload["result"]["schedule"], "0 * * * *");
    assert_eq!(payload["result"]["command"], "echo hello");
    assert_eq!(payload["result"]["agent"], "codex");

    let contents = fs::read_to_string(
        temp.path()
            .join(format!("{}/cron/nightly.md", branding::PROJECT_DIR)),
    )?;
    assert!(contents.contains("agent: codex"));
    assert!(!contents.contains("prompt: Review the command output"));
    assert!(contents.contains("Review the command output"));

    Ok(())
}

#[test]
fn cron_init_no_interactive_missing_schedule_emits_structured_json_error()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "")?;

    let assert = cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "runtime",
            "cron",
            "init",
            "nightly",
            "--no-interactive",
            "--command",
            "echo hello",
        ])
        .assert()
        .failure();

    let payload: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout)?;
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["command"], "runtime.cron.init");
    assert_eq!(payload["error"]["code"], "invalid_input");
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("`--schedule` is required")
    );
    assert!(assert.get_output().stderr.is_empty());

    Ok(())
}

#[test]
fn cron_run_executes_shell_command_and_agent_with_shared_context() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    let output_dir = temp.path().join("agent-output");
    let stub_path = temp.path().join("cron-agent-stub");

    fs::create_dir_all(&output_dir)?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            stub_path.display()
        ),
    )?;
    fs::write(
        &stub_path,
        r#"#!/bin/sh
printf '%s' "$PWD" > "$TEST_OUTPUT_DIR/cwd.txt"
printf '%s' "$1" > "$TEST_OUTPUT_DIR/prompt.txt"
printf '%s' "$METASTACK_CRON_JOB_COMMAND_EXIT_CODE" > "$TEST_OUTPUT_DIR/command-exit-code.txt"
printf '%s' "$METASTACK_CRON_JOB_LOG_PATH" > "$TEST_OUTPUT_DIR/log-path.txt"
printf '%s' "$METASTACK_CRON_JOB_WORKING_DIRECTORY" > "$TEST_OUTPUT_DIR/working-directory.txt"
printf '%s' "$METASTACK_AGENT_PROVIDER_SOURCE" > "$TEST_OUTPUT_DIR/provider-source.txt"
printf '%s' "$METASTACK_AGENT_ROUTE_KEY" > "$TEST_OUTPUT_DIR/route-key.txt"
if [ -f "$PWD/shell-output.txt" ]; then
  cat "$PWD/shell-output.txt" > "$TEST_OUTPUT_DIR/observed-shell.txt"
fi
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "cron",
            "init",
            "nightly",
            "--no-interactive",
            "--schedule",
            "0 * * * *",
            "--command",
            "printf 'hello' > shell-output.txt",
            "--prompt",
            "Inspect the command output",
        ])
        .assert()
        .success();

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args(["runtime", "cron", "run", "nightly"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Ran cron workflow `nightly` successfully as `nightly-",
        ));

    let canonical_root = fs::canonicalize(temp.path())?;
    assert_eq!(
        fs::read_to_string(output_dir.join("observed-shell.txt"))?,
        "hello"
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("command-exit-code.txt"))?,
        "0"
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("working-directory.txt"))?,
        canonical_root.display().to_string()
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("cwd.txt"))?,
        canonical_root.display().to_string()
    );

    let prompt = fs::read_to_string(output_dir.join("prompt.txt"))?;
    assert!(prompt.contains("Inspect the command output"));
    assert!(prompt.contains("## Cron Execution Context"));
    assert!(prompt.contains("Command exit code: 0"));
    assert!(
        fs::read_to_string(output_dir.join("log-path.txt"))?.starts_with(&format!(
            "{}/cron/.runtime/logs/nightly-",
            branding::PROJECT_DIR
        ))
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("provider-source.txt"))?,
        "explicit_override"
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("route-key.txt"))?,
        "runtime.cron.prompt"
    );
    let runtime_log_path = fs::read_to_string(output_dir.join("log-path.txt"))?;
    assert!(runtime_log_path.starts_with(&format!(
        "{}/cron/.runtime/logs/nightly-",
        branding::PROJECT_DIR
    )));
    let runtime_log = fs::read_to_string(temp.path().join(runtime_log_path.trim()))?;
    assert!(runtime_log.contains("Resolved provider: stub"));
    assert!(runtime_log.contains("Resolved route key: runtime.cron.prompt"));
    assert_eq!(run_state_files(temp.path())?.len(), 1);

    Ok(())
}
#[test]
fn cron_run_supports_agent_only_jobs_without_a_shell_command() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    let output_dir = temp.path().join("agent-output");
    let stub_path = temp.path().join("cron-agent-stub");

    fs::create_dir_all(&output_dir)?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            stub_path.display()
        ),
    )?;
    fs::write(
        &stub_path,
        r#"#!/bin/sh
printf '%s' "$1" > "$TEST_OUTPUT_DIR/prompt.txt"
printf '%s' "$METASTACK_CRON_JOB_COMMAND" > "$TEST_OUTPUT_DIR/command.txt"
printf '%s' "$METASTACK_CRON_JOB_COMMAND_EXIT_CODE" > "$TEST_OUTPUT_DIR/command-exit-code.txt"
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "cron",
            "init",
            "nightly",
            "--no-interactive",
            "--schedule",
            "0 * * * *",
            "--prompt",
            "Scan reddit for top posts in r/programming and r/rust",
        ])
        .assert()
        .success();

    let contents = fs::read_to_string(
        temp.path()
            .join(format!("{}/cron/nightly.md", branding::PROJECT_DIR)),
    )?;
    assert!(!contents.contains("command:"));
    assert!(contents.contains("Scan reddit for top posts in r/programming and r/rust"));

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args(["runtime", "cron", "run", "nightly"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Ran cron workflow `nightly` successfully as `nightly-",
        ));

    assert_eq!(fs::read_to_string(output_dir.join("command.txt"))?, "");
    assert_eq!(
        fs::read_to_string(output_dir.join("command-exit-code.txt"))?,
        ""
    );
    assert!(
        fs::read_to_string(output_dir.join("prompt.txt"))?
            .contains("Scan reddit for top posts in r/programming and r/rust")
    );
    assert!(fs::read_to_string(output_dir.join("prompt.txt"))?.contains("Command phase: skipped"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn cron_run_prefers_route_specific_agent_over_global_default() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    let output_dir = temp.path().join("agent-output");
    let bin_dir = temp.path().join("bin");
    let global_stub_path = bin_dir.join("global-stub");
    let route_stub_path = bin_dir.join("cron-route-stub");

    fs::create_dir_all(&output_dir)?;
    fs::create_dir_all(&bin_dir)?;
    write_onboarded_config(
        &config_path,
        r#"[agents]
default_agent = "global-stub"

[agents.routing.commands."runtime.cron.prompt"]
provider = "cron-route-stub"

[agents.commands.global-stub]
command = "global-stub"
args = ["{{payload}}"]
transport = "arg"

[agents.commands.cron-route-stub]
command = "cron-route-stub"
args = ["{{payload}}"]
transport = "arg"
"#,
    )?;
    fs::write(
        &global_stub_path,
        r#"#!/bin/sh
printf '%s' "$1" > "$TEST_OUTPUT_DIR/global.txt"
"#,
    )?;
    let mut permissions = fs::metadata(&global_stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&global_stub_path, permissions)?;
    fs::write(
        &route_stub_path,
        r#"#!/bin/sh
printf '%s' "$1" > "$TEST_OUTPUT_DIR/route.txt"
"#,
    )?;
    let mut permissions = fs::metadata(&route_stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&route_stub_path, permissions)?;

    cli()
        .current_dir(temp.path())
        .env("PATH", prepend_path(&bin_dir)?)
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "cron",
            "init",
            "nightly",
            "--no-interactive",
            "--schedule",
            "0 * * * *",
            "--prompt",
            "Inspect the latest logs",
        ])
        .assert()
        .success();

    cli()
        .current_dir(temp.path())
        .env("PATH", prepend_path(&bin_dir)?)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args(["runtime", "cron", "run", "nightly"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Ran cron workflow `nightly` successfully as `nightly-",
        ));

    assert!(output_dir.join("route.txt").is_file());
    assert!(!output_dir.join("global.txt").exists());
    assert!(fs::read_to_string(output_dir.join("route.txt"))?.contains("Inspect the latest logs"));

    Ok(())
}

#[test]
fn cron_run_reports_agent_failures_and_records_runtime_error() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("cron-agent-stub");

    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "stub"

[agents.commands.stub]
command = "{}"
transport = "arg"
"#,
            stub_path.display()
        ),
    )?;
    fs::write(
        &stub_path,
        r#"#!/bin/sh
echo "agent failed" >&2
exit 9
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "cron",
            "init",
            "nightly",
            "--no-interactive",
            "--schedule",
            "0 * * * *",
            "--prompt",
            "Inspect the latest logs",
        ])
        .assert()
        .success();

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["runtime", "cron", "run", "nightly"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "cron workflow `nightly` failed in run `nightly-",
        ))
        .stderr(predicate::str::contains(
            "agent `stub` exited unsuccessfully (9)",
        ));

    let state = fs::read_to_string(temp.path().join(format!(
        "{}/cron/.runtime/scheduler.json",
        branding::PROJECT_DIR
    )))?;
    let run_state_path = run_state_files(temp.path())?
        .into_iter()
        .next()
        .ok_or_else(|| std::io::Error::other("missing run state file"))?;
    let run_state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&run_state_path)?)?;
    let log = fs::read_to_string(
        temp.path().join(
            run_state["log_path"]
                .as_str()
                .ok_or_else(|| std::io::Error::other("missing log path"))?,
        ),
    )?;
    assert!(state.contains("agent `stub` exited unsuccessfully (9)"));
    assert!(log.contains("agent phase start"));

    Ok(())
}

#[test]
fn cron_status_reports_known_jobs_while_stopped() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "")?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "cron",
            "init",
            "nightly",
            "--schedule",
            "0 * * * *",
            "--command",
            "echo hello",
        ])
        .assert()
        .success();

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["cron", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Cron scheduler: stopped"))
        .stdout(predicate::str::contains("- nightly [enabled]"))
        .stdout(predicate::str::contains("schedule: 0 * * * *"))
        .stdout(predicate::str::contains("next run:"));

    Ok(())
}

#[test]
fn cron_start_replaces_a_stale_pid_and_restarts_the_scheduler() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    let runtime_dir = temp
        .path()
        .join(format!("{}/cron/.runtime", branding::PROJECT_DIR));
    let pid_path = runtime_dir.join("scheduler.pid");
    write_onboarded_config(&config_path, "")?;
    fs::create_dir_all(&runtime_dir)?;
    fs::write(&pid_path, "999999\n")?;

    let assert = meta()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["cron", "start", "--poll-interval-seconds", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Started cron scheduler in the background",
        ));

    let stdout = String::from_utf8(assert.get_output().stdout.clone())?;
    wait_for_path(&pid_path)?;
    let pid = fs::read_to_string(&pid_path)?.trim().parse::<u32>()?;
    assert_ne!(pid, 999999);
    assert!(stdout.contains(&pid.to_string()));

    meta()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["cron", "stop"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "Stopped cron scheduler pid {pid}."
        )));

    wait_for_pid_to_stop(pid)?;
    Ok(())
}

#[test]
fn cron_start_status_and_stop_manage_a_detached_scheduler() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "")?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "cron",
            "init",
            "nightly",
            "--schedule",
            "0 * * * *",
            "--command",
            "echo hello",
        ])
        .assert()
        .success();

    meta()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["cron", "start", "--poll-interval-seconds", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Started cron scheduler in the background",
        ));

    let pid_path = temp.path().join(format!(
        "{}/cron/.runtime/scheduler.pid",
        branding::PROJECT_DIR
    ));
    wait_for_path(&pid_path)?;
    let pid = fs::read_to_string(&pid_path)?.trim().parse::<u32>()?;

    meta()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["cron", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "Cron scheduler: running (pid {pid})"
        )))
        .stdout(predicate::str::contains("- nightly [enabled]"));

    meta()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["cron", "stop"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "Stopped cron scheduler pid {pid}."
        )));

    wait_for_pid_to_stop(pid)?;
    assert!(
        !pid_path.exists(),
        "expected pid file to be removed after stop, found {}",
        pid_path.display()
    );

    meta()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["cron", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Cron scheduler: stopped"));

    Ok(())
}

#[test]
fn cron_list_prefers_repository_definitions_over_install_scoped_duplicates()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    let install_dir = temp.path().join("data/cron");
    let repo_dir = temp.path().join(format!("{}/cron", branding::PROJECT_DIR));
    write_onboarded_config(&config_path, "")?;
    fs::create_dir_all(&install_dir)?;
    fs::create_dir_all(&repo_dir)?;

    fs::write(
        install_dir.join("shared.md"),
        r#"---
schedule: "5 * * * *"
steps:
  - id: install
    type: shell
    command: "echo install"
---
"#,
    )?;
    fs::write(
        repo_dir.join("shared.md"),
        r#"---
schedule: "0 * * * *"
steps:
  - id: repo
    type: shell
    command: "echo repo"
---
"#,
    )?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["runtime", "cron", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("- shared [valid] enabled"))
        .stdout(predicate::str::contains("source: repository"))
        .stdout(predicate::str::contains(format!(
            "file: {}/cron/shared.md",
            branding::PROJECT_DIR
        )))
        .stdout(predicate::str::contains("schedule: 0 * * * *"))
        .stdout(predicate::str::contains("steps: 1"));

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["runtime", "cron", "validate"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Validated 1 cron workflow definition(s) successfully.",
        ));

    Ok(())
}

#[test]
fn cron_validate_rejects_forward_when_references() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "")?;
    fs::create_dir_all(temp.path().join(format!("{}/cron", branding::PROJECT_DIR)))?;
    fs::write(
        temp.path()
            .join(format!("{}/cron/invalid-when.md", branding::PROJECT_DIR)),
        r#"---
schedule: "0 * * * *"
mode: workflow
steps:
  - id: deploy
    type: shell
    command: "printf deploy"
    when:
      step: approve
      exists: true
  - id: approve
    type: approval
    approval_message: "Approve deploy"
---
"#,
    )?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["runtime", "cron", "validate"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "references unknown or later step `approve` in `when.step`",
        ));

    Ok(())
}

#[test]
fn cron_validate_rejects_ambiguous_when_conditions() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "")?;
    fs::create_dir_all(temp.path().join(format!("{}/cron", branding::PROJECT_DIR)))?;
    fs::write(
        temp.path()
            .join(format!("{}/cron/ambiguous-when.md", branding::PROJECT_DIR)),
        r#"---
schedule: "0 * * * *"
mode: workflow
steps:
  - id: prep
    type: shell
    command: "printf prep"
  - id: deploy
    type: shell
    command: "printf deploy"
    when:
      step: prep
      equals: "ready"
      exists: true
---
"#,
    )?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["runtime", "cron", "validate"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "must set only one of `when.equals`, `when.not_equals`, or `when.exists`",
        ));

    Ok(())
}

#[test]
fn cron_run_waits_for_approval_and_approve_resumes_the_run() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "")?;
    fs::create_dir_all(temp.path().join(format!("{}/cron", branding::PROJECT_DIR)))?;
    fs::write(
        temp.path()
            .join(format!("{}/cron/review.md", branding::PROJECT_DIR)),
        r#"---
schedule: "0 * * * *"
mode: workflow
steps:
  - id: prep
    type: shell
    command: "printf prep > prep.txt"
  - id: approval
    type: approval
    approval_message: "Approve release packaging"
  - id: finalize
    type: shell
    command: "printf done > done.txt"
---
"#,
    )?;

    let run_output = cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["runtime", "cron", "run", "review"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Cron workflow `review` is waiting for approval in run `review-",
        ));

    let run_state_path = run_state_files(temp.path())?
        .into_iter()
        .next()
        .ok_or_else(|| std::io::Error::other("missing run state file"))?;
    let run_state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&run_state_path)?)?;
    let run_id = run_state["run_id"]
        .as_str()
        .ok_or_else(|| std::io::Error::other("missing run id"))?
        .to_string();

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["runtime", "cron", "approvals", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains(&run_id))
        .stdout(predicate::str::contains("Approve release packaging"));

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["runtime", "cron", "approve", &run_id, "--note", "ship it"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Ran cron workflow `review` successfully as `review-",
        ));

    assert_eq!(fs::read_to_string(temp.path().join("prep.txt"))?, "prep");
    assert_eq!(fs::read_to_string(temp.path().join("done.txt"))?, "done");
    let updated_run: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&run_state_path)?)?;
    assert_eq!(updated_run["status"], "succeeded");
    assert_eq!(updated_run["steps"][1]["output"]["status"], "approved");
    assert_eq!(updated_run["steps"][1]["output"]["note"], "ship it");
    let _ = run_output;

    Ok(())
}

#[test]
fn cron_reject_marks_waiting_run_as_rejected() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "")?;
    fs::create_dir_all(temp.path().join(format!("{}/cron", branding::PROJECT_DIR)))?;
    fs::write(
        temp.path()
            .join(format!("{}/cron/review.md", branding::PROJECT_DIR)),
        r#"---
schedule: "0 * * * *"
steps:
  - id: approval
    type: approval
    approval_message: "Approve release packaging"
---
"#,
    )?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["runtime", "cron", "run", "review"])
        .assert()
        .success();

    let run_state_path = run_state_files(temp.path())?
        .into_iter()
        .next()
        .ok_or_else(|| std::io::Error::other("missing run state file"))?;
    let run_state: serde_json::Value = serde_json::from_str(&fs::read_to_string(&run_state_path)?)?;
    let run_id = run_state["run_id"]
        .as_str()
        .ok_or_else(|| std::io::Error::other("missing run id"))?
        .to_string();

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "runtime",
            "cron",
            "reject",
            &run_id,
            "--reason",
            "not ready",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rejected cron workflow run"));

    let updated_run: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&run_state_path)?)?;
    assert_eq!(updated_run["status"], "rejected");
    assert_eq!(updated_run["steps"][0]["output"]["reason"], "not ready");

    Ok(())
}

#[test]
fn cron_resume_reuses_completed_steps_after_a_failed_run() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "")?;
    fs::create_dir_all(temp.path().join(format!("{}/cron", branding::PROJECT_DIR)))?;
    let workflow_path = temp
        .path()
        .join(format!("{}/cron/resume.md", branding::PROJECT_DIR));
    fs::write(
        &workflow_path,
        r#"---
schedule: "0 * * * *"
retry:
  max_attempts: 1
steps:
  - id: first
    type: shell
    command: "count=$(cat first-count.txt 2>/dev/null || echo 0); count=$((count + 1)); printf '%s' \"$count\" > first-count.txt"
  - id: second
    type: shell
    command: "exit 1"
---
"#,
    )?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["runtime", "cron", "run", "resume"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "cron workflow `resume` failed in run `resume-",
        ));

    let run_state_path = run_state_files(temp.path())?
        .into_iter()
        .next()
        .ok_or_else(|| std::io::Error::other("missing run state file"))?;
    let failed_run: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&run_state_path)?)?;
    let run_id = failed_run["run_id"]
        .as_str()
        .ok_or_else(|| std::io::Error::other("missing run id"))?
        .to_string();
    assert_eq!(
        fs::read_to_string(temp.path().join("first-count.txt"))?,
        "1"
    );

    fs::write(
        &workflow_path,
        r#"---
schedule: "0 * * * *"
retry:
  max_attempts: 1
steps:
  - id: first
    type: shell
    command: "count=$(cat first-count.txt 2>/dev/null || echo 0); count=$((count + 1)); printf '%s' \"$count\" > first-count.txt"
  - id: second
    type: shell
    command: "printf done > second.txt"
---
"#,
    )?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["runtime", "cron", "resume", &run_id])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Ran cron workflow `resume` successfully as `resume-",
        ));

    assert_eq!(
        fs::read_to_string(temp.path().join("first-count.txt"))?,
        "1"
    );
    assert_eq!(fs::read_to_string(temp.path().join("second.txt"))?, "done");
    let resumed_run: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&run_state_path)?)?;
    assert_eq!(resumed_run["status"], "succeeded");
    assert_eq!(resumed_run["steps"][0]["attempt_count"], 1);
    assert_eq!(resumed_run["steps"][1]["attempt_count"], 2);

    Ok(())
}

#[cfg(unix)]
#[test]
fn cron_resume_rejects_runs_still_marked_running() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "")?;
    fs::create_dir_all(
        temp.path()
            .join(format!("{}/cron/.runtime/runs", branding::PROJECT_DIR)),
    )?;

    let now = chrono::Utc::now().to_rfc3339();
    fs::write(
        temp.path().join(format!(
            "{}/cron/.runtime/runs/running-demo.json",
            branding::PROJECT_DIR
        )),
        format!(
            r#"{{
  "version": 1,
  "run_id": "running-demo",
  "job_name": "demo",
  "definition_path": "{project_dir}/cron/demo.md",
  "source": {{
    "kind": "repository",
    "label": "repository",
    "path": "{project_dir}/cron"
  }},
  "trigger": "manual",
  "status": "running",
  "created_at": "{now}",
  "updated_at": "{now}",
  "started_at": "{now}",
  "retry": {{
    "max_attempts": 1,
    "backoff_seconds": 0
  }},
  "log_path": "{project_dir}/cron/.runtime/logs/running-demo.log",
  "steps": [],
  "attempts": []
}}"#,
            project_dir = branding::PROJECT_DIR,
        ),
    )?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["runtime", "cron", "resume", "running-demo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "is still marked as running; wait for it to finish or restart the scheduler to reconcile it before resuming",
        ));

    Ok(())
}

#[test]
fn cron_validate_accepts_shipped_sample_workflows() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    let repo_cron_dir = temp
        .path()
        .join(format!("{}/cron/samples", branding::PROJECT_DIR));
    write_onboarded_config(&config_path, "")?;
    fs::create_dir_all(&repo_cron_dir)?;

    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::copy(
        repo_root.join("src/artifacts/cron/linear-triage-sample.md"),
        repo_cron_dir.join("linear-triage-sample.md"),
    )?;
    fs::copy(
        repo_root.join("src/artifacts/cron/github-pr-review-sample.md"),
        repo_cron_dir.join("github-pr-review-sample.md"),
    )?;

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["runtime", "cron", "validate"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Validated 2 cron workflow definition(s) successfully.",
        ));

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["runtime", "cron", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("linear-triage-sample"))
        .stdout(predicate::str::contains("github-pr-review-sample"))
        .stdout(predicate::str::contains("disabled"));

    Ok(())
}
