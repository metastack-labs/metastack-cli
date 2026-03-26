use std::io::{Read, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result, anyhow, bail};

use crate::agent_provider::builtin_provider_adapter;
use crate::cli::RunAgentArgs;
use crate::config::{AppConfig, PlanningMeta, PromptTransport};

use super::resolution::{
    ResolvedAgentInvocation, command_args_for_invocation_with_options,
    resolve_agent_invocation_for_planning, validate_invocation_command_surface,
};

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

// ---------------------------------------------------------------------------
// Environment injection
// ---------------------------------------------------------------------------

/// Applies the full set of agent invocation environment variables to a command.
///
/// Sets non-interactive color guards plus `METASTACK_AGENT_*` variables exposing the resolved
/// provider, model, reasoning, route key, family key, and their configuration sources.
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
        super::resolution::format_agent_config_source(&invocation.provider_source),
    );
    command.env(
        "METASTACK_AGENT_MODEL_SOURCE",
        invocation
            .model_source
            .as_ref()
            .map(super::resolution::format_agent_config_source)
            .unwrap_or_default(),
    );
    command.env(
        "METASTACK_AGENT_REASONING_SOURCE",
        invocation
            .reasoning_source
            .as_ref()
            .map(super::resolution::format_agent_config_source)
            .unwrap_or_default(),
    );
    command.env(
        "METASTACK_AGENT_ATTACHMENT_COUNT",
        invocation.attachments.len().to_string(),
    );
}

/// Applies minimal non-interactive environment variables (color suppression) to a command.
pub(crate) fn apply_noninteractive_agent_environment(command: &mut Command) {
    command.env("TERM", "dumb");
    command.env("NO_COLOR", "1");
    command.env("CLICOLOR", "0");
    command.env("CLICOLOR_FORCE", "0");
    command.env("FORCE_COLOR", "0");
    command.env_remove("COLORTERM");
}

// ---------------------------------------------------------------------------
// Subprocess execution with continuation/resume handling
// ---------------------------------------------------------------------------

/// Runs one non-interactive agent turn and returns the captured final assistant output.
///
/// Returns an error when config resolution fails, the configured command surface is invalid, the
/// subprocess cannot be launched, or the agent exits unsuccessfully.
pub fn run_agent_capture(args: &RunAgentArgs) -> Result<AgentCaptureReport> {
    let mut continuation = None;
    run_agent_capture_with_continuation(args, &mut continuation)
}

/// Runs one non-interactive agent turn, streaming stdout chunks to the provided callback.
///
/// If a continuation handle is provided and the provider reports an invalid-resume error, the
/// continuation is cleared and the invocation is retried without it.
///
/// Returns an error when config resolution fails, the configured command surface is invalid, the
/// subprocess cannot be launched, or the agent exits unsuccessfully.
pub(crate) fn run_agent_streaming_text_with_continuation(
    args: &RunAgentArgs,
    continuation: &mut Option<AgentContinuation>,
    mut on_stdout: impl FnMut(&str),
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
    match run_agent_streaming_text_attempt(
        &invocation,
        args,
        attempted_continuation,
        &mut on_stdout,
    ) {
        Ok(report) => Ok(report),
        Err(error)
            if attempted_continuation.is_some()
                && invocation.builtin_provider
                && builtin_provider_adapter(&invocation.agent).is_some_and(|provider| {
                    provider.is_invalid_resume_error(&error.to_string())
                }) =>
        {
            run_agent_streaming_text_attempt(&invocation, args, None, &mut on_stdout)
        }
        Err(error) => Err(error),
    }
}

/// Runs one non-interactive agent turn with continuation state management.
///
/// On success the continuation handle is updated to reflect the new session. If the provider
/// reports an invalid-resume error for the supplied continuation, the continuation is cleared and
/// the invocation is retried without it.
///
/// Returns an error when config resolution fails, the configured command surface is invalid, the
/// subprocess cannot be launched, or the agent exits unsuccessfully.
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

