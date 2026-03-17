use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::fs::{PlanningPaths, canonicalize_existing_dir, ensure_dir, write_text_file};
use crate::scaffold::ensure_planning_layout;

#[derive(Debug, Clone, Default)]
pub(crate) struct TicketMetadata {
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) state: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentBriefRequest {
    pub(crate) ticket: String,
    pub(crate) title_override: Option<String>,
    pub(crate) goal: Option<String>,
    pub(crate) metadata: TicketMetadata,
    pub(crate) output: Option<PathBuf>,
}

pub(crate) fn write_agent_brief(root: &Path, request: AgentBriefRequest) -> Result<PathBuf> {
    let root = canonicalize_existing_dir(root)?;
    ensure_planning_layout(&root, false)?;
    let paths = PlanningPaths::new(&root);
    ensure_dir(&paths.agent_briefs_dir)?;

    let output_path = request.output.clone().unwrap_or_else(|| {
        paths
            .agent_briefs_dir
            .join(format!("{}.md", sanitize_ticket(&request.ticket)))
    });
    let contents = render_brief(&request, &paths)?;
    write_text_file(&output_path, &contents, true)?;

    Ok(output_path)
}

fn render_brief(request: &AgentBriefRequest, paths: &PlanningPaths) -> Result<String> {
    let scan = read_context(&paths.scan_path())?;
    let architecture = read_context(&paths.architecture_path())?;
    let concerns = read_context(&paths.concerns_path())?;
    let conventions = read_context(&paths.conventions_path())?;
    let integrations = read_context(&paths.integrations_path())?;
    let stack = read_context(&paths.stack_path())?;
    let structure = read_context(&paths.structure_path())?;
    let testing = read_context(&paths.testing_path())?;
    let title = request
        .metadata
        .title
        .clone()
        .or_else(|| request.title_override.clone())
        .unwrap_or_else(|| "Title unavailable".to_string());

    let mut lines = vec![
        format!("# Agent Kickoff: {}", request.ticket),
        String::new(),
        "## Objective".to_string(),
        String::new(),
        format!("- Ticket: `{}`", request.ticket),
        format!("- Title: {}", title),
    ];

    if let Some(goal) = request.goal.as_deref() {
        lines.push(format!("- Goal: {}", goal));
    }

    if let Some(state) = &request.metadata.state {
        lines.push(format!("- Current state: {}", state));
    }

    if let Some(url) = &request.metadata.url {
        lines.push(format!("- Linear URL: {}", url));
    }

    lines.extend([
        String::new(),
        "## Guidance".to_string(),
        String::new(),
        "- Reconfirm the issue scope and current repository state before editing.".to_string(),
        "- Use `.metastack/codebase/*.md` as the reusable source of context for future agents.".to_string(),
        "- Capture reproduction, implement the requested change, validate with focused command proofs, and update the workpad.".to_string(),
        String::new(),
        "## Scan".to_string(),
        String::new(),
        scan,
        String::new(),
        "## Architecture".to_string(),
        String::new(),
        architecture,
        String::new(),
        "## Concerns".to_string(),
        String::new(),
        concerns,
        String::new(),
        "## Conventions".to_string(),
        String::new(),
        conventions,
        String::new(),
        "## Integrations".to_string(),
        String::new(),
        integrations,
        String::new(),
        "## Stack".to_string(),
        String::new(),
        stack,
        String::new(),
        "## Structure".to_string(),
        String::new(),
        structure,
        String::new(),
        "## Testing".to_string(),
        String::new(),
        testing,
    ]);

    if let Some(description) = &request.metadata.description {
        lines.extend([
            String::new(),
            "## Linear Description".to_string(),
            String::new(),
            description.clone(),
        ]);
    }

    Ok(lines.join("\n"))
}

fn read_context(path: &PathBuf) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(format!(
            "_Missing `{}`. Run `metastack-cli scan` to generate it._",
            path.file_name()
                .map(|value| value.to_string_lossy())
                .unwrap_or_default()
        )),
        Err(error) => Err(error).with_context(|| format!("failed to read `{}`", path.display())),
    }
}

fn sanitize_ticket(ticket: &str) -> String {
    ticket
        .chars()
        .map(|character| match character {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' => character,
            _ => '-',
        })
        .collect()
}
