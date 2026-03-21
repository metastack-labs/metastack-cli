#![allow(dead_code, unused_imports)]

include!("support/common.rs");

fn ensure_workflow_test_config(config_path: &Path) -> Result<(), Box<dyn Error>> {
    let onboarding_block = "[onboarding]\ncompleted = true\n";
    let updated = match fs::read_to_string(config_path) {
        Ok(existing) => {
            if existing.contains("[onboarding]") {
                existing
            } else if existing.trim().is_empty() {
                onboarding_block.to_string()
            } else {
                format!("{onboarding_block}\n{existing}")
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => onboarding_block.to_string(),
        Err(error) => return Err(Box::new(error)),
    };

    fs::write(config_path, updated)?;
    Ok(())
}

fn workflow_test_command(
    mut command: Command,
    repo_root: &Path,
    config_path: &Path,
) -> Result<Command, Box<dyn Error>> {
    ensure_workflow_test_config(config_path)?;
    command.current_dir(repo_root);
    command.env("METASTACK_CONFIG", config_path);
    Ok(command)
}

trait WorkflowCommandExt {
    fn workflow_repo(self, repo_root: &Path, config_path: &Path)
    -> Result<Command, Box<dyn Error>>;
}

impl WorkflowCommandExt for Command {
    fn workflow_repo(
        self,
        repo_root: &Path,
        config_path: &Path,
    ) -> Result<Command, Box<dyn Error>> {
        workflow_test_command(self, repo_root, config_path)
    }
}

#[test]
fn workflows_list_shows_builtin_playbooks() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    ensure_workflow_test_config(&config_path)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "workflows",
            "list",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Available workflows"))
        .stdout(predicate::str::contains("backlog-planning"))
        .stdout(predicate::str::contains("ticket-implementation"))
        .stdout(predicate::str::contains("pr-review"))
        .stdout(predicate::str::contains("incident-triage"));

    Ok(())
}

#[test]
fn workflows_explain_describes_ticket_implementation_contract() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    ensure_workflow_test_config(&config_path)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "workflows",
            "explain",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "ticket-implementation",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Workflow: ticket-implementation"))
        .stdout(predicate::str::contains("Linear issue parameter: `issue`"))
        .stdout(predicate::str::contains("implementation_notes"))
        .stdout(predicate::str::contains("Validation"))
        .stdout(predicate::str::contains("Prompt Template"));

    Ok(())
}

