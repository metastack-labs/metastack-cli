use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::branding;
use crate::cli::{
    ContextArgs, ContextCommands, ContextDoctorArgs, ContextMapArgs, ContextReloadArgs,
    ContextShowArgs, ScanArgs,
};
use crate::codebase_context::{
    CodebaseContextSection, MissingCodebaseContextHint, codebase_context_paths,
    load_codebase_context_bundle as load_shared_codebase_context_bundle,
};
use crate::config::AGENT_ROUTE_CONTEXT_RELOAD;
use crate::config::{AppConfig, PlanningMeta, detect_supported_agents};
use crate::fs::{PlanningPaths, canonicalize_existing_dir, display_path};
use crate::repo_target::RepoTarget;
use crate::scan::{CodebaseContext, run_scan, run_scan_for_route};
use crate::workflow_contract::{
    InstructionSource, WorkflowInstructionBundle, no_repo_overlays_message,
    no_repo_scoped_instructions_message, render_repo_overlay_bundle,
    render_repo_scoped_instructions, render_workflow_contract,
};

#[derive(Debug, Clone)]
struct DoctorReport {
    root: PathBuf,
    issues: Vec<String>,
    notices: Vec<String>,
}

pub fn run_context_command(args: &ContextArgs) -> Result<String> {
    match &args.command {
        ContextCommands::Show(show_args) => run_context_show(show_args),
        ContextCommands::Scan(scan_args) => {
            let report = run_scan(scan_args)?;
            if scan_args.json {
                report.render_json()
            } else {
                Ok(report.render())
            }
        }
        ContextCommands::Reload(reload_args) => run_context_reload(reload_args),
        ContextCommands::Map(map_args) => run_context_map(map_args),
        ContextCommands::Doctor(doctor_args) => run_context_doctor(doctor_args),
    }
}

pub(crate) fn load_codebase_context_bundle(root: &Path) -> Result<String> {
    load_shared_codebase_context_bundle(
        root,
        &[
            CodebaseContextSection::Scan,
            CodebaseContextSection::Architecture,
            CodebaseContextSection::Concerns,
            CodebaseContextSection::Conventions,
            CodebaseContextSection::Integrations,
            CodebaseContextSection::Stack,
            CodebaseContextSection::Structure,
            CodebaseContextSection::Testing,
        ],
        MissingCodebaseContextHint::ReloadOrScan,
    )
}

pub(crate) fn load_effective_instructions(root: &Path) -> Result<String> {
    render_repo_scoped_instructions(root)
}

pub(crate) fn load_workflow_contract(root: &Path) -> Result<String> {
    render_workflow_contract(root, RepoTarget::from_root(root))
}

pub(crate) fn load_project_rules_bundle(root: &Path) -> Result<String> {
    render_repo_overlay_bundle(root)
}

pub(crate) fn render_repo_map(root: &Path) -> Result<String> {
    Ok(CodebaseContext::collect(root)?.render_prompt_summary())
}

