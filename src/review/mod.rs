pub(crate) mod dashboard;
mod state;
pub(crate) mod store;

use std::collections::BTreeSet;
use std::io;
use std::io::IsTerminal;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use serde::Serialize;

use crate::agents::{
    render_invocation_diagnostics, resolve_agent_invocation_for_planning, run_agent_capture,
};
use crate::cli::{ReviewArgs, ReviewDashboardEventArg, ReviewRunArgs, RunAgentArgs};
use crate::config::{
    AGENT_ROUTE_AGENTS_REVIEW, AppConfig, LinearConfig, LinearConfigOverrides, PlanningMeta,
};
use crate::context::{load_codebase_context_bundle, load_workflow_contract, render_repo_map};
use crate::fs::{
    canonicalize_existing_dir, ensure_dir, ensure_workspace_path_is_safe, sibling_workspace_root,
};
use crate::github_pr::GhCli;
use crate::linear::{IssueComment, IssueSummary, LinearService, ReqwestLinearClient};

use dashboard::{
    ReviewBrowserAction, ReviewBrowserState, ReviewListView, render,
    render_review_dashboard_snapshot,
};
use state::{ReviewPhase, ReviewSession};
use store::ReviewProjectStore;

const REVIEW_INSTRUCTIONS: &str = include_str!("../artifacts/REVIEW.md");
const METASTACK_LABEL: &str = "metastack";
const INPUT_POLL_INTERVAL_MILLIS: u64 = 100;
const TERMINAL_REFRESH_INTERVAL_SECONDS: u64 = 1;
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 60;

/// Dashboard data for the review listener.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReviewDashboardData {
    pub(crate) scope: String,
    pub(crate) cycle_summary: String,
    pub(crate) eligible_prs: usize,
    pub(crate) sessions: Vec<ReviewSession>,
    pub(crate) now_epoch_seconds: u64,
    pub(crate) notes: Vec<String>,
    pub(crate) state_file: String,
}

impl ReviewDashboardData {
    /// Render a human-readable summary for --once output.
    ///
    /// Returns a multi-line string suitable for terminal display.
    pub(crate) fn render_summary(&self) -> String {
        let mut lines = vec![
            format!("Review Dashboard: {}", self.scope),
            self.cycle_summary.clone(),
            format!("Eligible PRs: {}", self.eligible_prs),
            format!("State file: {}", self.state_file),
        ];
        if !self.notes.is_empty() {
            lines.push("Notes:".to_string());
            for note in &self.notes {
                lines.push(format!("  - {note}"));
            }
        }
        if !self.sessions.is_empty() {
            lines.push("Sessions:".to_string());
            for session in &self.sessions {
                lines.push(format!(
                    "  - #{} [{}] {} (remediation: {})",
                    session.pr_number,
                    session.phase.display_label(),
                    session.summary,
                    session.remediation_label()
                ));
            }
        }
        lines.join("\n")
    }

    fn sessions_for_view(&self, view: ReviewListView) -> Vec<&ReviewSession> {
        self.sessions
            .iter()
            .filter(|s| match view {
                ReviewListView::Active => !s.phase.is_completed(),
                ReviewListView::Completed => s.phase.is_completed(),
            })
            .collect()
    }
}