#[test]
fn workflows_dry_run_reports_resolved_agent_diagnostics() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "agent": {
    "provider": "claude",
    "model": "sonnet",
    "reasoning": "medium"
  }
}
"#,
    )?;
    fs::write(
        &config_path,
        r#"[agents]
default_agent = "codex"
default_model = "gpt-5.4"
default_reasoning = "low"
"#,
    )?;
    ensure_workflow_test_config(&config_path)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "workflows",
            "run",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "backlog-planning",
            "--dry-run",
            "--param",
            "request=Plan the next release",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Resolved provider: claude"))
        .stdout(predicate::str::contains("Resolved model: sonnet"))
        .stdout(predicate::str::contains("Resolved reasoning: medium"))
        .stdout(predicate::str::contains("Provider source: repo_default"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflows_run_executes_builtin_codex_provider_adapter() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    fs::create_dir_all(repo_root.join(".metastack/workflows"))?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    fs::write(
        repo_root.join(".metastack/workflows/builtin-proof.md"),
        r#"---
name: builtin-proof
summary: Minimal workflow for builtin provider execution.
provider: codex
model: gpt-5.4
reasoning: medium
---

Summarize the builtin provider launch behavior.
"#,
    )?;
    fs::write(
        &config_path,
        r#"[agents]
default_agent = "codex"
default_model = "gpt-5.4"
default_reasoning = "medium"
"#,
    )?;

    let stub_path = bin_dir.join("codex");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
if [ "$1" = "--help" ]; then
  cat <<'EOF'
-a, --ask-for-approval <APPROVAL_POLICY>
-s, --sandbox <SANDBOX_MODE>
-C, --cd <DIR>
    --add-dir <DIR>
    --dangerously-bypass-approvals-and-sandbox
EOF
  exit 0
fi
if [ "$1" = "exec" ] && [ "$2" = "--help" ]; then
  cat <<'EOF'
-j, --json
-m, --model <MODEL>
-c, --config <key=value>
EOF
  exit 0
fi
printf '%s\n' "$@" > "$TEST_OUTPUT_DIR/args.txt"
printf '%s' "$METASTACK_AGENT_NAME" > "$TEST_OUTPUT_DIR/agent.txt"
printf '%s' "$METASTACK_AGENT_MODEL" > "$TEST_OUTPUT_DIR/model.txt"
printf '%s' "$METASTACK_AGENT_REASONING" > "$TEST_OUTPUT_DIR/reasoning.txt"
printf '%s\n' '{"type":"thread.started","thread_id":"workflow-thread"}'
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"codex builtin ok"}}'
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

    let current_path = std::env::var("PATH")?;
    meta()
        .workflow_repo(&repo_root, &config_path)?
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "workflows",
            "run",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "builtin-proof",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("provider `codex`"))
        .stdout(predicate::str::contains("codex builtin ok"));

    let args = fs::read_to_string(stub_dir.join("args.txt"))?;
    assert!(args.contains("--sandbox"));
    assert!(args.contains("workspace-write"));
    assert!(args.contains("--ask-for-approval"));
    assert!(args.contains("never"));
    assert!(args.contains("exec"));
    assert!(args.contains("--json"));
    assert!(args.contains("--model=gpt-5.4"));
    assert!(args.contains("-c"));
    assert!(args.contains("reasoning.effort=\"medium\""));
    assert!(!args.contains("--reasoning="));
    assert!(args.contains("Summarize the builtin provider launch behavior."));
    assert_eq!(fs::read_to_string(stub_dir.join("agent.txt"))?, "codex");
    assert_eq!(fs::read_to_string(stub_dir.join("model.txt"))?, "gpt-5.4");
    assert_eq!(
        fs::read_to_string(stub_dir.join("reasoning.txt"))?,
        "medium"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflows_run_executes_builtin_claude_provider_adapter() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    fs::create_dir_all(repo_root.join(".metastack/workflows"))?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    fs::write(
        repo_root.join(".metastack/workflows/builtin-proof.md"),
        r#"---
name: builtin-proof
summary: Minimal workflow for builtin provider execution.
provider: claude
model: sonnet
reasoning: high
---

Summarize the builtin provider launch behavior.
"#,
    )?;
    fs::write(
        &config_path,
        r#"[agents]
default_agent = "claude"
default_model = "sonnet"
default_reasoning = "high"
"#,
    )?;

    let stub_path = bin_dir.join("claude");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
if [ "$1" = "-p" ] && [ "$2" = "--help" ]; then
  cat <<'EOF'
-p, --print
--model <model>
--effort <level>
--output-format <format>
--permission-mode <mode>
EOF
  exit 0
fi
printf '%s\n' "$@" > "$TEST_OUTPUT_DIR/args.txt"
printf '%s' "$METASTACK_AGENT_NAME" > "$TEST_OUTPUT_DIR/agent.txt"
printf '%s' "$METASTACK_AGENT_MODEL" > "$TEST_OUTPUT_DIR/model.txt"
printf '%s' "$METASTACK_AGENT_REASONING" > "$TEST_OUTPUT_DIR/reasoning.txt"
printf '%s' '{"type":"result","subtype":"success","result":"claude builtin ok","session_id":"workflow-session"}'
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

    let current_path = std::env::var("PATH")?;
    meta()
        .workflow_repo(&repo_root, &config_path)?
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "workflows",
            "run",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "builtin-proof",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("provider `claude`"))
        .stdout(predicate::str::contains("claude builtin ok"));

    let args = fs::read_to_string(stub_dir.join("args.txt"))?;
    assert!(args.contains("-p"));
    assert!(args.contains("--output-format=json"));
    assert!(args.contains("--model=sonnet"));
    assert!(args.contains("--effort=high"));
    assert!(!args.contains("--reasoning="));
    assert!(args.contains("Summarize the builtin provider launch behavior."));
    assert_eq!(fs::read_to_string(stub_dir.join("agent.txt"))?, "claude");
    assert_eq!(fs::read_to_string(stub_dir.join("model.txt"))?, "sonnet");
    assert_eq!(fs::read_to_string(stub_dir.join("reasoning.txt"))?, "high");

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflows_run_fails_fast_when_builtin_codex_help_surface_drift_is_detected()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(repo_root.join(".metastack/workflows"))?;
    fs::create_dir_all(&bin_dir)?;

    fs::write(
        repo_root.join(".metastack/workflows/builtin-proof.md"),
        r#"---
name: builtin-proof
summary: Minimal workflow for builtin provider execution.
provider: codex
model: gpt-5.4
reasoning: medium
---

Summarize the builtin provider launch behavior.
"#,
    )?;
    fs::write(
        &config_path,
        r#"[agents]
default_agent = "codex"
default_model = "gpt-5.4"
default_reasoning = "medium"
"#,
    )?;

    let stub_path = bin_dir.join("codex");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
