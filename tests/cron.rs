#![allow(dead_code, unused_imports)]

include!("support/common.rs");

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
        .stdout(predicate::str::contains(
            "Created cron job template at .metastack/cron/nightly.md",
        ));

    let contents = fs::read_to_string(temp.path().join(".metastack/cron/nightly.md"))?;
    assert!(temp.path().join(".metastack/cron/README.md").is_file());
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
    fs::create_dir_all(temp.path().join(".metastack/cron"))?;
    write_onboarded_config(&config_path, "")?;
    fs::write(
        temp.path().join(".metastack/cron/nightly.md"),
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
    fs::create_dir_all(temp.path().join(".metastack/cron"))?;
    write_onboarded_config(&config_path, "")?;
    fs::write(
        temp.path().join(".metastack/cron/nightly.md"),
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
            "echo hello",
            "--agent",
            "codex",
            "--prompt",
            "Review the command output",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created cron job template at .metastack/cron/nightly.md",
        ));

    let contents = fs::read_to_string(temp.path().join(".metastack/cron/nightly.md"))?;
    assert!(contents.contains("agent: codex"));
    assert!(!contents.contains("prompt: Review the command output"));
    assert!(contents.contains("Review the command output"));

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
        .args(["cron", "run", "nightly"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Ran cron job `nightly` successfully.",
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
    assert_eq!(
        fs::read_to_string(output_dir.join("log-path.txt"))?,
        ".metastack/cron/.runtime/logs/nightly.log"
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("provider-source.txt"))?,
        "explicit_override"
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("route-key.txt"))?,
        "runtime.cron.prompt"
    );
    let runtime_log = fs::read_to_string(
        temp.path()
            .join(".metastack/cron/.runtime/logs/nightly.log"),
    )?;
    assert!(runtime_log.contains("Resolved provider: stub"));
    assert!(runtime_log.contains("Resolved route key: runtime.cron.prompt"));

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

    let contents = fs::read_to_string(temp.path().join(".metastack/cron/nightly.md"))?;
    assert!(!contents.contains("command:"));
    assert!(contents.contains("Scan reddit for top posts in r/programming and r/rust"));

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args(["cron", "run", "nightly"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Ran cron job `nightly` successfully.",
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
        .args(["cron", "run", "nightly"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Ran cron job `nightly` successfully.",
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
        .args(["cron", "run", "nightly"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "cron job `nightly` failed: agent `stub` exited unsuccessfully (9)",
        ));

    let state = fs::read_to_string(temp.path().join(".metastack/cron/.runtime/scheduler.json"))?;
    let log = fs::read_to_string(
        temp.path()
            .join(".metastack/cron/.runtime/logs/nightly.log"),
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
    let runtime_dir = temp.path().join(".metastack/cron/.runtime");
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

    let pid_path = temp.path().join(".metastack/cron/.runtime/scheduler.pid");
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
