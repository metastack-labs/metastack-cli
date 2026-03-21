pub(crate) mod dashboard;
mod state;
pub(crate) mod store;

use std::io;
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
use crate::config::{AGENT_ROUTE_AGENTS_REVIEW, AppConfig, PlanningMeta};
use crate::context::{load_codebase_context_bundle, load_workflow_contract, render_repo_map};
use crate::fs::canonicalize_existing_dir;
use crate::github_pr::GhCli;

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
        run_review_one_shot(&args.run, pr_number)
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
        return print_dry_run_output(&config, &planning_meta, &root, &pr, &linear_identifier);
    }

    let diff = fetch_pr_diff(&root, pr_number)?;

    let context_bundle = load_codebase_context_bundle(&root).unwrap_or_default();
    let workflow_contract = load_workflow_contract(&root).unwrap_or_default();
    let repo_map = render_repo_map(&root).unwrap_or_default();
    let ticket_context = gather_linear_ticket_context(&linear_identifier);

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
        run_remediation(
            &root,
            &pr,
            &linear_identifier,
            &review_output,
            &config,
            &planning_meta,
            args,
        )?;
    } else {
        eprintln!("No remediation required for PR #{pr_number}.");
        println!("{review_output}");
    }

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
    let body_text = pr.body.clone().unwrap_or_default();
    let candidates = [&pr.title as &str, &pr.head_ref_name, &body_text];
    for candidate in &candidates {
        if let Some(identifier) = extract_linear_identifier(candidate) {
            return Ok(identifier);
        }
    }
    bail!(
        "no Linear ticket identifier found in PR #{} title, branch, or body. \
         Expected a pattern like `MET-42` or `ENG-1234`.",
        pr.number
    );
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

