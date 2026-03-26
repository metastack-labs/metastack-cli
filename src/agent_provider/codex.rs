use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::validation::{FlagSupport, validate_help_surface};
use super::{
    BuiltinCaptureOutput, BuiltinInvocationContext, BuiltinModelCatalogEntry,
    BuiltinProviderAdapter, REASONING_LOW_MEDIUM_HIGH,
};
use crate::config::PromptTransport;
use crate::fs::canonicalize_existing_dir;

const CODEX_MODELS: &[BuiltinModelCatalogEntry] = &[
    BuiltinModelCatalogEntry {
        key: "gpt-5.4",
        reasoning_options: REASONING_LOW_MEDIUM_HIGH,
    },
    BuiltinModelCatalogEntry {
        key: "gpt-5.3-codex",
        reasoning_options: REASONING_LOW_MEDIUM_HIGH,
    },
    BuiltinModelCatalogEntry {
        key: "gpt-5.2-codex",
        reasoning_options: REASONING_LOW_MEDIUM_HIGH,
    },
    BuiltinModelCatalogEntry {
        key: "gpt-5.1-codex-max",
        reasoning_options: REASONING_LOW_MEDIUM_HIGH,
    },
    BuiltinModelCatalogEntry {
        key: "gpt-5.1-codex",
        reasoning_options: REASONING_LOW_MEDIUM_HIGH,
    },
    BuiltinModelCatalogEntry {
        key: "gpt-5.1-codex-mini",
        reasoning_options: REASONING_LOW_MEDIUM_HIGH,
    },
    BuiltinModelCatalogEntry {
        key: "gpt-5-codex",
        reasoning_options: REASONING_LOW_MEDIUM_HIGH,
    },
    BuiltinModelCatalogEntry {
        key: "gpt-5-codex-mini",
        reasoning_options: REASONING_LOW_MEDIUM_HIGH,
    },
];

pub(crate) struct CodexProviderAdapter;

