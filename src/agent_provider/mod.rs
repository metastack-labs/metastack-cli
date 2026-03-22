mod claude;
mod codex;
pub(crate) mod validation;

use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use crate::agents::AgentTokenUsage;
use crate::config::{AgentCommandConfig, PromptTransport, normalize_agent_name};

use claude::ClaudeProviderAdapter;
use codex::CodexProviderAdapter;

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

/// Contract that each built-in provider adapter must implement.
///
/// A provider adapter translates the generic agent invocation model into the specific CLI
/// arguments, output formats, and continuation semantics of a single agent backend (e.g. Codex,
/// Claude). The adapter is responsible for:
///
/// - **Model catalog**: listing supported model keys and their reasoning options.
/// - **Launch shaping**: producing the CLI command and initial arguments for a given model and
///   reasoning configuration.
/// - **Command preparation**: transforming launch arguments into the full argument vector for a
///   specific invocation context (planning, scan, listen, etc.), including sandbox, working
///   directory, capture-output, and continuation flags.
/// - **Surface validation**: verifying that the installed CLI binary actually supports the flags
///   the adapter intends to emit.
/// - **Output parsing**: extracting the assistant response text, continuation handle, and token
///   usage from the provider's machine-readable output.
/// - **Resume error detection**: recognizing provider-specific error messages that indicate an
///   invalid or expired continuation handle so the caller can retry without one.
///
/// Adding a new built-in provider requires only:
/// 1. Implementing this trait in a new submodule.
/// 2. Registering the adapter in [`builtin_provider_adapter`].
///
/// No changes to orchestration, execution, or resolution code are needed.
pub trait BuiltinProviderAdapter: Sync {
    /// Returns the static model catalog for this provider.
    fn models(&self) -> &'static [BuiltinModelCatalogEntry];

    /// Returns the CLI command name used to launch this provider.
    fn launch_command(&self) -> &'static str;

    /// Builds the initial CLI arguments for a given model and reasoning configuration.
    fn launch_args(&self, model: Option<&str>, reasoning: Option<&str>) -> Vec<String>;

    /// Returns the default prompt transport mechanism for this provider.
    fn transport(&self) -> PromptTransport;

    /// Transforms launch arguments into the full command-line argument vector for a specific
    /// invocation, incorporating sandbox mode, working directory, capture flags, and continuation.
    ///
    /// Returns an error when working directory canonicalization or git metadata lookup fails.
    fn prepare_command_args(
        &self,
        launch_args: &[String],
        working_dir: Option<&Path>,
        context: BuiltinInvocationContext,
        transport: PromptTransport,
        capture_output: bool,
        continuation: Option<&str>,
    ) -> Result<Vec<String>>;

    /// Validates that the installed CLI binary supports all flags the adapter will emit.
    ///
    /// Returns an error with a diagnostic message when a required flag is missing from the help
    /// output.
    fn validate_command_args(&self, command_args: &[String]) -> Result<()>;

    /// Parses the provider's machine-readable stdout into a structured capture result.
    ///
    /// Returns an error when the output cannot be parsed as the expected format.
    fn parse_capture_output(&self, raw_stdout: &str) -> Result<BuiltinCaptureOutput>;

    /// Returns `true` when the error message indicates an invalid or expired continuation handle.
    fn is_invalid_resume_error(&self, message: &str) -> bool;

    /// Returns a default command definition derived from this adapter's launch command and
    /// transport.
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

pub(crate) const REASONING_LOW_MEDIUM_HIGH: &[BuiltinReasoningOption] =
    &[REASONING_LOW, REASONING_MEDIUM, REASONING_HIGH];
pub(crate) const REASONING_LOW_MEDIUM_HIGH_MAX: &[BuiltinReasoningOption] = &[
    REASONING_LOW,
    REASONING_MEDIUM,
    REASONING_HIGH,
    REASONING_MAX,
];

static CODEX_PROVIDER: CodexProviderAdapter = CodexProviderAdapter;
static CLAUDE_PROVIDER: ClaudeProviderAdapter = ClaudeProviderAdapter;

/// Returns the built-in provider adapter for the given (normalized) name, or `None` if the name
/// does not match a known built-in provider.
pub fn builtin_provider_adapter(name: &str) -> Option<&'static dyn BuiltinProviderAdapter> {
    match normalize_agent_name(name).as_str() {
        "codex" => Some(&CODEX_PROVIDER),
        "claude" => Some(&CLAUDE_PROVIDER),
        _ => None,
    }
}

/// Returns the list of all built-in provider names.
pub fn builtin_provider_names() -> &'static [&'static str] {
    &["codex", "claude"]
}

/// Returns the model keys supported by the named built-in provider.
pub fn builtin_provider_model_keys(name: &str) -> Vec<&'static str> {
    builtin_provider_adapter(name)
        .map(|provider| provider.models().iter().map(|model| model.key).collect())
        .unwrap_or_default()
}

/// Returns the reasoning effort keys available for a specific model under the named provider.
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

// ---------------------------------------------------------------------------
// Token-usage extraction helpers
// ---------------------------------------------------------------------------

pub(crate) fn extract_usage_from_value(value: &Value) -> Option<AgentTokenUsage> {
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

pub(crate) fn merge_usage(existing: &mut Option<AgentTokenUsage>, update: Option<AgentTokenUsage>) {
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