if [ "$1" = "--help" ]; then
  cat <<'EOF'
-a, --ask-for-approval <APPROVAL_POLICY>
-s, --sandbox <SANDBOX_MODE>
-C, --cd <DIR>
    --add-dir <DIR>
EOF
  exit 0
fi
if [ "$1" = "exec" ] && [ "$2" = "--help" ]; then
  cat <<'EOF'
-m, --model <MODEL>
EOF
  exit 0
fi
printf 'unexpected launch'
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

    let current_path = std::env::var("PATH")?;
    meta()
        .workflow_repo(&repo_root, &config_path)?
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "workflows",
            "run",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "builtin-proof",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "built-in provider `codex` launch validation failed before running",
        ))
        .stderr(predicate::str::contains("model: gpt-5.4"))
        .stderr(predicate::str::contains("reasoning: medium"))
        .stderr(predicate::str::contains("codex --sandbox workspace-write"))
        .stderr(predicate::str::contains("codex exec --help"))
        .stderr(predicate::str::contains(
            "does not advertise emitted flag `-c`",
        ));

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflows_run_provider_override_skips_incompatible_route_model_and_reasoning()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    fs::create_dir_all(repo_root.join(".metastack/workflows"))?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    write_minimal_planning_context(
        &repo_root,
        r#"{
  "agent": {
    "provider": "claude",
    "model": "sonnet",
    "reasoning": "medium"
  }
}
"#,
    )?;
    fs::write(
        repo_root.join(".metastack/workflows/provider-override-proof.md"),
        r#"---
name: provider-override-proof
summary: Verify provider overrides skip incompatible route defaults.
provider: codex
---

Summarize the resolved provider selection.
"#,
    )?;
    fs::write(
        &config_path,
        r#"[agents]
default_agent = "codex"
default_model = "gpt-5.4"
default_reasoning = "low"

[agents.routing.commands."agents.workflows.run"]
provider = "codex"
model = "gpt-5.4"
reasoning = "high"
"#,
    )?;

    let stub_path = bin_dir.join("claude");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
if [ "$1" = "-p" ] && [ "$2" = "--help" ]; then
  cat <<'EOF'
-p, --print
--model <model>
--effort <level>
--output-format <format>
--permission-mode <mode>
EOF
  exit 0
