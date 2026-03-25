use std::path::Path;

use anyhow::{Context, Result};

use super::validation::{FlagSupport, validate_help_surface};
use super::{
    BuiltinCaptureOutput, BuiltinInvocationContext, BuiltinModelCatalogEntry,
    BuiltinProviderAdapter, REASONING_LOW_MEDIUM_HIGH_MAX,
};
use crate::config::PromptTransport;

const CLAUDE_MODELS: &[BuiltinModelCatalogEntry] = &[
    BuiltinModelCatalogEntry {
        key: "sonnet",
        reasoning_options: REASONING_LOW_MEDIUM_HIGH_MAX,
    },
    BuiltinModelCatalogEntry {
        key: "opus",
        reasoning_options: REASONING_LOW_MEDIUM_HIGH_MAX,
    },
    BuiltinModelCatalogEntry {
        key: "haiku",
        reasoning_options: REASONING_LOW_MEDIUM_HIGH_MAX,
    },
    BuiltinModelCatalogEntry {
        key: "sonnet[1m]",
        reasoning_options: REASONING_LOW_MEDIUM_HIGH_MAX,
    },
    BuiltinModelCatalogEntry {
        key: "opusplan",
        reasoning_options: REASONING_LOW_MEDIUM_HIGH_MAX,
    },
];

pub(crate) struct ClaudeProviderAdapter;