/// GitHub PR metadata as returned by `gh pr view --json`.
#[derive(Debug, Clone, serde::Deserialize)]
struct GhPrMetadata {
    number: u64,
    title: String,
    url: String,
    body: Option<String>,
    author: GhPrAuthor,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    #[serde(rename = "changedFiles")]
    changed_files: u64,
    additions: u64,
    deletions: u64,
    state: String,
    labels: Vec<GhPrLabel>,
    #[serde(rename = "reviewDecision", default)]
    review_decision: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct GhPrAuthor {
    login: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct GhPrLabel {
    name: String,
}

/// Minimal PR listing entry from `gh pr list --json`.
#[derive(Debug, Clone, serde::Deserialize)]
struct GhPrListEntry {
    number: u64,
    title: String,
    url: String,
    #[allow(dead_code)]
    labels: Vec<GhPrLabel>,
}

/// Run the unified `meta agents review` command.
///
/// Dispatches between one-shot PR review (when `pr_number` is provided) and
/// listener/dashboard mode (when `pr_number` is omitted).
///
/// Returns an error when prerequisite checks fail, agent execution fails,
/// or required external tools are unavailable.
pub(crate) async fn run_review(args: &ReviewArgs) -> Result<()> {
    if let Some(pr_number) = args.pr_number {
        if should_launch_interactive_review_dashboard(&args.run) {
            run_review_one_shot_with_dashboard(&args.run, pr_number)
        } else {
            run_review_one_shot(&args.run, pr_number)
        }
    } else {
        run_review_listener(&args.run).await
    }
}

// ---------------------------------------------------------------------------
// One-shot PR review
// ---------------------------------------------------------------------------

fn run_review_one_shot(args: &ReviewRunArgs, pr_number: u64) -> Result<()> {
    let root = canonicalize_existing_dir(&args.root)?;
    let config = AppConfig::load()?;
    let planning_meta = crate::config::load_required_planning_meta(&root, "meta agents review")?;
    let gh = GhCli;

    verify_gh_auth(&root)?;

    let pr = fetch_pr_metadata(&gh, &root, pr_number)?;
    let linear_identifier = resolve_linear_identifier(&pr)?;

    if args.dry_run {
        return print_dry_run_output(
            &config,
            &planning_meta,
            &root,
            &pr,
            &linear_identifier,
            args,
        );
    }

    let diff = fetch_pr_diff(&root, pr_number)?;

    let context_bundle = load_codebase_context_bundle(&root).unwrap_or_default();
    let workflow_contract = load_workflow_contract(&root).unwrap_or_default();
    let repo_map = render_repo_map(&root).unwrap_or_default();
    let ticket_context =
        gather_linear_ticket_context(&root, &config, &planning_meta, &linear_identifier)?;

    let review_prompt = assemble_review_prompt(
        &pr,
        &linear_identifier,
        &diff,
        &context_bundle,
        &workflow_contract,
        &repo_map,
        &ticket_context,
    );

    let agent_args = RunAgentArgs {
        root: Some(root.clone()),
        route_key: Some(AGENT_ROUTE_AGENTS_REVIEW.to_string()),
        agent: args.agent.clone(),
        prompt: review_prompt,
        instructions: Some(REVIEW_INSTRUCTIONS.to_string()),
        model: args.model.clone(),
        reasoning: args.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    };

    let invocation = resolve_agent_invocation_for_planning(&config, &planning_meta, &agent_args)?;

    eprintln!(
        "Reviewing PR #{pr_number} ({}) with {}...",
        pr.title, invocation.agent
    );

    let report = run_agent_capture(&agent_args)?;
    let review_output = report.stdout.trim().to_string();
    let remediation_required = review_output_requires_remediation(&review_output);

    if remediation_required {
        eprintln!("Remediation required. Creating follow-up branch and PR...");
        let outcome = run_remediation(
            &root,
            &pr,
            &linear_identifier,
            &review_output,
            &config,
            &planning_meta,
            args,
        )?;
        eprintln!(
            "Remediation PR #{} created: {}",
            outcome.pr_number, outcome.pr_url
        );
    } else {
        eprintln!("No remediation required for PR #{pr_number}.");
        println!("{review_output}");
    }

    Ok(())
}

fn run_review_one_shot_with_dashboard(args: &ReviewRunArgs, pr_number: u64) -> Result<()> {
    let root = canonicalize_existing_dir(&args.root)?;
    let config = AppConfig::load()?;
    let planning_meta = crate::config::load_required_planning_meta(&root, "meta agents review")?;
    let gh = GhCli;
    let mut dashboard = ReviewTerminalDashboard::open()?;
    let mut data = one_shot_dashboard_data(
        &root,
        ReviewSession {
            pr_number,
            pr_title: format!("PR #{pr_number}"),
            pr_url: None,
            pr_author: None,
            head_branch: None,
            base_branch: None,
            linear_identifier: None,
            phase: ReviewPhase::Claimed,
            summary: "Initializing review dashboard".to_string(),
            updated_at_epoch_seconds: now_epoch_seconds(),
            remediation_required: None,
            remediation_pr_number: None,
            remediation_pr_url: None,
        },
        vec!["Initializing one-shot review dashboard.".to_string()],
    );
    dashboard.draw(&data)?;

    set_session_status(
        &mut data,
        ReviewPhase::Claimed,
        "Verifying GitHub CLI authentication",
        "Checking `gh auth status` before review starts.",
    );
    dashboard.draw(&data)?;
    verify_gh_auth(&root)?;

    set_session_status(
        &mut data,
        ReviewPhase::ReviewStarted,
        "Loading pull request metadata",
        format!("Fetching PR #{pr_number} metadata from GitHub."),
    );
    dashboard.draw(&data)?;
    let pr = fetch_pr_metadata(&gh, &root, pr_number)?;
    populate_session_metadata(&mut data.sessions[0], &pr);
    let linear_identifier = resolve_linear_identifier(&pr)?;
    data.sessions[0].linear_identifier = Some(linear_identifier.clone());
    dashboard.draw(&data)?;

    let diff = {
        set_session_status(
            &mut data,
            ReviewPhase::ReviewStarted,
            "Loading pull request diff and repository context",
            format!(
                "Resolving linked Linear ticket `{linear_identifier}` and gathering repository context."
            ),
        );
        dashboard.draw(&data)?;
        fetch_pr_diff(&root, pr_number)?
    };

    let context_bundle = load_codebase_context_bundle(&root).unwrap_or_default();
    let workflow_contract = load_workflow_contract(&root).unwrap_or_default();
    let repo_map = render_repo_map(&root).unwrap_or_default();
    let ticket_context =
        gather_linear_ticket_context(&root, &config, &planning_meta, &linear_identifier)?;

    let review_prompt = assemble_review_prompt(
        &pr,
        &linear_identifier,
        &diff,
        &context_bundle,
        &workflow_contract,
        &repo_map,
        &ticket_context,
    );

    let agent_args = RunAgentArgs {
        root: Some(root.clone()),
        route_key: Some(AGENT_ROUTE_AGENTS_REVIEW.to_string()),
        agent: args.agent.clone(),
        prompt: review_prompt,
        instructions: Some(REVIEW_INSTRUCTIONS.to_string()),
        model: args.model.clone(),
        reasoning: args.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    };

    let invocation = resolve_agent_invocation_for_planning(&config, &planning_meta, &agent_args)?;
    set_session_status(
        &mut data,
        ReviewPhase::Running,
        format!("Running agent review with {}", invocation.agent),
        format!(
            "Resolved provider `{}` with model `{}` and reasoning `{}`.",
            invocation.agent,
            invocation.model.as_deref().unwrap_or("unset"),
            invocation.reasoning.as_deref().unwrap_or("unset")
        ),
    );
    dashboard.draw(&data)?;

    let report = run_agent_capture(&agent_args)?;
    let review_output = report.stdout.trim().to_string();
    let remediation_required = review_output_requires_remediation(&review_output);

    if remediation_required {
        set_session_status(
            &mut data,
            ReviewPhase::Running,
            "Creating remediation branch and PR",
            "Required fixes were found. Opening a remediation branch and follow-up PR.",
        );
        data.sessions[0].remediation_required = Some(true);
        dashboard.draw(&data)?;

        let outcome = run_remediation(
            &root,
            &pr,
            &linear_identifier,
            &review_output,
            &config,
            &planning_meta,
            args,
        )?;
        data.sessions[0].phase = ReviewPhase::Completed;
        data.sessions[0].summary = "Remediation PR created".to_string();
        data.sessions[0].remediation_pr_number = Some(outcome.pr_number);
        data.sessions[0].remediation_pr_url = Some(outcome.pr_url.clone());
        data.sessions[0].updated_at_epoch_seconds = now_epoch_seconds();
        push_note(
            &mut data,
            format!(
                "Opened remediation PR #{} for original PR #{pr_number}.",
                outcome.pr_number
            ),
        );
        dashboard.draw(&data)?;
    } else {
        data.sessions[0].phase = ReviewPhase::Completed;
        data.sessions[0].summary = "No remediation required".to_string();
        data.sessions[0].remediation_required = Some(false);
        data.sessions[0].updated_at_epoch_seconds = now_epoch_seconds();
        push_note(
            &mut data,
            format!("Review finished for PR #{pr_number} without remediation."),
        );
        dashboard.draw(&data)?;
    }

    dashboard.close()?;
    println!("{review_output}");

    Ok(())
}

fn verify_gh_auth(root: &Path) -> Result<()> {
    let output = Command::new("gh")
        .args(["auth", "status"])
        .current_dir(root)
        .output()
        .context("failed to run `gh auth status` — is `gh` installed and on PATH?")?;
    if !output.status.success() {
        bail!(
            "`gh auth status` failed: {}. Run `gh auth login` to authenticate.",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn fetch_pr_metadata(gh: &GhCli, root: &Path, pr_number: u64) -> Result<GhPrMetadata> {
    gh.run_json(
        root,
        &[
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "number,title,url,body,author,headRefName,baseRefName,changedFiles,additions,deletions,state,labels,reviewDecision",
        ],
    )
    .with_context(|| format!("failed to fetch PR #{pr_number} metadata — does the PR exist?"))
}

fn resolve_linear_identifier(pr: &GhPrMetadata) -> Result<String> {
    let identifiers = collect_linear_identifiers(pr);
    match identifiers.as_slice() {
        [identifier] => Ok(identifier.clone()),
        [] => bail!(
            "no Linear ticket identifier found in PR #{} title, branch, or body. \
             Expected a pattern like `MET-42` or `ENG-1234`.",
            pr.number
        ),
        _ => bail!(
            "multiple Linear ticket identifiers found in PR #{}: {}. \
             Link exactly one ticket in the title, branch, or body.",
            pr.number,
            identifiers.join(", ")
        ),
    }
}

fn collect_linear_identifiers(pr: &GhPrMetadata) -> Vec<String> {
    let mut identifiers = BTreeSet::new();
    for candidate in [
        &pr.title as &str,
        &pr.head_ref_name,
        pr.body.as_deref().unwrap_or(""),
    ] {
        collect_linear_identifiers_from_text(candidate, &mut identifiers);
    }
    identifiers.into_iter().collect()
}

fn collect_linear_identifiers_from_text(text: &str, identifiers: &mut BTreeSet<String>) {
    if let Some(identifier) = extract_linear_identifier(text) {
        identifiers.insert(identifier);
    }
    for segment in text.split([' ', ':', '/', '_', '(', ')', '[', ']', ',', '\n', '\t']) {
        if is_linear_identifier(segment) {
            identifiers.insert(segment.to_uppercase());
        }
        let parts: Vec<&str> = segment.split('-').collect();
        if parts.len() >= 2 {
            let candidate = format!("{}-{}", parts[0], parts[1]);
            if is_linear_identifier(&candidate) {
                identifiers.insert(candidate.to_uppercase());
            }
        }
    }
}

fn extract_linear_identifier(text: &str) -> Option<String> {
    // Split on common delimiters (spaces, colons, slashes, underscores)
    for segment in text.split([' ', ':', '/', '_', '(', ')', '[', ']']) {
        // Try the segment itself
        if is_linear_identifier(segment) {
            return Some(segment.to_uppercase());
        }
        // For branch-style names like "met-74-implement-review", try prefix matches
        let parts: Vec<&str> = segment.split('-').collect();
        if parts.len() >= 2 {
            let candidate = format!("{}-{}", parts[0], parts[1]);
            if is_linear_identifier(&candidate) {
                return Some(candidate.to_uppercase());
            }
        }
    }
    None
}

fn is_linear_identifier(s: &str) -> bool {
    let parts: Vec<&str> = s.splitn(2, '-').collect();
    if parts.len() != 2 {
        return false;
    }
    let prefix = parts[0];
    let number = parts[1];
    !prefix.is_empty()
        && prefix.chars().all(|c| c.is_ascii_alphabetic())
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
}

fn fetch_pr_diff(root: &Path, pr_number: u64) -> Result<String> {
    let output = Command::new("gh")
        .args(["pr", "diff", &pr_number.to_string()])
        .current_dir(root)
        .output()
        .context("failed to run `gh pr diff`")?;
    if !output.status.success() {
        bail!(
            "failed to fetch diff for PR #{pr_number}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn gather_linear_ticket_context(
    root: &Path,
    config: &AppConfig,
    planning_meta: &PlanningMeta,
    identifier: &str,
) -> Result<String> {
    let issue = load_linear_issue(root, config, planning_meta, identifier)?;
    Ok(render_linear_ticket_context(&issue))
}

fn load_linear_issue(
    root: &Path,
    config: &AppConfig,
    planning_meta: &PlanningMeta,
    identifier: &str,
) -> Result<IssueSummary> {
    let linear_config = LinearConfig::from_sources(
        config,
        planning_meta,
        Some(root),
        LinearConfigOverrides::default(),
    )?;
    let default_team = linear_config.default_team.clone();
    let service = LinearService::new(ReqwestLinearClient::new(linear_config)?, default_team);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to initialize Linear runtime for review")?
        .block_on(async move { service.load_issue(identifier).await })
        .with_context(|| format!("failed to load Linear ticket `{identifier}`"))
}

fn render_linear_ticket_context(issue: &IssueSummary) -> String {
    let labels = if issue.labels.is_empty() {
        "none".to_string()
    } else {
        issue
            .labels
            .iter()
            .map(|label| label.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let acceptance_criteria = extract_markdown_section(
        issue.description.as_deref().unwrap_or(""),
        &["Acceptance Criteria", "Acceptance", "Requirements"],
    )
    .unwrap_or_else(|| {
        "No explicit acceptance criteria section found in the Linear description.".to_string()
    });
    let workpad = active_workpad_comment(issue)
        .map(|comment| comment.body.trim().to_string())
        .unwrap_or_else(|| "No active workpad comment found.".to_string());

    format!(
        "Identifier: {identifier}\n\
         Title: {title}\n\
         URL: {url}\n\
         State: {state}\n\
         Priority: {priority}\n\
         Project: {project}\n\
         Labels: {labels}\n\
\n\
         ## Acceptance Criteria\n\
         {acceptance_criteria}\n\
\n\
         ## Description\n\
         {description}\n\
\n\
         ## Active Workpad\n\
         {workpad}\n\
\n\
         ## Ticket Discussion\n\
         {discussion}",
        identifier = issue.identifier,
        title = issue.title,
        url = issue.url,
        state = issue
            .state
            .as_ref()
            .map(|state| state.name.as_str())
            .unwrap_or("unknown"),
        priority = issue
            .priority
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unset".to_string()),
        project = issue
            .project
            .as_ref()
            .map(|project| project.name.as_str())
            .unwrap_or("none"),
        labels = labels,
        acceptance_criteria = acceptance_criteria,
        description = issue
            .description
            .as_deref()
            .unwrap_or("No Linear description provided."),
        workpad = workpad,
        discussion = render_recent_linear_comments(issue),
    )
}

fn extract_markdown_section(body: &str, headings: &[&str]) -> Option<String> {
    let mut lines = Vec::new();
    let mut capturing = false;

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            let heading = trimmed.trim_start_matches('#').trim();
            if headings
                .iter()
                .any(|candidate| heading.eq_ignore_ascii_case(candidate))
            {
                capturing = true;
                continue;
            }
            if capturing {
                break;
            }
        }

        if capturing {
            lines.push(line);
        }
    }

    let rendered = lines.join("\n").trim().to_string();
    (!rendered.is_empty()).then_some(rendered)
}

fn active_workpad_comment(issue: &IssueSummary) -> Option<&IssueComment> {
    issue
        .comments
        .iter()
        .rev()
        .find(|comment| comment.resolved_at.is_none() && comment.body.contains("## Codex Workpad"))
}

fn render_recent_linear_comments(issue: &IssueSummary) -> String {
    let mut rendered = issue
        .comments
        .iter()
        .rev()
        .filter(|comment| !comment.body.trim().is_empty())
        .filter(|comment| !comment.body.contains("## Codex Workpad"))
        .take(5)
        .map(|comment| {
            let author = comment.user_name.as_deref().unwrap_or("unknown");
            let created_at = comment.created_at.as_deref().unwrap_or("unknown");
            format!("- {author} ({created_at})\n{}", comment.body.trim())
        })
        .collect::<Vec<_>>();
    rendered.reverse();

    if rendered.is_empty() {
        "No ticket comments found.".to_string()
    } else {
        rendered.join("\n\n")
    }
}

fn assemble_review_prompt(
    pr: &GhPrMetadata,
    linear_identifier: &str,
    diff: &str,
    context_bundle: &str,
    workflow_contract: &str,
    repo_map: &str,
    ticket_context: &str,
) -> String {
    let review_state = pr.review_decision.as_deref().unwrap_or("PENDING");

    let diff_display = if diff.len() > 100_000 {
        format!(
            "{}\n\n... (diff truncated at 100,000 chars; {} total)",
            &diff[..100_000],
            diff.len()
        )
    } else {
        diff.to_string()
    };

    format!(
        r#"# PR Review Request

## PR Metadata
- Number: #{number}
- Title: {title}
- URL: {url}
- Author: {author}
- Head Branch: {head}
- Base Branch: {base}
- Changed Files: {changed_files}
- Additions: +{additions}
- Deletions: -{deletions}
- State: {state}
- Review Decision: {review_state}
- Labels: {labels}
- Linear Ticket: {linear_identifier}

## Linked Linear Ticket
{ticket_context}

## PR Description
{body}

## Diff
```diff
{diff_display}
```

## Review Instructions
{review_instructions}

## Workflow Contract
{workflow_contract}

## Codebase Context
{context_bundle}

## Repository Map
{repo_map}
"#,
        number = pr.number,
        title = pr.title,
        url = pr.url,
        author = pr.author.login,
        head = pr.head_ref_name,
        base = pr.base_ref_name,
        changed_files = pr.changed_files,
        additions = pr.additions,
        deletions = pr.deletions,
        state = pr.state,
        review_state = review_state,
        labels = pr
            .labels
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        body = pr.body.as_deref().unwrap_or("(no description)"),
        diff_display = diff_display,
        review_instructions = REVIEW_INSTRUCTIONS,
        ticket_context = ticket_context,
        workflow_contract = workflow_contract,
        context_bundle = context_bundle,
        repo_map = repo_map,
    )
}

fn review_output_requires_remediation(output: &str) -> bool {
    let lower = output.to_lowercase();
    if let Some(pos) = lower.find("### remediation required") {
        let rest = &lower[pos..];
        for line in rest.lines().skip(1).take(3) {
            let trimmed = line.trim();
            if trimmed.starts_with("yes") {
                return true;
            }
            if trimmed.starts_with("no") {
                return false;
            }
        }
    }
    false
}

fn print_dry_run_output(
    config: &AppConfig,
    planning_meta: &PlanningMeta,
    root: &Path,
    pr: &GhPrMetadata,
    linear_identifier: &str,
    args: &ReviewRunArgs,
) -> Result<()> {
    let invocation = resolve_agent_invocation_for_planning(
        config,
        planning_meta,
        &RunAgentArgs {
            root: Some(root.to_path_buf()),
            route_key: Some(AGENT_ROUTE_AGENTS_REVIEW.to_string()),
            agent: args.agent.clone(),
            prompt: "(dry-run preview)".to_string(),
            instructions: None,
            model: args.model.clone(),
            reasoning: args.reasoning.clone(),
            transport: None,
            attachments: Vec::new(),
        },
    )?;
    let diagnostics = render_invocation_diagnostics(&invocation);

    println!("--- dry-run: meta agents review #{} ---", pr.number);
    println!("PR: #{} — {}", pr.number, pr.title);
    println!("URL: {}", pr.url);
    println!("Author: {}", pr.author.login);
    println!("Branch: {} -> {}", pr.head_ref_name, pr.base_ref_name);
    println!(
        "Changed: {} files (+{}, -{})",
        pr.changed_files, pr.additions, pr.deletions
    );
    println!("Linear: {linear_identifier}");
    println!(
        "Review state: {}",
        pr.review_decision.as_deref().unwrap_or("PENDING")
    );
    println!();
    for line in &diagnostics {
        println!("{line}");
    }
    println!();
    println!("No mutations will be performed (dry-run mode).");

    Ok(())
}

// ---------------------------------------------------------------------------
// Remediation workflow
// ---------------------------------------------------------------------------

/// Result of a successful remediation workflow, carrying the follow-up PR details.
struct RemediationOutcome {
    pr_number: u64,
    pr_url: String,
}

fn run_remediation(
    root: &Path,
    pr: &GhPrMetadata,
    linear_identifier: &str,
    review_output: &str,
    config: &AppConfig,
    planning_meta: &PlanningMeta,
    args: &ReviewRunArgs,
) -> Result<RemediationOutcome> {
    let gh = GhCli;
    let remediation_branch = format!("review/remediation-pr-{}", pr.number);
    let workspace_path = prepare_remediation_workspace(root, pr.number)?;
    materialize_pull_request_head(&workspace_path, pr.number, &remediation_branch)?;
    let starting_head = git_stdout(&workspace_path, &["rev-parse", "HEAD"])?;

    let fix_prompt = format!(
        "You are applying required fixes from a code review to this branch.\n\n\
         ## Review Output\n{review_output}\n\n\
         ## Instructions\n\
         Apply ONLY the required fixes identified in the review above. Do not apply optional recommendations.\n\
         Make minimal, targeted changes. Commit each logical fix separately with clear commit messages.\n\
         After applying all fixes, verify the changes compile and pass basic checks.\n"
    );

    let fix_args = RunAgentArgs {
        root: Some(workspace_path.clone()),
        route_key: Some(AGENT_ROUTE_AGENTS_REVIEW.to_string()),
        agent: args.agent.clone(),
        prompt: fix_prompt,
        instructions: None,
        model: args.model.clone(),
        reasoning: args.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    };

    let report = run_agent_capture(&fix_args)?;
    eprintln!("{}", report.stdout.trim());

    ensure_remediation_commits_created(&workspace_path, &starting_head)?;

    run_git(
        &workspace_path,
        &["push", "-u", "origin", &remediation_branch],
    )
    .map_err(|e| {
        anyhow!(
            "failed to push remediation branch `{remediation_branch}`: {e}. \
             Check repository write permissions."
        )
    })?;

    let pr_title = format!("review: remediation for PR #{}", pr.number);
    let pr_body = format!(
        "## Summary\n\n\
         Automated remediation PR for #{pr_number} based on `meta agents review` audit.\n\n\
         ## Review Findings\n\n\
         {review_output}\n\n\
        ## Linear Ticket\n\n\
         {linear_identifier}\n",
        pr_number = pr.number,
    );
    let body_path = workspace_path.join(".metastack").join("review-pr-body.md");
    ensure_dir(&workspace_path.join(".metastack"))?;
    std::fs::write(&body_path, &pr_body).context("failed to write remediation PR body")?;

    let result = gh.publish_branch_pull_request(
        &workspace_path,
        crate::github_pr::PullRequestPublishRequest {
            head_branch: &remediation_branch,
            base_branch: &pr.base_ref_name,
            title: &pr_title,
            body_path: &body_path,
            mode: crate::github_pr::PullRequestPublishMode::Ready,
        },
    )?;

    let _ = gh.ensure_label_exists(
        &workspace_path,
        METASTACK_LABEL,
        "5319E7",
        "MetaStack managed PR",
    );
    let _ = gh.add_label_to_pull_request(&workspace_path, result.number, METASTACK_LABEL);

    eprintln!("Remediation PR #{} created: {}", result.number, result.url);

    post_linear_remediation_comment(
        root,
        config,
        planning_meta,
        linear_identifier,
        &result.url,
        pr.number,
    )?;

    let _ = std::fs::remove_file(&body_path);

    println!("{review_output}");
    println!(
        "\nRemediation PR #{} opened against `{}` from `{}`: {}",
        result.number,
        pr.base_ref_name,
        workspace_path.display(),
        result.url
    );

    Ok(RemediationOutcome {
        pr_number: result.number,
        pr_url: result.url,
    })
}

fn prepare_remediation_workspace(root: &Path, pr_number: u64) -> Result<std::path::PathBuf> {
    let workspace_root = sibling_workspace_root(root)?.join("review-runs");
    ensure_dir(&workspace_root)?;
    let workspace_path = workspace_root.join(format!("pr-{pr_number}"));

    if workspace_path.exists() {
        ensure_workspace_path_is_safe(root, &workspace_root, &workspace_path)?;
        std::fs::remove_dir_all(&workspace_path).with_context(|| {
            format!(
                "failed to remove existing remediation workspace `{}`",
                workspace_path.display()
            )
        })?;
    }

    run_git(
        root,
        &[
            "clone",
            root.to_str()
                .ok_or_else(|| anyhow!("repository path is not valid utf-8"))?,
            workspace_path
                .to_str()
                .ok_or_else(|| anyhow!("workspace path is not valid utf-8"))?,
        ],
    )?;
    ensure_workspace_path_is_safe(root, &workspace_root, &workspace_path)?;
    configure_workspace_git_identity(root, &workspace_path)?;
    Ok(workspace_path)
}

fn materialize_pull_request_head(
    workspace_path: &Path,
    pr_number: u64,
    remediation_branch: &str,
) -> Result<()> {
    let output = Command::new("gh")
        .args(["pr", "checkout", &pr_number.to_string(), "--detach"])
        .current_dir(workspace_path)
        .output()
        .context("failed to run `gh pr checkout` for remediation workspace")?;
    if !output.status.success() {
        bail!(
            "failed to materialize PR #{} in remediation workspace: {}",
            pr_number,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    run_git(
        workspace_path,
        &["checkout", "-B", remediation_branch, "HEAD"],
    )?;
    Ok(())
}

fn ensure_remediation_commits_created(workspace_path: &Path, starting_head: &str) -> Result<()> {
    let commit_count = git_stdout(
        workspace_path,
        &["rev-list", "--count", &format!("{starting_head}..HEAD")],
    )?;
    if commit_count.trim() == "0" {
        bail!(
            "remediation agent did not create any commits in `{}`",
            workspace_path.display()
        );
    }
    Ok(())
}

fn configure_workspace_git_identity(source_root: &Path, workspace_path: &Path) -> Result<()> {
    for key in ["user.email", "user.name"] {
        let value = git_stdout(source_root, &["config", "--get", key]).unwrap_or_default();
        let value = value.trim();
        if !value.is_empty() {
            run_git(workspace_path, &["config", key, value])?;
        }
    }
    Ok(())
}

fn post_linear_remediation_comment(
    root: &Path,
    config: &AppConfig,
    planning_meta: &PlanningMeta,
    linear_identifier: &str,
    remediation_pr_url: &str,
    original_pr_number: u64,
) -> Result<()> {
    let issue = load_linear_issue(root, config, planning_meta, linear_identifier)?;
    let linear_config = LinearConfig::from_sources(
        config,
        planning_meta,
        Some(root),
        LinearConfigOverrides::default(),
    )?;
    let default_team = linear_config.default_team.clone();
    let service = LinearService::new(ReqwestLinearClient::new(linear_config)?, default_team);
    let body = format!(
        "## MetaStack Review Remediation\n\nOpened remediation follow-up for PR #{original_pr_number}: {remediation_pr_url}"
    );

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to initialize Linear runtime for remediation comment")?
        .block_on(async move {
            service
                .upsert_comment_with_marker(&issue, "## MetaStack Review Remediation", body)
                .await
        })
        .with_context(|| {
            format!("failed to post remediation comment to Linear ticket `{linear_identifier}`")
        })?;

    Ok(())
}

fn run_git(root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("failed to run `git {}`", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn git_stdout(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
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

// ---------------------------------------------------------------------------
// Listener mode
// ---------------------------------------------------------------------------

async fn run_review_listener(args: &ReviewRunArgs) -> Result<()> {
    if args.json && !args.once {
        bail!("`--json` requires `--once` for `meta agents review`");
    }

    let root = canonicalize_existing_dir(&args.root)?;

    if args.check {
        return run_review_check(&root, args);
    }

    verify_gh_auth(&root)?;
    let store = ReviewProjectStore::resolve(&root)?;

    if args.once || args.json {
        return run_review_once(&root, &store, args);
    }

    if args.render_once {
        return run_review_render_once(&root, &store, args);
    }

    run_review_daemon(&root, &store, args).await
}

fn run_review_check(root: &Path, args: &ReviewRunArgs) -> Result<()> {
    let config = AppConfig::load()?;
    let planning_meta = crate::config::load_required_planning_meta(root, "meta agents review")?;

    verify_gh_auth(root)?;
    println!("gh auth: ok");

    let invocation = resolve_agent_invocation_for_planning(
        &config,
        &planning_meta,
        &RunAgentArgs {
            root: Some(root.to_path_buf()),
            route_key: Some(AGENT_ROUTE_AGENTS_REVIEW.to_string()),
            agent: args.agent.clone(),
            prompt: "(check)".to_string(),
            instructions: None,
            model: args.model.clone(),
            reasoning: args.reasoning.clone(),
            transport: None,
            attachments: Vec::new(),
        },
    )?;
    println!("agent: {} ({})", invocation.agent, invocation.command);
    let diagnostics = render_invocation_diagnostics(&invocation);
    for line in &diagnostics {
        println!("{line}");
    }

    let origin = store::resolve_origin_remote(root)?;
    println!("origin: {origin}");

    println!("\nAll review prerequisites satisfied.");
    Ok(())
}

fn run_review_once(root: &Path, store: &ReviewProjectStore, args: &ReviewRunArgs) -> Result<()> {
    let data = run_single_review_cycle(root, store, args)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&data)
                .context("failed to serialize review dashboard data")?
        );
    } else {
        println!("{}", data.render_summary());
    }
    Ok(())
}

fn run_review_render_once(
    root: &Path,
    store: &ReviewProjectStore,
    args: &ReviewRunArgs,
) -> Result<()> {
    let data = run_single_review_cycle(root, store, args)?;
    let mut state = ReviewBrowserState::default();
    for event_arg in &args.events {
        let action = match event_arg {
            ReviewDashboardEventArg::Up => ReviewBrowserAction::Up,
            ReviewDashboardEventArg::Down => ReviewBrowserAction::Down,
            ReviewDashboardEventArg::Tab => ReviewBrowserAction::Tab,
            ReviewDashboardEventArg::Enter => ReviewBrowserAction::Enter,
            ReviewDashboardEventArg::Back => ReviewBrowserAction::Back,
            ReviewDashboardEventArg::Esc => ReviewBrowserAction::Esc,
            ReviewDashboardEventArg::PageUp => ReviewBrowserAction::PageUp,
            ReviewDashboardEventArg::PageDown => ReviewBrowserAction::PageDown,
        };
        state.apply_action(action, &data);
    }
    let snapshot = render_review_dashboard_snapshot(args.width, args.height, &data, &state)?;
    println!("{snapshot}");
    Ok(())
}

async fn run_review_daemon(
    root: &Path,
    store: &ReviewProjectStore,
    args: &ReviewRunArgs,
) -> Result<()> {
    let _lock = store.acquire_lock()?;
    let mut terminal = ReviewTerminalDashboard::open()?;
    let mut browser_state = ReviewBrowserState::default();
    let mut last_poll = Instant::now() - Duration::from_secs(DEFAULT_POLL_INTERVAL_SECONDS + 1);
    let mut last_render =
        Instant::now() - Duration::from_secs(TERMINAL_REFRESH_INTERVAL_SECONDS + 1);
    let mut latest_data = initial_listener_dashboard_data(root, store);

    loop {
        if last_poll.elapsed() >= Duration::from_secs(DEFAULT_POLL_INTERVAL_SECONDS) {
            match run_single_review_cycle(root, store, args) {
                Ok(data) => {
                    latest_data = data;
                    last_poll = Instant::now();
                }
                Err(e) => {
                    push_note(&mut latest_data, format!("Review poll failed: {e}"));
                }
            }
        }

        if last_render.elapsed() >= Duration::from_secs(TERMINAL_REFRESH_INTERVAL_SECONDS) {
            terminal.draw_dashboard(&latest_data, &browser_state)?;
            last_render = Instant::now();
        }

        if event::poll(Duration::from_millis(INPUT_POLL_INTERVAL_MILLIS))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        browser_state.apply_action(ReviewBrowserAction::Up, &latest_data);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        browser_state.apply_action(ReviewBrowserAction::Down, &latest_data);
                    }
                    KeyCode::Tab => {
                        browser_state.apply_action(ReviewBrowserAction::Tab, &latest_data);
                    }
                    KeyCode::PageUp => {
                        browser_state.apply_action(ReviewBrowserAction::PageUp, &latest_data);
                    }
                    KeyCode::PageDown => {
                        browser_state.apply_action(ReviewBrowserAction::PageDown, &latest_data);
                    }
                    _ => {}
                }
            }
        }
    }

    terminal.close()?;

    Ok(())
}

fn run_single_review_cycle(
    root: &Path,
    store: &ReviewProjectStore,
    args: &ReviewRunArgs,
) -> Result<ReviewDashboardData> {
    let gh = GhCli;
    let mut state = store.load_state()?;
    let now = now_epoch_seconds();

    let eligible_prs = discover_eligible_prs(&gh, root)?;
    let eligible_count = eligible_prs.len();
    let mut notes = Vec::new();

    for pr_entry in &eligible_prs {
        if state.blocks_pickup(pr_entry.number) {
            continue;
        }
        let session = ReviewSession {
            pr_number: pr_entry.number,
            pr_title: pr_entry.title.clone(),
            pr_url: Some(pr_entry.url.clone()),
            pr_author: None,
            head_branch: None,
            base_branch: None,
            linear_identifier: None,
            phase: ReviewPhase::Claimed,
            summary: "Claimed for review".to_string(),
            updated_at_epoch_seconds: now,
            remediation_required: None,
            remediation_pr_number: None,
            remediation_pr_url: None,
        };
        state.upsert(session);
    }

    for session in &mut state.sessions {
        if session.phase != ReviewPhase::Claimed {
            continue;
        }
        session.phase = ReviewPhase::ReviewStarted;
        session.summary = "Resolving PR context".to_string();
        session.updated_at_epoch_seconds = now;

        session.phase = ReviewPhase::Running;
        session.summary = "Running agent review".to_string();
        session.updated_at_epoch_seconds = now_epoch_seconds();

        match run_review_for_session(root, session.pr_number, args) {
            Ok(result) => {
                session.phase = ReviewPhase::Completed;
                session.summary = result.summary;
                session.remediation_required = Some(result.remediation_required);
                session.remediation_pr_number = result.remediation_pr_number;
                session.remediation_pr_url = result.remediation_pr_url;
                session.linear_identifier = result.linear_identifier;
                session.updated_at_epoch_seconds = now_epoch_seconds();
            }
            Err(e) => {
                session.phase = ReviewPhase::Blocked;
                session.summary = format!("Review failed: {e}");
                session.updated_at_epoch_seconds = now_epoch_seconds();
                notes.push(format!("PR #{}: {e}", session.pr_number));
            }
        }
    }

    store.save_state(&state)?;

    let origin = store::resolve_origin_remote(root).unwrap_or_else(|_| "unknown".to_string());
    Ok(ReviewDashboardData {
        scope: origin,
        cycle_summary: format!(
            "Discovered {} eligible PRs, {} sessions total",
            eligible_count,
            state.sessions.len()
        ),
        eligible_prs: eligible_count,
        sessions: state.sorted_sessions(),
        now_epoch_seconds: now_epoch_seconds(),
        notes,
        state_file: store.paths().state_path.display().to_string(),
    })
}

struct ReviewResult {
    summary: String,
    remediation_required: bool,
    remediation_pr_number: Option<u64>,
    remediation_pr_url: Option<String>,
    linear_identifier: Option<String>,
}

fn run_review_for_session(
    root: &Path,
    pr_number: u64,
    args: &ReviewRunArgs,
) -> Result<ReviewResult> {
    let config = AppConfig::load()?;
    let planning_meta = crate::config::load_required_planning_meta(root, "meta agents review")?;
    let gh = GhCli;

    let pr = fetch_pr_metadata(&gh, root, pr_number)?;
    let linear_id = Some(resolve_linear_identifier(&pr)?);

    let diff = fetch_pr_diff(root, pr_number)?;
    let context_bundle = load_codebase_context_bundle(root).unwrap_or_default();
    let workflow_contract = load_workflow_contract(root).unwrap_or_default();
    let repo_map = render_repo_map(root).unwrap_or_default();
    let ticket_context = gather_linear_ticket_context(
        root,
        &config,
        &planning_meta,
        linear_id
            .as_deref()
            .expect("linear identifier should exist"),
    )?;

    let review_prompt = assemble_review_prompt(
        &pr,
        linear_id.as_deref().unwrap_or("UNKNOWN"),
        &diff,
        &context_bundle,
        &workflow_contract,
        &repo_map,
        &ticket_context,
    );

    let agent_args = RunAgentArgs {
        root: Some(root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_AGENTS_REVIEW.to_string()),
        agent: args.agent.clone(),
        prompt: review_prompt,
        instructions: Some(REVIEW_INSTRUCTIONS.to_string()),
        model: args.model.clone(),
        reasoning: args.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    };

    let report = run_agent_capture(&agent_args)?;
    let review_output = report.stdout.trim().to_string();
    let remediation_required = review_output_requires_remediation(&review_output);

    let mut result = ReviewResult {
        summary: if remediation_required {
            "Remediation required".to_string()
        } else {
            "No remediation required".to_string()
        },
        remediation_required,
        remediation_pr_number: None,
        remediation_pr_url: None,
        linear_identifier: linear_id.clone(),
    };

    if remediation_required {
        let linear_id = linear_id
            .as_deref()
            .ok_or_else(|| anyhow!("missing linked Linear ticket for remediation"))?;
        let outcome = run_remediation(
            root,
            &pr,
            linear_id,
            &review_output,
            &config,
            &planning_meta,
            args,
        )?;
        result.summary = "Remediation PR created".to_string();
        result.remediation_pr_number = Some(outcome.pr_number);
        result.remediation_pr_url = Some(outcome.pr_url);
    }

    Ok(result)
}

fn discover_eligible_prs(gh: &GhCli, root: &Path) -> Result<Vec<GhPrListEntry>> {
    gh.run_json(
        root,
        &[
            "pr",
            "list",
            "--state",
            "open",
            "--label",
            METASTACK_LABEL,
            "--json",
            "number,title,url,labels",
        ],
    )
}

fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}

fn should_launch_interactive_review_dashboard(args: &ReviewRunArgs) -> bool {
    io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && !args.dry_run
        && !args.check
        && !args.once
        && !args.json
        && !args.render_once
}

fn one_shot_dashboard_data(
    root: &Path,
    session: ReviewSession,
    notes: Vec<String>,
) -> ReviewDashboardData {
    ReviewDashboardData {
        scope: store::resolve_origin_remote(root).unwrap_or_else(|_| root.display().to_string()),
        cycle_summary: format!("Reviewing PR #{}", session.pr_number),
        eligible_prs: 1,
        sessions: vec![session],
        now_epoch_seconds: now_epoch_seconds(),
        notes,
        state_file: "one-shot review".to_string(),
    }
}

fn initial_listener_dashboard_data(root: &Path, store: &ReviewProjectStore) -> ReviewDashboardData {
    let sessions = store
        .load_state()
        .map(|state| state.sorted_sessions())
        .unwrap_or_default();
    ReviewDashboardData {
        scope: store::resolve_origin_remote(root).unwrap_or_else(|_| root.display().to_string()),
        cycle_summary: "Starting dashboard before the first review poll completes.".to_string(),
        eligible_prs: 0,
        sessions,
        now_epoch_seconds: now_epoch_seconds(),
        notes: vec!["Starting dashboard before the first review poll completes.".to_string()],
        state_file: store.paths().state_path.display().to_string(),
    }
}

fn set_session_status(
    data: &mut ReviewDashboardData,
    phase: ReviewPhase,
    summary: impl Into<String>,
    note: impl Into<String>,
) {
    if let Some(session) = data.sessions.first_mut() {
        session.phase = phase;
        session.summary = summary.into();
        session.updated_at_epoch_seconds = now_epoch_seconds();
    }
    push_note(data, note.into());
}

fn push_note(data: &mut ReviewDashboardData, note: impl Into<String>) {
    data.now_epoch_seconds = now_epoch_seconds();
    data.notes.insert(0, note.into());
    data.notes.truncate(6);
}

fn populate_session_metadata(session: &mut ReviewSession, pr: &GhPrMetadata) {
    session.pr_title = pr.title.clone();
    session.pr_url = Some(pr.url.clone());
    session.pr_author = Some(pr.author.login.clone());
    session.head_branch = Some(pr.head_ref_name.clone());
    session.base_branch = Some(pr.base_ref_name.clone());
    session.updated_at_epoch_seconds = now_epoch_seconds();
}

struct ReviewTerminalDashboard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    open: bool,
}

impl ReviewTerminalDashboard {
    /// Open the shared review dashboard terminal session.
    ///
    /// Returns an error when raw mode, alternate screen setup, or terminal construction fails.
    fn open() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            terminal,
            open: true,
        })
    }