impl BuiltinProviderAdapter for CodexProviderAdapter {
    fn models(&self) -> &'static [BuiltinModelCatalogEntry] {
        CODEX_MODELS
    }

    fn launch_command(&self) -> &'static str {
        "codex"
    }

    fn launch_args(&self, model: Option<&str>, reasoning: Option<&str>) -> Vec<String> {
        let mut args = vec!["exec".to_string()];
        if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
            args.push(format!("--model={model}"));
        }
        if let Some(reasoning) = reasoning.map(str::trim).filter(|value| !value.is_empty()) {
            args.push("-c".to_string());
            args.push(format!("reasoning.effort=\"{reasoning}\""));
        }
        args
    }

    fn transport(&self) -> PromptTransport {
        PromptTransport::Stdin
    }

    fn prepare_command_args(
        &self,
        launch_args: &[String],
        working_dir: Option<&Path>,
        context: BuiltinInvocationContext,
        transport: PromptTransport,
        capture_output: bool,
        continuation: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut args = match context {
            BuiltinInvocationContext::Listen => {
                vec!["--dangerously-bypass-approvals-and-sandbox".to_string()]
            }
            _ => vec![
                "--sandbox".to_string(),
                "workspace-write".to_string(),
                "--ask-for-approval".to_string(),
                "never".to_string(),
            ],
        };

        if let Some(working_dir) = working_dir {
            let workspace = canonicalize_existing_dir(working_dir)?;
            args.push("--cd".to_string());
            args.push(workspace.display().to_string());

            for writable_root in codex_additional_writable_roots(&workspace)? {
                args.push("--add-dir".to_string());
                args.push(writable_root.display().to_string());
            }
        }

        let prompt_arg = if transport == PromptTransport::Arg {
            launch_args.last().cloned()
        } else {
            None
        };
        let exec_args_end = launch_args
            .len()
            .saturating_sub(usize::from(prompt_arg.is_some()));
        let exec_args = launch_args
            .get(1..exec_args_end)
            .unwrap_or_default()
            .to_vec();

        args.push("exec".to_string());
        if continuation.is_some() {
            args.push("resume".to_string());
        }
        if capture_output {
            args.push("--json".to_string());
        }
        args.extend(exec_args);
        if let Some(continuation) = continuation {
            args.push(continuation.to_string());
        }
        if let Some(prompt_arg) = prompt_arg {
            args.push(prompt_arg);
        }
        Ok(args)
    }

    fn validate_command_args(&self, command_args: &[String]) -> Result<()> {
        let exec_index = command_args
            .iter()
            .position(|arg| arg == "exec")
            .ok_or_else(|| anyhow::anyhow!("built-in codex launch args are missing `exec`"))?;

        let top_level_args = &command_args[..exec_index];
        let exec_args = &command_args[exec_index + 1..];

        validate_help_surface(
            "codex",
            &["--help"],
            &[
                (
                    top_level_args.iter().any(|arg| arg == "--sandbox"),
                    "top-level sandbox flags",
                    &[FlagSupport::new(
                        "--sandbox",
                        &["--sandbox <SANDBOX_MODE>", "-s, --sandbox <SANDBOX_MODE>"],
                    )][..],
                ),
                (
                    top_level_args
                        .iter()
                        .any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox"),
                    "top-level sandbox flags",
                    &[FlagSupport::new(
                        "--dangerously-bypass-approvals-and-sandbox",
                        &["--dangerously-bypass-approvals-and-sandbox"],
                    )][..],
                ),
                (
                    top_level_args.iter().any(|arg| arg == "--ask-for-approval"),
                    "top-level approval flags",
                    &[FlagSupport::new(
                        "--ask-for-approval",
                        &[
                            "--ask-for-approval <APPROVAL_POLICY>",
                            "-a, --ask-for-approval <APPROVAL_POLICY>",
                        ],
                    )][..],
                ),
                (
                    top_level_args.iter().any(|arg| arg == "--cd"),
                    "top-level working-directory flags",
                    &[FlagSupport::new("--cd", &["--cd <DIR>", "-C, --cd <DIR>"])][..],
                ),
                (
                    top_level_args.iter().any(|arg| arg == "--add-dir"),
                    "top-level writable-root flags",
                    &[FlagSupport::new("--add-dir", &["--add-dir <DIR>"])][..],
                ),
            ],
        )?;
        validate_help_surface(
            "codex",
            &["exec", "--help"],
            &[
                (
                    exec_args.iter().any(|arg| arg.starts_with("--model=")),
                    "exec model flags",
                    &[FlagSupport::new(
                        "--model",
                        &["--model <MODEL>", "-m, --model <MODEL>"],
                    )][..],
                ),
                (
                    exec_args.iter().any(|arg| arg == "-c"),
                    "exec config flags",
                    &[FlagSupport::new(
                        "-c",
                        &["-c, --config <key=value>", "--config <key=value>"],
                    )][..],
                ),
                (
                    exec_args.iter().any(|arg| arg == "--json"),
                    "exec machine-readable flags",
                    &[FlagSupport::new("--json", &["--json"])][..],
                ),
            ],
        )?;

        Ok(())
    }

    fn parse_capture_output(&self, raw_stdout: &str) -> Result<BuiltinCaptureOutput> {
        let mut response_text = None;
        let mut continuation = None;
        let mut usage = None;

        for line in raw_stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            let value = match serde_json::from_str::<serde_json::Value>(line) {
                Ok(value) => value,
                Err(_) => continue,
            };
            super::merge_usage(&mut usage, super::extract_usage_from_value(&value));
            match value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .or_else(|| value.get("method").and_then(serde_json::Value::as_str))
            {
                Some("thread.started") => {
                    continuation = value
                        .get("thread_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string);
                }
                Some("thread/started") => {
                    continuation = value
                        .get("params")
                        .and_then(|params| {
                            params
                                .get("thread_id")
                                .or_else(|| params.get("threadId"))
                                .or_else(|| params.get("id"))
                        })
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                        .or(continuation);
                }
                Some("item.completed") => {
                    let item = value.get("item").unwrap_or(&serde_json::Value::Null);
                    if item.get("type").and_then(serde_json::Value::as_str) == Some("agent_message")
                    {
                        response_text = item
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .or(response_text);
                    }
                }
                Some("item/completed") => {
                    let item = value
                        .get("params")
                        .and_then(|params| params.get("item"))
                        .unwrap_or(&serde_json::Value::Null);
                    if item
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|kind| {
                            kind.eq_ignore_ascii_case("agent_message")
                                || kind.eq_ignore_ascii_case("agentMessage")
                        })
                    {
                        response_text = item
                            .get("text")
                            .or_else(|| item.get("content"))
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .or(response_text);
                    }
                }
                _ => {}
            }
        }

        Ok(BuiltinCaptureOutput {
            response_text,
            continuation,
            usage,
        })
    }

    fn is_invalid_resume_error(&self, message: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        lower.contains("could not find thread")
            || lower.contains("no session found")
            || lower.contains("unknown session")
    }
}

