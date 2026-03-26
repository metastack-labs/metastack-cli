use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::branding;
use crate::config::PlanningMeta;
use crate::fs::display_path;
use crate::repo_target::RepoTarget;

const BUILTIN_WORKFLOW_CONTRACT: &str = include_str!(concat!(
    env!("OUT_DIR"),
    "/artifacts/injected-agent-workflow-contract.md"
));
const NO_REPO_OVERLAYS_MESSAGE: &str = "_No repo overlay files were found. `AGENTS.md` and legacy `WORKFLOW.md` are optional additive inputs._";
fn no_repo_scoped_instructions_text() -> String {
    format!(
        "_No repo-scoped instructions file is configured in `{}/meta.json`._",
        branding::PROJECT_DIR
    )
}

#[derive(Debug, Clone)]
pub(crate) struct InstructionSource {
    pub(crate) label: &'static str,
    pub(crate) path: PathBuf,
    pub(crate) contents: String,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkflowInstructionBundle {
    pub(crate) repo_target: RepoTarget,
    pub(crate) repo_overlays: Vec<InstructionSource>,
    pub(crate) repo_scoped_instructions: Option<InstructionSource>,
}

impl WorkflowInstructionBundle {
    pub(crate) fn load(root: &Path, repo_target: RepoTarget) -> Result<Self> {
        Ok(Self {
            repo_target,
            repo_overlays: load_repo_overlay_sources(root)?,
            repo_scoped_instructions: load_repo_scoped_instructions_source(root)?,
        })
    }

    pub(crate) fn builtin_contract(&self) -> &'static str {
        builtin_workflow_contract()
    }

    pub(crate) fn render_for_prompt(&self) -> String {
        let mut lines = vec![
            "## Built-in Workflow Contract".to_string(),
            String::new(),
            self.builtin_contract().to_string(),
            String::new(),
            "## Repository Scope".to_string(),
            String::new(),
            self.repo_target.prompt_scope_block(),
            String::new(),
            "## Repo Overlays".to_string(),
            String::new(),
        ];

        if self.repo_overlays.is_empty() {
            lines.push(NO_REPO_OVERLAYS_MESSAGE.to_string());
        } else {
            for source in &self.repo_overlays {
                lines.push(format!(
                    "### `{}`",
                    display_path(&source.path, self.repo_target.repo_root())
                ));
                lines.push(String::new());
                lines.push(source.contents.clone());
                lines.push(String::new());
            }
        }

        lines.extend(["## Repo-Scoped Instructions".to_string(), String::new()]);
        match &self.repo_scoped_instructions {
            Some(source) => {
                lines.push(format!(
                    "Source: `{}`",
                    display_path(&source.path, self.repo_target.repo_root())
                ));
                lines.push(String::new());
                lines.push(source.contents.clone());
            }
            None => lines.push(no_repo_scoped_instructions_text()),
        }

        lines.join("\n")
    }

    fn render_for_listen_prompt(&self) -> String {
        let mut lines = vec![
            "## Built-in Workflow Contract".to_string(),
            String::new(),
            self.builtin_contract().to_string(),
            String::new(),
            "## Repository Scope".to_string(),
            String::new(),
            self.repo_target.prompt_scope_block(),
            String::new(),
            "## Repo Overlays".to_string(),
            String::new(),
        ];

        if self.repo_overlays.is_empty() {
            lines.push(NO_REPO_OVERLAYS_MESSAGE.to_string());
        } else {
            for source in &self.repo_overlays {
                lines.push(format!(
                    "- `{}`: read this file directly from disk before acting on repo-specific rules.",
                    display_path(&source.path, self.repo_target.repo_root())
                ));
                if source.label == "WORKFLOW.md" {
                    lines.push(
                        "  Treat this as legacy compatibility/documentation context; consult it only when the ticket or repo state requires clarification."
                            .to_string(),
                    );
                }
            }
        }

        lines.extend([
            "".to_string(),
            "## Repo-Scoped Instructions".to_string(),
            String::new(),
        ]);
        match &self.repo_scoped_instructions {
            Some(source) => {
                lines.push(format!(
                    "Read `{}` directly from disk before starting. Use it as additional repo-specific guidance.",
                    display_path(&source.path, self.repo_target.repo_root())
                ));
            }
            None => lines.push(no_repo_scoped_instructions_text()),
        }

        lines.join("\n")
    }
}

pub(crate) fn builtin_workflow_contract() -> &'static str {
    BUILTIN_WORKFLOW_CONTRACT.trim()
}

pub(crate) fn load_repo_overlay_sources(root: &Path) -> Result<Vec<InstructionSource>> {
    let mut sources = Vec::new();
    for (label, relative_path) in [("AGENTS.md", "AGENTS.md"), ("WORKFLOW.md", "WORKFLOW.md")] {
        let path = root.join(relative_path);
        if !path.is_file() {
            continue;
        }
        sources.push(InstructionSource {
            label,
            path: path.clone(),
            contents: fs::read_to_string(&path)
                .with_context(|| format!("failed to read `{}`", path.display()))?,
        });
    }
    Ok(sources)
}