    /// Draw the review dashboard with the provided data and browser state.
    ///
    /// Returns an error when the terminal backend cannot render the frame.
    fn draw_dashboard(
        &mut self,
        data: &ReviewDashboardData,
        state: &ReviewBrowserState,
    ) -> Result<()> {
        self.terminal.draw(|frame| render(frame, data, state))?;
        Ok(())
    }

    /// Draw a one-shot review frame using the default browser state.
    ///
    /// Returns an error when the terminal backend cannot render the frame.
    fn draw(&mut self, data: &ReviewDashboardData) -> Result<()> {
        self.draw_dashboard(data, &ReviewBrowserState::default())
    }

    /// Restore the normal terminal screen and cursor.
    ///
    /// Returns an error when screen restoration fails.
    fn close(&mut self) -> Result<()> {
        if !self.open {
            return Ok(());
        }
        disable_raw_mode()?;
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
        self.terminal.show_cursor()?;
        self.open = false;
        Ok(())
    }
}

impl Drop for ReviewTerminalDashboard {
    fn drop(&mut self) {
        if self.open {
            let _ = disable_raw_mode();
            let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
            let _ = self.terminal.show_cursor();
            self.open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_linear_identifier_from_branch() {
        assert_eq!(
            extract_linear_identifier("met-74-implement-review"),
            Some("MET-74".to_string())
        );
    }

    #[test]
    fn extract_linear_identifier_from_title() {
        assert_eq!(
            extract_linear_identifier("MET-74: Implement review command"),
            Some("MET-74".to_string())
        );
    }

    #[test]
    fn extract_linear_identifier_missing() {
        assert_eq!(extract_linear_identifier("fix the thing"), None);
    }

    #[test]
    fn resolve_linear_identifier_rejects_ambiguous_ticket_links() {
        let pr = GhPrMetadata {
            number: 42,
            title: "MET-74: Review flow".to_string(),
            url: "https://example.test/pull/42".to_string(),
            body: Some("Also references MET-99".to_string()),
            author: GhPrAuthor {
                login: "metasudo".to_string(),
            },
            head_ref_name: "met-74-review".to_string(),
            base_ref_name: "main".to_string(),
            changed_files: 1,
            additions: 1,
            deletions: 0,
            state: "OPEN".to_string(),
            labels: Vec::new(),
            review_decision: None,
        };

        let error = resolve_linear_identifier(&pr).expect_err("multiple tickets should fail");
        assert!(
            error
                .to_string()
                .contains("multiple Linear ticket identifiers")
        );
    }

    #[test]
    fn extract_markdown_section_returns_heading_body() {
        let body =
            "# Overview\nhello\n\n## Acceptance Criteria\n- first\n- second\n\n## Notes\nmore";
        assert_eq!(
            extract_markdown_section(body, &["Acceptance Criteria"]),
            Some("- first\n- second".to_string())
        );
    }

    #[test]
    fn remediation_detection_yes() {
        let output = "### Remediation Required\nYES\n\nSome explanation.";
        assert!(review_output_requires_remediation(output));
    }

    #[test]
    fn remediation_detection_no() {
        let output = "### Remediation Required\nNO\n\nAll good.";
        assert!(!review_output_requires_remediation(output));
    }

    #[test]
    fn remediation_detection_no_fixes() {
        let output =
            "### Required Fixes\nNo required fixes identified.\n\n### Remediation Required\nNO";
        assert!(!review_output_requires_remediation(output));
    }

    #[test]
    fn format_duration_displays_correctly() {
        assert_eq!(format_duration(30), "30s");
        assert_eq!(format_duration(120), "2m");
        assert_eq!(format_duration(7200), "2h");
        assert_eq!(format_duration(172800), "2d");
    }

    #[test]
    fn dashboard_data_renders_summary() {
        let data = ReviewDashboardData {
            scope: "test-repo".to_string(),
            cycle_summary: "1 eligible PR".to_string(),
            eligible_prs: 1,
            sessions: vec![],
            now_epoch_seconds: 1000,
            notes: vec![],
            state_file: "/tmp/test.json".to_string(),
        };
        let summary = data.render_summary();
        assert!(summary.contains("Review Dashboard: test-repo"));
        assert!(summary.contains("1 eligible PR"));
    }
}
