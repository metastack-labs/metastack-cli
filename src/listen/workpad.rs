use crate::linear::IssueSummary;

use super::workspace::{TicketWorkspace, TicketWorkspaceProvisioning};

pub fn render_bootstrap_workpad(
    issue: &IssueSummary,
    workspace: &TicketWorkspace,
    timestamp: &str,
) -> String {
    let plan_requirements = extract_requirements(issue.description.as_deref());
    let acceptance = if plan_requirements.is_empty() {
        vec![
            format!(
                "Implement the requested behavior for `{}` in the dedicated ticket workspace.",
                issue.identifier
            ),
            "Keep a single persistent `## Codex Workpad` comment updated throughout execution."
                .to_string(),
            "Validate the changed behavior with direct command-path proofs before review."
                .to_string(),
        ]
    } else {
        plan_requirements.clone()
    };

    let mut lines = vec![
        "## Codex Workpad".to_string(),
        String::new(),
        "```text".to_string(),
        format!(
            "{}:{}@{}",
            local_hostname(),
            workspace.workspace_path.display(),
            workspace.head_sha
        ),
        "```".to_string(),
        String::new(),
        "### Plan".to_string(),
        String::new(),
        format!(
            "- [ ] 1\\. Reproduce the current behavior and confirm the scope for `{}`",
            issue.identifier
        ),
        format!(
            "  - [ ] 1.1 Capture a deterministic reproduction signal for `{}`",
            issue.identifier
        ),
        "  - [ ] 1.2 Inventory the affected code paths and constraints before editing".to_string(),
        format!(
            "- [ ] 2\\. Complete the local backlog for `{}` in the dedicated workspace clone",
            issue.identifier
        ),
    ];

    if plan_requirements.is_empty() {
        lines.extend([
            "  - [ ] 2.1 Build the feature and config changes described in the issue".to_string(),
            "  - [ ] 2.2 Keep the workpad current as implementation milestones land".to_string(),
        ]);
    } else {
        for (index, item) in plan_requirements.iter().take(4).enumerate() {
            lines.push(format!("  - [ ] 2.{} {}", index + 1, item));
        }
    }

    lines.extend([
        "- [ ] 3\\. Validate, publish, and prepare the change for review".to_string(),
        "  - [ ] 3.1 Run focused tests plus required quality gates".to_string(),
        "  - [ ] 3.2 Commit, push, and attach the PR to the Linear issue".to_string(),
        String::new(),
        "### Acceptance Criteria".to_string(),
        String::new(),
    ]);

    for item in acceptance.iter().take(6) {
        lines.push(format!("- [ ] {item}"));
    }
    lines.extend([
        format!(
            "- [ ] Work is executed from `{}` instead of the source repository checkout.",
            workspace.workspace_path.display()
        ),
        format!(
            "- [ ] Local backlog `.metastack/backlog/{}` stays in sync with the work completed for `{}`.",
            issue.identifier,
            issue.identifier
        ),
        String::new(),
        "### Validation".to_string(),
        String::new(),
        "- [ ] targeted tests: `cargo test`".to_string(),
        "- [ ] quality gates: `cargo fmt --check`".to_string(),
        "- [ ] lint: `cargo clippy --all-targets --all-features -- -D warnings`".to_string(),
        "- [ ] command-path proof: run the changed CLI flow against a deterministic local or mocked setup".to_string(),
        String::new(),
        "### Notes".to_string(),
        String::new(),
        format!(
            "- {timestamp} Prepared working branch `{}` from `{}` in workspace `{}`.",
            workspace.branch,
            workspace.base_ref,
            workspace.workspace_path.display()
        ),
        format!(
            "- {timestamp} Local backlog for `{}` is tracked at `.metastack/backlog/{}`.",
            issue.identifier,
            issue.identifier
        ),
        format!(
            "- {timestamp} Workspace root: `{}`.",
            workspace.workspace_root.display()
        ),
        format!(
            "- {timestamp} Workspace {} at HEAD `{}`.",
            match workspace.provisioning {
                TicketWorkspaceProvisioning::Created => "created",
                TicketWorkspaceProvisioning::Refreshed => "refreshed",
                TicketWorkspaceProvisioning::Recreated => "recreated",
            },
            workspace.head_sha
        ),
    ]);

    lines.join("\n")
}

pub(crate) fn extract_requirements(description: Option<&str>) -> Vec<String> {
    let Some(description) = description else {
        return Vec::new();
    };

    let mut requirements = Vec::new();
    let mut in_code_block = false;

    for raw_line in description.lines() {
        let line = raw_line.trim();
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block || line.is_empty() {
            continue;
        }

        if let Some(item) = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| numbered_item(line))
        {
            let cleaned = clean_requirement(item);
            if !cleaned.is_empty() {
                requirements.push(cleaned);
            }
            continue;
        }

        if line.starts_with("We also need")
            || line.starts_with("During the")
            || line.starts_with("At times I only want")
            || line.starts_with("We'll have")
        {
            let cleaned = clean_requirement(line);
            if !cleaned.is_empty() {
                requirements.push(cleaned);
            }
        }
    }

    requirements.sort();
    requirements.dedup();
    requirements
}

fn numbered_item(line: &str) -> Option<&str> {
    let (prefix, rest) = line.split_once('.')?;
    prefix
        .chars()
        .all(|ch| ch.is_ascii_digit())
        .then_some(rest.trim())
}

fn clean_requirement(line: &str) -> String {
    line.replace('`', "")
        .trim_matches(|ch: char| ch == '-' || ch == ':' || ch.is_whitespace())
        .to_string()
}

fn local_hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}
