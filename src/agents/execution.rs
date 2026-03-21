use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use crate::agent_provider::{BuiltinInvocationContext, builtin_provider_adapter};
use crate::cli::RunAgentArgs;
use crate::config::{
    AGENT_ROUTE_AGENTS_LISTEN, AGENT_ROUTE_BACKLOG_IMPROVE, AGENT_ROUTE_BACKLOG_PLAN,
    AGENT_ROUTE_BACKLOG_SPLIT, AGENT_ROUTE_CONTEXT_RELOAD, AGENT_ROUTE_CONTEXT_SCAN,
    AGENT_ROUTE_LINEAR_ISSUES_REFINE, AgentConfigOverrides, AgentConfigSource, AppConfig,
    PlanningMeta, PromptTransport, normalize_agent_name, resolve_agent_config,
};
use crate::tui::prompt_images::{PromptImageAttachment, encode_prompt_images_for_provider};

#[derive(Debug, Clone, Default)]
pub(crate) struct AgentExecutionOptions {
    pub(crate) working_dir: Option<PathBuf>,
    pub(crate) extra_env: Vec<(String, String)>,
    pub(crate) capture_output: bool,
    pub(crate) continuation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentContinuation {
    pub(crate) provider: String,
    pub(crate) session_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentTokenUsage {
    pub input: Option<u64>,
    pub output: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct AgentCaptureReport {
    pub(crate) continuation: Option<AgentContinuation>,
    pub stdout: String,
    #[allow(dead_code)]
    pub usage: Option<AgentTokenUsage>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedAgentInvocation {
    pub(crate) agent: String,
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    pub(crate) context: BuiltinInvocationContext,
    pub(crate) model: Option<String>,
    pub(crate) reasoning: Option<String>,
    pub(crate) route_key: Option<String>,
    pub(crate) family_key: Option<String>,
    pub(crate) provider_source: AgentConfigSource,
    pub(crate) model_source: Option<AgentConfigSource>,
    pub(crate) reasoning_source: Option<AgentConfigSource>,
    pub(crate) transport: PromptTransport,
    pub(crate) payload: String,
    pub(crate) attachments: Vec<PromptImageAttachment>,
    pub(crate) builtin_provider: bool,
}

pub(crate) fn render_invocation_diagnostics(invocation: &ResolvedAgentInvocation) -> Vec<String> {
    vec![
        format!("Resolved provider: {}", invocation.agent),
        format!(
            "Resolved model: {}",
            invocation.model.as_deref().unwrap_or("unset")
        ),
        format!(
            "Resolved reasoning: {}",
            invocation.reasoning.as_deref().unwrap_or("unset")
        ),
        format!(
            "Resolved route key: {}",
            invocation.route_key.as_deref().unwrap_or("unset")
        ),
        format!(
            "Resolved family key: {}",
            invocation.family_key.as_deref().unwrap_or("unset")
        ),
        format!(
            "Provider source: {}",
            format_agent_config_source(&invocation.provider_source)
        ),
        format!(
            "Model source: {}",
            invocation
                .model_source
                .as_ref()
                .map(format_agent_config_source)
                .unwrap_or_else(|| "unset".to_string())
        ),
        format!(
            "Reasoning source: {}",
            invocation
                .reasoning_source
                .as_ref()
                .map(format_agent_config_source)
                .unwrap_or_else(|| "unset".to_string())
        ),
    ]
}

pub(crate) fn apply_invocation_environment(
    command: &mut Command,
    invocation: &ResolvedAgentInvocation,
    prompt: &str,
    instructions: Option<&str>,
) {
    command.env("TERM", "dumb");
    command.env("NO_COLOR", "1");
    command.env("CLICOLOR", "0");
    command.env("CLICOLOR_FORCE", "0");
    command.env("FORCE_COLOR", "0");
    command.env("METASTACK_AGENT_NAME", &invocation.agent);
    command.env("METASTACK_AGENT_PROMPT", prompt);
    command.env(
        "METASTACK_AGENT_INSTRUCTIONS",
        instructions.unwrap_or_default(),
    );
    command.env(
        "METASTACK_AGENT_MODEL",
        invocation.model.as_deref().unwrap_or(""),
    );
    command.env(
        "METASTACK_AGENT_REASONING",
        invocation.reasoning.as_deref().unwrap_or(""),
    );
    command.env(
        "METASTACK_AGENT_ROUTE_KEY",
        invocation.route_key.as_deref().unwrap_or(""),
    );
    command.env(
        "METASTACK_AGENT_FAMILY_KEY",
        invocation.family_key.as_deref().unwrap_or(""),
    );
    command.env(
        "METASTACK_AGENT_PROVIDER_SOURCE",
        format_agent_config_source(&invocation.provider_source),
    );
    command.env(
        "METASTACK_AGENT_MODEL_SOURCE",
        invocation
            .model_source
            .as_ref()
            .map(format_agent_config_source)
            .unwrap_or_default(),
    );
    command.env(
        "METASTACK_AGENT_REASONING_SOURCE",
        invocation
            .reasoning_source
            .as_ref()
            .map(format_agent_config_source)
            .unwrap_or_default(),
    );
    command.env(
        "METASTACK_AGENT_ATTACHMENT_COUNT",
        invocation.attachments.len().to_string(),
    );
}

pub(crate) fn apply_noninteractive_agent_environment(command: &mut Command) {
    command.env("TERM", "dumb");
    command.env("NO_COLOR", "1");
    command.env("CLICOLOR", "0");
    command.env("CLICOLOR_FORCE", "0");
    command.env("FORCE_COLOR", "0");
    command.env_remove("COLORTERM");
}

pub(crate) fn attempted_command(command: &str, command_args: &[String]) -> String {
    format!("{command} {}", command_args.join(" "))
}

pub(crate) fn validate_invocation_command_surface(
    invocation: &ResolvedAgentInvocation,
    command_args: &[String],
) -> Result<String> {
    let attempted = attempted_command(&invocation.command, command_args);
    if invocation.builtin_provider {
        builtin_provider_adapter(&invocation.agent)
            .ok_or_else(|| anyhow!("builtin provider `{}` is not configured", invocation.agent))?
            .validate_command_args(command_args)
            .with_context(|| {
                format!(
                    "built-in provider `{}` launch validation failed before running `{attempted}` (model: {}, reasoning: {})",
                    invocation.agent,
                    invocation.model.as_deref().unwrap_or("unset"),
                    invocation.reasoning.as_deref().unwrap_or("unset"),
                )
            })?;
    }

    Ok(attempted)
}

/// Runs one non-interactive agent turn and returns the captured final assistant output.
///
/// Returns an error when config resolution fails, the configured command surface is invalid, the
/// subprocess cannot be launched, or the agent exits unsuccessfully.
pub fn run_agent_capture(args: &RunAgentArgs) -> Result<AgentCaptureReport> {
    let mut continuation = None;
    run_agent_capture_with_continuation(args, &mut continuation)
}

pub(crate) fn run_agent_capture_with_continuation(
    args: &RunAgentArgs,
    continuation: &mut Option<AgentContinuation>,
) -> Result<AgentCaptureReport> {
    let config = AppConfig::load()?;
    let planning_meta = match args.root.as_deref() {
        Some(root) => PlanningMeta::load(root)?,
        None => PlanningMeta::default(),
    };
    let invocation = resolve_agent_invocation_for_planning(&config, &planning_meta, args)?;
    let attempted_continuation = continuation
        .as_ref()
        .filter(|state| state.provider == invocation.agent);
    match run_agent_capture_attempt(&invocation, args, attempted_continuation) {
        Ok(report) => {
            *continuation = report.continuation.clone();
            Ok(report)
        }
        Err(error)
            if attempted_continuation.is_some()
                && invocation.builtin_provider
                && builtin_provider_adapter(&invocation.agent).is_some_and(|provider| {
                    provider.is_invalid_resume_error(&error.to_string())
                }) =>
        {
            *continuation = None;
            let retry = run_agent_capture_attempt(&invocation, args, None)?;
            *continuation = retry.continuation.clone();
            Ok(retry)
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn resolve_agent_invocation_for_planning(
    config: &AppConfig,
    planning_meta: &PlanningMeta,
    args: &RunAgentArgs,
) -> Result<ResolvedAgentInvocation> {
    let resolved = resolve_agent_config(
        config,
        planning_meta,
        args.route_key.as_deref(),
        AgentConfigOverrides {
            provider: args.agent.clone(),
            model: args.model.clone(),
            reasoning: args.reasoning.clone(),
        },
    )?;
    let agent_name = normalize_agent_name(&resolved.provider);
    let builtin_provider = builtin_provider_adapter(&agent_name).is_some();

    let model = resolved.model;
    let reasoning = resolved.reasoning;
    if !builtin_provider && !args.attachments.is_empty() {
        bail!(
            "agent `{agent_name}` does not support prompt image attachments; use built-in `codex` or `claude`, or remove the attachments"
        );
    }
    let payload = render_agent_payload(
        &agent_name,
        &args.prompt,
        args.instructions.as_deref(),
        model.as_deref(),
        reasoning.as_deref(),
        &args.attachments,
    )?;
    let context = builtin_invocation_context(args.route_key.as_deref());
    let (command, rendered_args, transport) =
        if let Some(provider) = builtin_provider_adapter(&agent_name) {
            let transport = args
                .transport
                .map(Into::into)
                .unwrap_or_else(|| provider.transport());
            let mut launch_args = provider.launch_args(model.as_deref(), reasoning.as_deref());
            if transport == PromptTransport::Arg {
                launch_args.push(payload.clone());
            }
            (
                provider.launch_command().to_string(),
                launch_args,
                transport,
            )
        } else {
            let mut definition = config
                .resolve_agent_definition(&agent_name)
                .ok_or_else(|| anyhow!("agent `{agent_name}` is not configured"))?;

            if let Some(transport) = args.transport {
                definition.transport = transport.into();
            }

            let mut rendered_args = render_command_args(
                &definition.args,
                &args.prompt,
                args.instructions.as_deref(),
                model.as_deref(),
                reasoning.as_deref(),
                &payload,
            );
            if definition.transport == PromptTransport::Arg
                && !definition
                    .args
                    .iter()
                    .any(|arg| arg.contains("{{payload}}") || arg.contains("{{prompt}}"))
            {
                rendered_args.push(payload.clone());
            }
            (definition.command, rendered_args, definition.transport)
        };

    Ok(ResolvedAgentInvocation {
        agent: agent_name,
        command,
        args: rendered_args,
        context,
        model,
        reasoning,
        route_key: resolved.route_key,
        family_key: resolved.family_key,
        provider_source: resolved.provider_source,
        model_source: resolved.model_source,
        reasoning_source: resolved.reasoning_source,
        transport,
        payload,
        attachments: args.attachments.clone(),
        builtin_provider,
    })
}

pub(crate) fn command_args_for_invocation(
    invocation: &ResolvedAgentInvocation,
    working_dir: Option<&Path>,
) -> Result<Vec<String>> {
    command_args_for_invocation_with_options(
        invocation,
        AgentExecutionOptions {
            working_dir: working_dir.map(Path::to_path_buf),
            extra_env: Vec::new(),
            capture_output: false,
            continuation: None,
        },
    )
}

pub(crate) fn command_args_for_invocation_with_options(
    invocation: &ResolvedAgentInvocation,
    options: AgentExecutionOptions,
) -> Result<Vec<String>> {
    if !invocation.builtin_provider {
        return Ok(invocation.args.clone());
    }

    builtin_provider_adapter(&invocation.agent)
        .ok_or_else(|| anyhow!("builtin provider `{}` is not configured", invocation.agent))?
        .prepare_command_args(
            &invocation.args,
            options.working_dir.as_deref(),
            invocation.context,
            invocation.transport,
            options.capture_output,
            options.continuation.as_deref(),
        )
}

fn builtin_invocation_context(route_key: Option<&str>) -> BuiltinInvocationContext {
    match route_key {
        Some(AGENT_ROUTE_AGENTS_LISTEN) => BuiltinInvocationContext::Listen,
        Some(AGENT_ROUTE_CONTEXT_SCAN | AGENT_ROUTE_CONTEXT_RELOAD) => {
            BuiltinInvocationContext::Scan
        }
        Some(
            AGENT_ROUTE_BACKLOG_PLAN
            | AGENT_ROUTE_BACKLOG_IMPROVE
            | AGENT_ROUTE_BACKLOG_SPLIT
            | AGENT_ROUTE_LINEAR_ISSUES_REFINE,
        ) => BuiltinInvocationContext::Planning,
        _ => BuiltinInvocationContext::Other,
    }
}

fn render_agent_payload(
    provider: &str,
    prompt: &str,
    instructions: Option<&str>,
    model: Option<&str>,
    reasoning: Option<&str>,
    attachments: &[PromptImageAttachment],
) -> Result<String> {
    let instructions = instructions
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let model = model.map(str::trim).filter(|value| !value.is_empty());
    let reasoning = reasoning.map(str::trim).filter(|value| !value.is_empty());

    if instructions.is_none() && model.is_none() && reasoning.is_none() && attachments.is_empty() {
        return Ok(prompt.to_string());
    }

    let mut sections = vec![format!("Prompt:\n{prompt}")];

    if let Some(instructions) = instructions {
        sections.push(format!("Additional instructions:\n{instructions}"));
    }

    if let Some(model) = model {
        sections.push(format!("Preferred model:\n{model}"));
    }

    if let Some(reasoning) = reasoning {
        sections.push(format!("Preferred reasoning effort:\n{reasoning}"));
    }

    if !attachments.is_empty() {
        sections.push(render_attachment_payload(provider, attachments)?);
    }

    Ok(sections.join("\n\n"))
}

fn render_attachment_payload(
    provider: &str,
    attachments: &[PromptImageAttachment],
) -> Result<String> {
    let encoded = encode_prompt_images_for_provider(attachments)?;
    let mut lines = vec![format!(
        "Prompt image attachments for built-in provider `{provider}`:"
    )];
    for (index, attachment) in encoded.iter().enumerate() {
        lines.push(format!("[Image #{}]", index + 1));
        lines.push(format!("name: {}", attachment.display_name));
        lines.push("mime: image/png".to_string());
        lines.push(format!(
            "dimensions: {}x{}{}",
            attachment.width,
            attachment.height,
            if attachment.resized {
                " (resized to fit 2048x768)"
            } else {
                ""
            }
        ));
        lines.push("base64:".to_string());
        lines.push(attachment.base64_png.clone());
    }
    Ok(lines.join("\n"))
}

fn render_command_args(
    template: &[String],
    prompt: &str,
    instructions: Option<&str>,
    model: Option<&str>,
    reasoning: Option<&str>,
    payload: &str,
) -> Vec<String> {
    let model_arg = model
        .map(|value| format!("--model={value}"))
        .unwrap_or_default();
    let reasoning_arg = reasoning
        .map(|value| format!("--reasoning={value}"))
        .unwrap_or_default();

    template
        .iter()
        .filter_map(|value| {
            let rendered = value
                .replace("{{prompt}}", prompt)
                .replace("{{instructions}}", instructions.unwrap_or(""))
                .replace("{{model}}", model.unwrap_or(""))
                .replace("{{reasoning}}", reasoning.unwrap_or(""))
                .replace("{{model_arg}}", &model_arg)
                .replace("{{reasoning_arg}}", &reasoning_arg)
                .replace("{{payload}}", payload);

            if rendered.is_empty() {
                None
            } else {
                Some(rendered)
            }
        })
        .collect()
}

fn run_agent_capture_attempt(
    invocation: &ResolvedAgentInvocation,
    args: &RunAgentArgs,
    continuation: Option<&AgentContinuation>,
) -> Result<AgentCaptureReport> {
    let command_args = command_args_for_invocation_with_options(
        invocation,
        AgentExecutionOptions {
            working_dir: None,
            extra_env: Vec::new(),
            capture_output: invocation.builtin_provider,
            continuation: continuation.map(|state| state.session_id.clone()),
        },
    )?;
    let attempted_command = validate_invocation_command_surface(invocation, &command_args)?;

    let mut command = Command::new(&invocation.command);
    command.args(&command_args);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    apply_noninteractive_agent_environment(&mut command);
    apply_invocation_environment(
        &mut command,
        invocation,
        &args.prompt,
        args.instructions.as_deref(),
    );

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to launch agent `{}` with command `{attempted_command}`",
            invocation.agent
        )
    })?;

    if invocation.transport == PromptTransport::Stdin {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to open stdin for agent `{}`", invocation.agent))?;
        stdin
            .write_all(invocation.payload.as_bytes())
            .with_context(|| {
                format!(
                    "failed to write prompt payload to agent `{}`",
                    invocation.agent
                )
            })?;
    }

    let output = child
        .wait_with_output()
        .with_context(|| format!("failed to wait for agent `{}`", invocation.agent))?;
    let raw_stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        let code = output
            .status
            .code()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "terminated by signal".to_string());
        bail!(
            "agent `{}` exited unsuccessfully ({code}) while running `{attempted_command}`: {}",
            invocation.agent,
            stderr.trim()
        );
    }

    let (stdout, continuation, usage) = if invocation.builtin_provider {
        let provider = builtin_provider_adapter(&invocation.agent)
            .ok_or_else(|| anyhow!("builtin provider `{}` is not configured", invocation.agent))?;
        let parsed = provider.parse_capture_output(&raw_stdout)?;
        let stdout = parsed.response_text.ok_or_else(|| {
            anyhow!(
                "builtin provider `{}` did not emit a final assistant response while running in capture mode",
                invocation.agent
            )
        })?;
        (
            stdout,
            parsed.continuation.map(|session_id| AgentContinuation {
                provider: invocation.agent.clone(),
                session_id,
            }),
            parsed.usage,
        )
    } else {
        (raw_stdout, None, None)
    };

    Ok(AgentCaptureReport {
        continuation,
        stdout,
        usage,
    })
}

pub(crate) fn format_agent_config_source(source: &AgentConfigSource) -> String {
    match source {
        AgentConfigSource::ExplicitOverride => "explicit_override".to_string(),
        AgentConfigSource::CommandRoute(route) => format!("command_route:{route}"),
        AgentConfigSource::FamilyRoute(route) => format!("family_route:{route}"),
        AgentConfigSource::RepoDefault => "repo_default".to_string(),
        AgentConfigSource::GlobalDefault => "global_default".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ResolvedAgentInvocation, apply_invocation_environment, render_invocation_diagnostics,
    };
    use crate::agent_provider::BuiltinInvocationContext;
    use crate::config::{AgentConfigSource, PromptTransport};
    use std::process::Command;

    fn test_invocation() -> ResolvedAgentInvocation {
        ResolvedAgentInvocation {
            agent: "codex".to_string(),
            command: "codex".to_string(),
            args: vec!["exec".to_string()],
            context: BuiltinInvocationContext::Planning,
            model: Some("gpt-5.4".to_string()),
            reasoning: Some("high".to_string()),
            route_key: Some("backlog.plan".to_string()),
            family_key: Some("backlog".to_string()),
            provider_source: AgentConfigSource::GlobalDefault,
            model_source: Some(AgentConfigSource::RepoDefault),
            reasoning_source: Some(AgentConfigSource::RepoDefault),
            transport: PromptTransport::Arg,
            payload: "Prompt:\nhello".to_string(),
            attachments: Vec::new(),
            builtin_provider: true,
        }
    }

    #[test]
    fn apply_invocation_environment_sets_non_interactive_color_guards() {
        let invocation = test_invocation();
        let mut command = Command::new("env");

        apply_invocation_environment(&mut command, &invocation, "hello", Some("be precise"));

        let envs = command
            .get_envs()
            .filter_map(|(key, value)| Some((key.to_str()?, value?.to_str()?)))
            .collect::<std::collections::BTreeMap<_, _>>();

        assert_eq!(envs.get("TERM"), Some(&"dumb"));
        assert_eq!(envs.get("NO_COLOR"), Some(&"1"));
        assert_eq!(envs.get("CLICOLOR"), Some(&"0"));
        assert_eq!(envs.get("CLICOLOR_FORCE"), Some(&"0"));
        assert_eq!(envs.get("FORCE_COLOR"), Some(&"0"));
        assert_eq!(envs.get("METASTACK_AGENT_PROMPT"), Some(&"hello"));
    }

    #[test]
    fn render_invocation_diagnostics_reports_resolved_sources() {
        let lines = render_invocation_diagnostics(&test_invocation());

        assert!(lines.iter().any(|line| line == "Resolved provider: codex"));
        assert!(lines.iter().any(|line| line == "Resolved model: gpt-5.4"));
        assert!(
            lines
                .iter()
                .any(|line| line == "Provider source: global_default")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "Model source: repo_default")
        );
    }
}
