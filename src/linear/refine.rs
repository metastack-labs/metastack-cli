use std::path::Path;

use anyhow::{Result, bail};

use crate::cli::{IssueRefineArgs, LinearClientArgs};
use crate::config::AGENT_ROUTE_LINEAR_ISSUES_REFINE;
use crate::fs::{canonicalize_existing_dir, display_path};
use crate::review::{ReviewEngineConfig, ReviewReport, review_issue};
use crate::scaffold::ensure_planning_layout;

use super::IssueSummary;
use super::command::load_linear_command_context;

/// Run the `meta linear issues refine` command.
///
/// This delegates to the shared review engine in [`crate::review`] while
/// preserving the existing refinement semantics: single-agent resolution
/// via the `linear.issues.refine` route, artifacts stored under
/// `artifacts/refinement/`, and critique-only by default.
pub(crate) async fn run_issue_refine_command(
    client_args: &LinearClientArgs,
    cli_default_team: Option<String>,
    args: IssueRefineArgs,
) -> Result<()> {
    let root = canonicalize_existing_dir(&client_args.root)?;
    ensure_planning_layout(&root, false)?;
    if args.passes == 0 {
        bail!("`meta issues refine` requires `--passes` to be at least 1");
    }
    let command_context = load_linear_command_context(client_args, cli_default_team)?;
    let mut reports = Vec::with_capacity(args.issues.len());

    for identifier in &args.issues {
        let issue = command_context.service.load_issue(identifier).await?;
        validate_issue_scope(
            &issue,
            command_context.default_team.as_deref(),
            command_context.default_project_id.as_deref(),
        )?;

        let config = ReviewEngineConfig {
            root: root.clone(),
            artifact_kind: "refinement".to_string(),
            route_key: AGENT_ROUTE_LINEAR_ISSUES_REFINE.to_string(),
            critique_only: !args.apply,
            passes: args.passes,
            agent_chain: Vec::new(),
            fallback_enabled: false,
            agent: args.agent.clone(),
            model: args.model.clone(),
            reasoning: args.reasoning.clone(),
            prompt_pass_label: "Refinement".to_string(),
        };

        let report = review_issue(&config, Some(&command_context.service), &issue).await?;
        reports.push(report);
    }

    println!("{}", render_refinement_reports(&root, &reports));
    Ok(())
}

/// Render refinement reports using the legacy format for backward compatibility.
fn render_refinement_reports(root: &Path, reports: &[ReviewReport]) -> String {
    let mut lines = vec![format!("Refined {} issue(s):", reports.len())];

    for report in reports {
        let mode = if report.apply_requested {
            "applied"
        } else {
            "critique-only"
        };
        lines.push(format!(
            "- {}: {} ({})",
            report.issue_identifier,
            mode,
            display_path(&report.run_dir, root)
        ));
    }

    lines.join("\n")
}

fn validate_issue_scope(
    issue: &IssueSummary,
    default_team: Option<&str>,
    default_project_id: Option<&str>,
) -> Result<()> {
    if let Some(team) = default_team
        && !issue.team.key.eq_ignore_ascii_case(team)
    {
        bail!(
            "issue `{}` belongs to team `{}`, outside the configured repo team scope `{}`",
            issue.identifier,
            issue.team.key,
            team
        );
    }

    if let Some(project_selector) = default_project_id {
        let Some(project) = issue.project.as_ref() else {
            bail!(
                "issue `{}` has no project, outside the configured repo project scope `{}`",
                issue.identifier,
                project_selector
            );
        };
        let matches =
            project.id == project_selector || project.name.eq_ignore_ascii_case(project_selector);
        if !matches {
            bail!(
                "issue `{}` belongs to project `{}` (`{}`), outside the configured repo project scope `{}`",
                issue.identifier,
                project.name,
                project.id,
                project_selector
            );
        }
    }

    Ok(())
}