fn run_context_show(args: &ContextShowArgs) -> Result<String> {
    let root = canonicalize_existing_dir(&args.root.root)?;
    let app_config = AppConfig::load()?;
    let workflow_bundle = WorkflowInstructionBundle::load(&root, RepoTarget::from_root(&root))?;
    let repo_map = render_repo_map(&root)?;
    let codebase_paths = codebase_context_paths(
        &root,
        &[
            CodebaseContextSection::Scan,
            CodebaseContextSection::Architecture,
            CodebaseContextSection::Concerns,
            CodebaseContextSection::Conventions,
            CodebaseContextSection::Integrations,
            CodebaseContextSection::Stack,
            CodebaseContextSection::Structure,
            CodebaseContextSection::Testing,
        ],
    );
    let mut lines = vec![
        "# Effective Context".to_string(),
        String::new(),
        format!("Repository root: `{}`", root.display()),
        format!(
            "Default agent: `{}`",
            app_config
                .agents
                .default_agent
                .as_deref()
                .unwrap_or("unset")
        ),
        format!(
            "Default model: `{}`",
            app_config
                .agents
                .default_model
                .as_deref()
                .unwrap_or("unset")
        ),
        String::new(),
        "## Built-in Workflow Contract".to_string(),
        String::new(),
        workflow_bundle.builtin_contract().to_string(),
        String::new(),
        "## Repository Scope".to_string(),
        String::new(),
        workflow_bundle.repo_target.prompt_scope_block(),
        String::new(),
        "## Repo Overlay Sources".to_string(),
        String::new(),
    ];

    if workflow_bundle.repo_overlays.is_empty() {
        lines.push(format!("- {}", no_repo_overlays_message()));
    } else {
        for source in &workflow_bundle.repo_overlays {
            lines.push(format!("- `{}`", display_path(&source.path, &root)));
        }
    }

    lines.extend([
        String::new(),
        "## Codebase Context Sources".to_string(),
        String::new(),
    ]);
    for (title, path) in &codebase_paths {
        let status = if path.is_file() { "present" } else { "missing" };
        lines.push(format!(
            "- [{status}] `{}` ({title})",
            display_path(path, &root)
        ));
    }

    lines.extend([
        String::new(),
        "## Repo Overlay Contents".to_string(),
        String::new(),
    ]);
    if workflow_bundle.repo_overlays.is_empty() {
        lines.push(no_repo_overlays_message().to_string());
    } else {
        for source in workflow_bundle.repo_overlays {
            lines.push(format!("### `{}`", display_path(&source.path, &root)));
            lines.push(String::new());
            lines.push(source.contents);
            lines.push(String::new());
        }
    }

    lines.extend(["## Repo-Scoped Instructions".to_string(), String::new()]);
    lines.extend(render_source_block(
        workflow_bundle.repo_scoped_instructions.as_ref(),
        &root,
    ));

    lines.extend(["## Repo Map".to_string(), String::new(), repo_map]);

    Ok(lines.join("\n"))
}

fn run_context_reload(args: &ContextReloadArgs) -> Result<String> {
    let report = run_scan_for_route(
        &ScanArgs {
            root: args.root.root.clone(),
            json: false,
        },
        AGENT_ROUTE_CONTEXT_RELOAD,
    )?;
    Ok(report.render())
}

fn run_context_map(args: &ContextMapArgs) -> Result<String> {
    let root = canonicalize_existing_dir(&args.root.root)?;
    let lines = [
        "# Repo Map".to_string(),
        String::new(),
        format!("Repository root: `{}`", root.display()),
        String::new(),
        render_repo_map(&root)?,
    ];
    Ok(lines.join("\n"))
}

fn run_context_doctor(args: &ContextDoctorArgs) -> Result<String> {
    let root = canonicalize_existing_dir(&args.root.root)?;
    let report = diagnose_context(&root)?;
    if report.issues.is_empty() {
        return Ok(report.render());
    }

    Err(anyhow!(report.render()))
}