// ---------------------------------------------------------------------------
// Internal attempt implementations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug)]
struct OutputChunk {
    stream: OutputStream,
    text: String,
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

    ensure_nonempty_capture_output(&stdout)?;

    Ok(AgentCaptureReport {
        continuation,
        stdout,
        usage,
    })
}

fn ensure_nonempty_capture_output(stdout: &str) -> Result<()> {
    if stdout.trim().is_empty() {
        bail!("agent returned empty response — check provider CLI version or agent configuration");
    }
    Ok(())
}

fn run_agent_streaming_text_attempt(
    invocation: &ResolvedAgentInvocation,
    args: &RunAgentArgs,
    continuation: Option<&AgentContinuation>,
    on_stdout: &mut impl FnMut(&str),
) -> Result<AgentCaptureReport> {
    let command_args = command_args_for_invocation_with_options(
        invocation,
        AgentExecutionOptions {
            working_dir: None,
            extra_env: Vec::new(),
            capture_output: false,
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

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to open stdout for agent `{}`", invocation.agent))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to open stderr for agent `{}`", invocation.agent))?;
    let (sender, receiver) = mpsc::channel();
    spawn_output_reader(stdout, OutputStream::Stdout, sender.clone());
    spawn_output_reader(stderr, OutputStream::Stderr, sender);

    let mut raw_stdout = String::new();
    let mut raw_stderr = String::new();
    while let Ok(chunk) = receiver.recv() {
        match chunk.stream {
            OutputStream::Stdout => {
                raw_stdout.push_str(&chunk.text);
                on_stdout(&chunk.text);
            }
            OutputStream::Stderr => raw_stderr.push_str(&chunk.text),
        }
    }

    let status = child
        .wait()
        .with_context(|| format!("failed to wait for agent `{}`", invocation.agent))?;
    if !status.success() {
        let code = status
            .code()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "terminated by signal".to_string());
        bail!(
            "agent `{}` exited unsuccessfully ({code}) while running `{attempted_command}`: {}",
            invocation.agent,
            raw_stderr.trim()
        );
    }

    Ok(AgentCaptureReport {
        continuation: None,
        stdout: raw_stdout,
        usage: None,
    })
}