fi
printf '%s\n' "$@" > "$TEST_OUTPUT_DIR/args.txt"
printf '%s' "$METASTACK_AGENT_NAME" > "$TEST_OUTPUT_DIR/agent.txt"
printf '%s' "$METASTACK_AGENT_MODEL" > "$TEST_OUTPUT_DIR/model.txt"
printf '%s' "$METASTACK_AGENT_REASONING" > "$TEST_OUTPUT_DIR/reasoning.txt"
printf '%s' "$METASTACK_AGENT_PROVIDER_SOURCE" > "$TEST_OUTPUT_DIR/provider-source.txt"
printf '%s' "$METASTACK_AGENT_MODEL_SOURCE" > "$TEST_OUTPUT_DIR/model-source.txt"
printf '%s' "$METASTACK_AGENT_REASONING_SOURCE" > "$TEST_OUTPUT_DIR/reasoning-source.txt"
printf '%s' '{"type":"result","subtype":"success","result":"claude override ok","session_id":"workflow-session"}'
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

    let current_path = std::env::var("PATH")?;
    meta()
        .workflow_repo(&repo_root, &config_path)?
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "workflows",
            "run",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "provider-override-proof",
            "--provider",
            "claude",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("provider `claude`"))
        .stdout(predicate::str::contains("claude override ok"));

    let args = fs::read_to_string(stub_dir.join("args.txt"))?;
    assert!(args.contains("-p"));
    assert!(args.contains("--output-format=json"));
    assert!(args.contains("--model=sonnet"));
    assert!(!args.contains("--model=gpt-5.4"));
    assert!(!args.contains("--reasoning="));
    assert_eq!(fs::read_to_string(stub_dir.join("agent.txt"))?, "claude");
    assert_eq!(fs::read_to_string(stub_dir.join("model.txt"))?, "sonnet");
    assert_eq!(
        fs::read_to_string(stub_dir.join("reasoning.txt"))?,
        "medium"
    );
    assert_eq!(
        fs::read_to_string(stub_dir.join("provider-source.txt"))?,
        "explicit_override"
    );
    assert_eq!(
        fs::read_to_string(stub_dir.join("model-source.txt"))?,
        "repo_default"
    );
    assert_eq!(
        fs::read_to_string(stub_dir.join("reasoning-source.txt"))?,
        "repo_default"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflows_run_resolves_linear_issue_and_executes_selected_provider() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(repo_root.join("src"))?;
    fs::create_dir_all(repo_root.join("instructions"))?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    fs::write(
        repo_root.join("Cargo.toml"),
        r#"[package]
name = "workflow-demo"
version = "0.1.0"
edition = "2024"
"#,
    )?;
    fs::write(repo_root.join("README.md"), "# Workflow Demo\n")?;
    fs::write(repo_root.join("src/main.rs"), "fn main() {}\n")?;
    fs::write(
        repo_root.join("AGENTS.md"),
        "# Repo Rules\nUse focused validation.\n",
    )?;
    fs::write(
        repo_root.join("instructions/listen.md"),
        "# Listener Instructions\nKeep the workpad current.\n",
    )?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "instructions_path": "instructions/listen.md"
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

[agents.commands.workflow-stub]
command = "workflow-stub"
args = ["{{{{payload}}}}"]
transport = "arg"
"#,
        ),
    )?;

    let stub_path = bin_dir.join("workflow-stub");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
