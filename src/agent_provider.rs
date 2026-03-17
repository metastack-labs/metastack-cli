use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::{AgentCommandConfig, PromptTransport, normalize_agent_name};
use crate::fs::canonicalize_existing_dir;

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
    ) -> Result<Vec<String>>;

    fn command_definition(&self) -> AgentCommandConfig {
        AgentCommandConfig {
            command: self.launch_command().to_string(),
            args: Vec::new(),
            transport: self.transport(),
        }
    }
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

const REASONING_LOW_MEDIUM: &[BuiltinReasoningOption] = &[REASONING_LOW, REASONING_MEDIUM];
const REASONING_LOW_MEDIUM_HIGH: &[BuiltinReasoningOption] =
    &[REASONING_LOW, REASONING_MEDIUM, REASONING_HIGH];
const REASONING_HIGH_ONLY: &[BuiltinReasoningOption] = &[REASONING_HIGH];

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
        reasoning_options: REASONING_LOW_MEDIUM_HIGH,
    },
    BuiltinModelCatalogEntry {
        key: "opus",
        reasoning_options: REASONING_LOW_MEDIUM_HIGH,
    },
    BuiltinModelCatalogEntry {
        key: "haiku",
        reasoning_options: REASONING_LOW_MEDIUM,
    },
    BuiltinModelCatalogEntry {
        key: "sonnet[1m]",
        reasoning_options: REASONING_MEDIUM_HIGH,
    },
    BuiltinModelCatalogEntry {
        key: "opusplan",
        reasoning_options: REASONING_HIGH_ONLY,
    },
];

const REASONING_MEDIUM_HIGH: &[BuiltinReasoningOption] = &[REASONING_MEDIUM, REASONING_HIGH];

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
            args.push(format!("--reasoning={reasoning}"));
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
    ) -> Result<Vec<String>> {
        let mut args = vec![
            "--sandbox".to_string(),
            "workspace-write".to_string(),
            "--ask-for-approval".to_string(),
            "never".to_string(),
        ];

        if let Some(working_dir) = working_dir {
            let workspace = canonicalize_existing_dir(working_dir)?;
            args.push("--cd".to_string());
            args.push(workspace.display().to_string());

            for writable_root in codex_additional_writable_roots(&workspace)? {
                args.push("--add-dir".to_string());
                args.push(writable_root.display().to_string());
            }
        }

        args.extend(launch_args.to_vec());
        Ok(args)
    }
}

impl BuiltinProviderAdapter for ClaudeProviderAdapter {
    fn models(&self) -> &'static [BuiltinModelCatalogEntry] {
        CLAUDE_MODELS
    }

    fn launch_command(&self) -> &'static str {
        "claude"
    }

    fn launch_args(&self, model: Option<&str>, _reasoning: Option<&str>) -> Vec<String> {
        let mut args = vec!["-p".to_string()];
        if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
            args.push(format!("--model={model}"));
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
    ) -> Result<Vec<String>> {
        Ok(launch_args.to_vec())
    }
}

static CODEX_PROVIDER: CodexProviderAdapter = CodexProviderAdapter;
static CLAUDE_PROVIDER: ClaudeProviderAdapter = ClaudeProviderAdapter;

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