fn spawn_output_reader(
    mut reader: impl Read + Send + 'static,
    stream: OutputStream,
    sender: mpsc::Sender<OutputChunk>,
) {
    thread::spawn(move || {
        let mut buffer = [0u8; 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    let text = String::from_utf8_lossy(&buffer[..count]).to_string();
                    if sender.send(OutputChunk { stream, text }).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::super::resolution::ResolvedAgentInvocation;
    use super::{AgentContinuation, apply_invocation_environment, ensure_nonempty_capture_output};
    use crate::agent_provider::{BuiltinInvocationContext, builtin_provider_adapter};
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

    // -----------------------------------------------------------------------
    // Continuation/resume handling tests
    // -----------------------------------------------------------------------

    #[test]
    fn continuation_is_skipped_when_provider_does_not_match() {
        let invocation = test_invocation(); // agent = "codex"
        let continuation = Some(AgentContinuation {
            provider: "claude".to_string(),
            session_id: "session-1".to_string(),
        });

        let attempted = continuation
            .as_ref()
            .filter(|state| state.provider == invocation.agent);
        assert!(
            attempted.is_none(),
            "continuation should be skipped when provider does not match invocation agent"
        );
    }

    #[test]
    fn continuation_is_used_when_provider_matches() {
        let invocation = test_invocation(); // agent = "codex"
        let continuation = Some(AgentContinuation {
            provider: "codex".to_string(),
            session_id: "thread-42".to_string(),
        });

        let attempted = continuation
            .as_ref()
            .filter(|state| state.provider == invocation.agent);
        assert!(
            attempted.is_some(),
            "continuation should be used when provider matches invocation agent"
        );
        assert_eq!(attempted.unwrap().session_id, "thread-42");
    }

    #[test]
    fn codex_resume_error_detection_covers_expected_patterns() {
        let provider = builtin_provider_adapter("codex").expect("codex adapter should exist");

        // Positive cases: these messages should trigger a retry without continuation
        assert!(provider.is_invalid_resume_error("could not find thread thread-42"));
        assert!(provider.is_invalid_resume_error("No session found for the given ID"));
        assert!(provider.is_invalid_resume_error("Unknown session: abc123"));

        // Negative cases: unrelated errors should not trigger a retry
        assert!(!provider.is_invalid_resume_error("permission denied"));
        assert!(!provider.is_invalid_resume_error("network timeout"));
        assert!(!provider.is_invalid_resume_error("rate limit exceeded"));
    }

    #[test]
    fn claude_resume_error_detection_covers_expected_patterns() {
        let provider = builtin_provider_adapter("claude").expect("claude adapter should exist");

        // Positive cases: these messages should trigger a retry without continuation
        assert!(provider.is_invalid_resume_error(
            "No conversation found with session ID: 550e8400-e29b-41d4-a716-446655440000"
        ));
        assert!(
            provider.is_invalid_resume_error("--resume requires a valid session ID to continue")
        );

        // Negative cases: unrelated errors should not trigger a retry
        assert!(!provider.is_invalid_resume_error("permission denied"));
        assert!(!provider.is_invalid_resume_error("API key invalid"));
        assert!(!provider.is_invalid_resume_error("model not found"));
    }

    #[test]
    fn codex_capture_output_returns_continuation_from_response() {
        let provider = builtin_provider_adapter("codex").expect("codex adapter should exist");
        let parsed = provider
            .parse_capture_output(
                r#"{"type":"thread.started","thread_id":"thread-abc"}
{"type":"item.completed","item":{"type":"agent_message","text":"result"}}"#,
            )
            .expect("codex output should parse");

        assert_eq!(parsed.continuation.as_deref(), Some("thread-abc"));
        assert_eq!(parsed.response_text.as_deref(), Some("result"));
    }

    #[test]
    fn claude_capture_output_returns_continuation_from_response() {
        let provider = builtin_provider_adapter("claude").expect("claude adapter should exist");
        let parsed = provider
            .parse_capture_output(
                r#"{"type":"result","subtype":"success","result":"ok","session_id":"sess-xyz"}"#,
            )
            .expect("claude output should parse");

        assert_eq!(parsed.continuation.as_deref(), Some("sess-xyz"));
        assert_eq!(parsed.response_text.as_deref(), Some("ok"));
    }

    #[test]
    fn codex_capture_output_handles_missing_continuation() {
        let provider = builtin_provider_adapter("codex").expect("codex adapter should exist");
        let parsed = provider
            .parse_capture_output(
                r#"{"type":"item.completed","item":{"type":"agent_message","text":"done"}}"#,
            )
            .expect("codex output should parse");

        assert!(
            parsed.continuation.is_none(),
            "continuation should be None when thread.started is missing"
        );
        assert_eq!(parsed.response_text.as_deref(), Some("done"));
    }

    #[test]
    fn claude_capture_output_handles_missing_continuation() {
        let provider = builtin_provider_adapter("claude").expect("claude adapter should exist");
        let parsed = provider
            .parse_capture_output(r#"{"type":"result","subtype":"success","result":"done"}"#)
            .expect("claude output should parse");

        assert!(
            parsed.continuation.is_none(),
            "continuation should be None when session_id is missing"
        );
        assert_eq!(parsed.response_text.as_deref(), Some("done"));
    }

    #[test]
    fn empty_capture_output_is_rejected_before_downstream_parsing() {
        let error = ensure_nonempty_capture_output("   ")
            .expect_err("whitespace-only output should fail in the capture layer");

        assert!(error.to_string().contains("agent returned empty response"));
    }
}