printf '%s' "$METASTACK_AGENT_PROMPT" > "$TEST_OUTPUT_DIR/prompt.txt"
printf '%s' "$METASTACK_AGENT_INSTRUCTIONS" > "$TEST_OUTPUT_DIR/instructions.txt"
printf 'workflow stub ok'
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

    let issues_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [issue_node(
                        "issue-93",
                        "MET-93",
                        "Introduce reusable workflow playbooks",
                        "Add workflow and context commands",
                        "state-2",
                        "In Progress"
                    )]
                }
            }
        }));
    });
    let detail_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)");
        then.status(200).json_body(json!({
            "data": {
                "issue": listen_issue_detail_node(
                    "issue-93",
                    "MET-93",
                    "Introduce reusable workflow playbooks",
                    "Add workflow and context commands",
                    "state-2",
                    "In Progress",
                    Vec::new(),
                    Vec::new(),
                    Vec::new()
                )
            }
        }));
    });

    let current_path = std::env::var("PATH")?;
    meta()
        .workflow_repo(&repo_root, &config_path)?
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "workflows",
            "run",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "ticket-implementation",
            "--provider",
            "workflow-stub",
            "--param",
            "issue=MET-93",
            "--param",
            "implementation_notes=Focus on repeatable CLI behavior.",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Ran workflow `ticket-implementation`",
        ))
        .stdout(predicate::str::contains("workflow stub ok"));

    issues_mock.assert_calls(1);
    detail_mock.assert_calls(1);
    let prompt = fs::read_to_string(stub_dir.join("prompt.txt"))?;
    let instructions = fs::read_to_string(stub_dir.join("instructions.txt"))?;
    assert!(prompt.contains("Introduce reusable workflow playbooks"));
    assert!(prompt.contains("Focus on repeatable CLI behavior."));
    assert!(prompt.contains("## Built-in Workflow Contract"));
    assert!(prompt.contains("Repo Rules"));
    assert!(instructions.contains("senior engineer preparing to implement"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflows_run_uses_route_specific_provider_defaults() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    let stub_dir = temp.path().join("stub-output");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    fs::create_dir_all(repo_root.join("src"))?;
    fs::create_dir_all(repo_root.join("instructions"))?;
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&stub_dir)?;

    fs::write(
        repo_root.join("Cargo.toml"),
        r#"[package]
name = "workflow-demo"
version = "0.1.0"
edition = "2024"
"#,
    )?;
    fs::write(repo_root.join("README.md"), "# Workflow Demo\n")?;
    fs::write(repo_root.join("src/main.rs"), "fn main() {}\n")?;
    fs::write(
        repo_root.join("instructions/listen.md"),
        "# Listener Instructions\nKeep the workpad current.\n",
    )?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "instructions_path": "instructions/listen.md"
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

[agents]
default_agent = "codex"
default_model = "gpt-5.4"

[agents.routing.commands."agents.workflows.run"]
provider = "workflow-stub"

[agents.commands.workflow-stub]
command = "workflow-stub"
args = ["{{{{payload}}}}"]
transport = "arg"
"#,
        ),
    )?;

    let stub_path = bin_dir.join("workflow-stub");
    fs::write(
        &stub_path,
        r#"#!/bin/sh
printf '%s' "$METASTACK_AGENT_NAME" > "$TEST_OUTPUT_DIR/agent.txt"
printf 'workflow route stub ok'
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

    let issues_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [issue_node(
                        "issue-93",
                        "MET-93",
                        "Introduce reusable workflow playbooks",
                        "Add workflow and context commands",
                        "state-2",
                        "In Progress"
                    )]
                }
            }
        }));
    });
    let detail_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue($id: String!)");
        then.status(200).json_body(json!({
            "data": {
                "issue": listen_issue_detail_node(
                    "issue-93",
                    "MET-93",
                    "Introduce reusable workflow playbooks",
                    "Add workflow and context commands",
                    "state-2",
                    "In Progress",
                    Vec::new(),
                    Vec::new(),
                    Vec::new()
                )
            }
        }));
    });

    let current_path = std::env::var("PATH")?;
    meta()
        .workflow_repo(&repo_root, &config_path)?
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &stub_dir)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "workflows",
            "run",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "ticket-implementation",
            "--param",
            "issue=MET-93",
            "--param",
            "implementation_notes=Route-specific defaults should win.",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("provider `workflow-stub`"))
        .stdout(predicate::str::contains("workflow route stub ok"));

    assert_eq!(
        fs::read_to_string(stub_dir.join("agent.txt"))?,
        "workflow-stub"
    );
    issues_mock.assert_calls(1);
    detail_mock.assert_calls(1);

    Ok(())
}

#[test]
fn unsupported_provider_model_combination_returns_actionable_error() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(repo_root.join(".metastack/workflows"))?;
    ensure_workflow_test_config(&config_path)?;
    fs::write(
        repo_root.join(".metastack/workflows/invalid-provider.md"),
        r#"---
name: invalid-provider
summary: Minimal workflow for model validation.
provider: codex
---

Validate provider/model compatibility.
"#,
    )?;

    meta()
        .workflow_repo(&repo_root, &config_path)?
        .args([
            "workflows",
            "run",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
            "invalid-provider",
            "--provider",
            "codex",
            "--model",
            "opus",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "model `opus` is not supported for agent `codex`",
        ))
        .stderr(predicate::str::contains("supported models"));

    Ok(())
}