fn codex_additional_writable_roots(workspace: &Path) -> Result<Vec<PathBuf>> {
    let mut writable_roots = Vec::new();

    for args in [
        ["rev-parse", "--path-format=absolute", "--git-dir"].as_slice(),
        ["rev-parse", "--path-format=absolute", "--git-common-dir"].as_slice(),
    ] {
        let path = git_stdout(workspace, args)?;
        let candidate = PathBuf::from(path);
        if candidate.exists() && candidate != workspace {
            writable_roots.push(candidate);
        }
    }

    writable_roots.sort();
    writable_roots.dedup();
    Ok(writable_roots)
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

#[cfg(test)]
mod tests {
    use super::super::{BuiltinInvocationContext, BuiltinProviderAdapter};
    use super::CodexProviderAdapter;
    use crate::agents::AgentTokenUsage;
    use crate::config::PromptTransport;

    #[test]
    fn codex_capture_command_args_use_resume_json_and_preserve_prompt_order() {
        let adapter = CodexProviderAdapter;
        let args = adapter
            .prepare_command_args(
                &[
                    "exec".to_string(),
                    "--model=gpt-5.4".to_string(),
                    "-c".to_string(),
                    "reasoning.effort=\"high\"".to_string(),
                    "plan the work".to_string(),
                ],
                None,
                BuiltinInvocationContext::Planning,
                PromptTransport::Arg,
                true,
                Some("thread-1"),
            )
            .expect("codex args should render");

        assert_eq!(args[0], "--sandbox");
        assert!(args.contains(&"exec".to_string()));
        assert!(args.contains(&"resume".to_string()));
        assert!(args.contains(&"--json".to_string()));
        assert!(args.contains(&"thread-1".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("plan the work"));
    }

    #[test]
    fn codex_default_transport_uses_stdin() {
        let adapter = CodexProviderAdapter;
        assert_eq!(adapter.transport(), PromptTransport::Stdin);
    }

    #[test]
    fn codex_stdin_transport_omits_prompt_argument() {
        let adapter = CodexProviderAdapter;
        let args = adapter
            .prepare_command_args(
                &[
                    "exec".to_string(),
                    "--model=gpt-5.4".to_string(),
                    "-c".to_string(),
                    "reasoning.effort=\"high\"".to_string(),
                ],
                None,
                BuiltinInvocationContext::Planning,
                PromptTransport::Stdin,
                true,
                Some("thread-1"),
            )
            .expect("codex args should render");

        assert_eq!(args[0], "--sandbox");
        assert!(args.contains(&"exec".to_string()));
        assert!(args.contains(&"resume".to_string()));
        assert!(args.contains(&"--json".to_string()));
        assert!(args.contains(&"--model=gpt-5.4".to_string()));
        assert!(args.contains(&"-c".to_string()));
        assert!(args.contains(&"reasoning.effort=\"high\"".to_string()));
        assert!(!args.iter().any(|arg| arg == "plan the work"));
    }

    #[test]
    fn codex_capture_output_extracts_last_agent_message_and_thread_id() {
        let adapter = CodexProviderAdapter;
        let parsed = adapter
            .parse_capture_output(
                r#"{"type":"thread.started","thread_id":"thread-123"}
{"type":"turn.started"}
{"type":"item.completed","item":{"type":"agent_message","text":"{\"questions\":[]}"}}"#,
            )
            .expect("codex output should parse");

        assert_eq!(parsed.response_text.as_deref(), Some(r#"{"questions":[]}"#));
        assert_eq!(parsed.continuation.as_deref(), Some("thread-123"));
        assert_eq!(parsed.usage, None);
    }

    #[test]
    fn codex_capture_output_extracts_usage_from_method_notifications() {
        let adapter = CodexProviderAdapter;
        let parsed = adapter
            .parse_capture_output(
                r#"{"method":"thread/started","params":{"id":"thread-456"}}
{"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"inputTokens":321,"outputTokens":123}}}
{"method":"item/completed","params":{"item":{"type":"agentMessage","text":"done"}}}"#,
            )
            .expect("codex output should parse");

        assert_eq!(parsed.response_text.as_deref(), Some("done"));
        assert_eq!(parsed.continuation.as_deref(), Some("thread-456"));
        assert_eq!(
            parsed.usage,
            Some(AgentTokenUsage {
                input: Some(321),
                output: Some(123),
            })
        );
    }
}
