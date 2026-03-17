use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use crate::agent_provider::builtin_provider_adapter;
use crate::cli::RunAgentArgs;
use crate::config::{
    AgentConfigOverrides, AgentConfigSource, AppConfig, PlanningMeta, PromptTransport,
    normalize_agent_name, resolve_agent_config,
};

#[derive(Debug, Clone, Default)]
pub(crate) struct AgentExecutionOptions {
    pub(crate) working_dir: Option<PathBuf>,
    pub(crate) extra_env: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct AgentCaptureReport {
    pub stdout: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedAgentInvocation {
    pub(crate) agent: String,
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    pub(crate) model: Option<String>,
    pub(crate) reasoning: Option<String>,
    pub(crate) route_key: Option<String>,
    pub(crate) family_key: Option<String>,
    pub(crate) provider_source: AgentConfigSource,
    pub(crate) model_source: Option<AgentConfigSource>,
    pub(crate) reasoning_source: Option<AgentConfigSource>,
    pub(crate) transport: PromptTransport,
    pub(crate) payload: String,
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
}

pub fn run_agent_capture(args: &RunAgentArgs) -> Result<AgentCaptureReport> {
    let config = AppConfig::load()?;
    let planning_meta = match args.root.as_deref() {
        Some(root) => PlanningMeta::load(root)?,
        None => PlanningMeta::default(),
    };
    let invocation = resolve_agent_invocation_for_planning(&config, &planning_meta, args)?;
    let command_args = command_args_for_invocation(&invocation, None)?;

    let mut command = Command::new(&invocation.command);
    command.args(&command_args);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    apply_invocation_environment(
        &mut command,
        &invocation,
        &args.prompt,
        args.instructions.as_deref(),
    );

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to launch agent `{}` with command `{}`",
            invocation.agent, invocation.command
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
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        let code = output
            .status
            .code()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "terminated by signal".to_string());
        bail!(
            "agent `{}` exited unsuccessfully ({code}): {}",
            invocation.agent,
            stderr.trim()
        );
    }

    Ok(AgentCaptureReport { stdout })
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
    let payload = render_agent_payload(
        &args.prompt,
        args.instructions.as_deref(),
        model.as_deref(),
        reasoning.as_deref(),
    );
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
        model,
        reasoning,
        route_key: resolved.route_key,
        family_key: resolved.family_key,
        provider_source: resolved.provider_source,
        model_source: resolved.model_source,
        reasoning_source: resolved.reasoning_source,
        transport,
        payload,
        builtin_provider,
    })
}

pub(crate) fn command_args_for_invocation(
    invocation: &ResolvedAgentInvocation,
    working_dir: Option<&Path>,
) -> Result<Vec<String>> {
    command_args_for_options(
        invocation,
        AgentExecutionOptions {
            working_dir: working_dir.map(Path::to_path_buf),
            extra_env: Vec::new(),
        },
    )
}

fn command_args_for_options(
    invocation: &ResolvedAgentInvocation,
    options: AgentExecutionOptions,
) -> Result<Vec<String>> {
    if !invocation.builtin_provider {
        return Ok(invocation.args.clone());
    }

    builtin_provider_adapter(&invocation.agent)
        .ok_or_else(|| anyhow!("builtin provider `{}` is not configured", invocation.agent))?
        .prepare_command_args(&invocation.args, options.working_dir.as_deref())
}

fn render_agent_payload(
    prompt: &str,
    instructions: Option<&str>,
    model: Option<&str>,
    reasoning: Option<&str>,
) -> String {
    let instructions = instructions
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let model = model.map(str::trim).filter(|value| !value.is_empty());
    let reasoning = reasoning.map(str::trim).filter(|value| !value.is_empty());

    if instructions.is_none() && model.is_none() && reasoning.is_none() {
        return prompt.to_string();
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

    sections.join("\n\n")
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

pub(crate) fn format_agent_config_source(source: &AgentConfigSource) -> String {
    match source {
        AgentConfigSource::ExplicitOverride => "explicit_override".to_string(),
        AgentConfigSource::CommandRoute(route) => format!("command_route:{route}"),
        AgentConfigSource::FamilyRoute(route) => format!("family_route:{route}"),
        AgentConfigSource::RepoDefault => "repo_default".to_string(),
        AgentConfigSource::GlobalDefault => "global_default".to_string(),
    }
}