fn gather_linear_ticket_context(identifier: &str) -> String {
    format!(
        "Linked Linear ticket: {identifier}\n\
         (Full ticket context should be gathered from Linear API when available.)"
    )
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
) -> Result<()> {
    let invocation = resolve_agent_invocation_for_planning(
        config,
        planning_meta,
        &RunAgentArgs {
            root: Some(root.to_path_buf()),
            route_key: Some(AGENT_ROUTE_AGENTS_REVIEW.to_string()),
            agent: None,
            prompt: "(dry-run preview)".to_string(),
            instructions: None,
            model: None,
            reasoning: None,
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

fn run_remediation(
    root: &Path,
    pr: &GhPrMetadata,
    linear_identifier: &str,
    review_output: &str,
    _config: &AppConfig,
    _planning_meta: &PlanningMeta,
    args: &ReviewRunArgs,
) -> Result<()> {
    let gh = GhCli;
    let remediation_branch = format!("review/remediation-pr-{}", pr.number);
    let base_branch = &pr.head_ref_name;

    run_git(root, &["fetch", "origin", base_branch])?;
    run_git(
        root,
        &[
            "checkout",
            "-b",
            &remediation_branch,
            &format!("origin/{base_branch}"),
        ],
    )?;

    let fix_prompt = format!(
        "You are applying required fixes from a code review to this branch.\n\n\
         ## Review Output\n{review_output}\n\n\
         ## Instructions\n\
         Apply ONLY the required fixes identified in the review above. Do not apply optional recommendations.\n\
         Make minimal, targeted changes. Commit each logical fix separately with clear commit messages.\n\
         After applying all fixes, verify the changes compile and pass basic checks.\n"
    );

    let fix_args = RunAgentArgs {
        root: Some(root.to_path_buf()),
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

    run_git(root, &["push", "-u", "origin", &remediation_branch]).map_err(|e| {
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
    let body_path = root.join(".metastack").join("review-pr-body.md");
    crate::fs::ensure_dir(&root.join(".metastack"))?;
    std::fs::write(&body_path, &pr_body).context("failed to write remediation PR body")?;

    let result = gh.publish_branch_pull_request(
        root,
        crate::github_pr::PullRequestPublishRequest {
            head_branch: &remediation_branch,
            base_branch,
            title: &pr_title,
            body_path: &body_path,
            mode: crate::github_pr::PullRequestPublishMode::Ready,
        },
    )?;

    let _ = gh.ensure_label_exists(root, METASTACK_LABEL, "5319E7", "MetaStack managed PR");
    let _ = gh.add_label_to_pull_request(root, result.number, METASTACK_LABEL);

    eprintln!("Remediation PR #{} created: {}", result.number, result.url);

    if let Err(e) = post_linear_remediation_comment(linear_identifier, &result.url, pr.number) {
        eprintln!("warning: failed to post Linear comment for {linear_identifier}: {e}");
    }

    let _ = run_git(root, &["checkout", &pr.base_ref_name]);
    let _ = std::fs::remove_file(&body_path);

    println!("{review_output}");
    println!(
        "\nRemediation PR #{} opened against `{base_branch}`: {}",
        result.number, result.url
    );

    Ok(())
}

fn post_linear_remediation_comment(
    linear_identifier: &str,
    remediation_pr_url: &str,
    original_pr_number: u64,
) -> Result<()> {
    eprintln!(
        "note: Linear comment for {linear_identifier} remediation of PR #{original_pr_number} -> \
         {remediation_pr_url} would be posted here when Linear API is available in-session."
    );
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

// ---------------------------------------------------------------------------
// Listener mode
// ---------------------------------------------------------------------------

async fn run_review_listener(args: &ReviewRunArgs) -> Result<()> {
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

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut browser_state = ReviewBrowserState::default();
    let mut last_poll = Instant::now() - Duration::from_secs(DEFAULT_POLL_INTERVAL_SECONDS + 1);
    let mut last_render =
        Instant::now() - Duration::from_secs(TERMINAL_REFRESH_INTERVAL_SECONDS + 1);
    let mut latest_data: Option<ReviewDashboardData> = None;

    loop {
        if last_poll.elapsed() >= Duration::from_secs(DEFAULT_POLL_INTERVAL_SECONDS) {
            match run_single_review_cycle(root, store, args) {
                Ok(data) => {
                    latest_data = Some(data);
                    last_poll = Instant::now();
                }
                Err(e) => {
                    eprintln!("poll error: {e}");
                }
            }
        }

        if last_render.elapsed() >= Duration::from_secs(TERMINAL_REFRESH_INTERVAL_SECONDS) {
            if let Some(ref data) = latest_data {
                terminal.draw(|frame| render(frame, data, &browser_state))?;
                last_render = Instant::now();
            }
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
                        if let Some(ref data) = latest_data {
                            browser_state.apply_action(ReviewBrowserAction::Up, data);
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if let Some(ref data) = latest_data {
                            browser_state.apply_action(ReviewBrowserAction::Down, data);
                        }
                    }
                    KeyCode::Tab => {
                        if let Some(ref data) = latest_data {
                            browser_state.apply_action(ReviewBrowserAction::Tab, data);
                        }
                    }
                    KeyCode::PageUp => {
                        if let Some(ref data) = latest_data {
                            browser_state.apply_action(ReviewBrowserAction::PageUp, data);
                        }
                    }
                    KeyCode::PageDown => {
                        if let Some(ref data) = latest_data {
                            browser_state.apply_action(ReviewBrowserAction::PageDown, data);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

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
        session.summary = "Starting review".to_string();
        session.updated_at_epoch_seconds = now;

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
    let linear_id = resolve_linear_identifier(&pr).ok();

    let diff = fetch_pr_diff(root, pr_number)?;
    let context_bundle = load_codebase_context_bundle(root).unwrap_or_default();
    let workflow_contract = load_workflow_contract(root).unwrap_or_default();
    let repo_map = render_repo_map(root).unwrap_or_default();
    let ticket_context = linear_id
        .as_deref()
        .map(gather_linear_ticket_context)
        .unwrap_or_else(|| "No linked Linear ticket found.".to_string());

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
        if let Some(ref linear_id) = linear_id {
            match run_remediation(
                root,
                &pr,
                linear_id,
                &review_output,
                &config,
                &planning_meta,
                args,
            ) {
                Ok(()) => {
                    result.summary = "Remediation PR created".to_string();
                }
                Err(e) => {
                    result.summary = format!("Remediation failed: {e}");
                }
            }
        } else {
            result.summary = "Remediation required but no Linear ticket linked".to_string();
        }
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