pub(crate) fn load_repo_scoped_instructions_source(
    root: &Path,
) -> Result<Option<InstructionSource>> {
    let planning_meta = PlanningMeta::load(root)?;
    let Some(relative_path) = planning_meta
        .listen
        .instructions_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };

    let path = root.join(relative_path);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            format!(
                "Configured instructions file `{}` is missing.",
                display_path(&path, root)
            )
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read `{}`", path.display()));
        }
    };

    Ok(Some(InstructionSource {
        label: "repo-scoped instructions",
        path,
        contents,
    }))
}

pub(crate) fn no_repo_overlays_message() -> &'static str {
    NO_REPO_OVERLAYS_MESSAGE
}

pub(crate) fn no_repo_scoped_instructions_message() -> String {
    no_repo_scoped_instructions_text()
}

pub(crate) fn render_repo_overlay_bundle(root: &Path) -> Result<String> {
    let sources = load_repo_overlay_sources(root)?;
    if sources.is_empty() {
        return Ok(NO_REPO_OVERLAYS_MESSAGE.to_string());
    }

    let mut lines = Vec::new();
    for source in sources {
        lines.push(format!("## {}", source.label));
        lines.push(String::new());
        lines.push(source.contents);
        lines.push(String::new());
    }

    Ok(lines.join("\n"))
}

pub(crate) fn render_repo_scoped_instructions(root: &Path) -> Result<String> {
    match load_repo_scoped_instructions_source(root)? {
        Some(source) => Ok(source.contents),
        None => Ok(no_repo_scoped_instructions_text()),
    }
}

pub(crate) fn render_workflow_contract(root: &Path, repo_target: RepoTarget) -> Result<String> {
    Ok(WorkflowInstructionBundle::load(root, repo_target)?.render_for_prompt())
}

pub(crate) fn render_workflow_contract_for_listen(
    root: &Path,
    repo_target: RepoTarget,
) -> Result<String> {
    Ok(WorkflowInstructionBundle::load(root, repo_target)?.render_for_listen_prompt())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{WorkflowInstructionBundle, builtin_workflow_contract};
    use crate::branding;
    use crate::repo_target::RepoTarget;

    #[test]
    fn workflow_instruction_bundle_renders_builtin_contract_and_optional_sections() {
        let temp = tempdir().expect("tempdir should create");
        let root = temp.path();
        std::fs::create_dir_all(root.join("instructions")).expect("instructions dir should exist");
        std::fs::write(root.join("AGENTS.md"), "# AGENTS\nUse tests.\n")
            .expect("agents should write");
        std::fs::write(
            root.join("instructions/listen.md"),
            "# Listener Instructions\nKeep work scoped.\n",
        )
        .expect("instructions should write");
        std::fs::create_dir_all(root.join(branding::PROJECT_DIR))
            .expect("metastack dir should exist");
        std::fs::write(
            root.join(format!("{}/meta.json", branding::PROJECT_DIR)),
            r#"{"listen":{"instructions_path":"instructions/listen.md"}}"#,
        )
        .expect("meta should write");

        let bundle = WorkflowInstructionBundle::load(root, RepoTarget::from_root(root))
            .expect("bundle should load");
        let rendered = bundle.render_for_prompt();

        assert!(rendered.contains("## Built-in Workflow Contract"));
        assert!(rendered.contains("## Repository Scope"));
        assert!(rendered.contains("## Repo Overlays"));
        assert!(rendered.contains("## Repo-Scoped Instructions"));
        assert!(rendered.contains("Use tests."));
        assert!(rendered.contains("Keep work scoped."));
    }

    #[test]
    fn builtin_contract_is_not_empty() {
        assert!(builtin_workflow_contract().contains("Inject"));
    }

    #[test]
    fn listen_prompt_render_references_overlay_paths_without_inlining_contents() {
        let temp = tempdir().expect("tempdir should create");
        let root = temp.path();
        std::fs::write(root.join("AGENTS.md"), "# AGENTS\nUse tests.\n")
            .expect("agents should write");
        std::fs::write(
            root.join("WORKFLOW.md"),
            "# Workflow\nVery long legacy guidance.\n",
        )
        .expect("workflow should write");

        let bundle = WorkflowInstructionBundle::load(root, RepoTarget::from_root(root))
            .expect("bundle should load");
        let rendered = bundle.render_for_listen_prompt();

        assert!(rendered.contains("## Repo Overlays"));
        assert!(rendered.contains("`AGENTS.md`: read this file directly from disk"));
        assert!(rendered.contains("`WORKFLOW.md`: read this file directly from disk"));
        assert!(rendered.contains("legacy compatibility/documentation context"));
        assert!(!rendered.contains("Very long legacy guidance."));
    }
}