fn diagnose_context(root: &Path) -> Result<DoctorReport> {
    let planning_meta = PlanningMeta::load(root)?;
    let app_config = AppConfig::load()?;
    let paths = PlanningPaths::new(root);
    let workflow_bundle = WorkflowInstructionBundle::load(root, RepoTarget::from_root(root))?;
    let mut issues = Vec::new();
    let mut notices = Vec::new();

    if !paths.meta_path().is_file() {
        issues.push(format!(
            "Missing `{}`. Run `meta runtime setup --root {}` to bootstrap repo-scoped defaults.",
            display_path(&paths.meta_path(), root),
            root.display()
        ));
    } else {
        notices.push(format!(
            "Found `{}`.",
            display_path(&paths.meta_path(), root)
        ));
    }

    if workflow_bundle.repo_overlays.is_empty() {
        notices.push(
            "No repo overlay files were found; relying on the injected workflow contract."
                .to_string(),
        );
    } else {
        notices.push(format!(
            "Loaded repo overlays: {}.",
            workflow_bundle
                .repo_overlays
                .iter()
                .map(|source| format!("`{}`", display_path(&source.path, root)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    if let Some(relative_path) = planning_meta
        .listen
        .instructions_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let instructions_path = root.join(relative_path);
        if instructions_path.is_file() {
            notices.push(format!(
                "Configured instructions file is present at `{}`.",
                display_path(&instructions_path, root)
            ));
        } else {
            issues.push(format!(
                "Configured instructions file `{}` is missing. Update `{}/meta.json` or create the file.",
                display_path(&instructions_path, root),
                branding::PROJECT_DIR
            ));
        }
    } else {
        notices.push("No repo-scoped instructions file is configured.".to_string());
    }

    let missing_codebase = codebase_context_paths(
        root,
        &[
            CodebaseContextSection::Scan,
            CodebaseContextSection::Architecture,
            CodebaseContextSection::Concerns,
            CodebaseContextSection::Conventions,
            CodebaseContextSection::Integrations,
            CodebaseContextSection::Stack,
            CodebaseContextSection::Structure,
            CodebaseContextSection::Testing,
        ],
    )
    .into_iter()
    .filter_map(|(_, path)| (!path.is_file()).then(|| display_path(&path, root)))
    .collect::<Vec<_>>();
    if missing_codebase.is_empty() {
        notices.push(format!(
            "All expected `{}/codebase/*.md` files are present.",
            branding::PROJECT_DIR
        ));
    } else {
        issues.push(format!(
            "Missing codebase context files: {}. Run `{} context reload --root {}` or `{} context scan --root {}`.",
            missing_codebase.join(", "),
            branding::COMMAND_NAME,
            root.display(),
            branding::COMMAND_NAME,
            root.display()
        ));
    }

    if let Some(agent) = app_config.agents.default_agent.as_deref() {
        notices.push(format!("Configured default agent: `{agent}`."));
    } else {
        let detected_agents = detect_supported_agents();
        if detected_agents.is_empty() {
            issues.push(
                "No default agent is configured and no supported built-in agents were found on `PATH`. Run `meta runtime config` before using agent-backed workflows."
                    .to_string(),
            );
        } else {
            notices.push(format!(
                "No default agent configured, but detected built-in agents: {}.",
                detected_agents
                    .into_iter()
                    .map(|agent| format!("`{agent}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    Ok(DoctorReport {
        root: root.to_path_buf(),
        issues,
        notices,
    })
}

fn render_source_block(source: Option<&InstructionSource>, root: &Path) -> Vec<String> {
    match source {
        Some(source) => vec![
            format!("Source: `{}`", display_path(&source.path, root)),
            String::new(),
            source.contents.clone(),
        ],
        None => vec![no_repo_scoped_instructions_message()],
    }
}

impl DoctorReport {
    fn render(&self) -> String {
        let mut lines = vec![format!("Context doctor for `{}`", self.root.display())];

        if self.issues.is_empty() {
            lines.push("Status: ok".to_string());
        } else {
            lines.push(format!("Status: found {} issue(s)", self.issues.len()));
        }

        if !self.notices.is_empty() {
            lines.push(String::new());
            lines.push("Healthy signals:".to_string());
            for notice in &self.notices {
                lines.push(format!("- {notice}"));
            }
        }

        if !self.issues.is_empty() {
            lines.push(String::new());
            lines.push("Issues:".to_string());
            for issue in &self.issues {
                lines.push(format!("- {issue}"));
            }
        }

        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn bundled_context_includes_all_sections_and_reload_or_scan_hints() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let root = temp.path();
        let paths = PlanningPaths::new(root);
        fs::create_dir_all(&paths.codebase_dir).expect("codebase dir should be created");

        let bundle = load_codebase_context_bundle(root).expect("bundle should render");

        assert!(bundle.contains("## SCAN.md"));
        assert!(bundle.contains("## ARCHITECTURE.md"));
        assert!(bundle.contains("## CONCERNS.md"));
        assert!(bundle.contains("## CONVENTIONS.md"));
        assert!(bundle.contains("## INTEGRATIONS.md"));
        assert!(bundle.contains("## STACK.md"));
        assert!(bundle.contains("## STRUCTURE.md"));
        assert!(bundle.contains("## TESTING.md"));
        assert!(bundle.contains(&format!(
            "_Missing `SCAN.md`. Run `{} context reload --root ",
            branding::COMMAND_NAME
        )));
        assert!(bundle.contains(&format!(
            "` or `{} context scan --root ",
            branding::COMMAND_NAME
        )));
        assert!(bundle.contains("` to generate it._"));
    }
}
