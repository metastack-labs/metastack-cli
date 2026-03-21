use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::agents::AgentTokenUsage;
use crate::config::{AgentCommandConfig, PromptTransport, normalize_agent_name};
use crate::fs::canonicalize_existing_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinInvocationContext {
    Planning,
    Scan,
    Listen,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinReasoningOption {
    pub key: &'static str,
    pub label: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinModelCatalogEntry {
    pub key: &'static str,
    pub reasoning_options: &'static [BuiltinReasoningOption],
}

pub trait BuiltinProviderAdapter: Sync {
    fn models(&self) -> &'static [BuiltinModelCatalogEntry];
    fn launch_command(&self) -> &'static str;
    fn launch_args(&self, model: Option<&str>, reasoning: Option<&str>) -> Vec<String>;
    fn transport(&self) -> PromptTransport;
    fn prepare_command_args(
        &self,
        launch_args: &[String],
        working_dir: Option<&Path>,
        context: BuiltinInvocationContext,
        transport: PromptTransport,
        capture_output: bool,
        continuation: Option<&str>,
    ) -> Result<Vec<String>>;
    fn validate_command_args(&self, command_args: &[String]) -> Result<()>;
    fn parse_capture_output(&self, raw_stdout: &str) -> Result<BuiltinCaptureOutput>;
    fn is_invalid_resume_error(&self, message: &str) -> bool;

    fn command_definition(&self) -> AgentCommandConfig {
        AgentCommandConfig {
            command: self.launch_command().to_string(),
            args: Vec::new(),
            transport: self.transport(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuiltinCaptureOutput {
    pub response_text: Option<String>,
    pub continuation: Option<String>,
    pub usage: Option<AgentTokenUsage>,
}

const REASONING_LOW: BuiltinReasoningOption = BuiltinReasoningOption {
    key: "low",
    label: "Low",
};
const REASONING_MEDIUM: BuiltinReasoningOption = BuiltinReasoningOption {
    key: "medium",
    label: "Medium",
};
const REASONING_HIGH: BuiltinReasoningOption = BuiltinReasoningOption {
    key: "high",
    label: "High",
};
const REASONING_MAX: BuiltinReasoningOption = BuiltinReasoningOption {
    key: "max",
    label: "Max",
};

const REASONING_LOW_MEDIUM_HIGH: &[BuiltinReasoningOption] =
    &[REASONING_LOW, REASONING_MEDIUM, REASONING_HIGH];
const REASONING_LOW_MEDIUM_HIGH_MAX: &[BuiltinReasoningOption] = &[
    REASONING_LOW,
    REASONING_MEDIUM,
    REASONING_HIGH,
    REASONING_MAX,
];

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

struct CodexProviderAdapter;
struct ClaudeProviderAdapter;

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
        PromptTransport::Arg
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
            let value = match serde_json::from_str::<Value>(line) {
                Ok(value) => value,
                Err(_) => continue,
            };
            merge_usage(&mut usage, extract_usage_from_value(&value));
            match value
                .get("type")
                .and_then(Value::as_str)
                .or_else(|| value.get("method").and_then(Value::as_str))
            {
                Some("thread.started") => {
                    continuation = value
                        .get("thread_id")
                        .and_then(Value::as_str)
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
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or(continuation);
                }
                Some("item.completed") => {
                    let item = value.get("item").unwrap_or(&Value::Null);
                    if item.get("type").and_then(Value::as_str) == Some("agent_message") {
                        response_text = item
                            .get("text")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .or(response_text);
                    }
                }
                Some("item/completed") => {
                    let item = value
                        .get("params")
                        .and_then(|params| params.get("item"))
                        .unwrap_or(&Value::Null);
                    if item
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| {
                            kind.eq_ignore_ascii_case("agent_message")
                                || kind.eq_ignore_ascii_case("agentMessage")
                        })
                    {
                        response_text = item
                            .get("text")
                            .or_else(|| item.get("content"))
                            .and_then(Value::as_str)
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
            let value = match serde_json::from_str::<Value>(line) {
                Ok(value) => value,
                Err(_) => continue,
            };
            parsed_any = true;
            merge_usage(&mut usage, extract_usage_from_value(&value));
            response_text = match value.get("result") {
                Some(Value::String(text)) => Some(text.clone()),
                Some(value) => Some(
                    serde_json::to_string(value)
                        .context("failed to serialize Claude structured result payload")?,
                ),
                None => response_text,
            };
            continuation = value
                .get("session_id")
                .or_else(|| value.get("sessionId"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or(continuation);
        }

        if !parsed_any {
            let value: Value = serde_json::from_str(trimmed)
                .context("failed to parse Claude `--output-format=json` response")?;
            merge_usage(&mut usage, extract_usage_from_value(&value));
            response_text = match value.get("result") {
                Some(Value::String(text)) => Some(text.clone()),
                Some(value) => Some(
                    serde_json::to_string(value)
                        .context("failed to serialize Claude structured result payload")?,
                ),
                None => None,
            };
            continuation = value
                .get("session_id")
                .or_else(|| value.get("sessionId"))
                .and_then(Value::as_str)
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

static CODEX_PROVIDER: CodexProviderAdapter = CodexProviderAdapter;
static CLAUDE_PROVIDER: ClaudeProviderAdapter = ClaudeProviderAdapter;

fn extract_usage_from_value(value: &Value) -> Option<AgentTokenUsage> {
    fn parse_u64(value: &Value) -> Option<u64> {
        value
            .as_u64()
            .or_else(|| {
                value
                    .as_i64()
                    .filter(|number| *number >= 0)
                    .map(|number| number as u64)
            })
            .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
    }

    fn extract_direct_usage(value: &Value) -> Option<AgentTokenUsage> {
        let input = [
            "inputTokens",
            "input_tokens",
            "promptTokens",
            "prompt_tokens",
        ]
        .into_iter()
        .find_map(|key| value.get(key).and_then(parse_u64));
        let output = [
            "outputTokens",
            "output_tokens",
            "completionTokens",
            "completion_tokens",
        ]
        .into_iter()
        .find_map(|key| value.get(key).and_then(parse_u64));
        (input.is_some() || output.is_some()).then_some(AgentTokenUsage { input, output })
    }

    if let Some(usage) = extract_direct_usage(value) {
        return Some(usage);
    }

    match value {
        Value::Array(values) => values.iter().find_map(extract_usage_from_value),
        Value::Object(map) => map.values().find_map(extract_usage_from_value),
        _ => None,
    }
}

fn merge_usage(existing: &mut Option<AgentTokenUsage>, update: Option<AgentTokenUsage>) {
    let Some(update) = update else {
        return;
    };
    match existing {
        Some(existing) => {
            if update.input.is_some() {
                existing.input = update.input;
            }
            if update.output.is_some() {
                existing.output = update.output;
            }
        }
        None => *existing = Some(update),
    }
}

pub fn builtin_provider_adapter(name: &str) -> Option<&'static dyn BuiltinProviderAdapter> {
    match normalize_agent_name(name).as_str() {
        "codex" => Some(&CODEX_PROVIDER),
        "claude" => Some(&CLAUDE_PROVIDER),
        _ => None,
    }
}

pub fn builtin_provider_names() -> &'static [&'static str] {
    &["codex", "claude"]
}

pub fn builtin_provider_model_keys(name: &str) -> Vec<&'static str> {
    builtin_provider_adapter(name)
        .map(|provider| provider.models().iter().map(|model| model.key).collect())
        .unwrap_or_default()
}

pub fn builtin_provider_reasoning_keys(provider: &str, model: Option<&str>) -> Vec<&'static str> {
    let Some(provider) = builtin_provider_adapter(provider) else {
        return Vec::new();
    };
    let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) else {
        return Vec::new();
    };

    provider
        .models()
        .iter()
        .find(|entry| entry.key.eq_ignore_ascii_case(model))
        .map(|entry| {
            entry
                .reasoning_options
                .iter()
                .map(|option| option.key)
                .collect()
        })
        .unwrap_or_default()
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

#[derive(Clone, Copy)]
struct FlagSupport {
    emitted_flag: &'static str,
    accepted_patterns: &'static [&'static str],
}

impl FlagSupport {
    const fn new(emitted_flag: &'static str, accepted_patterns: &'static [&'static str]) -> Self {
        Self {
            emitted_flag,
            accepted_patterns,
        }
    }
}

static HELP_OUTPUT_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn validate_help_surface(
    command: &str,
    help_args: &[&str],
    checks: &[(bool, &str, &[FlagSupport])],
) -> Result<()> {
    if checks.iter().all(|(required, _, _)| !*required) {
        return Ok(());
    }

    let help_output = cached_help_output(command, help_args)?;
    for (required, scope, flags) in checks.iter().copied() {
        if !required {
            continue;
        }
        for flag in flags {
            if flag
                .accepted_patterns
                .iter()
                .all(|pattern| !help_output.contains(*pattern))
            {
                bail!(
                    "installed `{}` {} does not advertise emitted flag `{}`; checked `{}`",
                    command,
                    scope,
                    flag.emitted_flag,
                    std::iter::once(command)
                        .chain(help_args.iter().copied())
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
        }
    }

    Ok(())
}

fn cached_help_output(command: &str, help_args: &[&str]) -> Result<String> {
    let path_key = env::var("PATH").unwrap_or_default();
    let cache_key = std::iter::once(command)
        .chain(help_args.iter().copied())
        .chain(std::iter::once(path_key.as_str()))
        .collect::<Vec<_>>()
        .join("\u{0}");
    let cache = HELP_OUTPUT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    {
        let cache_guard = cache
            .lock()
            .map_err(|_| anyhow::anyhow!("built-in CLI help cache lock is poisoned"))?;
        if let Some(output) = cache_guard.get(&cache_key) {
            return Ok(output.clone());
        }
    }

    let output = Command::new(command)
        .args(help_args)
        .output()
        .with_context(|| {
            format!(
                "failed to run `{}` while validating built-in CLI flags",
                std::iter::once(command)
                    .chain(help_args.iter().copied())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })?;
    if !output.status.success() {
        bail!(
            "`{}` failed while validating built-in CLI flags: {}",
            std::iter::once(command)
                .chain(help_args.iter().copied())
                .collect::<Vec<_>>()
                .join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let help_output = String::from_utf8_lossy(&output.stdout).to_string();
    let mut cache_guard = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("built-in CLI help cache lock is poisoned"))?;
    cache_guard.insert(cache_key, help_output.clone());
    Ok(help_output)
}

#[cfg(test)]
mod tests {
    use super::{
        BuiltinInvocationContext, BuiltinProviderAdapter, ClaudeProviderAdapter,
        CodexProviderAdapter,
    };
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
    fn claude_invalid_resume_detection_is_narrow() {
        let adapter = ClaudeProviderAdapter;
        assert!(adapter.is_invalid_resume_error(
            "No conversation found with session ID: 550e8400-e29b-41d4-a716-446655440000"
        ));
        assert!(!adapter.is_invalid_resume_error("permission denied"));
    }
}