impl BuiltinProviderAdapter for ClaudeProviderAdapter {
    fn models(&self) -> &'static [BuiltinModelCatalogEntry] {
        CLAUDE_MODELS
    }

    fn launch_command(&self) -> &'static str {
        "claude"
    }

    fn launch_args(&self, model: Option<&str>, reasoning: Option<&str>) -> Vec<String> {
        let mut args = vec!["-p".to_string()];
        if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
            args.push(format!("--model={model}"));
        }
        if let Some(reasoning) = reasoning.map(str::trim).filter(|value| !value.is_empty()) {
            args.push(format!("--effort={reasoning}"));
        }
        args
    }

    fn transport(&self) -> PromptTransport {
        PromptTransport::Arg
    }

    fn prepare_command_args(
        &self,
        launch_args: &[String],
        _working_dir: Option<&Path>,
        context: BuiltinInvocationContext,
        transport: PromptTransport,
        capture_output: bool,
        continuation: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut args = Vec::new();
        if context == BuiltinInvocationContext::Listen {
            args.push("--permission-mode=bypassPermissions".to_string());
            args.push("--verbose".to_string());
            args.push("--output-format=stream-json".to_string());
        }
        let prompt_arg = if transport == PromptTransport::Arg {
            launch_args.last().cloned()
        } else {
            None
        };
        let option_args_end = launch_args
            .len()
            .saturating_sub(usize::from(prompt_arg.is_some()));
        args.push("-p".to_string());
        if capture_output {
            args.push("--output-format=json".to_string());
        }
        args.extend(
            launch_args
                .get(1..option_args_end)
                .unwrap_or_default()
                .to_vec(),
        );
        if let Some(continuation) = continuation {
            args.push("--resume".to_string());
            args.push(continuation.to_string());
        }
        if let Some(prompt_arg) = prompt_arg {
            args.push(prompt_arg);
        }
        Ok(args)
    }

    fn validate_command_args(&self, command_args: &[String]) -> Result<()> {
        validate_help_surface(
            "claude",
            &["-p", "--help"],
            &[
                (
                    command_args.iter().any(|arg| arg == "-p"),
                    "print-mode flags",
                    &[FlagSupport::new("-p", &["-p, --print", "--print"])][..],
                ),
                (
                    command_args.iter().any(|arg| arg.starts_with("--model=")),
                    "model flags",
                    &[FlagSupport::new("--model", &["--model <model>"])][..],
                ),
                (
                    command_args.iter().any(|arg| arg.starts_with("--effort=")),
                    "effort flags",
                    &[FlagSupport::new("--effort", &["--effort <level>"])][..],
                ),
                (
                    command_args.iter().any(|arg| arg == "--verbose"),
                    "verbose flags",
                    &[FlagSupport::new("--verbose", &["--verbose"])][..],
                ),
                (
                    command_args
                        .iter()
                        .any(|arg| arg.starts_with("--output-format=")),
                    "output-format flags",
                    &[FlagSupport::new(
                        "--output-format",
                        &["--output-format <format>"],
                    )][..],
                ),
                (
                    command_args.iter().any(|arg| {
                        arg == "--permission-mode" || arg.starts_with("--permission-mode=")
                    }),
                    "permission-mode flags",
                    &[FlagSupport::new(
                        "--permission-mode",
                        &["--permission-mode <mode>"],
                    )][..],
                ),
            ],
        )
    }

    fn parse_capture_output(&self, raw_stdout: &str) -> Result<BuiltinCaptureOutput> {
        let trimmed = raw_stdout.trim();
        let mut response_text = None;
        let mut continuation = None;
        let mut usage = None;
        let mut parsed_any = false;

        for line in trimmed
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            let raw_value = match serde_json::from_str::<serde_json::Value>(line) {
                Ok(value) => value,
                Err(_) => continue,
            };
            parsed_any = true;
            // Claude stream-json wraps events in an array: [{...}]
            let value = unwrap_stream_array(&raw_value);
            super::merge_usage(&mut usage, super::extract_usage_from_value(&raw_value));
            response_text = match value.get("result") {
                Some(serde_json::Value::String(text)) => Some(text.clone()),
                Some(value) => Some(
                    serde_json::to_string(value)
                        .context("failed to serialize Claude structured result payload")?,
                ),
                None => response_text,
            };
            continuation = value
                .get("session_id")
                .or_else(|| value.get("sessionId"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or(continuation);
        }

        if !parsed_any {
            let raw_value: serde_json::Value = serde_json::from_str(trimmed)
                .context("failed to parse Claude `--output-format=json` response")?;
            let value = unwrap_stream_array(&raw_value);
            super::merge_usage(&mut usage, super::extract_usage_from_value(&raw_value));
            response_text = match value.get("result") {
                Some(serde_json::Value::String(text)) => Some(text.clone()),
                Some(value) => Some(
                    serde_json::to_string(value)
                        .context("failed to serialize Claude structured result payload")?,
                ),
                None => None,
            };
            continuation = value
                .get("session_id")
                .or_else(|| value.get("sessionId"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
        }

        Ok(BuiltinCaptureOutput {
            response_text,
            continuation,
            usage,
        })
    }

    fn is_invalid_resume_error(&self, message: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        lower.contains("no conversation found with session id")
            || lower.contains("--resume requires a valid session id")
    }
}

/// Claude `--output-format=stream-json` wraps each event in a JSON array: `[{...}]`.
/// This unwraps to the inner object so field lookups like `.get("session_id")` work.
fn unwrap_stream_array(value: &serde_json::Value) -> &serde_json::Value {
    value
        .as_array()
        .and_then(|arr| arr.first())
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::super::{BuiltinInvocationContext, BuiltinProviderAdapter};
    use super::ClaudeProviderAdapter;
    use crate::agents::AgentTokenUsage;
    use crate::config::PromptTransport;

    #[test]
    fn claude_capture_command_args_use_json_and_resume_flags() {
        let adapter = ClaudeProviderAdapter;
        let args = adapter
            .prepare_command_args(
                &[
                    "-p".to_string(),
                    "--model=haiku".to_string(),
                    "--effort=low".to_string(),
                    "plan the work".to_string(),
                ],
                None,
                BuiltinInvocationContext::Planning,
                PromptTransport::Arg,
                true,
                Some("session-123"),
            )
            .expect("claude args should render");

        assert_eq!(args[0], "-p");
        assert!(args.contains(&"--output-format=json".to_string()));
        assert!(args.contains(&"--resume".to_string()));
        assert!(args.contains(&"session-123".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("plan the work"));
    }

    #[test]
    fn claude_capture_output_extracts_result_and_session_id() {
        let adapter = ClaudeProviderAdapter;
        let parsed = adapter
            .parse_capture_output(
                r#"{"type":"result","subtype":"success","result":"{\"questions\":[]}","session_id":"session-123"}"#,
            )
            .expect("claude output should parse");

        assert_eq!(parsed.response_text.as_deref(), Some(r#"{"questions":[]}"#));
        assert_eq!(parsed.continuation.as_deref(), Some("session-123"));
        assert_eq!(parsed.usage, None);
    }

    #[test]
    fn claude_capture_output_extracts_usage_from_json_lines() {
        let adapter = ClaudeProviderAdapter;
        let parsed = adapter
            .parse_capture_output(
                r#"{"type":"message_start","message":{"usage":{"input_tokens":210}}}
{"type":"message_delta","usage":{"output_tokens":34}}
{"type":"result","subtype":"success","result":"{\"summary\":\"ok\"}","session_id":"session-456"}"#,
            )
            .expect("claude output should parse");

        assert_eq!(parsed.response_text.as_deref(), Some(r#"{"summary":"ok"}"#));
        assert_eq!(parsed.continuation.as_deref(), Some("session-456"));
        assert_eq!(
            parsed.usage,
            Some(AgentTokenUsage {
                input: Some(210),
                output: Some(34),
            })
        );
    }

    #[test]
    fn claude_capture_output_extracts_session_id_from_stream_json_array() {
        let adapter = ClaudeProviderAdapter;
        // Claude --output-format=stream-json wraps each event in an array
        let parsed = adapter
            .parse_capture_output(
                r#"[{"type":"system","subtype":"init","session_id":"22ca497e-d7da-4118-9433-1902769c6737","tools":["Bash","Read"]}]
[{"type":"result","subtype":"success","result":"done","session_id":"22ca497e-d7da-4118-9433-1902769c6737"}]"#,
            )
            .expect("array-wrapped output should parse");

        assert_eq!(
            parsed.continuation.as_deref(),
            Some("22ca497e-d7da-4118-9433-1902769c6737")
        );
        assert_eq!(parsed.response_text.as_deref(), Some("done"));
    }

    #[test]
    fn claude_invalid_resume_detection_is_narrow() {
        let adapter = ClaudeProviderAdapter;
        assert!(adapter.is_invalid_resume_error(
            "No conversation found with session ID: 550e8400-e29b-41d4-a716-446655440000"
        ));
        assert!(!adapter.is_invalid_resume_error("permission denied"));
    }
}
