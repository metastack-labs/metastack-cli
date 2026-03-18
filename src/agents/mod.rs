mod brief;
mod execution;

pub(crate) use execution::run_agent_capture;

pub(crate) use brief::{AgentBriefRequest, TicketMetadata, write_agent_brief};
pub(crate) use execution::{
    AgentExecutionOptions, apply_invocation_environment, command_args_for_invocation,
    format_agent_config_source, render_invocation_diagnostics,
    resolve_agent_invocation_for_planning, validate_invocation_command_surface,
};

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use anyhow::{Context, Result, bail};
    use tempfile::tempdir;

    use super::{
        AgentBriefRequest, TicketMetadata, command_args_for_invocation,
        resolve_agent_invocation_for_planning, write_agent_brief,
    };
    use crate::cli::RunAgentArgs;
    use crate::config::{
        AGENT_ROUTE_AGENTS_LISTEN, AgentCommandConfig, AgentSettings, AppConfig, PlanningMeta,
        PromptTransport,
    };
    use crate::fs::{PlanningPaths, canonicalize_existing_dir, ensure_dir, write_text_file};

    #[test]
    fn write_agent_brief_renders_deterministic_sections() -> Result<()> {
        let temp = tempdir()?;
        let root = temp.path();
        let paths = PlanningPaths::new(root);
        ensure_dir(&paths.codebase_dir)?;
        for (path, contents) in [
            (paths.scan_path(), "# Scan"),
            (paths.architecture_path(), "# Architecture"),
            (paths.concerns_path(), "# Concerns"),
            (paths.conventions_path(), "# Conventions"),
            (paths.integrations_path(), "# Integrations"),
            (paths.stack_path(), "# Stack"),
            (paths.structure_path(), "# Structure"),
            (paths.testing_path(), "# Testing"),
        ] {
            write_text_file(&path, contents, true)?;
        }

        let output = write_agent_brief(
            root,
            AgentBriefRequest {
                ticket: "MET-11".to_string(),
                title_override: Some("CLI Scaffolding & Modules".to_string()),
                goal: None,
                metadata: TicketMetadata::default(),
                output: None,
            },
        )?;

        let brief = fs::read_to_string(output)?;
        assert!(brief.contains("# Agent Kickoff: MET-11"));
        assert!(brief.contains("CLI Scaffolding & Modules"));
        assert!(brief.contains("## Scan"));
        assert!(brief.contains("## Architecture"));
        assert!(brief.contains("## Concerns"));
        assert!(brief.contains("## Integrations"));
        assert!(brief.contains("## Stack"));
        assert!(brief.contains("## Testing"));

        Ok(())
    }

    #[test]
    fn resolve_agent_invocation_uses_configured_default_agent_command() -> Result<()> {
        let mut commands = BTreeMap::new();
        commands.insert(
            "capture".to_string(),
            AgentCommandConfig {
                command: "capture-agent".to_string(),
                args: vec!["{{payload}}".to_string()],
                transport: PromptTransport::Stdin,
            },
        );
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: Some("capture".to_string()),
                default_model: Some("gpt-5".to_string()),
                default_reasoning: None,
                routing: Default::default(),
                commands,
            },
            ..AppConfig::default()
        };

        let invocation = resolve_agent_invocation_for_planning(
            &config,
            &PlanningMeta::default(),
            &RunAgentArgs {
                root: None,
                route_key: None,
                agent: None,
                prompt: "Investigate config loading".to_string(),
                instructions: Some("Reply with a plan".to_string()),
                model: None,
                reasoning: None,
                transport: None,
            },
        )?;

        assert_eq!(invocation.agent, "capture");
        assert_eq!(invocation.command, "capture-agent");
        assert_eq!(invocation.args, vec![invocation.payload.clone()]);
        assert_eq!(invocation.model.as_deref(), Some("gpt-5"));
        assert_eq!(invocation.transport, PromptTransport::Stdin);
        assert!(invocation.payload.contains("Investigate config loading"));
        assert!(invocation.payload.contains("Reply with a plan"));
        assert!(invocation.payload.contains("gpt-5"));

        Ok(())
    }

    #[test]
    fn resolve_agent_invocation_supports_builtin_codex_preset() -> Result<()> {
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: None,
                default_model: Some("gpt-5.3-codex".to_string()),
                default_reasoning: None,
                routing: Default::default(),
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };

        let invocation = resolve_agent_invocation_for_planning(
            &config,
            &PlanningMeta::default(),
            &RunAgentArgs {
                root: None,
                route_key: None,
                agent: Some("codex".to_string()),
                prompt: "Ship setup flow".to_string(),
                instructions: Some("Use concise output".to_string()),
                model: None,
                reasoning: None,
                transport: None,
            },
        )?;

        assert_eq!(invocation.command, "codex");
        assert_eq!(invocation.args[0], "exec");
        assert_eq!(invocation.args[1], "--model=gpt-5.3-codex");
        assert!(invocation.args[2].contains("Ship setup flow"));
        assert!(invocation.args[2].contains("Use concise output"));

        Ok(())
    }

    #[test]
    fn builtin_codex_execution_adds_workspace_write_flags_for_workspace_clones() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let workspace_root = temp.path().join("workspace").join("MET-1");
        fs::create_dir_all(&repo_root)?;
        fs::create_dir_all(
            workspace_root
                .parent()
                .expect("workspace parent should exist"),
        )?;

        run_git(&repo_root, &["init", "-b", "main"])?;
        run_git(&repo_root, &["config", "user.email", "codex@example.com"])?;
        run_git(&repo_root, &["config", "user.name", "Codex Tests"])?;
        fs::write(repo_root.join("README.md"), "# Demo\n")?;
        run_git(&repo_root, &["add", "README.md"])?;
        run_git(&repo_root, &["commit", "-m", "init"])?;
        let remote_root = temp.path().join("origin.git");
        run_git(
            temp.path(),
            &[
                "init",
                "--bare",
                remote_root
                    .to_str()
                    .expect("remote path should be valid utf-8"),
            ],
        )?;
        run_git(
            &repo_root,
            &[
                "remote",
                "add",
                "origin",
                remote_root
                    .to_str()
                    .expect("remote path should be valid utf-8"),
            ],
        )?;
        run_git(&repo_root, &["push", "-u", "origin", "main"])?;
        run_git(
            temp.path(),
            &[
                "clone",
                remote_root
                    .to_str()
                    .expect("remote path should be valid utf-8"),
                workspace_root
                    .to_str()
                    .expect("workspace path should be valid utf-8"),
            ],
        )?;

        let config = AppConfig {
            agents: AgentSettings {
                default_agent: None,
                default_model: Some("gpt-5.3-codex".to_string()),
                default_reasoning: None,
                routing: Default::default(),
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };
        let invocation = resolve_agent_invocation_for_planning(
            &config,
            &PlanningMeta::default(),
            &RunAgentArgs {
                root: None,
                route_key: None,
                agent: Some("codex".to_string()),
                prompt: "Ship setup flow".to_string(),
                instructions: None,
                model: None,
                reasoning: None,
                transport: None,
            },
        )?;

        let command_args = command_args_for_invocation(&invocation, Some(&workspace_root))?;
        let git_dir = git_stdout(
            &workspace_root,
            &["rev-parse", "--path-format=absolute", "--git-dir"],
        )?;
        let git_common_dir = git_stdout(
            &workspace_root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;

        assert_eq!(command_args[0], "--sandbox");
        assert_eq!(command_args[1], "workspace-write");
        assert_eq!(command_args[2], "--ask-for-approval");
        assert_eq!(command_args[3], "never");
        assert_eq!(command_args[4], "--cd");
        assert_eq!(
            command_args[5],
            canonicalize_existing_dir(&workspace_root)?
                .display()
                .to_string()
        );
        assert!(
            command_args
                .windows(2)
                .any(|pair| { pair[0] == "--add-dir" && pair[1] == git_dir })
        );
        assert!(
            command_args
                .windows(2)
                .any(|pair| { pair[0] == "--add-dir" && pair[1] == git_common_dir })
        );
        assert!(command_args.iter().any(|arg| arg == "exec"));

        Ok(())
    }

    #[test]
    fn builtin_codex_execution_uses_config_override_for_reasoning() -> Result<()> {
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: None,
                default_model: Some("gpt-5.4".to_string()),
                default_reasoning: Some("medium".to_string()),
                routing: Default::default(),
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };
        let invocation = resolve_agent_invocation_for_planning(
            &config,
            &PlanningMeta::default(),
            &RunAgentArgs {
                root: None,
                route_key: None,
                agent: Some("codex".to_string()),
                prompt: "Ship setup flow".to_string(),
                instructions: None,
                model: None,
                reasoning: None,
                transport: None,
            },
        )?;

        assert_eq!(invocation.args[0], "exec");
        assert_eq!(invocation.args[1], "--model=gpt-5.4");
        assert_eq!(invocation.args[2], "-c");
        assert_eq!(invocation.args[3], "reasoning.effort=\"medium\"");

        Ok(())
    }

    #[test]
    fn builtin_codex_execution_omits_reasoning_override_when_unset() -> Result<()> {
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: None,
                default_model: Some("gpt-5.4".to_string()),
                default_reasoning: None,
                routing: Default::default(),
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };
        let invocation = resolve_agent_invocation_for_planning(
            &config,
            &PlanningMeta::default(),
            &RunAgentArgs {
                root: None,
                route_key: None,
                agent: Some("codex".to_string()),
                prompt: "Ship setup flow".to_string(),
                instructions: None,
                model: None,
                reasoning: None,
                transport: None,
            },
        )?;

        assert_eq!(invocation.args[0], "exec");
        assert_eq!(invocation.args[1], "--model=gpt-5.4");
        assert!(!invocation.args.iter().any(|arg| arg == "-c"));
        assert!(
            !invocation
                .args
                .iter()
                .any(|arg| arg.starts_with("reasoning.effort="))
        );

        Ok(())
    }

    #[test]
    fn builtin_codex_listen_execution_uses_unrestricted_permissions() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        fs::create_dir_all(&repo_root)?;
        run_git(&repo_root, &["init", "-b", "main"])?;

        let config = AppConfig {
            agents: AgentSettings {
                default_agent: None,
                default_model: Some("gpt-5.4".to_string()),
                default_reasoning: Some("high".to_string()),
                routing: Default::default(),
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };
        let invocation = resolve_agent_invocation_for_planning(
            &config,
            &PlanningMeta::default(),
            &RunAgentArgs {
                root: None,
                route_key: Some(AGENT_ROUTE_AGENTS_LISTEN.to_string()),
                agent: Some("codex".to_string()),
                prompt: "Ship setup flow".to_string(),
                instructions: None,
                model: None,
                reasoning: None,
                transport: None,
            },
        )?;

        let command_args = command_args_for_invocation(&invocation, Some(&repo_root))?;
        assert_eq!(
            command_args[0],
            "--dangerously-bypass-approvals-and-sandbox"
        );
        assert!(command_args.windows(3).any(|window| {
            window[0] == "exec"
                && window[1] == "-c"
                && window[2] == "mcp_servers.linear.enabled=false"
        }));
        assert!(command_args.iter().any(|arg| arg == "--cd"));
        assert!(!command_args.iter().any(|arg| arg == "--sandbox"));
        assert!(!command_args.iter().any(|arg| arg == "--ask-for-approval"));

        Ok(())
    }

    #[test]
    fn resolve_agent_invocation_supports_builtin_claude_preset() -> Result<()> {
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: None,
                default_model: Some("sonnet".to_string()),
                default_reasoning: None,
                routing: Default::default(),
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };

        let invocation = resolve_agent_invocation_for_planning(
            &config,
            &PlanningMeta::default(),
            &RunAgentArgs {
                root: None,
                route_key: None,
                agent: Some("claude".to_string()),
                prompt: "Draft the review summary".to_string(),
                instructions: None,
                model: None,
                reasoning: None,
                transport: None,
            },
        )?;

        assert_eq!(invocation.command, "claude");
        assert_eq!(invocation.args[0], "-p");
        assert_eq!(invocation.args[1], "--model=sonnet");
        assert!(invocation.args[2].contains("Draft the review summary"));

        Ok(())
    }

    #[test]
    fn builtin_claude_execution_adds_effort_when_reasoning_is_configured() -> Result<()> {
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: None,
                default_model: Some("sonnet".to_string()),
                default_reasoning: Some("max".to_string()),
                routing: Default::default(),
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };
        let invocation = resolve_agent_invocation_for_planning(
            &config,
            &PlanningMeta::default(),
            &RunAgentArgs {
                root: None,
                route_key: None,
                agent: Some("claude".to_string()),
                prompt: "Draft the review summary".to_string(),
                instructions: None,
                model: None,
                reasoning: None,
                transport: None,
            },
        )?;

        assert_eq!(invocation.args[0], "-p");
        assert_eq!(invocation.args[1], "--model=sonnet");
        assert_eq!(invocation.args[2], "--effort=max");

        Ok(())
    }

    #[test]
    fn builtin_claude_execution_omits_effort_when_reasoning_is_unset() -> Result<()> {
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: None,
                default_model: Some("sonnet".to_string()),
                default_reasoning: None,
                routing: Default::default(),
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };
        let invocation = resolve_agent_invocation_for_planning(
            &config,
            &PlanningMeta::default(),
            &RunAgentArgs {
                root: None,
                route_key: None,
                agent: Some("claude".to_string()),
                prompt: "Draft the review summary".to_string(),
                instructions: None,
                model: None,
                reasoning: None,
                transport: None,
            },
        )?;

        assert_eq!(invocation.args[0], "-p");
        assert_eq!(invocation.args[1], "--model=sonnet");
        assert!(
            !invocation
                .args
                .iter()
                .any(|arg| arg.starts_with("--effort="))
        );

        Ok(())
    }

    #[test]
    fn builtin_claude_listen_execution_bypasses_permissions() -> Result<()> {
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: None,
                default_model: Some("sonnet".to_string()),
                default_reasoning: Some("high".to_string()),
                routing: Default::default(),
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };
        let invocation = resolve_agent_invocation_for_planning(
            &config,
            &PlanningMeta::default(),
            &RunAgentArgs {
                root: None,
                route_key: Some(AGENT_ROUTE_AGENTS_LISTEN.to_string()),
                agent: Some("claude".to_string()),
                prompt: "Draft the review summary".to_string(),
                instructions: None,
                model: None,
                reasoning: None,
                transport: None,
            },
        )?;

        let command_args = command_args_for_invocation(&invocation, None)?;
        assert_eq!(command_args[0], "--permission-mode=bypassPermissions");
        assert!(command_args.iter().any(|arg| arg == "--effort=high"));

        Ok(())
    }

    fn run_git(root: &Path, args: &[&str]) -> Result<()> {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .with_context(|| format!("failed to run `git {}`", args.join(" ")))?;
        if !status.success() {
            bail!("git {} failed", args.join(" "));
        }

        Ok(())
    }

    fn git_stdout(root: &Path, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .with_context(|| format!("failed to run `git {}`", args.join(" ")))?;
        if !output.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}
