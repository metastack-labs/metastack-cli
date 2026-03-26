pub(crate) mod dashboard;
mod state;
pub(crate) mod store;

use std::collections::BTreeSet;
use std::io;
use std::io::IsTerminal;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, ListItem, ListState, Padding, Wrap};

use crate::tui::markdown::render_markdown;
use crate::tui::spaced_list::{render_github_session_row, spaced_list, spaced_list_item};
use serde::Serialize;

use crate::agents::{
    render_invocation_diagnostics, resolve_agent_invocation_for_planning, run_agent_capture,
};
use crate::backlog_defaults::{
    PlanTicketResolutionInput, TicketOptionOverrides, load_remembered_backlog_selection,
    resolve_plan_ticket_defaults, save_remembered_backlog_selection,
};
use crate::cli::{RetroArgs, ReviewArgs, ReviewDashboardEventArg, ReviewRunArgs, RunAgentArgs};
use crate::config::{
    AGENT_ROUTE_AGENTS_REVIEW, AppConfig, LinearConfig, LinearConfigOverrides, PlanningMeta,
};
use crate::context::{load_codebase_context_bundle, load_workflow_contract, render_repo_map};
use crate::fs::{
    canonicalize_existing_dir, ensure_dir, ensure_workspace_path_is_safe, sibling_workspace_root,
};
use crate::github_pr::GhCli;
use crate::linear::{
    IssueComment, IssueCreateSpec, IssueSummary, LinearService, ReqwestLinearClient,
};
use crate::progress::render_loading_panel;
use crate::tui::fields::InputFieldState;
use crate::tui::scroll::{ScrollState, scrollable_content_paragraph, wrapped_rows};
use crate::tui::theme::{
    Tone, badge, emphasis_style, empty_state, key_hints, label_style, list, muted_style,
    panel_title, paragraph,
};

use dashboard::{
    ReviewBrowserAction, ReviewBrowserState, ReviewListView, render,
    render_review_dashboard_snapshot,
};
use state::{ReviewPhase, ReviewSession};
use store::ReviewProjectStore;

const REVIEW_INSTRUCTIONS: &str = include_str!(concat!(env!("OUT_DIR"), "/artifacts/REVIEW.md"));
const VIEW_LINEAR_INSTRUCTIONS: &str =
    include_str!(concat!(env!("OUT_DIR"), "/artifacts/VIEW_LINEAR.md"));
const METASTACK_LABEL: &str = "metastack";
const INPUT_POLL_INTERVAL_MILLIS: u64 = 100;
const TERMINAL_REFRESH_INTERVAL_SECONDS: u64 = 1;
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 60;
const REMEDIATION_RETRY_DELAY_SECONDS: u64 = 2;

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
    #[serde(default)]
    assignees: Vec<GhPrAuthor>,
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

#[derive(Debug, Clone, serde::Deserialize)]
struct GhRepoView {
    url: String,
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

#[derive(Debug, Clone)]
struct ReviewLaunchCandidate {
    pr_number: u64,
    title: String,
    url: String,
    author: String,
    head_ref: String,
    base_ref: String,
    review_state: String,
    changed_files: u64,
    additions: u64,
    deletions: u64,
    linear_identifier: Option<String>,
    linear_error: Option<String>,
    /// Normalized PR state: `"open"` or `"closed"` (GitHub MERGED maps to `"closed"`).
    candidate_state: String,
    /// Label names attached to the PR.
    candidate_labels: Vec<String>,
    /// Assignee logins, empty when unassigned.
    candidate_assignees: Vec<String>,
}

#[derive(Debug, Clone)]
struct InteractiveReviewOutcome {
    kind: InteractiveSessionKind,
    candidate: ReviewLaunchCandidate,
    summary: String,
    review_output: String,
    follow_up_ticket_set: Option<FollowUpTicketSet>,
    remediation_required: bool,
    linear_identifier: Option<String>,
    remediation_pr_number: Option<u64>,
    remediation_pr_url: Option<String>,
}

#[derive(Debug, Clone)]
enum InteractiveReviewDialog {
    LaunchReviews(Vec<ReviewLaunchCandidate>),
    LaunchFollowUpTickets(Vec<ReviewLaunchCandidate>),
    StartRemediation(u64),
    SkipRemediation(u64),
    DeleteSession(u64, InteractiveSessionKind),
    CancelSession(u64, InteractiveSessionKind),
}

#[derive(Debug, Clone)]
enum InteractiveReviewAction {
    LaunchReviews(Vec<ReviewLaunchCandidate>),
    LaunchFollowUpTickets(Vec<ReviewLaunchCandidate>),
    StartRemediation(u64),
    SkipRemediation(u64),
    DeleteSession(u64, InteractiveSessionKind),
    CancelSession(u64, InteractiveSessionKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveSessionKind {
    Review,
    FollowUpTickets,
}

impl InteractiveSessionKind {
    fn label(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::FollowUpTickets => "linear ideas",
        }
    }

    fn tone(self) -> Tone {
        match self {
            Self::Review => Tone::Accent,
            Self::FollowUpTickets => Tone::Info,
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::FollowUpTickets => "ticket analysis",
        }
    }

    fn title_label(self) -> &'static str {
        match self {
            Self::Review => "Review",
            Self::FollowUpTickets => "Ticket analysis",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveReviewStage {
    Loading,
    Select,
    Confirm,
    TicketReview,
    TicketLoading,
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveReviewMode {
    Direct,
    Discovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveReviewTab {
    Candidates,
    Sessions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveReviewFocus {
    CandidateList,
    CandidatePreview,
    SessionList,
    SessionPreview,
}

#[derive(Debug, Clone)]
struct InteractiveReviewSession {
    kind: InteractiveSessionKind,
    candidate: ReviewLaunchCandidate,
    phase: ReviewPhase,
    summary: String,
    notes: Vec<String>,
    review_output: Option<String>,
    follow_up_ticket_set: Option<FollowUpTicketSet>,
    created_follow_up_issues: Vec<IssueSummary>,
    remediation_required: Option<bool>,
    remediation_pr_number: Option<u64>,
    remediation_pr_url: Option<String>,
    remediation_declined: bool,
    cancel_requested: bool,
    error: Option<String>,
    updated_at_epoch_seconds: u64,
}

#[derive(Debug, Clone)]
struct InteractiveReviewApp {
    command: ReviewCommandKind,
    mode: InteractiveReviewMode,
    stage: InteractiveReviewStage,
    tab: InteractiveReviewTab,
    focus: InteractiveReviewFocus,
    query: InputFieldState,
    candidates: Vec<ReviewLaunchCandidate>,
    selected_index: usize,
    session_index: usize,
    selected_prs: BTreeSet<u64>,
    sessions: Vec<InteractiveReviewSession>,
    preview_scroll: ScrollState,
    session_preview_scroll: ScrollState,
    status: String,
    notes: Vec<String>,
    error: Option<String>,
    refresh_requested: bool,
    dialog: Option<InteractiveReviewDialog>,
    ticket_review: Option<FollowUpTicketReviewApp>,
    /// In-place candidate filter (retro flow only).
    filter: CandidateFilter,
    /// Whether the lightweight filter panel overlay is visible.
    filter_panel_open: bool,
    /// Rows displayed inside the filter panel.
    filter_panel_rows: Vec<FilterPanelRow>,
    /// Cursor position within `filter_panel_rows`.
    filter_panel_cursor: usize,
}

#[derive(Debug)]
struct InteractiveWorkerHandle {
    kind: InteractiveSessionKind,
    pr_number: u64,
    receiver: Receiver<ReviewExecutionEvent>,
    cancel: Arc<AtomicBool>,
}

struct ReviewExecutionContext<'a> {
    root: &'a Path,
    config: &'a AppConfig,
    planning_meta: &'a PlanningMeta,
    args: &'a ReviewRunArgs,
    store: Option<&'a ReviewProjectStore>,
    cancel: &'a AtomicBool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewCommandKind {
    Review,
    Retro,
}

impl ReviewCommandKind {
    fn command_name(self) -> String {
        match self {
            Self::Review => format!("{} agents review", crate::branding::COMMAND_NAME),
            Self::Retro => format!("{} agents retro", crate::branding::COMMAND_NAME),
        }
    }

    fn dashboard_title(self) -> String {
        match self {
            Self::Review => format!("{} agents review", crate::branding::COMMAND_NAME),
            Self::Retro => format!("{} agents retro", crate::branding::COMMAND_NAME),
        }
    }
}

// ---------------------------------------------------------------------------
// Candidate filter model
// ---------------------------------------------------------------------------

const UNASSIGNED_FILTER_VALUE: &str = "(unassigned)";

/// Filter state for narrowing the retro candidate list in-place.
///
/// All categories combine conjunctively: a candidate must satisfy every active
/// category to remain visible.  Within a single category the semantics vary:
/// - **state**: candidate state must be in the selected set.
/// - **author**: candidate author must be in the selected set.
/// - **labels**: candidate must contain *all* selected labels (AND).
/// - **assignees**: candidate must match *any* selected assignee (OR), with an
///   explicit `(unassigned)` option for PRs that have no assignees.
#[derive(Debug, Clone, Default)]
struct CandidateFilter {
    states: BTreeSet<String>,
    authors: BTreeSet<String>,
    labels: BTreeSet<String>,
    assignees: BTreeSet<String>,
}

impl CandidateFilter {
    /// Returns `true` when at least one filter category is constraining results.
    fn is_active(&self) -> bool {
        !self.states.is_empty()
            || !self.authors.is_empty()
            || !self.labels.is_empty()
            || !self.assignees.is_empty()
    }

    /// Test whether `candidate` passes every active filter category.
    fn matches(&self, candidate: &ReviewLaunchCandidate) -> bool {
        if !self.states.is_empty() && !self.states.contains(&candidate.candidate_state) {
            return false;
        }
        if !self.authors.is_empty() && !self.authors.contains(&candidate.author) {
            return false;
        }
        // Labels: candidate must have ALL selected labels.
        if !self.labels.is_empty()
            && !self
                .labels
                .iter()
                .all(|label| candidate.candidate_labels.contains(label))
        {
            return false;
        }
        // Assignees: candidate must match ANY selected assignee.
        if !self.assignees.is_empty() {
            let matched = if candidate.candidate_assignees.is_empty() {
                self.assignees.contains(UNASSIGNED_FILTER_VALUE)
            } else {
                candidate
                    .candidate_assignees
                    .iter()
                    .any(|a| self.assignees.contains(a))
            };
            if !matched {
                return false;
            }
        }
        true
    }

    /// Reset all filters to the unfiltered state.
    fn clear(&mut self) {
        self.states.clear();
        self.authors.clear();
        self.labels.clear();
        self.assignees.clear();
    }

    /// Compact human-readable summary of active filters for the dashboard chrome.
    fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.states.is_empty() {
            parts.push(format!(
                "state={}",
                self.states.iter().cloned().collect::<Vec<_>>().join(",")
            ));
        }
        if !self.authors.is_empty() {
            parts.push(format!(
                "author={}",
                self.authors.iter().cloned().collect::<Vec<_>>().join(",")
            ));
        }
        if !self.labels.is_empty() {
            parts.push(format!(
                "labels={}",
                self.labels.iter().cloned().collect::<Vec<_>>().join(",")
            ));
        }
        if !self.assignees.is_empty() {
            parts.push(format!(
                "assignee={}",
                self.assignees.iter().cloned().collect::<Vec<_>>().join(",")
            ));
        }
        parts.join("  ")
    }
}

/// One selectable row inside the filter panel overlay.
#[derive(Debug, Clone)]
struct FilterPanelRow {
    category: FilterCategory,
    value: String,
    selected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterCategory {
    State,
    Author,
    Label,
    Assignee,
}

impl FilterCategory {
    fn label(self) -> &'static str {
        match self {
            Self::State => "State",
            Self::Author => "Author",
            Self::Label => "Labels",
            Self::Assignee => "Assignees",
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct FollowUpTicketDraft {
    title: String,
    #[serde(default)]
    why_now: String,
    #[serde(default)]
    outcome: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    acceptance_criteria: Vec<String>,
    #[serde(default)]
    priority: Option<u8>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct FollowUpTicketSet {
    summary: String,
    #[serde(default)]
    tickets: Vec<FollowUpTicketDraft>,
    #[serde(default)]
    notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FollowUpTicketReviewFocus {
    Tickets,
    SelectedTicket,
    Overview,
    CombinationPlan,
}

#[derive(Debug, Clone)]
struct FollowUpTicketReviewApp {
    pr_number: u64,
    candidate: ReviewLaunchCandidate,
    plan: FollowUpTicketSet,
    selected: usize,
    decisions: Vec<usize>,
    revision: usize,
    focus: FollowUpTicketReviewFocus,
    overview_scroll: ScrollState,
    selected_ticket_scroll: ScrollState,
    combination_scroll: ScrollState,
    error: Option<String>,
}

struct PendingFollowUpTicketJob {
    receiver: Receiver<FollowUpTicketJobEvent>,
}

enum FollowUpTicketJobEvent {
    RevisionReady(Box<FollowUpTicketReviewApp>),
    Created {
        pr_number: u64,
        issues: Vec<IssueSummary>,
    },
    Failed {
        pr_number: u64,
        error: String,
    },
}

#[derive(Debug, Clone)]
struct RemediationLaunchRequest {
    candidate: ReviewLaunchCandidate,
    linear_identifier: String,
    review_output: String,
}

#[derive(Debug, Clone)]
enum ReviewExecutionEvent {
    Progress {
        kind: InteractiveSessionKind,
        candidate: ReviewLaunchCandidate,
        phase: ReviewPhase,
        summary: String,
        note: Option<String>,
        remediation_required: Option<bool>,
    },
    Completed(InteractiveReviewOutcome),
    Cancelled {
        kind: InteractiveSessionKind,
        candidate: ReviewLaunchCandidate,
        summary: String,
        note: String,
        review_output: Option<String>,
        remediation_required: Option<bool>,
    },
    Failed {
        kind: InteractiveSessionKind,
        candidate: ReviewLaunchCandidate,
        error: String,
    },
}

/// Run the unified `meta agents review` command.
///
/// Dispatches between one-shot PR review (when `pr_number` is provided) and
/// listener/dashboard mode (when `pr_number` is omitted).
///
/// Returns an error when prerequisite checks fail, agent execution fails,
/// or required external tools are unavailable.
pub(crate) async fn run_review(args: &ReviewArgs) -> Result<()> {
    run_review_command(ReviewCommandKind::Review, args.pr_number, &args.run).await
}

/// Run the unified `meta agents retro` command.
///
/// Dispatches between one-shot PR retro analysis and the interactive retro dashboard.
pub(crate) async fn run_retro(args: &RetroArgs) -> Result<()> {
    run_review_command(ReviewCommandKind::Retro, args.pr_number, &args.run).await
}

async fn run_review_command(
    command: ReviewCommandKind,
    pr_number: Option<u64>,
    args: &ReviewRunArgs,
) -> Result<()> {
    if let Some(target_pr) = args.fix_pr {
        return run_fix_pr(args, target_pr);
    }
    if let Some(target_pr) = args.skip_pr {
        return run_skip_pr(args, target_pr);
    }
    if let Some(pr_number) = pr_number {
        if should_launch_interactive_review_dashboard(args) {
            run_review_interactive(args, Some(pr_number), command)
        } else {
            match command {
                ReviewCommandKind::Review => run_review_one_shot(args, pr_number),
                ReviewCommandKind::Retro => run_retro_one_shot(args, pr_number),
            }
        }
    } else if should_launch_interactive_review_dashboard(args) {
        run_review_interactive(args, None, command)
    } else {
        match command {
            ReviewCommandKind::Review => run_review_listener(args).await,
            ReviewCommandKind::Retro => bail!(
                "the interactive retro dashboard requires a TTY; rerun `{}` in a terminal with an optional PR number",
                command.command_name()
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// One-shot PR review
// ---------------------------------------------------------------------------

fn run_review_interactive(
    args: &ReviewRunArgs,
    pr_number: Option<u64>,
    command: ReviewCommandKind,
) -> Result<()> {
    let root = canonicalize_existing_dir(&args.root)?;
    let config = AppConfig::load()?;
    let planning_meta = crate::config::load_required_planning_meta(&root, &command.command_name())?;
    let gh = GhCli;
    let store = ReviewProjectStore::resolve(&root).ok();
    if let Some(ref store) = store {
        reset_review_store(store)?;
    }
    let mode = if pr_number.is_some() {
        InteractiveReviewMode::Direct
    } else {
        InteractiveReviewMode::Discovery
    };

    let mut terminal = ReviewTerminalDashboard::open()?;
    let mut app = InteractiveReviewApp::new(mode, command);
    app.set_loading(
        match command {
            ReviewCommandKind::Review => "Preparing review dashboard".to_string(),
            ReviewCommandKind::Retro => "Preparing retro dashboard".to_string(),
        },
        match command {
            ReviewCommandKind::Review => {
                "Opening the review workflow and resolving prerequisites.".to_string()
            }
            ReviewCommandKind::Retro => {
                "Opening the retro workflow and resolving prerequisites.".to_string()
            }
        },
    );
    terminal.draw_interactive(&app)?;

    app.set_loading(
        "Verifying GitHub authentication".to_string(),
        "Checking `gh auth status` before discovery begins.".to_string(),
    );
    terminal.draw_interactive(&app)?;
    verify_gh_auth(&root).inspect_err(|error| {
        app.fail(error.to_string());
        let _ = terminal.draw_interactive(&app);
    })?;

    app.set_loading(
        "Discovering review candidates".to_string(),
        match pr_number {
            Some(number) => match command {
                ReviewCommandKind::Review => {
                    format!("Loading PR #{number} and linked review context.")
                }
                ReviewCommandKind::Retro => {
                    format!("Loading PR #{number} and linked retro context.")
                }
            },
            None => match command {
                ReviewCommandKind::Review => "Finding open `metastack` pull requests that are ready for explicit review approval."
                    .to_string(),
                ReviewCommandKind::Retro => "Finding `metastack` pull requests for retro ticket analysis."
                    .to_string(),
            },
        },
    );
    terminal.draw_interactive(&app)?;

    let candidates =
        discover_review_candidates(&root, &gh, pr_number, command, &mut app, &mut terminal)?;
    app.load_candidates(candidates);

    // Restore sessions from persistent state on dashboard re-entry.
    if let Some(ref store) = store {
        if let Ok(state) = store.load_state() {
            app.restore_from_persistent_state(&state.sorted_sessions());
        }
    }
    terminal.draw_interactive(&app)?;

    let mut worker_rxs: Vec<InteractiveWorkerHandle> = Vec::new();
    let mut pending_ticket_job: Option<PendingFollowUpTicketJob> = None;
    let mut next_pulse_at = Instant::now() + Duration::from_millis(150);

    loop {
        if app.refresh_requested {
            let candidates = discover_review_candidates(
                &root,
                &gh,
                pr_number,
                command,
                &mut app,
                &mut terminal,
            )?;
            app.refresh_candidates(candidates);
            terminal.draw_interactive(&app)?;
        }

        if next_pulse_at <= Instant::now() {
            app.tick();
            terminal.draw_interactive(&app)?;
            next_pulse_at = Instant::now() + Duration::from_millis(150);
        }

        let mut remove_indices = Vec::new();
        for (index, handle) in worker_rxs.iter().enumerate() {
            match handle.receiver.try_recv() {
                Ok(event) => {
                    let finished = matches!(
                        event,
                        ReviewExecutionEvent::Completed(_)
                            | ReviewExecutionEvent::Failed { .. }
                            | ReviewExecutionEvent::Cancelled { .. }
                    );
                    app.apply_worker_event(event);
                    terminal.draw_interactive(&app)?;
                    if finished {
                        remove_indices.push(index);
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    app.fail(format!(
                        "review worker for PR #{} disconnected unexpectedly",
                        handle.pr_number
                    ));
                    remove_indices.push(index);
                    terminal.draw_interactive(&app)?;
                }
            }
        }
        for index in remove_indices.into_iter().rev() {
            worker_rxs.remove(index);
        }

        if let Some(job) = &pending_ticket_job {
            match job.receiver.try_recv() {
                Ok(event) => {
                    app.apply_follow_up_ticket_job_event(event);
                    pending_ticket_job = None;
                    terminal.draw_interactive(&app)?;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    app.fail("follow-up ticket workflow disconnected unexpectedly".to_string());
                    pending_ticket_job = None;
                    terminal.draw_interactive(&app)?;
                }
            }
        }

        if !event::poll(Duration::from_millis(INPUT_POLL_INTERVAL_MILLIS))? {
            continue;
        }

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let preview = interactive_preview_viewport(terminal.size()?);
                let stage_before_key = app.stage;
                if let Some(action) = app.handle_key(key, preview)? {
                    match action {
                        InteractiveReviewAction::LaunchReviews(selection) => {
                            for candidate in &selection {
                                let handle = spawn_review_execution(
                                    root.clone(),
                                    config.clone(),
                                    planning_meta.clone(),
                                    args.clone(),
                                    candidate.clone(),
                                    store.clone(),
                                );
                                worker_rxs.push(handle);
                            }
                            app.begin_running(&selection);
                        }
                        InteractiveReviewAction::LaunchFollowUpTickets(selection) => {
                            for candidate in &selection {
                                let handle = spawn_follow_up_ticket_execution(
                                    root.clone(),
                                    config.clone(),
                                    planning_meta.clone(),
                                    args.clone(),
                                    candidate.clone(),
                                    store.clone(),
                                );
                                worker_rxs.push(handle);
                            }
                            app.begin_running_with_kind(
                                &selection,
                                InteractiveSessionKind::FollowUpTickets,
                            );
                        }
                        InteractiveReviewAction::StartRemediation(pr_number) => {
                            if let Some(session) = app
                                .sessions
                                .iter()
                                .find(|session| {
                                    session.candidate.pr_number == pr_number
                                        && session.kind == InteractiveSessionKind::Review
                                })
                                .cloned()
                                && let Some(review_output) = session.review_output.clone()
                                && let Some(linear_identifier) =
                                    session.candidate.linear_identifier.clone()
                            {
                                let handle = spawn_remediation_execution(
                                    root.clone(),
                                    config.clone(),
                                    planning_meta.clone(),
                                    args.clone(),
                                    RemediationLaunchRequest {
                                        candidate: session.candidate,
                                        linear_identifier,
                                        review_output,
                                    },
                                    store.clone(),
                                );
                                worker_rxs.push(handle);
                                app.status =
                                    format!("Starting remediation workflow for PR #{pr_number}.");
                            }
                        }
                        InteractiveReviewAction::SkipRemediation(pr_number) => {
                            if let Some(session) = app.sessions.iter_mut().find(|session| {
                                session.candidate.pr_number == pr_number
                                    && session.kind == InteractiveSessionKind::Review
                            }) {
                                session.remediation_declined = true;
                                session.phase = ReviewPhase::Skipped;
                                session.summary =
                                    "Recommendation kept without remediation PR".to_string();
                                session.push_note(
                                    "User kept the review report without opening a remediation PR."
                                        .to_string(),
                                );
                                app.status = format!(
                                    "Kept report for PR #{} without creating remediation.",
                                    pr_number
                                );
                                if let Some(ref store) = store {
                                    let persisted = ReviewSession {
                                        pr_number: session.candidate.pr_number,
                                        pr_title: session.candidate.title.clone(),
                                        pr_url: Some(session.candidate.url.clone()),
                                        pr_author: Some(session.candidate.author.clone()),
                                        head_branch: Some(session.candidate.head_ref.clone()),
                                        base_branch: Some(session.candidate.base_ref.clone()),
                                        linear_identifier: session
                                            .candidate
                                            .linear_identifier
                                            .clone(),
                                        phase: session.phase,
                                        summary: session.summary.clone(),
                                        updated_at_epoch_seconds: session.updated_at_epoch_seconds,
                                        review_output: session.review_output.clone(),
                                        remediation_required: session.remediation_required,
                                        remediation_pr_number: session.remediation_pr_number,
                                        remediation_pr_url: session.remediation_pr_url.clone(),
                                    };
                                    persist_review_session(Some(store), &persisted)?;
                                }
                            }
                        }
                        InteractiveReviewAction::DeleteSession(pr_number, kind) => {
                            app.delete_session(pr_number, kind);
                            if kind == InteractiveSessionKind::Review
                                && let Some(ref store) = store
                            {
                                let _ = store.delete_session(pr_number)?;
                            }
                        }
                        InteractiveReviewAction::CancelSession(pr_number, kind) => {
                            if let Some(handle) = worker_rxs
                                .iter()
                                .find(|handle| handle.pr_number == pr_number && handle.kind == kind)
                            {
                                handle.cancel.store(true, Ordering::Relaxed);
                            }
                            if let Some(session) = app.sessions.iter_mut().find(|session| {
                                session.candidate.pr_number == pr_number && session.kind == kind
                            }) {
                                session.cancel_requested = true;
                                session.phase = ReviewPhase::Blocked;
                                session.summary = format!("{} cancellation requested", kind.noun());
                                session.push_note(format!(
                                    "User requested cancellation for this {} session.",
                                    kind.noun()
                                ));
                                app.status = format!(
                                    "Cancellation requested for PR #{} {}. The session will stop at the next checkpoint.",
                                    pr_number,
                                    kind.label()
                                );
                            }
                        }
                    }
                    terminal.draw_interactive(&app)?;
                } else if stage_before_key != InteractiveReviewStage::TicketReview
                    && app.stage == InteractiveReviewStage::TicketReview
                {
                    terminal.draw_interactive(&app)?;
                } else if app.stage == InteractiveReviewStage::TicketReview {
                    if let Some(review) = app.ticket_review.as_mut() {
                        match handle_follow_up_ticket_review_key(review, key, terminal.size()?) {
                            FollowUpTicketReviewAction::None => {}
                            FollowUpTicketReviewAction::Close => {
                                app.stage = InteractiveReviewStage::Select;
                                app.ticket_review = None;
                                app.status = format!(
                                    "Returned to sessions. {} active review session(s) still running.",
                                    app.active_session_count()
                                );
                            }
                            FollowUpTicketReviewAction::OpenCreate => {
                                if let Some(review) = app.ticket_review.clone() {
                                    app.stage = InteractiveReviewStage::TicketLoading;
                                    app.status = format!(
                                        "Creating {} follow-up Linear ticket(s) for PR #{}.",
                                        follow_up_ticket_kept_indices(&review).len(),
                                        review.pr_number
                                    );
                                    pending_ticket_job = Some(PendingFollowUpTicketJob {
                                        receiver: spawn_follow_up_ticket_create_job(
                                            root.clone(),
                                            config.clone(),
                                            planning_meta.clone(),
                                            review,
                                        ),
                                    });
                                }
                            }
                            FollowUpTicketReviewAction::OpenRevision => {
                                if let Some(review) = app.ticket_review.clone() {
                                    app.stage = InteractiveReviewStage::TicketLoading;
                                    app.status = format!(
                                        "Rebuilding follow-up ticket preview for PR #{}.",
                                        review.pr_number
                                    );
                                    pending_ticket_job = Some(PendingFollowUpTicketJob {
                                        receiver: spawn_follow_up_ticket_revision_job(
                                            root.clone(),
                                            args.clone(),
                                            review,
                                        ),
                                    });
                                }
                            }
                        }
                    }
                    terminal.draw_interactive(&app)?;
                } else if app.should_exit(key.code) {
                    break;
                }
            }
            Event::Mouse(mouse)
                if matches!(
                    mouse.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                ) =>
            {
                if app.stage == InteractiveReviewStage::TicketReview {
                    if let Some(review) = app.ticket_review.as_mut() {
                        let _ =
                            handle_follow_up_ticket_review_mouse(review, mouse, terminal.size()?);
                    }
                } else {
                    let viewport = interactive_preview_viewport(terminal.size()?);
                    let _ = app.handle_preview_mouse(mouse, viewport);
                }
            }
            _ => {}
        }
    }

    terminal.close()?;
    if let Some(pr_number) = pr_number {
        let review_session = app.sessions.iter().find(|session| {
            session.candidate.pr_number == pr_number
                && session.kind == InteractiveSessionKind::Review
        });
        let fallback_session = app
            .sessions
            .iter()
            .find(|session| session.candidate.pr_number == pr_number);
        if let Some(session) = review_session.or(fallback_session)
            && let Some(output) = session.review_output.as_deref()
        {
            println!("{output}");
            if let Some(pr_url) = session.remediation_pr_url.as_deref() {
                println!(
                    "\nRemediation PR #{} opened: {}",
                    session.remediation_pr_number.unwrap_or_default(),
                    pr_url
                );
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Non-interactive remediation dispatch (--fix-pr / --skip-pr)
// ---------------------------------------------------------------------------

/// Dispatch the fix-agent for a previously reviewed PR without an interactive
/// TUI session.
///
/// Loads the persistent review store, validates that the PR has a completed
/// review awaiting a remediation decision, and runs the remediation pipeline
/// synchronously. The session state is updated through
/// `FixAgentPending -> FixAgentInProgress -> FixAgentComplete/FixAgentFailed`.
///
/// Returns an error when the PR has no eligible session, when the agent fails,
/// or when the resulting PR cannot be published.
fn run_fix_pr(args: &ReviewRunArgs, pr_number: u64) -> Result<()> {
    let root = canonicalize_existing_dir(&args.root)?;
    let config = AppConfig::load()?;
    let planning_meta = crate::config::load_required_planning_meta(
        &root,
        &format!("{} agents review", crate::branding::COMMAND_NAME),
    )?;
    let gh = GhCli;
    let store = ReviewProjectStore::resolve(&root)?;
    let state = store.load_state()?;

    let persistent_session = state
        .find_session(pr_number)
        .ok_or_else(|| anyhow!("no review session found for PR #{pr_number}"))?;

    let eligible = persistent_session.needs_remediation_decision()
        || (persistent_session.phase == ReviewPhase::Completed
            && persistent_session.remediation_required == Some(true)
            && persistent_session.remediation_pr_url.is_none());

    if !eligible {
        bail!(
            "PR #{pr_number} is not eligible for fix-agent dispatch (phase: {}, remediation_required: {:?})",
            persistent_session.phase.display_label(),
            persistent_session.remediation_required
        );
    }

    let linear_identifier = persistent_session
        .linear_identifier
        .clone()
        .ok_or_else(|| anyhow!("no Linear identifier found for PR #{pr_number}"))?;

    verify_gh_auth(&root)?;

    // Validate and transition to FixAgentPending.
    if !persistent_session
        .phase
        .can_transition_to(ReviewPhase::FixAgentPending)
    {
        bail!(
            "invalid state transition for PR #{pr_number}: {} -> Fix Agent Pending",
            persistent_session.phase.display_label()
        );
    }
    let mut session = persistent_session.clone();
    session.phase = ReviewPhase::FixAgentPending;
    session.summary = format!("Fix agent pending for PR #{pr_number}");
    session.updated_at_epoch_seconds = now_epoch_seconds();
    persist_review_session(Some(&store), &session)?;

    eprintln!("Dispatching fix agent for PR #{pr_number}...");

    // Re-fetch PR metadata and review diff for the remediation prompt.
    let pr = fetch_pr_metadata(&gh, &root, pr_number)?;

    // For scripted dispatch we re-run a lightweight review to get the review
    // output (the full output is not persisted to disk).
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
    let report = run_agent_capture(&agent_args)?;
    let review_output = report.stdout.trim().to_string();

    // Transition to FixAgentInProgress.
    session.phase = ReviewPhase::FixAgentInProgress;
    session.summary = format!("Fix agent running for PR #{pr_number}");
    session.updated_at_epoch_seconds = now_epoch_seconds();
    persist_review_session(Some(&store), &session)?;

    match run_remediation(
        &root,
        &pr,
        &linear_identifier,
        &review_output,
        &config,
        &planning_meta,
        args,
    ) {
        Ok(outcome) => {
            session.phase = ReviewPhase::FixAgentComplete;
            session.summary = format!("Remediation PR #{} created", outcome.pr_number);
            session.remediation_pr_number = Some(outcome.pr_number);
            session.remediation_pr_url = Some(outcome.pr_url.clone());
            session.remediation_required = Some(true);
            session.updated_at_epoch_seconds = now_epoch_seconds();
            persist_review_session(Some(&store), &session)?;

            if args.json {
                println!(
                    "{}",
                    crate::output::render_json_success(
                        "agents.review.fix_pr",
                        &serde_json::json!({
                            "pr_number": pr_number,
                            "remediation_pr_number": outcome.pr_number,
                            "remediation_pr_url": outcome.pr_url,
                            "phase": "fix_agent_complete",
                        }),
                    )?
                );
            } else {
                eprintln!(
                    "Remediation PR #{} created: {}",
                    outcome.pr_number, outcome.pr_url
                );
            }
            Ok(())
        }
        Err(error) => {
            session.phase = ReviewPhase::FixAgentFailed;
            session.summary = format!("Fix agent failed: {error}");
            session.updated_at_epoch_seconds = now_epoch_seconds();
            persist_review_session(Some(&store), &session)?;

            if args.json {
                println!(
                    "{}",
                    crate::output::render_json_error("agents.review.fix_pr", &error,)
                );
                Ok(())
            } else {
                Err(error.context(format!(
                    "fix-agent failed for PR #{pr_number}; session state updated to FixAgentFailed"
                )))
            }
        }
    }
}

/// Non-interactively skip remediation for a previously reviewed PR.
///
/// Transitions the persistent session state to `Skipped` and prints a
/// confirmation message.
///
/// Returns an error when no eligible session exists for the given PR.
fn run_skip_pr(args: &ReviewRunArgs, pr_number: u64) -> Result<()> {
    let root = canonicalize_existing_dir(&args.root)?;
    let store = ReviewProjectStore::resolve(&root)?;
    let state = store.load_state()?;

    let persistent_session = state
        .find_session(pr_number)
        .ok_or_else(|| anyhow!("no review session found for PR #{pr_number}"))?;

    let eligible = persistent_session.needs_remediation_decision()
        || (persistent_session.phase == ReviewPhase::Completed
            && persistent_session.remediation_required == Some(true)
            && persistent_session.remediation_pr_url.is_none());

    if !eligible {
        bail!(
            "PR #{pr_number} is not eligible for skip (phase: {}, remediation_required: {:?})",
            persistent_session.phase.display_label(),
            persistent_session.remediation_required
        );
    }

    if !persistent_session
        .phase
        .can_transition_to(ReviewPhase::Skipped)
    {
        bail!(
            "invalid state transition for PR #{pr_number}: {} -> Skipped",
            persistent_session.phase.display_label()
        );
    }
    let mut session = persistent_session.clone();
    session.phase = ReviewPhase::Skipped;
    session.summary = format!("Remediation skipped for PR #{pr_number}");
    session.updated_at_epoch_seconds = now_epoch_seconds();
    persist_review_session(Some(&store), &session)?;

    if args.json {
        println!(
            "{}",
            crate::output::render_json_success(
                "agents.review.skip_pr",
                &serde_json::json!({
                    "pr_number": pr_number,
                    "phase": "skipped",
                }),
            )?
        );
    } else {
        eprintln!("Remediation skipped for PR #{pr_number}.");
    }
    Ok(())
}

fn run_review_one_shot(args: &ReviewRunArgs, pr_number: u64) -> Result<()> {
    let root = canonicalize_existing_dir(&args.root)?;
    let config = AppConfig::load()?;
    let planning_meta = crate::config::load_required_planning_meta(
        &root,
        &format!("{} agents review", crate::branding::COMMAND_NAME),
    )?;
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

fn run_retro_one_shot(args: &ReviewRunArgs, pr_number: u64) -> Result<()> {
    let root = canonicalize_existing_dir(&args.root)?;
    let config = AppConfig::load()?;
    let planning_meta = crate::config::load_required_planning_meta(
        &root,
        &format!("{} agents retro", crate::branding::COMMAND_NAME),
    )?;
    let gh = GhCli;

    verify_gh_auth(&root)?;

    let pr = fetch_pr_metadata(&gh, &root, pr_number)?;
    let linear_identifier = resolve_linear_identifier(&pr)?;

    let diff = fetch_pr_diff(&root, pr_number)?;
    let context_bundle = load_codebase_context_bundle(&root).unwrap_or_default();
    let workflow_contract = load_workflow_contract(&root).unwrap_or_default();
    let repo_map = render_repo_map(&root).unwrap_or_default();
    let ticket_context =
        gather_linear_ticket_context(&root, &config, &planning_meta, &linear_identifier)?;

    let prompt = assemble_follow_up_linear_prompt(
        &pr,
        &linear_identifier,
        &diff,
        &context_bundle,
        &workflow_contract,
        &repo_map,
        &ticket_context,
    );

    let report = run_agent_capture(&RunAgentArgs {
        root: Some(root),
        route_key: Some(AGENT_ROUTE_AGENTS_REVIEW.to_string()),
        agent: args.agent.clone(),
        prompt,
        instructions: Some(VIEW_LINEAR_INSTRUCTIONS.to_string()),
        model: args.model.clone(),
        reasoning: args.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    })?;
    let ticket_set = normalize_follow_up_ticket_set(parse_follow_up_ticket_set(&report.stdout)?)?;
    println!("{}", render_follow_up_ticket_set_markdown(&ticket_set));
    Ok(())
}

impl InteractiveReviewApp {
    fn new(mode: InteractiveReviewMode, command: ReviewCommandKind) -> Self {
        Self {
            command,
            mode,
            stage: InteractiveReviewStage::Loading,
            tab: InteractiveReviewTab::Candidates,
            focus: InteractiveReviewFocus::CandidateList,
            query: InputFieldState::default(),
            candidates: Vec::new(),
            selected_index: 0,
            session_index: 0,
            selected_prs: BTreeSet::new(),
            sessions: Vec::new(),
            preview_scroll: ScrollState::default(),
            session_preview_scroll: ScrollState::default(),
            status: "Preparing review dashboard".to_string(),
            notes: Vec::new(),
            error: None,
            refresh_requested: false,
            dialog: None,
            ticket_review: None,
            filter: CandidateFilter::default(),
            filter_panel_open: false,
            filter_panel_rows: Vec::new(),
            filter_panel_cursor: 0,
        }
    }

    fn set_loading(&mut self, status: String, note: String) {
        self.stage = InteractiveReviewStage::Loading;
        self.status = status;
        self.push_note(note);
        self.error = None;
        self.refresh_requested = false;
        self.dialog = None;
        self.ticket_review = None;
    }

    fn load_candidates(&mut self, candidates: Vec<ReviewLaunchCandidate>) {
        self.candidates = candidates;
        self.selected_index = 0;
        self.preview_scroll.reset();
        self.error = None;
        self.refresh_requested = false;
        self.dialog = None;
        if self.candidates.is_empty() {
            self.stage = InteractiveReviewStage::Empty;
            self.status = "No review candidates found".to_string();
        } else {
            self.stage = InteractiveReviewStage::Select;
            self.status = match (self.command, self.mode) {
                (ReviewCommandKind::Review, InteractiveReviewMode::Direct) => {
                    "Review candidate loaded. Confirm when you want to start the audit."
                        .to_string()
                }
                (ReviewCommandKind::Review, InteractiveReviewMode::Discovery) => {
                    "Select a PR to review, inspect the preview, then explicitly start the audit."
                        .to_string()
                }
                (ReviewCommandKind::Retro, InteractiveReviewMode::Direct) => {
                    "Retro candidate loaded. Confirm when you want to start follow-up ticket analysis."
                        .to_string()
                }
                (ReviewCommandKind::Retro, InteractiveReviewMode::Discovery) => {
                    "Select a PR to inspect, then explicitly start the retro ticket analysis."
                        .to_string()
                }
            };
        }
    }

    fn refresh_candidates(&mut self, candidates: Vec<ReviewLaunchCandidate>) {
        let selected = self.selected_prs.clone();
        self.candidates = candidates;
        self.selected_index = self
            .selected_index
            .min(self.visible_candidate_indices().len().saturating_sub(1));
        self.selected_prs = selected
            .into_iter()
            .filter(|pr_number| {
                self.candidates
                    .iter()
                    .any(|candidate| candidate.pr_number == *pr_number)
            })
            .collect();
        self.preview_scroll.reset();
        self.refresh_requested = false;
        self.dialog = None;
        self.ticket_review = None;
        self.stage = if self.candidates.is_empty() {
            InteractiveReviewStage::Empty
        } else {
            InteractiveReviewStage::Select
        };
        self.status = format!(
            "{} candidate PR(s) available. Select one or more {}, or switch to sessions to watch progress.",
            self.visible_candidate_indices().len(),
            match self.command {
                ReviewCommandKind::Review => "reviews",
                ReviewCommandKind::Retro => "retro analyses",
            }
        );
    }

    /// Restore in-memory sessions from persistent review state so the
    /// dashboard shows previously processed PRs on re-entry.
    fn restore_from_persistent_state(&mut self, sessions: &[ReviewSession]) {
        let mut restored = 0usize;
        for persistent in sessions {
            if persistent.phase.is_completed() && !persistent.needs_remediation_decision() {
                continue;
            }
            let candidate = self
                .candidates
                .iter()
                .find(|candidate| candidate.pr_number == persistent.pr_number)
                .cloned()
                .unwrap_or_else(|| ReviewLaunchCandidate {
                    pr_number: persistent.pr_number,
                    title: persistent.pr_title.clone(),
                    url: persistent.pr_url.clone().unwrap_or_default(),
                    author: persistent.pr_author.clone().unwrap_or_default(),
                    head_ref: persistent.head_branch.clone().unwrap_or_default(),
                    base_ref: persistent.base_branch.clone().unwrap_or_default(),
                    review_state: "UNKNOWN".to_string(),
                    changed_files: 0,
                    additions: 0,
                    deletions: 0,
                    linear_identifier: persistent.linear_identifier.clone(),
                    linear_error: None,
                    candidate_state: "open".to_string(),
                    candidate_labels: Vec::new(),
                    candidate_assignees: Vec::new(),
                });
            if !self
                .candidates
                .iter()
                .any(|existing| existing.pr_number == candidate.pr_number)
            {
                self.replace_candidate(candidate.clone());
            }
            let session = self.upsert_session(InteractiveReviewSession {
                kind: InteractiveSessionKind::Review,
                candidate,
                phase: persistent.phase,
                summary: persistent.summary.clone(),
                notes: Vec::new(),
                review_output: persistent.review_output.clone(),
                follow_up_ticket_set: None,
                created_follow_up_issues: Vec::new(),
                remediation_required: persistent.remediation_required,
                remediation_pr_number: persistent.remediation_pr_number,
                remediation_pr_url: persistent.remediation_pr_url.clone(),
                remediation_declined: persistent.phase == ReviewPhase::Skipped,
                cancel_requested: false,
                error: if persistent.phase == ReviewPhase::FixAgentFailed {
                    Some(persistent.summary.clone())
                } else {
                    None
                },
                updated_at_epoch_seconds: persistent.updated_at_epoch_seconds,
            });
            session.push_note(format!(
                "Restored from previous session (phase: {}).",
                persistent.phase.display_label()
            ));
            restored += 1;
        }
        if restored > 0 {
            self.tab = InteractiveReviewTab::Sessions;
            self.focus = InteractiveReviewFocus::SessionList;
            self.push_note(format!("Restored {restored} session(s) from previous run."));
        }
    }

    fn begin_running(&mut self, candidates: &[ReviewLaunchCandidate]) {
        self.begin_running_with_kind(candidates, InteractiveSessionKind::Review);
    }

    fn begin_running_with_kind(
        &mut self,
        candidates: &[ReviewLaunchCandidate],
        kind: InteractiveSessionKind,
    ) {
        self.stage = InteractiveReviewStage::Select;
        self.tab = InteractiveReviewTab::Sessions;
        self.focus = InteractiveReviewFocus::SessionList;
        self.error = None;
        for candidate in candidates {
            self.replace_candidate(candidate.clone());
            self.upsert_session(InteractiveReviewSession {
                kind,
                candidate: candidate.clone(),
                phase: ReviewPhase::Claimed,
                summary: match kind {
                    InteractiveSessionKind::Review => "Queued for review".to_string(),
                    InteractiveSessionKind::FollowUpTickets => {
                        "Queued for follow-up ticket analysis".to_string()
                    }
                },
                notes: vec![match kind {
                    InteractiveSessionKind::Review => {
                        "Waiting for the review worker to start.".to_string()
                    }
                    InteractiveSessionKind::FollowUpTickets => {
                        "Waiting for the follow-up ticket analysis to start.".to_string()
                    }
                }],
                review_output: None,
                follow_up_ticket_set: None,
                created_follow_up_issues: Vec::new(),
                remediation_required: None,
                remediation_pr_number: None,
                remediation_pr_url: None,
                remediation_declined: false,
                cancel_requested: false,
                error: None,
                updated_at_epoch_seconds: now_epoch_seconds(),
            });
        }
        self.selected_prs.clear();
        self.session_index = self.sessions.len().saturating_sub(1);
        self.session_preview_scroll.reset();
        self.status = format!(
            "{} session(s) running. You can switch back to candidates to queue more work or press `R` to refresh the queue.",
            self.active_session_count()
        );
    }

    fn apply_worker_event(&mut self, event: ReviewExecutionEvent) {
        match event {
            ReviewExecutionEvent::Progress {
                kind,
                candidate,
                phase,
                summary,
                note,
                remediation_required,
            } => {
                if self.is_cancel_requested(candidate.pr_number, kind) {
                    return;
                }
                self.status = summary.clone();
                self.error = None;
                self.replace_candidate(candidate.clone());
                let session = self.upsert_session(InteractiveReviewSession {
                    kind,
                    candidate: candidate.clone(),
                    phase,
                    summary,
                    notes: note.clone().into_iter().collect(),
                    review_output: None,
                    follow_up_ticket_set: None,
                    created_follow_up_issues: Vec::new(),
                    remediation_required,
                    remediation_pr_number: None,
                    remediation_pr_url: None,
                    remediation_declined: false,
                    cancel_requested: false,
                    error: None,
                    updated_at_epoch_seconds: now_epoch_seconds(),
                });
                if let Some(note) = note {
                    session.push_note(note);
                }
                session.push_note(format!(
                    "PR #{} entered `{}`.",
                    candidate.pr_number,
                    phase.display_label()
                ));
            }
            ReviewExecutionEvent::Completed(outcome) => {
                if self.is_cancel_requested(outcome.candidate.pr_number, outcome.kind) {
                    return;
                }
                self.status = outcome.summary.clone();
                self.error = None;
                let mut candidate = outcome.candidate.clone();
                candidate.linear_identifier = outcome.linear_identifier.clone();
                candidate.linear_error = None;
                self.replace_candidate(candidate.clone());
                let completed_phase = if outcome.remediation_pr_url.is_some() {
                    ReviewPhase::FixAgentComplete
                } else if outcome.remediation_required
                    && outcome.kind == InteractiveSessionKind::Review
                {
                    ReviewPhase::ReviewComplete
                } else {
                    ReviewPhase::Completed
                };
                let session = self.upsert_session(InteractiveReviewSession {
                    kind: outcome.kind,
                    candidate,
                    phase: completed_phase,
                    summary: outcome.summary.clone(),
                    notes: Vec::new(),
                    review_output: Some(outcome.review_output.clone()),
                    follow_up_ticket_set: outcome.follow_up_ticket_set.clone(),
                    created_follow_up_issues: Vec::new(),
                    remediation_required: Some(outcome.remediation_required),
                    remediation_pr_number: outcome.remediation_pr_number,
                    remediation_pr_url: outcome.remediation_pr_url.clone(),
                    remediation_declined: false,
                    cancel_requested: false,
                    error: None,
                    updated_at_epoch_seconds: now_epoch_seconds(),
                });
                session.push_note(match (outcome.remediation_required, outcome.remediation_pr_url.as_deref()) {
                    (_, _) if outcome.kind == InteractiveSessionKind::FollowUpTickets => format!(
                        "Follow-up Linear ticket recommendations are ready for PR #{}. Press `Enter` to review, merge, and create them in Linear.",
                        outcome.candidate.pr_number
                    ),
                    (true, Some(url)) => format!(
                        "Remediation PR #{} opened at {}.",
                        outcome.remediation_pr_number.unwrap_or_default(),
                        url
                    ),
                    (true, None) => format!(
                        "Review report is ready for PR #{}. Press `a` to create remediation or `n` to keep the report only.",
                        outcome.candidate.pr_number
                    ),
                    (false, _) => format!(
                        "Review finished for PR #{} without remediation.",
                        outcome.candidate.pr_number
                    ),
                });
                self.session_preview_scroll.reset();
            }
            ReviewExecutionEvent::Cancelled {
                kind,
                candidate,
                summary,
                note,
                review_output,
                remediation_required,
            } => {
                self.status = summary.clone();
                self.error = None;
                self.replace_candidate(candidate.clone());
                let session = self.upsert_session(InteractiveReviewSession {
                    kind,
                    candidate,
                    phase: ReviewPhase::Blocked,
                    summary,
                    notes: Vec::new(),
                    review_output,
                    follow_up_ticket_set: None,
                    created_follow_up_issues: Vec::new(),
                    remediation_required,
                    remediation_pr_number: None,
                    remediation_pr_url: None,
                    remediation_declined: false,
                    cancel_requested: true,
                    error: None,
                    updated_at_epoch_seconds: now_epoch_seconds(),
                });
                session.push_note(note);
                self.session_preview_scroll.reset();
            }
            ReviewExecutionEvent::Failed {
                kind,
                candidate,
                error,
            } => {
                if self.is_cancel_requested(candidate.pr_number, kind) {
                    return;
                }
                self.status = format!(
                    "{} failed for PR #{}",
                    kind.title_label(),
                    candidate.pr_number
                );
                self.error = None;
                self.replace_candidate(candidate.clone());
                let session = self.upsert_session(InteractiveReviewSession {
                    kind,
                    candidate: candidate.clone(),
                    phase: ReviewPhase::Blocked,
                    summary: format!(
                        "{} failed for PR #{}",
                        kind.title_label(),
                        candidate.pr_number
                    ),
                    notes: Vec::new(),
                    review_output: None,
                    follow_up_ticket_set: None,
                    created_follow_up_issues: Vec::new(),
                    remediation_required: None,
                    remediation_pr_number: None,
                    remediation_pr_url: None,
                    remediation_declined: false,
                    cancel_requested: false,
                    error: Some(error.clone()),
                    updated_at_epoch_seconds: now_epoch_seconds(),
                });
                session.push_note(error);
                self.tab = InteractiveReviewTab::Sessions;
                self.focus = InteractiveReviewFocus::SessionList;
                self.session_preview_scroll.reset();
            }
        }
    }

    fn fail(&mut self, error: String) {
        self.stage = if self.candidates.is_empty() {
            InteractiveReviewStage::Empty
        } else {
            InteractiveReviewStage::Select
        };
        self.status = "Review dashboard failed".to_string();
        self.error = Some(error.clone());
        self.push_note(error);
        self.refresh_requested = false;
        self.dialog = None;
        self.ticket_review = None;
    }

    fn tick(&mut self) {}

    fn handle_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        preview: Rect,
    ) -> Result<Option<InteractiveReviewAction>> {
        // When the filter panel overlay is open, route all input to it.
        if self.filter_panel_open {
            self.handle_filter_panel_key(key);
            return Ok(None);
        }

        match self.stage {
            InteractiveReviewStage::Loading | InteractiveReviewStage::TicketLoading => Ok(None),
            InteractiveReviewStage::Empty => {
                if matches!(key.code, KeyCode::Char('r') | KeyCode::Char('R')) {
                    self.stage = InteractiveReviewStage::Loading;
                    self.status = "Refreshing review candidates".to_string();
                    self.notes.clear();
                    self.push_note("Refreshing candidate discovery.".to_string());
                }
                Ok(None)
            }
            InteractiveReviewStage::Select => {
                match key.code {
                    KeyCode::Tab => {
                        self.focus = match self.focus {
                            InteractiveReviewFocus::CandidateList => {
                                InteractiveReviewFocus::CandidatePreview
                            }
                            InteractiveReviewFocus::CandidatePreview => {
                                InteractiveReviewFocus::SessionList
                            }
                            InteractiveReviewFocus::SessionList => {
                                InteractiveReviewFocus::SessionPreview
                            }
                            InteractiveReviewFocus::SessionPreview => {
                                InteractiveReviewFocus::CandidateList
                            }
                        };
                        self.tab = match self.focus {
                            InteractiveReviewFocus::CandidateList
                            | InteractiveReviewFocus::CandidatePreview => {
                                InteractiveReviewTab::Candidates
                            }
                            InteractiveReviewFocus::SessionList
                            | InteractiveReviewFocus::SessionPreview => {
                                InteractiveReviewTab::Sessions
                            }
                        };
                    }
                    KeyCode::Esc => {
                        if self.tab == InteractiveReviewTab::Sessions
                            || self.focus != InteractiveReviewFocus::CandidateList
                        {
                            self.tab = InteractiveReviewTab::Candidates;
                            self.focus = InteractiveReviewFocus::CandidateList;
                            self.status = format!(
                                "Returned to candidates. {} active review session(s) remain visible and candidate rows with running work stay highlighted.",
                                self.active_session_count()
                            );
                        }
                    }
                    KeyCode::Char('r') | KeyCode::Char('R') => {
                        self.stage = InteractiveReviewStage::Loading;
                        self.status = "Refreshing review candidates".to_string();
                        self.push_note("Refreshing candidate discovery from GitHub.".to_string());
                        self.refresh_requested = true;
                    }
                    KeyCode::Char(' ') if self.tab == InteractiveReviewTab::Candidates => {
                        if let Some(pr_number) = self
                            .selected_candidate()
                            .map(|candidate| candidate.pr_number)
                        {
                            if !self.selected_prs.insert(pr_number) {
                                self.selected_prs.remove(&pr_number);
                            }
                        }
                    }
                    KeyCode::Up => {
                        if self.focus == InteractiveReviewFocus::CandidateList {
                            self.selected_index = self.selected_index.saturating_sub(1);
                            self.preview_scroll.reset();
                        } else if self.focus == InteractiveReviewFocus::CandidatePreview {
                            let _ = self.preview_scroll.apply_key_code_in_viewport(
                                KeyCode::Up,
                                preview,
                                self.preview_rows(preview.width),
                            );
                        } else if self.focus == InteractiveReviewFocus::SessionList {
                            self.session_index = self.session_index.saturating_sub(1);
                            self.session_preview_scroll.reset();
                        } else {
                            let _ = self.session_preview_scroll.apply_key_code_in_viewport(
                                KeyCode::Up,
                                preview,
                                self.session_preview_rows(preview.width),
                            );
                        }
                    }
                    KeyCode::Down => {
                        if self.focus == InteractiveReviewFocus::CandidateList {
                            if self.selected_index + 1 < self.visible_candidate_indices().len() {
                                self.selected_index += 1;
                                self.preview_scroll.reset();
                            }
                        } else if self.focus == InteractiveReviewFocus::CandidatePreview {
                            let _ = self.preview_scroll.apply_key_code_in_viewport(
                                KeyCode::Down,
                                preview,
                                self.preview_rows(preview.width),
                            );
                        } else if self.focus == InteractiveReviewFocus::SessionList {
                            if self.session_index + 1 < self.sessions.len() {
                                self.session_index += 1;
                                self.session_preview_scroll.reset();
                            }
                        } else {
                            let _ = self.session_preview_scroll.apply_key_code_in_viewport(
                                KeyCode::Down,
                                preview,
                                self.session_preview_rows(preview.width),
                            );
                        }
                    }
                    KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
                        if self.focus == InteractiveReviewFocus::CandidatePreview =>
                    {
                        let _ = self.preview_scroll.apply_key_code_in_viewport(
                            key.code,
                            preview,
                            self.preview_rows(preview.width),
                        );
                    }
                    KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
                        if self.focus == InteractiveReviewFocus::SessionPreview =>
                    {
                        let _ = self.session_preview_scroll.apply_key_code_in_viewport(
                            key.code,
                            preview,
                            self.session_preview_rows(preview.width),
                        );
                    }
                    KeyCode::Enter
                        if self.tab == InteractiveReviewTab::Candidates
                            && match self.command {
                                ReviewCommandKind::Review => self
                                    .launch_candidates_for(InteractiveSessionKind::Review)
                                    .is_some(),
                                ReviewCommandKind::Retro => self
                                    .launch_candidates_for(InteractiveSessionKind::FollowUpTickets)
                                    .is_some(),
                            } =>
                    {
                        self.stage = InteractiveReviewStage::Confirm;
                        match self.command {
                            ReviewCommandKind::Review => {
                                self.dialog = self
                                    .launch_candidates_for(InteractiveSessionKind::Review)
                                    .map(InteractiveReviewDialog::LaunchReviews);
                                self.status = format!(
                                    "Confirm review start for {} candidate PR(s).",
                                    self.launch_candidates_for(InteractiveSessionKind::Review)
                                        .map(|entries| entries.len())
                                        .unwrap_or_default()
                                );
                            }
                            ReviewCommandKind::Retro => {
                                self.dialog = self
                                    .launch_candidates_for(InteractiveSessionKind::FollowUpTickets)
                                    .map(InteractiveReviewDialog::LaunchFollowUpTickets);
                                self.status = format!(
                                    "Confirm retro analysis start for {} candidate PR(s).",
                                    self.launch_candidates_for(
                                        InteractiveSessionKind::FollowUpTickets
                                    )
                                    .map(|entries| entries.len())
                                    .unwrap_or_default()
                                );
                            }
                        }
                    }
                    KeyCode::Enter
                        if self.tab == InteractiveReviewTab::Sessions
                            && self
                                .selected_session()
                                .is_some_and(Self::session_has_ticket_review) =>
                    {
                        self.open_selected_follow_up_ticket_review();
                    }
                    KeyCode::Char('l') | KeyCode::Char('L')
                        if self.command == ReviewCommandKind::Review
                            && self.tab == InteractiveReviewTab::Candidates
                            && self
                                .launch_candidates_for(InteractiveSessionKind::FollowUpTickets)
                                .is_some() =>
                    {
                        self.stage = InteractiveReviewStage::Confirm;
                        self.dialog = self
                            .launch_candidates_for(InteractiveSessionKind::FollowUpTickets)
                            .map(InteractiveReviewDialog::LaunchFollowUpTickets);
                        self.status = format!(
                            "Recommend follow-up Linear tickets for {} candidate PR(s).",
                            self.launch_candidates_for(InteractiveSessionKind::FollowUpTickets)
                                .map(|entries| entries.len())
                                .unwrap_or_default()
                        );
                    }
                    KeyCode::Char('a') | KeyCode::Char('A')
                        if self.tab == InteractiveReviewTab::Sessions
                            && self
                                .selected_session()
                                .is_some_and(Self::session_needs_remediation_decision) =>
                    {
                        if let Some(pr_number) = self
                            .selected_session()
                            .map(|session| session.candidate.pr_number)
                        {
                            self.stage = InteractiveReviewStage::Confirm;
                            self.dialog =
                                Some(InteractiveReviewDialog::StartRemediation(pr_number));
                            self.status =
                                format!("Confirm remediation PR creation for PR #{pr_number}.");
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Char('N')
                        if self.tab == InteractiveReviewTab::Sessions
                            && self
                                .selected_session()
                                .is_some_and(Self::session_needs_remediation_decision) =>
                    {
                        if let Some(pr_number) = self
                            .selected_session()
                            .map(|session| session.candidate.pr_number)
                        {
                            self.stage = InteractiveReviewStage::Confirm;
                            self.dialog = Some(InteractiveReviewDialog::SkipRemediation(pr_number));
                            self.status = format!(
                                "Confirm keeping the report without remediation for PR #{pr_number}."
                            );
                        }
                    }
                    KeyCode::Char('d') | KeyCode::Char('D')
                        if self.tab == InteractiveReviewTab::Sessions
                            && self.selected_session().is_some() =>
                    {
                        if let Some((pr_number, kind)) = self
                            .selected_session()
                            .map(|session| (session.candidate.pr_number, session.kind))
                        {
                            self.stage = InteractiveReviewStage::Confirm;
                            self.dialog =
                                Some(InteractiveReviewDialog::DeleteSession(pr_number, kind));
                            self.status = format!(
                                "Confirm deleting the stored {} session for PR #{}.",
                                kind.label(),
                                pr_number
                            );
                        }
                    }
                    KeyCode::Char('c')
                    | KeyCode::Char('C')
                    | KeyCode::Char('x')
                    | KeyCode::Char('X')
                        if self.tab == InteractiveReviewTab::Sessions
                            && self
                                .selected_session()
                                .is_some_and(Self::session_can_cancel) =>
                    {
                        if let Some(pr_number) = self
                            .selected_session()
                            .map(|session| session.candidate.pr_number)
                        {
                            self.stage = InteractiveReviewStage::Confirm;
                            let kind = self
                                .selected_session()
                                .map(|session| session.kind)
                                .unwrap_or(InteractiveSessionKind::Review);
                            self.dialog =
                                Some(InteractiveReviewDialog::CancelSession(pr_number, kind));
                            self.status = format!(
                                "Confirm cancellation for PR #{} {}.",
                                pr_number,
                                kind.label()
                            );
                        }
                    }
                    KeyCode::Char('f') | KeyCode::Char('F')
                        if self.tab == InteractiveReviewTab::Candidates =>
                    {
                        self.open_filter_panel();
                    }
                    _ if self.tab == InteractiveReviewTab::Candidates
                        && self.handle_query_key(key) => {}
                    _ => {}
                }
                Ok(None)
            }
            InteractiveReviewStage::Confirm => match key.code {
                KeyCode::Enter => {
                    let action = match self.dialog.clone() {
                        Some(InteractiveReviewDialog::LaunchReviews(selection)) => {
                            Some(InteractiveReviewAction::LaunchReviews(selection))
                        }
                        Some(InteractiveReviewDialog::LaunchFollowUpTickets(selection)) => {
                            Some(InteractiveReviewAction::LaunchFollowUpTickets(selection))
                        }
                        Some(InteractiveReviewDialog::StartRemediation(pr_number)) => {
                            Some(InteractiveReviewAction::StartRemediation(pr_number))
                        }
                        Some(InteractiveReviewDialog::SkipRemediation(pr_number)) => {
                            Some(InteractiveReviewAction::SkipRemediation(pr_number))
                        }
                        Some(InteractiveReviewDialog::DeleteSession(pr_number, kind)) => {
                            Some(InteractiveReviewAction::DeleteSession(pr_number, kind))
                        }
                        Some(InteractiveReviewDialog::CancelSession(pr_number, kind)) => {
                            Some(InteractiveReviewAction::CancelSession(pr_number, kind))
                        }
                        None => None,
                    };
                    self.stage = InteractiveReviewStage::Select;
                    self.dialog = None;
                    Ok(action)
                }
                KeyCode::Esc | KeyCode::Backspace => {
                    self.stage = InteractiveReviewStage::Select;
                    self.dialog = None;
                    Ok(None)
                }
                _ => Ok(None),
            },
            InteractiveReviewStage::TicketReview => Ok(None),
        }
    }

    fn should_exit(&self, code: KeyCode) -> bool {
        matches!(code, KeyCode::Char('q') | KeyCode::Esc)
    }

    fn handle_preview_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        viewport: Rect,
    ) -> bool {
        let row_count = if self.tab == InteractiveReviewTab::Candidates {
            self.preview_rows(viewport.width)
        } else {
            self.session_preview_rows(viewport.width)
        };
        if self.tab == InteractiveReviewTab::Candidates {
            self.preview_scroll
                .apply_mouse_in_viewport(mouse, viewport, row_count)
        } else {
            self.session_preview_scroll
                .apply_mouse_in_viewport(mouse, viewport, row_count)
        }
    }

    fn selected_candidate(&self) -> Option<&ReviewLaunchCandidate> {
        self.visible_candidate_indices()
            .get(self.selected_index)
            .and_then(|index| self.candidates.get(*index))
    }

    fn selected_session(&self) -> Option<&InteractiveReviewSession> {
        self.sessions.get(self.session_index)
    }

    fn open_selected_follow_up_ticket_review(&mut self) {
        let Some((candidate, plan)) = self.selected_session().and_then(|session| {
            session
                .follow_up_ticket_set
                .clone()
                .map(|plan| (session.candidate.clone(), plan))
        }) else {
            return;
        };
        let pr_number = candidate.pr_number;
        self.ticket_review = Some(FollowUpTicketReviewApp::new(candidate, plan));
        self.stage = InteractiveReviewStage::TicketReview;
        self.error = None;
        self.status = format!(
            "Review follow-up Linear ticket suggestions for PR #{} before creating them.",
            pr_number
        );
    }

    fn launch_candidates_for(
        &self,
        kind: InteractiveSessionKind,
    ) -> Option<Vec<ReviewLaunchCandidate>> {
        let selected = if !self.selected_prs.is_empty() {
            self.candidates
                .iter()
                .filter(|candidate| self.selected_prs.contains(&candidate.pr_number))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            self.selected_candidate()
                .cloned()
                .map(|candidate| vec![candidate])
                .unwrap_or_default()
        };

        let launchable = selected
            .into_iter()
            .filter(|candidate| !self.has_active_session(candidate.pr_number, kind))
            .collect::<Vec<_>>();
        (!launchable.is_empty()).then_some(launchable)
    }

    fn session_matches(
        session: &InteractiveReviewSession,
        pr_number: u64,
        kind: InteractiveSessionKind,
    ) -> bool {
        session.candidate.pr_number == pr_number && session.kind == kind
    }

    fn visible_candidate_indices(&self) -> Vec<usize> {
        let query = self.query.value().trim().to_ascii_lowercase();
        self.candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| {
                // Apply structured filter first.
                if !self.filter.matches(candidate) {
                    return false;
                }
                if query.is_empty() {
                    return true;
                }
                let haystack = format!(
                    "{} {} {} {} {} {}",
                    candidate.pr_number,
                    candidate.title,
                    candidate.author,
                    candidate.head_ref,
                    candidate.base_ref,
                    candidate.linear_identifier.as_deref().unwrap_or("")
                )
                .to_ascii_lowercase();
                haystack.contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn restore_selected_candidate(&mut self, pr_number: Option<u64>) {
        let visible = self.visible_candidate_indices();
        self.selected_index = pr_number
            .and_then(|selected_pr| {
                visible.iter().position(|index| {
                    self.candidates
                        .get(*index)
                        .is_some_and(|candidate| candidate.pr_number == selected_pr)
                })
            })
            .unwrap_or(0);
    }

    fn selected_candidate_text(&self) -> Text<'static> {
        if let Some(candidate) = self.selected_candidate() {
            let mut lines = vec![
                Line::from(vec![Span::styled(
                    format!("PR #{} — {}", candidate.pr_number, candidate.title),
                    emphasis_style(),
                )]),
                Line::from(vec![
                    Span::styled("Author ", label_style()),
                    Span::raw(candidate.author.clone()),
                    Span::styled("  Branch ", label_style()),
                    Span::raw(format!("{} -> {}", candidate.head_ref, candidate.base_ref)),
                ]),
                Line::from(vec![
                    Span::styled("Review ", label_style()),
                    Span::raw(candidate.review_state.clone()),
                    Span::styled("  Files ", label_style()),
                    Span::raw(format!(
                        "{} (+{}, -{})",
                        candidate.changed_files, candidate.additions, candidate.deletions
                    )),
                ]),
                Line::from(vec![
                    Span::styled("Linear ", label_style()),
                    Span::raw(
                        candidate
                            .linear_identifier
                            .clone()
                            .unwrap_or_else(|| "unresolved".to_string()),
                    ),
                    Span::styled("  State ", label_style()),
                    Span::raw(candidate.candidate_state.clone()),
                ]),
                Line::from(vec![
                    Span::styled("Labels ", label_style()),
                    Span::raw(if candidate.candidate_labels.is_empty() {
                        "none".to_string()
                    } else {
                        candidate.candidate_labels.join(", ")
                    }),
                ]),
                Line::from(vec![
                    Span::styled("Assignees ", label_style()),
                    Span::raw(if candidate.candidate_assignees.is_empty() {
                        "unassigned".to_string()
                    } else {
                        candidate.candidate_assignees.join(", ")
                    }),
                ]),
            ];

            let review_active =
                self.has_active_session(candidate.pr_number, InteractiveSessionKind::Review);
            let ideas_active = self
                .has_active_session(candidate.pr_number, InteractiveSessionKind::FollowUpTickets);
            if review_active || ideas_active {
                lines.push(Line::from(vec![
                    Span::styled("Active ", label_style()),
                    Span::raw(match (review_active, ideas_active) {
                        (true, true) => "review and linear ideas".to_string(),
                        (true, false) => "review".to_string(),
                        (false, true) => "linear ideas".to_string(),
                        (false, false) => String::new(),
                    }),
                ]));
            }

            if let Some(error) = candidate.linear_error.as_deref() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Linear linkage needs attention before remediation can proceed.",
                    Style::default().fg(ratatui::style::Color::Red),
                )));
                lines.push(Line::from(error.to_string()));
            }

            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Available actions", label_style())));
            match self.command {
                ReviewCommandKind::Review => {
                    lines.push(Line::from("- Enter queues a review session."));
                }
                ReviewCommandKind::Retro => {
                    lines.push(Line::from(
                        "- Enter queues a retro ticket-analysis session.",
                    ));
                }
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Recent status", label_style())));
            if self.notes.is_empty() {
                lines.push(Line::from("No progress notes recorded yet."));
            } else {
                for note in self.notes.iter().take(8) {
                    lines.push(Line::from(format!("- {note}")));
                }
            }

            return Text::from(lines);
        }

        empty_state(
            "No review candidate selected.",
            "Use Up/Down to select a pull request when candidates are available.",
        )
    }

    fn selected_session_text(&self) -> Text<'static> {
        let Some(session) = self.selected_session() else {
            return Text::from("No agent session selected yet.");
        };

        let mut lines = vec![
            Line::from(vec![
                badge(format!("#{}", session.candidate.pr_number), Tone::Accent),
                Span::raw(" "),
                badge(session.kind.label(), session.kind.tone()),
                Span::raw(" "),
                Span::styled(session.candidate.title.clone(), emphasis_style()),
            ]),
            Line::from(vec![
                Span::styled("Kind ", label_style()),
                Span::raw(session.kind.label()),
                Span::styled("  ", muted_style()),
                Span::styled("Stage ", label_style()),
                Span::raw(session.phase.display_label()),
                Span::styled("  Summary ", label_style()),
                Span::raw(session.summary.clone()),
            ]),
        ];

        if let Some(error) = session.error.as_deref() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                error.to_string(),
                Style::default().fg(ratatui::style::Color::Red),
            )));
        }

        lines.push(Line::from(vec![
            Span::styled("Updated ", label_style()),
            Span::raw(format_duration(
                now_epoch_seconds().saturating_sub(session.updated_at_epoch_seconds),
            )),
            Span::styled(" ago", muted_style()),
        ]));
        if session.kind == InteractiveSessionKind::Review
            && let Some(remediation_required) = session.remediation_required
        {
            lines.push(Line::from(vec![
                Span::styled("Remediation ", label_style()),
                Span::raw(if session.remediation_declined {
                    "skipped"
                } else if remediation_required {
                    "recommended"
                } else {
                    "not required"
                }),
            ]));
        }
        if session.kind == InteractiveSessionKind::Review
            && let Some(remediation_pr_number) = session.remediation_pr_number
        {
            lines.push(Line::from(vec![
                Span::styled("Remediation PR ", label_style()),
                Span::raw(format!("#{remediation_pr_number}")),
            ]));
        }
        if session.cancel_requested {
            lines.push(Line::from(vec![
                Span::styled("Session ", label_style()),
                Span::raw("cancelled"),
            ]));
        }
        if !session.created_follow_up_issues.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Created Linear Tickets",
                label_style(),
            )));
            for issue in &session.created_follow_up_issues {
                lines.push(Line::from(format!("- {}: {}", issue.identifier, issue.url)));
            }
        }

        if let Some(output) = session.review_output.as_deref() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                match session.kind {
                    InteractiveSessionKind::Review => "Review Report",
                    InteractiveSessionKind::FollowUpTickets => "Follow-Up Ticket Recommendations",
                },
                label_style(),
            )));
            lines.extend(render_markdown(output, muted_style(), &[]).lines);
        } else {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Progress Notes", label_style())));
            for note in &session.notes {
                lines.push(Line::from(format!("- {note}")));
            }
        }

        Text::from(lines)
    }

    fn preview_rows(&self, width: u16) -> usize {
        wrapped_rows(&self.selected_candidate_text().to_string(), width.max(1))
    }

    fn session_preview_rows(&self, width: u16) -> usize {
        wrapped_rows(&self.selected_session_text().to_string(), width.max(1))
    }

    fn push_note(&mut self, note: String) {
        if self.notes.first().is_some_and(|existing| existing == &note) {
            return;
        }
        self.notes.insert(0, note);
        self.notes.truncate(10);
    }

    fn handle_query_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        if self.tab != InteractiveReviewTab::Candidates
            || self.focus != InteractiveReviewFocus::CandidateList
        {
            return false;
        }
        let selected_pr = self
            .selected_candidate()
            .map(|candidate| candidate.pr_number);
        if self.query.handle_key(key) {
            self.restore_selected_candidate(selected_pr);
            self.preview_scroll.reset();
            return true;
        }
        false
    }

    // -----------------------------------------------------------------------
    // Filter panel helpers
    // -----------------------------------------------------------------------

    /// Build the filter panel rows from the current candidate set and filter state.
    fn build_filter_panel_rows(&self) -> Vec<FilterPanelRow> {
        let mut rows = Vec::new();

        // State options.
        for state in &["open", "closed"] {
            rows.push(FilterPanelRow {
                category: FilterCategory::State,
                value: (*state).to_string(),
                selected: self.filter.states.contains(*state),
            });
        }

        // Author options (unique, sorted).
        let mut authors: Vec<String> = self
            .candidates
            .iter()
            .map(|c| c.author.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        authors.sort();
        for author in authors {
            rows.push(FilterPanelRow {
                category: FilterCategory::Author,
                value: author.clone(),
                selected: self.filter.authors.contains(&author),
            });
        }

        // Label options (unique, sorted).
        let mut labels: Vec<String> = self
            .candidates
            .iter()
            .flat_map(|c| c.candidate_labels.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        labels.sort();
        for label in labels {
            rows.push(FilterPanelRow {
                category: FilterCategory::Label,
                value: label.clone(),
                selected: self.filter.labels.contains(&label),
            });
        }

        // Assignee options (unique, sorted) + unassigned.
        let has_unassigned = self
            .candidates
            .iter()
            .any(|c| c.candidate_assignees.is_empty());
        let mut assignees: Vec<String> = self
            .candidates
            .iter()
            .flat_map(|c| c.candidate_assignees.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        assignees.sort();
        if has_unassigned {
            assignees.insert(0, UNASSIGNED_FILTER_VALUE.to_string());
        }
        for assignee in assignees {
            rows.push(FilterPanelRow {
                category: FilterCategory::Assignee,
                value: assignee.clone(),
                selected: self.filter.assignees.contains(&assignee),
            });
        }

        rows
    }

    /// Open the filter panel and rebuild its row list.
    fn open_filter_panel(&mut self) {
        self.filter_panel_rows = self.build_filter_panel_rows();
        self.filter_panel_cursor = 0;
        self.filter_panel_open = true;
    }

    /// Close the filter panel and apply the toggled selections back to the filter.
    fn close_filter_panel(&mut self) {
        self.apply_filter_panel_selections();
        self.filter_panel_open = false;
        self.selected_index = 0;
        self.preview_scroll.reset();
        if self.filter.is_active() {
            let visible = self.visible_candidate_indices().len();
            self.status = format!(
                "{} of {} candidates shown (filtered: {})",
                visible,
                self.candidates.len(),
                self.filter.summary()
            );
        } else {
            self.status = format!(
                "All {} candidates shown (no active filters).",
                self.candidates.len()
            );
        }
    }

    /// Write the panel row selections back into the `CandidateFilter`.
    fn apply_filter_panel_selections(&mut self) {
        self.filter.clear();
        for row in &self.filter_panel_rows {
            if !row.selected {
                continue;
            }
            match row.category {
                FilterCategory::State => {
                    self.filter.states.insert(row.value.clone());
                }
                FilterCategory::Author => {
                    self.filter.authors.insert(row.value.clone());
                }
                FilterCategory::Label => {
                    self.filter.labels.insert(row.value.clone());
                }
                FilterCategory::Assignee => {
                    self.filter.assignees.insert(row.value.clone());
                }
            }
        }
    }

    /// Handle a key event while the filter panel is open. Returns `true` if consumed.
    fn handle_filter_panel_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        match key.code {
            KeyCode::Up => {
                self.filter_panel_cursor = self.filter_panel_cursor.saturating_sub(1);
            }
            KeyCode::Down => {
                if self.filter_panel_cursor + 1 < self.filter_panel_rows.len() {
                    self.filter_panel_cursor += 1;
                }
            }
            KeyCode::Char(' ') => {
                if let Some(row) = self.filter_panel_rows.get_mut(self.filter_panel_cursor) {
                    row.selected = !row.selected;
                }
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                for row in &mut self.filter_panel_rows {
                    row.selected = false;
                }
            }
            KeyCode::Enter | KeyCode::Char('f') | KeyCode::Char('F') | KeyCode::Esc => {
                self.close_filter_panel();
            }
            _ => return false,
        }
        true
    }

    fn replace_candidate(&mut self, candidate: ReviewLaunchCandidate) {
        if let Some(existing) = self
            .candidates
            .iter_mut()
            .find(|entry| entry.pr_number == candidate.pr_number)
        {
            *existing = candidate;
        } else {
            self.candidates.push(candidate);
            self.selected_index = self.candidates.len().saturating_sub(1);
        }
    }

    fn has_active_session(&self, pr_number: u64, kind: InteractiveSessionKind) -> bool {
        self.sessions.iter().any(|session| {
            Self::session_matches(session, pr_number, kind)
                && !matches!(session.phase, ReviewPhase::Completed | ReviewPhase::Blocked)
        })
    }

    fn is_cancel_requested(&self, pr_number: u64, kind: InteractiveSessionKind) -> bool {
        self.sessions
            .iter()
            .find(|session| Self::session_matches(session, pr_number, kind))
            .is_some_and(|session| session.cancel_requested)
    }

    fn session_needs_remediation_decision(session: &InteractiveReviewSession) -> bool {
        (session.phase == ReviewPhase::ReviewComplete
            || (session.phase == ReviewPhase::Completed
                && session.remediation_required == Some(true)))
            && session.remediation_pr_url.is_none()
            && !session.remediation_declined
            && !session.cancel_requested
            && session.review_output.is_some()
    }

    fn delete_session(&mut self, pr_number: u64, kind: InteractiveSessionKind) {
        if let Some(index) = self
            .sessions
            .iter()
            .position(|session| Self::session_matches(session, pr_number, kind))
        {
            self.sessions.remove(index);
            if self.sessions.is_empty() {
                self.session_index = 0;
                self.focus = InteractiveReviewFocus::CandidateList;
                self.tab = InteractiveReviewTab::Candidates;
            } else {
                self.session_index = self
                    .session_index
                    .min(self.sessions.len().saturating_sub(1));
            }
            self.status = format!(
                "Deleted stored {} session for PR #{}.",
                kind.label(),
                pr_number
            );
        }
    }

    fn session_can_cancel(session: &InteractiveReviewSession) -> bool {
        !session.phase.is_terminal()
            || session.phase.is_fix_agent_active()
            || Self::session_needs_remediation_decision(session)
    }

    fn session_has_ticket_review(session: &InteractiveReviewSession) -> bool {
        session.kind == InteractiveSessionKind::FollowUpTickets
            && session.phase == ReviewPhase::Completed
            && session.follow_up_ticket_set.is_some()
            && session.created_follow_up_issues.is_empty()
    }

    fn upsert_session(
        &mut self,
        session: InteractiveReviewSession,
    ) -> &mut InteractiveReviewSession {
        if let Some(index) = self.sessions.iter().position(|existing| {
            Self::session_matches(existing, session.candidate.pr_number, session.kind)
        }) {
            self.sessions[index] = session;
            return &mut self.sessions[index];
        }

        self.sessions.push(session);
        let index = self.sessions.len() - 1;
        &mut self.sessions[index]
    }

    fn active_session_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|session| !session.phase.is_terminal())
            .count()
    }

    fn apply_follow_up_ticket_job_event(&mut self, event: FollowUpTicketJobEvent) {
        match event {
            FollowUpTicketJobEvent::RevisionReady(review) => {
                self.status = format!(
                    "Updated follow-up ticket preview for PR #{}.",
                    review.pr_number
                );
                self.stage = InteractiveReviewStage::TicketReview;
                self.ticket_review = Some(*review);
                self.error = None;
            }
            FollowUpTicketJobEvent::Created { pr_number, issues } => {
                self.stage = InteractiveReviewStage::Select;
                self.status = format!(
                    "Created {} follow-up Linear ticket(s) for PR #{}.",
                    issues.len(),
                    pr_number
                );
                self.error = None;
                if let Some(session) = self.sessions.iter_mut().find(|session| {
                    session.candidate.pr_number == pr_number
                        && session.kind == InteractiveSessionKind::FollowUpTickets
                }) {
                    session.created_follow_up_issues = issues.clone();
                    session.summary = if issues.is_empty() {
                        "No follow-up tickets created".to_string()
                    } else {
                        format!("Created {} follow-up Linear ticket(s)", issues.len())
                    };
                    for issue in &issues {
                        session.push_note(format!("Created {}: {}", issue.identifier, issue.url));
                    }
                }
                self.ticket_review = None;
                self.tab = InteractiveReviewTab::Sessions;
                self.focus = InteractiveReviewFocus::SessionList;
            }
            FollowUpTicketJobEvent::Failed { pr_number, error } => {
                self.stage = if self.ticket_review.is_some() {
                    InteractiveReviewStage::TicketReview
                } else {
                    InteractiveReviewStage::Select
                };
                self.status = format!("Follow-up ticket workflow failed for PR #{}.", pr_number);
                self.error = Some(error.clone());
                if let Some(review) = self.ticket_review.as_mut() {
                    review.error = Some(error);
                }
            }
        }
    }
}

impl InteractiveReviewSession {
    fn push_note(&mut self, note: String) {
        if self.notes.first().is_some_and(|existing| existing == &note) {
            return;
        }
        self.notes.insert(0, note);
        self.notes.truncate(12);
        self.updated_at_epoch_seconds = now_epoch_seconds();
    }
}

#[derive(Clone, Copy)]
struct FollowUpTicketReviewLayout {
    issue_list: Rect,
    selected_ticket: Rect,
    overview: Rect,
    combination_plan: Rect,
    footer: Rect,
}

enum FollowUpTicketReviewAction {
    None,
    OpenCreate,
    OpenRevision,
    Close,
}

impl FollowUpTicketReviewApp {
    fn new(candidate: ReviewLaunchCandidate, plan: FollowUpTicketSet) -> Self {
        let decisions = vec![1; plan.tickets.len()];
        Self {
            pr_number: candidate.pr_number,
            candidate,
            plan,
            selected: 0,
            decisions,
            revision: 1,
            focus: FollowUpTicketReviewFocus::Tickets,
            overview_scroll: ScrollState::default(),
            selected_ticket_scroll: ScrollState::default(),
            combination_scroll: ScrollState::default(),
            error: None,
        }
    }

    fn overview_text(&self) -> Text<'static> {
        let decisions = follow_up_ticket_decision_counts(self);
        Text::from(
            vec![
                Line::from(format!("PR #{} follow-up suggestions", self.pr_number)),
                Line::from(format!("Draft batch: {}", self.revision)),
                Line::from(format!(
                    "Selected: {}/{}",
                    decisions.selected_count,
                    self.plan.tickets.len()
                )),
                Line::from(format!("Skipped: {}", decisions.skipped_count)),
                Line::from(format!("Keeping as-is: {}", decisions.keep_count)),
                Line::from(format!("Merge groups: {}", decisions.group_count)),
                Line::from(""),
                Line::from("Summary"),
                Line::from(""),
                Line::from(self.plan.summary.clone()),
                Line::from(""),
                if self.plan.notes.is_empty() {
                    Line::from("")
                } else {
                    Line::from("Notes")
                },
            ]
            .into_iter()
            .chain(
                self.plan
                    .notes
                    .iter()
                    .flat_map(|note| [Line::from(""), Line::from(format!("- {note}"))]),
            )
            .collect::<Vec<_>>(),
        )
    }

    fn selected_ticket(&self) -> Option<&FollowUpTicketDraft> {
        self.plan.tickets.get(self.selected)
    }

    fn selected_ticket_text(&self) -> Text<'static> {
        let Some(ticket) = self.selected_ticket() else {
            return Text::from("No suggested ticket selected.");
        };
        let mut lines = vec![
            Line::from(format!("Title: {}", ticket.title)),
            Line::from(format!(
                "Priority: {}",
                ticket
                    .priority
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unset".to_string())
            )),
            Line::from(""),
            Line::from("Why Now"),
            Line::from(""),
            Line::from(ticket.why_now.clone()),
            Line::from(""),
            Line::from("Outcome"),
            Line::from(""),
            Line::from(ticket.outcome.clone()),
            Line::from(""),
            Line::from("Scope"),
            Line::from(""),
            Line::from(ticket.scope.clone()),
        ];
        if !ticket.acceptance_criteria.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from("Suggested Acceptance Criteria"));
            lines.push(Line::from(""));
            lines.extend(
                ticket
                    .acceptance_criteria
                    .iter()
                    .map(|criterion| Line::from(format!("- {criterion}"))),
            );
        }
        Text::from(lines)
    }

    fn combination_plan_text(&self) -> Text<'static> {
        let decisions = follow_up_ticket_decision_counts(self);
        let mut lines = vec![
            Line::from("Space cycles the active ticket through review states."),
            Line::from(""),
            Line::from("[ ] Skip the ticket"),
            Line::from("[x] Keep the ticket as-is"),
            Line::from("[1], [2], ... Merge every ticket sharing that number"),
            Line::from(""),
            Line::from(format!(
                "Active ticket state: {}",
                follow_up_ticket_review_marker(
                    self.decisions
                        .get(self.selected)
                        .copied()
                        .unwrap_or_default()
                )
            )),
            Line::from(""),
        ];
        if decisions.selected_count == 0 {
            lines.push(Line::from(
                "Select at least one suggested ticket before continuing. Leave [ ] on tickets you want to skip.",
            ));
        } else if decisions.group_count == 0 {
            lines.push(Line::from(
                "Press Enter to create the checked [x] tickets in Linear. Unchecked [ ] tickets will be skipped.",
            ));
        } else {
            lines.push(Line::from(
                "Press Enter to rebuild the next preview from the checked [x] tickets and these merge groups. Unchecked [ ] tickets will be skipped:",
            ));
            lines.push(Line::from(""));
            lines.extend(render_follow_up_ticket_merge_group_lines(self));
        }
        Text::from(lines)
    }

    fn overview_rows(&self, width: u16) -> usize {
        wrapped_rows(&self.overview_text().to_string(), width.max(1))
    }

    fn selected_ticket_rows(&self, width: u16) -> usize {
        wrapped_rows(&self.selected_ticket_text().to_string(), width.max(1))
    }

    fn combination_plan_rows(&self, width: u16) -> usize {
        wrapped_rows(&self.combination_plan_text().to_string(), width.max(1))
    }

    fn move_selection(&mut self, delta: isize) {
        let next = if delta.is_negative() {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected
                .saturating_add(delta as usize)
                .min(self.plan.tickets.len().saturating_sub(1))
        };
        if next != self.selected {
            self.selected = next;
            self.selected_ticket_scroll.reset();
        }
    }
}

fn handle_follow_up_ticket_review_key(
    app: &mut FollowUpTicketReviewApp,
    key: crossterm::event::KeyEvent,
    area: Rect,
) -> FollowUpTicketReviewAction {
    match key.code {
        KeyCode::Esc => FollowUpTicketReviewAction::Close,
        KeyCode::BackTab => {
            app.focus = match app.focus {
                FollowUpTicketReviewFocus::Tickets => FollowUpTicketReviewFocus::CombinationPlan,
                FollowUpTicketReviewFocus::SelectedTicket => FollowUpTicketReviewFocus::Tickets,
                FollowUpTicketReviewFocus::Overview => FollowUpTicketReviewFocus::SelectedTicket,
                FollowUpTicketReviewFocus::CombinationPlan => FollowUpTicketReviewFocus::Overview,
            };
            app.error = None;
            FollowUpTicketReviewAction::None
        }
        KeyCode::Tab => {
            app.focus = match app.focus {
                FollowUpTicketReviewFocus::Tickets => FollowUpTicketReviewFocus::SelectedTicket,
                FollowUpTicketReviewFocus::SelectedTicket => FollowUpTicketReviewFocus::Overview,
                FollowUpTicketReviewFocus::Overview => FollowUpTicketReviewFocus::CombinationPlan,
                FollowUpTicketReviewFocus::CombinationPlan => FollowUpTicketReviewFocus::Tickets,
            };
            app.error = None;
            FollowUpTicketReviewAction::None
        }
        KeyCode::Up => {
            if app.focus == FollowUpTicketReviewFocus::Tickets {
                app.move_selection(-1);
            } else {
                let _ = handle_follow_up_ticket_scroll_key(app, key, area);
            }
            app.error = None;
            FollowUpTicketReviewAction::None
        }
        KeyCode::Down => {
            if app.focus == FollowUpTicketReviewFocus::Tickets {
                app.move_selection(1);
            } else {
                let _ = handle_follow_up_ticket_scroll_key(app, key, area);
            }
            app.error = None;
            FollowUpTicketReviewAction::None
        }
        KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End => {
            let _ = handle_follow_up_ticket_scroll_key(app, key, area);
            app.error = None;
            FollowUpTicketReviewAction::None
        }
        KeyCode::Char(' ') => {
            cycle_follow_up_ticket_decision(app);
            app.error = None;
            FollowUpTicketReviewAction::None
        }
        KeyCode::Char('x') | KeyCode::Char('X') => {
            if let Some(decision) = app.decisions.get_mut(app.selected) {
                *decision = 1;
            }
            app.error = None;
            FollowUpTicketReviewAction::None
        }
        KeyCode::Char('s') | KeyCode::Char('S') => {
            if let Some(decision) = app.decisions.get_mut(app.selected) {
                *decision = 0;
            }
            app.error = None;
            FollowUpTicketReviewAction::None
        }
        KeyCode::Char('u') | KeyCode::Char('U') => {
            for decision in &mut app.decisions {
                *decision = 0;
            }
            app.error = None;
            FollowUpTicketReviewAction::None
        }
        KeyCode::Enter => match follow_up_ticket_review_submission_action(app) {
            Ok(FollowUpTicketReviewSubmissionAction::ConfirmAsIs) => {
                FollowUpTicketReviewAction::OpenCreate
            }
            Ok(FollowUpTicketReviewSubmissionAction::RegeneratePreview) => {
                FollowUpTicketReviewAction::OpenRevision
            }
            Err(error) => {
                app.error = Some(error);
                FollowUpTicketReviewAction::None
            }
        },
        _ => FollowUpTicketReviewAction::None,
    }
}

fn handle_follow_up_ticket_scroll_key(
    app: &mut FollowUpTicketReviewApp,
    key: crossterm::event::KeyEvent,
    area: Rect,
) -> bool {
    let layout = follow_up_ticket_review_layout(area);
    match app.focus {
        FollowUpTicketReviewFocus::Tickets => false,
        FollowUpTicketReviewFocus::SelectedTicket => {
            app.selected_ticket_scroll.apply_key_in_viewport(
                key,
                layout.selected_ticket,
                app.selected_ticket_rows(layout.selected_ticket.width.saturating_sub(2)),
            )
        }
        FollowUpTicketReviewFocus::Overview => app.overview_scroll.apply_key_in_viewport(
            key,
            layout.overview,
            app.overview_rows(layout.overview.width.saturating_sub(2)),
        ),
        FollowUpTicketReviewFocus::CombinationPlan => app.combination_scroll.apply_key_in_viewport(
            key,
            layout.combination_plan,
            app.combination_plan_rows(layout.combination_plan.width.saturating_sub(2)),
        ),
    }
}

fn handle_follow_up_ticket_review_mouse(
    app: &mut FollowUpTicketReviewApp,
    mouse: crossterm::event::MouseEvent,
    area: Rect,
) -> bool {
    if !matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) {
        return false;
    }
    let layout = follow_up_ticket_review_layout(area);
    match app.focus {
        FollowUpTicketReviewFocus::Tickets => false,
        FollowUpTicketReviewFocus::SelectedTicket => {
            app.selected_ticket_scroll.apply_mouse_in_viewport(
                mouse,
                layout.selected_ticket,
                app.selected_ticket_rows(layout.selected_ticket.width.saturating_sub(2)),
            )
        }
        FollowUpTicketReviewFocus::Overview => app.overview_scroll.apply_mouse_in_viewport(
            mouse,
            layout.overview,
            app.overview_rows(layout.overview.width.saturating_sub(2)),
        ),
        FollowUpTicketReviewFocus::CombinationPlan => {
            app.combination_scroll.apply_mouse_in_viewport(
                mouse,
                layout.combination_plan,
                app.combination_plan_rows(layout.combination_plan.width.saturating_sub(2)),
            )
        }
    }
}

enum FollowUpTicketReviewSubmissionAction {
    ConfirmAsIs,
    RegeneratePreview,
}

fn follow_up_ticket_review_submission_action(
    app: &FollowUpTicketReviewApp,
) -> Result<FollowUpTicketReviewSubmissionAction, String> {
    if app.decisions.iter().all(|decision| *decision == 0) {
        return Err(
            "Select at least one suggested ticket before continuing. Leave [ ] on any ticket you want to skip, use [x] to keep it, or assign a number to merge it."
                .to_string(),
        );
    }

    let merge_groups = follow_up_ticket_merge_groups(app);
    for (group, indices) in &merge_groups {
        if indices.len() < 2 {
            return Err(format!(
                "Merge group {group} only has one ticket. Mark it as [x] or assign another ticket to [{group}]."
            ));
        }
    }

    if merge_groups.is_empty() {
        Ok(FollowUpTicketReviewSubmissionAction::ConfirmAsIs)
    } else {
        Ok(FollowUpTicketReviewSubmissionAction::RegeneratePreview)
    }
}

fn follow_up_ticket_review_marker(decision: usize) -> String {
    match decision {
        0 => "[ ]".to_string(),
        1 => "[x]".to_string(),
        value => format!("[{}]", value - 1),
    }
}

struct FollowUpTicketDecisionCounts {
    selected_count: usize,
    skipped_count: usize,
    keep_count: usize,
    group_count: usize,
}

fn follow_up_ticket_decision_counts(app: &FollowUpTicketReviewApp) -> FollowUpTicketDecisionCounts {
    let groups = follow_up_ticket_merge_groups(app);
    FollowUpTicketDecisionCounts {
        selected_count: app
            .decisions
            .iter()
            .filter(|decision| **decision > 0)
            .count(),
        skipped_count: app
            .decisions
            .iter()
            .filter(|decision| **decision == 0)
            .count(),
        keep_count: app
            .decisions
            .iter()
            .filter(|decision| **decision == 1)
            .count(),
        group_count: groups.len(),
    }
}

fn cycle_follow_up_ticket_decision(app: &mut FollowUpTicketReviewApp) {
    if app.plan.tickets.is_empty() {
        return;
    }
    let max_state = app.plan.tickets.len() + 1;
    if let Some(decision) = app.decisions.get_mut(app.selected) {
        *decision = (*decision + 1) % (max_state + 1);
    }
}

fn follow_up_ticket_merge_groups(
    app: &FollowUpTicketReviewApp,
) -> std::collections::BTreeMap<usize, Vec<usize>> {
    let mut groups = std::collections::BTreeMap::new();
    for (index, decision) in app.decisions.iter().copied().enumerate() {
        if decision >= 2 {
            groups
                .entry(decision - 1)
                .or_insert_with(Vec::new)
                .push(index);
        }
    }
    groups
}

fn follow_up_ticket_kept_indices(app: &FollowUpTicketReviewApp) -> Vec<usize> {
    app.decisions
        .iter()
        .enumerate()
        .filter_map(|(index, decision)| (*decision == 1).then_some(index))
        .collect()
}

fn selected_follow_up_ticket_plan(app: &FollowUpTicketReviewApp) -> FollowUpTicketSet {
    FollowUpTicketSet {
        summary: app.plan.summary.clone(),
        tickets: follow_up_ticket_kept_indices(app)
            .into_iter()
            .filter_map(|index| app.plan.tickets.get(index).cloned())
            .collect(),
        notes: app.plan.notes.clone(),
    }
}

fn render_follow_up_ticket_merge_group_lines(app: &FollowUpTicketReviewApp) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (group, indices) in follow_up_ticket_merge_groups(app) {
        let titles = indices
            .into_iter()
            .filter_map(|index| {
                app.plan
                    .tickets
                    .get(index)
                    .map(|ticket| ticket.title.clone())
            })
            .collect::<Vec<_>>()
            .join(" + ");
        lines.push(Line::from(format!("[{group}] {titles}")));
    }
    lines
}

fn follow_up_ticket_review_layout(frame_area: Rect) -> FollowUpTicketReviewLayout {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(4)])
        .split(frame_area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
        .split(layout[0]);
    let top_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(rows[0]);
    let bottom_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(rows[1]);
    FollowUpTicketReviewLayout {
        issue_list: top_row[0],
        selected_ticket: top_row[1],
        overview: bottom_row[0],
        combination_plan: bottom_row[1],
        footer: layout[1],
    }
}

fn discover_review_candidates(
    root: &Path,
    gh: &GhCli,
    pr_number: Option<u64>,
    command: ReviewCommandKind,
    app: &mut InteractiveReviewApp,
    terminal: &mut ReviewTerminalDashboard,
) -> Result<Vec<ReviewLaunchCandidate>> {
    let entries = if let Some(pr_number) = pr_number {
        let metadata = fetch_pr_metadata(gh, root, pr_number)?;
        vec![GhPrListEntry {
            number: metadata.number,
            title: metadata.title.clone(),
            url: metadata.url.clone(),
            labels: metadata.labels.clone(),
        }]
    } else {
        discover_eligible_prs(gh, root, command)?
    };

    let mut candidates = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        app.set_loading(
            format!("Inspecting PR #{}/{}", index + 1, entries.len()),
            format!(
                "Loading metadata for PR #{} so the dashboard can show a concrete review preview.",
                entry.number
            ),
        );
        terminal.draw_interactive(app)?;
        match fetch_pr_metadata(gh, root, entry.number) {
            Ok(metadata) => candidates.push(candidate_from_metadata(&metadata)),
            Err(error) => candidates.push(ReviewLaunchCandidate {
                pr_number: entry.number,
                title: entry.title.clone(),
                url: entry.url.clone(),
                author: "unknown".to_string(),
                head_ref: "unknown".to_string(),
                base_ref: "unknown".to_string(),
                review_state: "unknown".to_string(),
                changed_files: 0,
                additions: 0,
                deletions: 0,
                linear_identifier: None,
                linear_error: Some(error.to_string()),
                candidate_state: "open".to_string(),
                candidate_labels: Vec::new(),
                candidate_assignees: Vec::new(),
            }),
        }
    }

    Ok(candidates)
}

fn candidate_from_metadata(pr: &GhPrMetadata) -> ReviewLaunchCandidate {
    ReviewLaunchCandidate {
        pr_number: pr.number,
        title: pr.title.clone(),
        url: pr.url.clone(),
        author: pr.author.login.clone(),
        head_ref: pr.head_ref_name.clone(),
        base_ref: pr.base_ref_name.clone(),
        review_state: pr
            .review_decision
            .clone()
            .unwrap_or_else(|| "PENDING".to_string()),
        changed_files: pr.changed_files,
        additions: pr.additions,
        deletions: pr.deletions,
        linear_identifier: resolve_linear_identifier(pr).ok(),
        linear_error: resolve_linear_identifier(pr)
            .err()
            .map(|error| error.to_string()),
        candidate_state: normalize_pr_state(&pr.state),
        candidate_labels: pr.labels.iter().map(|l| l.name.clone()).collect(),
        candidate_assignees: pr.assignees.iter().map(|a| a.login.clone()).collect(),
    }
}

/// Normalize GitHub PR state (`OPEN`, `CLOSED`, `MERGED`) to `"open"` or `"closed"`.
fn normalize_pr_state(state: &str) -> String {
    match state {
        "OPEN" => "open".to_string(),
        _ => "closed".to_string(),
    }
}

fn spawn_review_execution(
    root: std::path::PathBuf,
    config: AppConfig,
    planning_meta: PlanningMeta,
    args: ReviewRunArgs,
    candidate: ReviewLaunchCandidate,
    store: Option<ReviewProjectStore>,
) -> InteractiveWorkerHandle {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_for_thread = Arc::clone(&cancel);
    let pr_number = candidate.pr_number;
    thread::spawn(move || {
        let context = ReviewExecutionContext {
            root: &root,
            config: &config,
            planning_meta: &planning_meta,
            args: &args,
            store: store.as_ref(),
            cancel: &cancel_for_thread,
        };
        let result = execute_review_with_progress(&context, &candidate, |event| {
            let _ = tx.send(event);
        });

        if let Err(error) = result {
            let _ = tx.send(ReviewExecutionEvent::Failed {
                kind: InteractiveSessionKind::Review,
                candidate,
                error: error.to_string(),
            });
        }
    });
    InteractiveWorkerHandle {
        kind: InteractiveSessionKind::Review,
        pr_number,
        receiver: rx,
        cancel,
    }
}

fn spawn_follow_up_ticket_execution(
    root: std::path::PathBuf,
    config: AppConfig,
    planning_meta: PlanningMeta,
    args: ReviewRunArgs,
    candidate: ReviewLaunchCandidate,
    store: Option<ReviewProjectStore>,
) -> InteractiveWorkerHandle {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_for_thread = Arc::clone(&cancel);
    let pr_number = candidate.pr_number;
    thread::spawn(move || {
        let context = ReviewExecutionContext {
            root: &root,
            config: &config,
            planning_meta: &planning_meta,
            args: &args,
            store: store.as_ref(),
            cancel: &cancel_for_thread,
        };
        let result = execute_follow_up_ticket_with_progress(&context, &candidate, |event| {
            let _ = tx.send(event);
        });

        if let Err(error) = result {
            let _ = tx.send(ReviewExecutionEvent::Failed {
                kind: InteractiveSessionKind::FollowUpTickets,
                candidate,
                error: error.to_string(),
            });
        }
    });
    InteractiveWorkerHandle {
        kind: InteractiveSessionKind::FollowUpTickets,
        pr_number,
        receiver: rx,
        cancel,
    }
}

fn spawn_remediation_execution(
    root: std::path::PathBuf,
    config: AppConfig,
    planning_meta: PlanningMeta,
    args: ReviewRunArgs,
    request: RemediationLaunchRequest,
    store: Option<ReviewProjectStore>,
) -> InteractiveWorkerHandle {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_for_thread = Arc::clone(&cancel);
    let pr_number = request.candidate.pr_number;
    thread::spawn(move || {
        let context = ReviewExecutionContext {
            root: &root,
            config: &config,
            planning_meta: &planning_meta,
            args: &args,
            store: store.as_ref(),
            cancel: &cancel_for_thread,
        };
        let result = execute_remediation_with_progress(&context, &request, |event| {
            let _ = tx.send(event);
        });

        if let Err(error) = result {
            let _ = tx.send(ReviewExecutionEvent::Failed {
                kind: InteractiveSessionKind::Review,
                candidate: request.candidate,
                error: error.to_string(),
            });
        }
    });
    InteractiveWorkerHandle {
        kind: InteractiveSessionKind::Review,
        pr_number,
        receiver: rx,
        cancel,
    }
}

fn execute_review_with_progress(
    context: &ReviewExecutionContext<'_>,
    initial_candidate: &ReviewLaunchCandidate,
    mut emit: impl FnMut(ReviewExecutionEvent),
) -> Result<()> {
    let gh = GhCli;
    let mut candidate = initial_candidate.clone();
    let mut session =
        review_session_from_candidate(&candidate, ReviewPhase::Claimed, "Queued for review");
    persist_review_session(context.store, &session)?;

    emit(ReviewExecutionEvent::Progress {
        kind: InteractiveSessionKind::Review,
        candidate: candidate.clone(),
        phase: ReviewPhase::Claimed,
        summary: format!("Queued review for PR #{}", candidate.pr_number),
        note: Some("Human approval received. Starting review workflow.".to_string()),
        remediation_required: None,
    });
    if context.cancel.load(Ordering::Relaxed) {
        emit(ReviewExecutionEvent::Cancelled {
            kind: InteractiveSessionKind::Review,
            candidate: candidate.clone(),
            summary: format!("Cancelled review for PR #{}", candidate.pr_number),
            note: "Review cancelled before loading PR context.".to_string(),
            review_output: None,
            remediation_required: None,
        });
        return Ok(());
    }

    let pr = fetch_pr_metadata(&gh, context.root, candidate.pr_number)?;
    candidate = candidate_from_metadata(&pr);
    session = review_session_from_candidate(
        &candidate,
        ReviewPhase::ReviewStarted,
        "Loading pull request and Linear context",
    );
    persist_review_session(context.store, &session)?;
    emit(ReviewExecutionEvent::Progress {
        kind: InteractiveSessionKind::Review,
        candidate: candidate.clone(),
        phase: ReviewPhase::ReviewStarted,
        summary: format!("Loading review context for PR #{}", candidate.pr_number),
        note: Some("Resolving PR metadata, diff scope, and linked Linear context.".to_string()),
        remediation_required: None,
    });

    let linear_identifier = resolve_linear_identifier(&pr)?;
    let diff = fetch_pr_diff(context.root, candidate.pr_number)?;
    let context_bundle = load_codebase_context_bundle(context.root).unwrap_or_default();
    let workflow_contract = load_workflow_contract(context.root).unwrap_or_default();
    let repo_map = render_repo_map(context.root).unwrap_or_default();
    let ticket_context = gather_linear_ticket_context(
        context.root,
        context.config,
        context.planning_meta,
        &linear_identifier,
    )?;

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
        root: Some(context.root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_AGENTS_REVIEW.to_string()),
        agent: context.args.agent.clone(),
        prompt: review_prompt,
        instructions: Some(REVIEW_INSTRUCTIONS.to_string()),
        model: context.args.model.clone(),
        reasoning: context.args.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    };
    let invocation =
        resolve_agent_invocation_for_planning(context.config, context.planning_meta, &agent_args)?;

    candidate.linear_identifier = Some(linear_identifier.clone());
    candidate.linear_error = None;
    session = review_session_from_candidate(
        &candidate,
        ReviewPhase::Running,
        format!("Running agent review with {}", invocation.agent),
    );
    persist_review_session(context.store, &session)?;
    emit(ReviewExecutionEvent::Progress {
        kind: InteractiveSessionKind::Review,
        candidate: candidate.clone(),
        phase: ReviewPhase::Running,
        summary: format!("Running agent review with {}", invocation.agent),
        note: Some(format!(
            "Provider `{}` is auditing the PR against repository context and ticket criteria.",
            invocation.agent
        )),
        remediation_required: None,
    });

    let report = run_agent_capture(&agent_args)?;
    let review_output = report.stdout.trim().to_string();
    let remediation_required = review_output_requires_remediation(&review_output);
    if context.cancel.load(Ordering::Relaxed) {
        emit(ReviewExecutionEvent::Cancelled {
            kind: InteractiveSessionKind::Review,
            candidate: candidate.clone(),
            summary: format!("Cancelled review for PR #{}", candidate.pr_number),
            note: "Review cancelled after the agent finished. Keeping the report without further action."
                .to_string(),
            review_output: Some(review_output),
            remediation_required: Some(remediation_required),
        });
        return Ok(());
    }

    let completion_phase = if remediation_required {
        ReviewPhase::ReviewComplete
    } else {
        ReviewPhase::Completed
    };
    let outcome = InteractiveReviewOutcome {
        kind: InteractiveSessionKind::Review,
        candidate: candidate.clone(),
        summary: if remediation_required {
            "Review report ready".to_string()
        } else {
            "No remediation required".to_string()
        },
        review_output,
        follow_up_ticket_set: None,
        remediation_required,
        linear_identifier: Some(linear_identifier.clone()),
        remediation_pr_number: None,
        remediation_pr_url: None,
    };
    session = review_session_from_candidate(
        &candidate,
        completion_phase,
        if remediation_required {
            "Review report ready"
        } else {
            "No remediation required"
        },
    );
    session.remediation_required = Some(remediation_required);
    session.linear_identifier = Some(linear_identifier);
    session.review_output = Some(outcome.review_output.clone());
    persist_review_session(context.store, &session)?;
    emit(ReviewExecutionEvent::Completed(outcome));

    Ok(())
}

fn execute_remediation_with_progress(
    context: &ReviewExecutionContext<'_>,
    request: &RemediationLaunchRequest,
    mut emit: impl FnMut(ReviewExecutionEvent),
) -> Result<()> {
    if context.cancel.load(Ordering::Relaxed) {
        emit(ReviewExecutionEvent::Cancelled {
            kind: InteractiveSessionKind::Review,
            candidate: request.candidate.clone(),
            summary: format!(
                "Cancelled remediation for PR #{}",
                request.candidate.pr_number
            ),
            note: "Remediation was cancelled before the PR was created.".to_string(),
            review_output: Some(request.review_output.clone()),
            remediation_required: Some(true),
        });
        return Ok(());
    }

    let gh = GhCli;
    let pr = fetch_pr_metadata(&gh, context.root, request.candidate.pr_number)?;

    // FixAgentPending -> FixAgentInProgress
    let mut session = review_session_from_candidate(
        &request.candidate,
        ReviewPhase::FixAgentPending,
        format!("Fix agent pending for PR #{}", request.candidate.pr_number),
    );
    session.review_output = Some(request.review_output.clone());
    session.remediation_required = Some(true);
    session.linear_identifier = Some(request.linear_identifier.clone());
    persist_review_session(context.store, &session)?;
    emit(ReviewExecutionEvent::Progress {
        kind: InteractiveSessionKind::Review,
        candidate: request.candidate.clone(),
        phase: ReviewPhase::FixAgentPending,
        summary: format!("Fix agent pending for PR #{}", request.candidate.pr_number),
        note: Some("Preparing remediation workspace and branch for the fix agent.".to_string()),
        remediation_required: Some(true),
    });

    session.phase = ReviewPhase::FixAgentInProgress;
    session.summary = format!("Fix agent running for PR #{}", request.candidate.pr_number);
    session.updated_at_epoch_seconds = now_epoch_seconds();
    session.review_output = Some(request.review_output.clone());
    persist_review_session(context.store, &session)?;
    emit(ReviewExecutionEvent::Progress {
        kind: InteractiveSessionKind::Review,
        candidate: request.candidate.clone(),
        phase: ReviewPhase::FixAgentInProgress,
        summary: format!("Fix agent running for PR #{}", request.candidate.pr_number),
        note: Some(
            "Running the fix agent to apply required changes from the review report.".to_string(),
        ),
        remediation_required: Some(true),
    });

    let remediation_context = RemediationContext {
        root: context.root,
        pr: &pr,
        linear_identifier: &request.linear_identifier,
        review_output: &request.review_output,
        config: context.config,
        planning_meta: context.planning_meta,
        args: context.args,
    };

    match run_remediation_with_retry(
        &remediation_context,
        Some(context.cancel),
        |attempt, error| {
            emit(ReviewExecutionEvent::Progress {
                kind: InteractiveSessionKind::Review,
                candidate: request.candidate.clone(),
                phase: ReviewPhase::FixAgentInProgress,
                summary: format!("Fix agent retrying for PR #{}", request.candidate.pr_number),
                note: Some(format!(
                    "Remediation attempt #{attempt} failed: {error}. Retrying until the PR is created or you cancel the session."
                )),
                remediation_required: Some(true),
            });
        },
    ) {
        Ok(remediation) => {
            let outcome = InteractiveReviewOutcome {
                kind: InteractiveSessionKind::Review,
                candidate: request.candidate.clone(),
                summary: "Remediation PR created".to_string(),
                review_output: request.review_output.clone(),
                follow_up_ticket_set: None,
                remediation_required: true,
                linear_identifier: Some(request.linear_identifier.clone()),
                remediation_pr_number: Some(remediation.pr_number),
                remediation_pr_url: Some(remediation.pr_url),
            };
            session = review_session_from_candidate(
                &request.candidate,
                ReviewPhase::FixAgentComplete,
                "Remediation PR created",
            );
            session.remediation_required = Some(true);
            session.remediation_pr_number = outcome.remediation_pr_number;
            session.remediation_pr_url = outcome.remediation_pr_url.clone();
            session.linear_identifier = Some(request.linear_identifier.clone());
            session.review_output = Some(request.review_output.clone());
            persist_review_session(context.store, &session)?;
            emit(ReviewExecutionEvent::Completed(outcome));
        }
        Err(error) => {
            session = review_session_from_candidate(
                &request.candidate,
                ReviewPhase::FixAgentFailed,
                format!("Fix agent failed: {error}"),
            );
            session.remediation_required = Some(true);
            session.linear_identifier = Some(request.linear_identifier.clone());
            session.review_output = Some(request.review_output.clone());
            persist_review_session(context.store, &session)?;
            emit(ReviewExecutionEvent::Failed {
                kind: InteractiveSessionKind::Review,
                candidate: request.candidate.clone(),
                error: format!(
                    "Fix agent failed for PR #{}: {error}",
                    request.candidate.pr_number
                ),
            });
            return Ok(());
        }
    }

    Ok(())
}

fn execute_follow_up_ticket_with_progress(
    context: &ReviewExecutionContext<'_>,
    initial_candidate: &ReviewLaunchCandidate,
    mut emit: impl FnMut(ReviewExecutionEvent),
) -> Result<()> {
    let gh = GhCli;
    let mut candidate = initial_candidate.clone();
    let mut session = review_session_from_candidate(
        &candidate,
        ReviewPhase::Claimed,
        "Queued for follow-up ticket analysis",
    );
    persist_review_session(context.store, &session)?;

    emit(ReviewExecutionEvent::Progress {
        kind: InteractiveSessionKind::FollowUpTickets,
        candidate: candidate.clone(),
        phase: ReviewPhase::Claimed,
        summary: format!(
            "Queued follow-up ticket analysis for PR #{}",
            candidate.pr_number
        ),
        note: Some("Human approval received. Starting follow-up ticket analysis.".to_string()),
        remediation_required: None,
    });
    if context.cancel.load(Ordering::Relaxed) {
        emit(ReviewExecutionEvent::Cancelled {
            kind: InteractiveSessionKind::FollowUpTickets,
            candidate: candidate.clone(),
            summary: format!("Cancelled ticket analysis for PR #{}", candidate.pr_number),
            note: "Ticket analysis cancelled before loading PR context.".to_string(),
            review_output: None,
            remediation_required: None,
        });
        return Ok(());
    }

    let pr = fetch_pr_metadata(&gh, context.root, candidate.pr_number)?;
    candidate = candidate_from_metadata(&pr);
    session = review_session_from_candidate(
        &candidate,
        ReviewPhase::ReviewStarted,
        "Loading PR context for follow-up ticket analysis",
    );
    persist_review_session(context.store, &session)?;
    emit(ReviewExecutionEvent::Progress {
        kind: InteractiveSessionKind::FollowUpTickets,
        candidate: candidate.clone(),
        phase: ReviewPhase::ReviewStarted,
        summary: format!("Loading ticket recommendation context for PR #{}", candidate.pr_number),
        note: Some(
            "Resolving PR metadata, diff scope, and linked ticket context for future work suggestions."
                .to_string(),
        ),
        remediation_required: None,
    });

    let linear_identifier = resolve_linear_identifier(&pr)?;
    let diff = fetch_pr_diff(context.root, candidate.pr_number)?;
    let context_bundle = load_codebase_context_bundle(context.root).unwrap_or_default();
    let workflow_contract = load_workflow_contract(context.root).unwrap_or_default();
    let repo_map = render_repo_map(context.root).unwrap_or_default();
    let ticket_context = gather_linear_ticket_context(
        context.root,
        context.config,
        context.planning_meta,
        &linear_identifier,
    )?;

    let prompt = assemble_follow_up_linear_prompt(
        &pr,
        &linear_identifier,
        &diff,
        &context_bundle,
        &workflow_contract,
        &repo_map,
        &ticket_context,
    );
    let agent_args = RunAgentArgs {
        root: Some(context.root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_AGENTS_REVIEW.to_string()),
        agent: context.args.agent.clone(),
        prompt,
        instructions: Some(VIEW_LINEAR_INSTRUCTIONS.to_string()),
        model: context.args.model.clone(),
        reasoning: context.args.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    };
    let invocation =
        resolve_agent_invocation_for_planning(context.config, context.planning_meta, &agent_args)?;

    candidate.linear_identifier = Some(linear_identifier.clone());
    candidate.linear_error = None;
    session = review_session_from_candidate(
        &candidate,
        ReviewPhase::Running,
        format!(
            "Running follow-up ticket analysis with {}",
            invocation.agent
        ),
    );
    persist_review_session(context.store, &session)?;
    emit(ReviewExecutionEvent::Progress {
        kind: InteractiveSessionKind::FollowUpTickets,
        candidate: candidate.clone(),
        phase: ReviewPhase::Running,
        summary: format!(
            "Running follow-up ticket analysis with {}",
            invocation.agent
        ),
        note: Some(
            "Analyzing the PR for non-blocking future Linear ticket recommendations.".to_string(),
        ),
        remediation_required: None,
    });

    let report = run_agent_capture(&agent_args)?;
    let ticket_set = normalize_follow_up_ticket_set(parse_follow_up_ticket_set(&report.stdout)?)?;
    let review_output = render_follow_up_ticket_set_markdown(&ticket_set);
    if context.cancel.load(Ordering::Relaxed) {
        emit(ReviewExecutionEvent::Cancelled {
            kind: InteractiveSessionKind::FollowUpTickets,
            candidate: candidate.clone(),
            summary: format!("Cancelled ticket analysis for PR #{}", candidate.pr_number),
            note: "Ticket analysis cancelled after the agent finished. Keeping the report."
                .to_string(),
            review_output: Some(review_output),
            remediation_required: None,
        });
        return Ok(());
    }

    let outcome = InteractiveReviewOutcome {
        kind: InteractiveSessionKind::FollowUpTickets,
        candidate: candidate.clone(),
        summary: "Follow-up ticket recommendations ready".to_string(),
        review_output,
        follow_up_ticket_set: Some(ticket_set),
        remediation_required: false,
        linear_identifier: Some(linear_identifier.clone()),
        remediation_pr_number: None,
        remediation_pr_url: None,
    };
    session = review_session_from_candidate(
        &candidate,
        ReviewPhase::Completed,
        "Follow-up ticket recommendations ready",
    );
    session.linear_identifier = Some(linear_identifier);
    persist_review_session(context.store, &session)?;
    emit(ReviewExecutionEvent::Completed(outcome));

    Ok(())
}

fn spawn_follow_up_ticket_revision_job(
    root: std::path::PathBuf,
    args: ReviewRunArgs,
    review: FollowUpTicketReviewApp,
) -> Receiver<FollowUpTicketJobEvent> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = revise_follow_up_ticket_plan(&root, &args, &review).map(|plan| {
            let mut next = FollowUpTicketReviewApp::new(review.candidate.clone(), plan);
            next.revision = review.revision + 1;
            FollowUpTicketJobEvent::RevisionReady(Box::new(next))
        });

        let event = match result {
            Ok(event) => event,
            Err(error) => FollowUpTicketJobEvent::Failed {
                pr_number: review.pr_number,
                error: error.to_string(),
            },
        };
        let _ = sender.send(event);
    });
    receiver
}

fn spawn_follow_up_ticket_create_job(
    root: std::path::PathBuf,
    config: AppConfig,
    planning_meta: PlanningMeta,
    review: FollowUpTicketReviewApp,
) -> Receiver<FollowUpTicketJobEvent> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result =
            create_follow_up_linear_issues(&root, &config, &planning_meta, &review).map(|issues| {
                FollowUpTicketJobEvent::Created {
                    pr_number: review.pr_number,
                    issues,
                }
            });
        let event = match result {
            Ok(event) => event,
            Err(error) => FollowUpTicketJobEvent::Failed {
                pr_number: review.pr_number,
                error: error.to_string(),
            },
        };
        let _ = sender.send(event);
    });
    receiver
}

fn revise_follow_up_ticket_plan(
    root: &Path,
    args: &ReviewRunArgs,
    review: &FollowUpTicketReviewApp,
) -> Result<FollowUpTicketSet> {
    let prompt = assemble_follow_up_ticket_revision_prompt(root, review)?;
    let output = run_agent_capture(&RunAgentArgs {
        root: Some(root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_AGENTS_REVIEW.to_string()),
        agent: args.agent.clone(),
        prompt,
        instructions: Some(VIEW_LINEAR_INSTRUCTIONS.to_string()),
        model: args.model.clone(),
        reasoning: args.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    })?;
    normalize_follow_up_ticket_set(parse_follow_up_ticket_set(&output.stdout)?)
}

fn create_follow_up_linear_issues(
    root: &Path,
    app_config: &AppConfig,
    planning_meta: &PlanningMeta,
    review: &FollowUpTicketReviewApp,
) -> Result<Vec<IssueSummary>> {
    let remembered_selection = load_remembered_backlog_selection(root)?;
    let plan = selected_follow_up_ticket_plan(review);
    let label = planning_meta.effective_plan_label(app_config);
    let linear_config = LinearConfig::from_sources(
        app_config,
        planning_meta,
        Some(root),
        LinearConfigOverrides::default(),
    )?;
    let default_team = linear_config.default_team.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to initialize Linear runtime for follow-up ticket creation")?;

    runtime.block_on(async move {
        let service = LinearService::new(ReqwestLinearClient::new(linear_config)?, default_team);
        let mut created = Vec::with_capacity(plan.tickets.len());
        for ticket in &plan.tickets {
            let resolved_defaults = resolve_plan_ticket_defaults(
                app_config,
                planning_meta,
                &remembered_selection,
                &PlanTicketResolutionInput {
                    zero_prompt: false,
                    explicit_team: None,
                    explicit_project: None,
                    overrides: TicketOptionOverrides::default(),
                    built_in_label: label.clone(),
                    generated_priority: ticket.priority,
                },
            );
            let assignee_id = service
                .resolve_assignee_id(resolved_defaults.assignee.as_deref())
                .await?;
            let issue = service
                .create_issue(IssueCreateSpec {
                    team: resolved_defaults.team.clone(),
                    title: ticket.title.clone(),
                    description: Some(render_follow_up_ticket_issue_description(
                        &review.candidate,
                        ticket,
                    )),
                    project: resolved_defaults.project.clone(),
                    project_id: resolved_defaults.project_id.clone(),
                    parent_id: None,
                    state: resolved_defaults.state.clone(),
                    priority: resolved_defaults.priority,
                    assignee_id,
                    labels: resolved_defaults.labels.clone(),
                })
                .await?;
            if let Err(error) = save_remembered_backlog_selection(root, &issue) {
                eprintln!("warning: failed to persist remembered backlog defaults: {error}");
            }
            created.push(issue);
        }
        Ok::<Vec<IssueSummary>, anyhow::Error>(created)
    })
}

fn render_follow_up_ticket_issue_description(
    candidate: &ReviewLaunchCandidate,
    ticket: &FollowUpTicketDraft,
) -> String {
    let mut lines = vec![
        format!(
            "Follow-up work identified from PR #{}: {}",
            candidate.pr_number, candidate.title
        ),
        String::new(),
        "## Why Now".to_string(),
        ticket.why_now.clone(),
        String::new(),
        "## Outcome".to_string(),
        ticket.outcome.clone(),
        String::new(),
        "## Scope".to_string(),
        ticket.scope.clone(),
    ];
    if !ticket.acceptance_criteria.is_empty() {
        lines.push(String::new());
        lines.push("## Suggested Acceptance Criteria".to_string());
        lines.extend(
            ticket
                .acceptance_criteria
                .iter()
                .map(|criterion| format!("- {criterion}")),
        );
    }
    lines.join("\n")
}

/// Parse agent output into a [`FollowUpTicketSet`], tolerating markdown code
/// fences and surrounding prose that Claude commonly wraps around JSON.
///
/// Tries, in order: raw trimmed input, code-fence-stripped input, and
/// first-`{`-to-last-`}` extraction.
fn parse_follow_up_ticket_set(output: &str) -> Result<FollowUpTicketSet> {
    let trimmed = output.trim();
    let mut candidates = vec![trimmed.to_string()];

    if let Some(stripped) = strip_follow_up_code_fence(trimmed) {
        candidates.push(stripped);
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}'))
        && start <= end
    {
        candidates.push(trimmed[start..=end].to_string());
    }

    for candidate in candidates {
        if let Ok(parsed) = serde_json::from_str::<FollowUpTicketSet>(&candidate) {
            return Ok(parsed);
        }
    }

    bail!(
        "follow-up ticket analysis returned invalid JSON: {}",
        preview_follow_up_text(trimmed)
    )
}

fn strip_follow_up_code_fence(raw: &str) -> Option<String> {
    let stripped = raw.strip_prefix("```")?;
    let stripped = stripped
        .strip_prefix("json\n")
        .or_else(|| stripped.strip_prefix("JSON\n"))
        .or_else(|| stripped.strip_prefix('\n'))
        .unwrap_or(stripped);
    let stripped = stripped.strip_suffix("```")?;
    Some(stripped.trim().to_string())
}

fn preview_follow_up_text(value: &str) -> String {
    const MAX_PREVIEW_LEN: usize = 240;
    if value.len() <= MAX_PREVIEW_LEN {
        value.to_string()
    } else {
        format!("{}...", &value[..MAX_PREVIEW_LEN])
    }
}

fn normalize_follow_up_ticket_set(parsed: FollowUpTicketSet) -> Result<FollowUpTicketSet> {
    let normalized = FollowUpTicketSet {
        summary: parsed.summary.trim().to_string(),
        tickets: parsed
            .tickets
            .into_iter()
            .map(|ticket| FollowUpTicketDraft {
                title: ticket.title.trim().to_string(),
                why_now: ticket.why_now.trim().to_string(),
                outcome: ticket.outcome.trim().to_string(),
                scope: ticket.scope.trim().to_string(),
                acceptance_criteria: ticket
                    .acceptance_criteria
                    .into_iter()
                    .map(|criterion| criterion.trim().to_string())
                    .filter(|criterion| !criterion.is_empty())
                    .collect(),
                priority: ticket.priority,
            })
            .filter(|ticket| !ticket.title.is_empty())
            .collect(),
        notes: parsed
            .notes
            .into_iter()
            .map(|note| note.trim().to_string())
            .filter(|note| !note.is_empty())
            .collect(),
    };

    Ok(normalized)
}

fn render_follow_up_ticket_set_markdown(plan: &FollowUpTicketSet) -> String {
    let mut lines = vec![
        "## Follow-Up Linear Recommendations".to_string(),
        String::new(),
        "### Summary".to_string(),
        plan.summary.clone(),
        String::new(),
        "### Recommended Tickets".to_string(),
    ];
    if plan.tickets.is_empty() {
        lines.push("No strong follow-up tickets recommended.".to_string());
    } else {
        for (index, ticket) in plan.tickets.iter().enumerate() {
            lines.push(format!("{}. **Title**: {}", index + 1, ticket.title));
            lines.push(format!("   - **Why now**: {}", ticket.why_now));
            lines.push(format!("   - **Outcome**: {}", ticket.outcome));
            lines.push(format!("   - **Scope**: {}", ticket.scope));
            lines.push("   - **Suggested acceptance criteria**:".to_string());
            if ticket.acceptance_criteria.is_empty() {
                lines.push("     - None provided".to_string());
            } else {
                lines.extend(
                    ticket
                        .acceptance_criteria
                        .iter()
                        .map(|criterion| format!("     - {criterion}")),
                );
            }
        }
    }
    lines.push(String::new());
    lines.push("### Nice-To-Have Notes".to_string());
    if plan.notes.is_empty() {
        lines.push("None.".to_string());
    } else {
        lines.extend(plan.notes.iter().map(|note| format!("- {note}")));
    }
    lines.join("\n")
}

fn assemble_follow_up_ticket_revision_prompt(
    root: &Path,
    review: &FollowUpTicketReviewApp,
) -> Result<String> {
    let workflow_contract = load_workflow_contract(root).unwrap_or_default();
    let context_bundle = load_codebase_context_bundle(root).unwrap_or_default();
    let current_plan = serde_json::to_string_pretty(&review.plan)
        .context("failed to serialize follow-up ticket draft for revision")?;
    let kept_tickets = follow_up_ticket_kept_indices(review)
        .into_iter()
        .filter_map(|index| review.plan.tickets.get(index))
        .cloned()
        .collect::<Vec<_>>();
    let kept_tickets_json = serde_json::to_string_pretty(&kept_tickets)
        .context("failed to serialize standalone follow-up tickets for revision")?;
    let merge_plan = follow_up_ticket_merge_groups(review)
        .iter()
        .map(|(group, indices)| {
            let tickets = indices
                .iter()
                .filter_map(|index| review.plan.tickets.get(*index))
                .cloned()
                .collect::<Vec<_>>();
            serde_json::json!({
                "group": group,
                "tickets": tickets,
            })
        })
        .collect::<Vec<_>>();
    let merge_plan_json = serde_json::to_string_pretty(&merge_plan)
        .context("failed to serialize follow-up merge groups for revision")?;

    Ok(format!(
        "You are revising follow-up Linear ticket recommendations for the active repository.\n\n\
PR context:\n- Number: #{pr_number}\n- Title: {title}\n- URL: {url}\n- Linked Linear ticket: {linear}\n\n\
Workflow contract:\n{workflow_contract}\n\n\
Repository context:\n{context_bundle}\n\n\
Current draft recommendation JSON:\n{current_plan}\n\n\
Selected standalone tickets to preserve:\n{kept_tickets_json}\n\n\
Merge groups:\n{merge_plan_json}\n\n\
Rebuild the next recommendation set from only the selected standalone tickets plus the numbered merge groups. Tickets omitted from both lists were intentionally skipped and must not appear in the rebuilt output. For each merge group, combine all tickets in that group into exactly one replacement ticket unless a tiny wording edit is needed for coherence.\n\n\
Return JSON only using this exact shape:\n\
{{\n  \"summary\":\"One paragraph summary of the overall recommendation set\",\n  \"tickets\":[\n    {{\n      \"title\":\"Issue title\",\n      \"why_now\":\"Why this PR makes the follow-up timely\",\n      \"outcome\":\"What shipping the ticket improves\",\n      \"scope\":\"Concrete scope boundaries\",\n      \"acceptance_criteria\":[\"criterion one\",\"criterion two\"],\n      \"priority\": 2\n    }}\n  ],\n  \"notes\":[\"Optional extra note\"]\n}}",
        pr_number = review.pr_number,
        title = review.candidate.title,
        url = review.candidate.url,
        linear = review
            .candidate
            .linear_identifier
            .clone()
            .unwrap_or_else(|| "unresolved".to_string()),
    ))
}

fn persist_review_session(
    store: Option<&ReviewProjectStore>,
    session: &ReviewSession,
) -> Result<()> {
    if let Some(store) = store {
        let session = session.clone();
        store.update_state(|state| {
            state.upsert(session);
        })?;
    }
    Ok(())
}

fn review_session_from_candidate(
    candidate: &ReviewLaunchCandidate,
    phase: ReviewPhase,
    summary: impl Into<String>,
) -> ReviewSession {
    ReviewSession {
        pr_number: candidate.pr_number,
        pr_title: candidate.title.clone(),
        pr_url: Some(candidate.url.clone()),
        pr_author: Some(candidate.author.clone()),
        head_branch: Some(candidate.head_ref.clone()),
        base_branch: Some(candidate.base_ref.clone()),
        linear_identifier: candidate.linear_identifier.clone(),
        phase,
        summary: summary.into(),
        updated_at_epoch_seconds: now_epoch_seconds(),
        review_output: None,
        remediation_required: None,
        remediation_pr_number: None,
        remediation_pr_url: None,
    }
}

fn interactive_preview_viewport(area: Rect) -> Rect {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Length(4),
            Constraint::Min(0),
            Constraint::Length(5),
        ])
        .split(area);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
        .split(outer[2]);
    body[1]
}

fn render_interactive_review(frame: &mut ratatui::Frame<'_>, app: &InteractiveReviewApp) {
    if app.stage == InteractiveReviewStage::TicketReview {
        if let Some(review) = app.ticket_review.as_ref() {
            render_follow_up_ticket_review(frame, review);
        }
        return;
    }
    if app.stage == InteractiveReviewStage::TicketLoading {
        render_loading_panel(
            frame,
            frame.area(),
            &crate::progress::LoadingPanelData {
                title: "Follow-Up Ticket Flow [loading]".to_string(),
                message: app.status.clone(),
                detail: "MetaStack is rebuilding the curated ticket preview or creating the selected issues in Linear."
                    .to_string(),
                spinner_index: 0,
                status_line:
                    "State: loading. The dashboard advances automatically when the job completes."
                        .to_string(),
            },
        );
        return;
    }
    let narrow = frame.area().width < 110;
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Length(4),
            Constraint::Min(0),
            Constraint::Length(5),
        ])
        .split(frame.area());
    let body = Layout::default()
        .direction(if narrow {
            Direction::Vertical
        } else {
            Direction::Horizontal
        })
        .constraints(if narrow {
            vec![Constraint::Percentage(46), Constraint::Percentage(54)]
        } else {
            vec![Constraint::Percentage(44), Constraint::Percentage(56)]
        })
        .split(outer[2]);
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(5), Constraint::Min(0)])
        .split(body[0]);

    let status_badge = match app.stage {
        InteractiveReviewStage::Loading => badge("loading", Tone::Info),
        InteractiveReviewStage::Confirm => badge("confirm", Tone::Accent),
        InteractiveReviewStage::Select if app.active_session_count() > 0 => {
            badge("running", Tone::Info)
        }
        InteractiveReviewStage::Select => badge("select", Tone::Accent),
        InteractiveReviewStage::TicketReview => badge("review", Tone::Accent),
        InteractiveReviewStage::TicketLoading => badge("loading", Tone::Info),
        InteractiveReviewStage::Empty => badge("empty", Tone::Muted),
    };
    let header = paragraph(
        Text::from(vec![
            Line::from(vec![
                status_badge,
                Span::raw(" "),
                Span::styled(app.command.dashboard_title(), emphasis_style()),
            ]),
            Line::from(app.status.clone()),
            {
                let mut spans = vec![
                    Span::styled("Mode ", label_style()),
                    Span::raw(match app.mode {
                        InteractiveReviewMode::Direct => "single PR".to_string(),
                        InteractiveReviewMode::Discovery => "guided queue".to_string(),
                    }),
                    Span::styled("  Candidates ", label_style()),
                    Span::raw(app.visible_candidate_indices().len().to_string()),
                    Span::styled("  Active ", label_style()),
                    Span::raw(app.active_session_count().to_string()),
                ];
                if app.filter.is_active() {
                    spans.push(Span::raw("  "));
                    spans.push(badge("filtered", Tone::Accent));
                }
                Line::from(spans)
            },
        ]),
        panel_title(
            match app.command {
                ReviewCommandKind::Review => "Review Flow",
                ReviewCommandKind::Retro => "Retro Flow",
            },
            false,
        ),
    );
    frame.render_widget(header, outer[0]);

    render_interactive_navigation(frame, outer[1], app);

    render_interactive_secondary_panel(frame, left[0], app);
    render_interactive_primary_list(frame, left[1], app);

    let preview = scrollable_content_paragraph(
        if app.tab == InteractiveReviewTab::Candidates {
            app.selected_candidate_text()
        } else {
            app.selected_session_text()
        },
        panel_title(
            match app.tab {
                InteractiveReviewTab::Candidates => "Selected PR Preview",
                InteractiveReviewTab::Sessions => "Session Detail",
            },
            matches!(
                app.focus,
                InteractiveReviewFocus::CandidatePreview | InteractiveReviewFocus::SessionPreview
            ),
        ),
        if app.tab == InteractiveReviewTab::Candidates {
            &app.preview_scroll
        } else {
            &app.session_preview_scroll
        },
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(preview, body[1]);

    let footer = paragraph(interactive_footer_text(app), panel_title("Controls", false))
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, outer[3]);

    // Filter panel overlay (retro flow only).
    if app.filter_panel_open {
        render_filter_panel_overlay(frame, app);
    }
}

fn render_follow_up_ticket_review(frame: &mut ratatui::Frame<'_>, app: &FollowUpTicketReviewApp) {
    let layout = follow_up_ticket_review_layout(frame.area());

    let items = app
        .plan
        .tickets
        .iter()
        .enumerate()
        .map(|(index, ticket)| {
            ListItem::new(format!(
                "{} {}",
                follow_up_ticket_review_marker(
                    app.decisions.get(index).copied().unwrap_or_default()
                ),
                ticket.title
            ))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    if !app.plan.tickets.is_empty() {
        state.select(Some(
            app.selected.min(app.plan.tickets.len().saturating_sub(1)),
        ));
    }
    let list_widget = list(
        items,
        panel_title(
            "Suggested Tickets",
            app.focus == FollowUpTicketReviewFocus::Tickets,
        ),
    );
    frame.render_stateful_widget(list_widget, layout.issue_list, &mut state);

    let detail = scrollable_content_paragraph(
        app.selected_ticket_text(),
        panel_title(
            if app.focus == FollowUpTicketReviewFocus::SelectedTicket {
                "Selected Ticket [scroll]"
            } else {
                "Selected Ticket"
            },
            app.focus == FollowUpTicketReviewFocus::SelectedTicket,
        ),
        &app.selected_ticket_scroll,
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(detail, layout.selected_ticket);

    let overview = scrollable_content_paragraph(
        app.overview_text(),
        panel_title(
            if app.focus == FollowUpTicketReviewFocus::Overview {
                "Overview [scroll]"
            } else {
                "Overview"
            },
            app.focus == FollowUpTicketReviewFocus::Overview,
        ),
        &app.overview_scroll,
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(overview, layout.overview);

    let merge = scrollable_content_paragraph(
        app.combination_plan_text(),
        panel_title(
            if app.focus == FollowUpTicketReviewFocus::CombinationPlan {
                "Combination Plan [scroll]"
            } else {
                "Combination Plan"
            },
            app.focus == FollowUpTicketReviewFocus::CombinationPlan,
        ),
        &app.combination_scroll,
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(merge, layout.combination_plan);

    let help = paragraph(
        Text::from(vec![
            Line::from(
                "Tab/Shift-Tab changes review focus. Space cycles [ ] skip -> [x] keep -> [1] -> [2] for the active ticket.",
            ),
            Line::from(
                "Enter creates the checked batch or rebuilds the next preview when merge groups are present. `x` keeps, `s` skips, `u` clears all marks, and Esc returns to sessions.",
            ),
            if let Some(error) = app.error.as_deref() {
                Line::from(format!("Error: {error}"))
            } else {
                Line::from("")
            },
        ]),
        panel_title("Controls", false),
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(help, layout.footer);
}

fn render_interactive_navigation(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &InteractiveReviewApp,
) {
    let text = Text::from(vec![
        Line::from(vec![
            Span::styled("View ", label_style()),
            tab_label(
                "Candidates",
                app.tab == InteractiveReviewTab::Candidates,
                app.visible_candidate_indices().len(),
            ),
            Span::raw("  "),
            tab_label(
                "Sessions",
                app.tab == InteractiveReviewTab::Sessions,
                app.sessions.len(),
            ),
        ]),
        Line::from(vec![
            Span::styled("Focus ", label_style()),
            badge(interactive_focus_label(app.focus), Tone::Accent),
            Span::raw("  "),
            Span::styled("Tab ", label_style()),
            Span::raw("rotate panes"),
            Span::raw("  "),
            Span::styled("Esc ", label_style()),
            Span::raw("back to candidates"),
        ]),
    ]);
    frame.render_widget(
        paragraph(text, panel_title("Navigation", false)).wrap(Wrap { trim: false }),
        area,
    );
}

fn render_interactive_secondary_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &InteractiveReviewApp,
) {
    match app.tab {
        InteractiveReviewTab::Candidates => {
            let rendered = app.query.render(
                "Search by PR number, title, author, branch, or Linear identifier...",
                app.focus == InteractiveReviewFocus::CandidateList,
            );
            let title = if app.focus == InteractiveReviewFocus::CandidateList {
                "Candidate Search [active]"
            } else {
                "Candidate Search"
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .title(title)
                .padding(Padding::new(1, 1, 1, 0));
            let inner = block.inner(area);
            frame.render_widget(rendered.paragraph(block), area);
            rendered.set_cursor(frame, inner);
        }
        InteractiveReviewTab::Sessions => {
            let text = Text::from(vec![
                Line::from(vec![
                    Span::styled("Active ", label_style()),
                    Span::raw(app.active_session_count().to_string()),
                    Span::styled("  Completed ", label_style()),
                    Span::raw(
                        app.sessions
                            .iter()
                            .filter(|session| matches!(session.phase, ReviewPhase::Completed))
                            .count()
                            .to_string(),
                    ),
                    Span::styled("  Blocked ", label_style()),
                    Span::raw(
                        app.sessions
                            .iter()
                            .filter(|session| matches!(session.phase, ReviewPhase::Blocked))
                            .count()
                            .to_string(),
                    ),
                ]),
                Line::from(""),
                Line::from(
                    "Press `A` to start a remediation agent PR from a review report, `D` to delete stored sessions, or Esc to return to candidates.",
                ),
            ]);
            let widget = ratatui::widgets::Paragraph::new(text)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(panel_title(
                            "Session Summary",
                            app.focus == InteractiveReviewFocus::SessionList,
                        ))
                        .padding(Padding::new(1, 1, 1, 0)),
                )
                .wrap(Wrap { trim: false });
            frame.render_widget(widget, area);
        }
    }
}

fn render_interactive_primary_list(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &InteractiveReviewApp,
) {
    match app.tab {
        InteractiveReviewTab::Candidates => render_interactive_candidate_list(frame, area, app),
        InteractiveReviewTab::Sessions => render_interactive_session_list(frame, area, app),
    }
}

fn render_interactive_candidate_list(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &InteractiveReviewApp,
) {
    let visible = app.visible_candidate_indices();
    let items = if visible.is_empty() {
        let hint = if app.filter.is_active() {
            "Adjust the search text, press `F` to change filters, or `R` to refresh."
        } else {
            "Adjust the search text or press `R` to refresh the candidate queue."
        };
        vec![ListItem::new(empty_state(
            "No pull requests matched the current candidate query.",
            hint,
        ))]
    } else {
        visible
            .iter()
            .filter_map(|index| app.candidates.get(*index))
            .map(|candidate| {
                let linear = candidate
                    .linear_identifier
                    .clone()
                    .unwrap_or_else(|| "unresolved".to_string());
                let review_active =
                    app.has_active_session(candidate.pr_number, InteractiveSessionKind::Review);
                let ideas_active = app.has_active_session(
                    candidate.pr_number,
                    InteractiveSessionKind::FollowUpTickets,
                );
                let selected = if app.selected_prs.contains(&candidate.pr_number) {
                    badge("selected", Tone::Success)
                } else if review_active && ideas_active {
                    badge("2 active", Tone::Info)
                } else if review_active {
                    badge("review", Tone::Info)
                } else if ideas_active {
                    badge("ideas", Tone::Info)
                } else {
                    badge("ready", Tone::Muted)
                };
                let mut first_line = vec![
                    badge(format!("#{}", candidate.pr_number), Tone::Accent),
                    Span::raw(" "),
                    selected,
                ];
                // Show state badge in retro flow for mixed open/closed candidates.
                if app.command == ReviewCommandKind::Retro {
                    let state_tone = if candidate.candidate_state == "open" {
                        Tone::Success
                    } else {
                        Tone::Muted
                    };
                    first_line.push(Span::raw(" "));
                    first_line.push(badge(candidate.candidate_state.clone(), state_tone));
                }
                first_line.push(Span::raw(" "));
                first_line.push(Span::styled(candidate.title.clone(), emphasis_style()));
                spaced_list_item(vec![
                    Line::from(first_line),
                    Line::from(vec![
                        Span::styled("Linear ", label_style()),
                        Span::raw(linear),
                        Span::styled("  Review ", label_style()),
                        Span::raw(candidate.review_state.clone()),
                    ]),
                    Line::from(Span::styled(
                        format!("{} -> {}", candidate.head_ref, candidate.base_ref),
                        muted_style(),
                    )),
                ])
            })
            .collect()
    };

    let mut state = ListState::default();
    if !visible.is_empty() {
        state.select(Some(
            app.selected_index.min(visible.len().saturating_sub(1)),
        ));
    }

    let title = match app.mode {
        InteractiveReviewMode::Direct => "Review Candidate",
        InteractiveReviewMode::Discovery => "Candidate PRs",
    };
    let widget = spaced_list(
        items,
        panel_title(title, app.focus == InteractiveReviewFocus::CandidateList),
    );
    frame.render_stateful_widget(widget, area, &mut state);
}

fn render_interactive_session_list(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &InteractiveReviewApp,
) {
    let items = if app.sessions.is_empty() {
        vec![ListItem::new(empty_state(
            "No agent sessions have started yet.",
            "Queue one or more review or follow-up-ticket sessions to watch live progress here.",
        ))]
    } else {
        app.sessions
            .iter()
            .map(|session| {
                let tone = match session.phase {
                    ReviewPhase::Completed => Tone::Success,
                    ReviewPhase::Blocked => Tone::Danger,
                    _ => Tone::Info,
                };
                render_github_session_row(
                    vec![
                        badge(format!("#{}", session.candidate.pr_number), Tone::Accent),
                        Span::raw(" "),
                        badge(session.kind.label(), session.kind.tone()),
                        Span::raw(" "),
                        badge(session.phase.display_label(), tone),
                    ],
                    &session.candidate.title,
                    &session.summary,
                )
            })
            .collect()
    };

    let mut state = ListState::default();
    if !app.sessions.is_empty() {
        state.select(Some(
            app.session_index.min(app.sessions.len().saturating_sub(1)),
        ));
    }

    let widget = spaced_list(
        items,
        panel_title(
            "Agent Sessions",
            app.focus == InteractiveReviewFocus::SessionList,
        ),
    );
    frame.render_stateful_widget(widget, area, &mut state);
}

/// Render the lightweight filter panel as a centred overlay.
fn render_filter_panel_overlay(frame: &mut ratatui::Frame<'_>, app: &InteractiveReviewApp) {
    let area = frame.area();
    // Size the popup: 50% width, up to 80% height.
    let popup_width = (area.width / 2).max(40).min(area.width.saturating_sub(4));
    let popup_height = (app.filter_panel_rows.len() as u16 + 10)
        .min(area.height * 80 / 100)
        .max(10);
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup = Rect::new(x, y, popup_width, popup_height);

    // Clear the popup area with a background block.
    let clear = ratatui::widgets::Clear;
    frame.render_widget(clear, popup);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_category: Option<FilterCategory> = None;

    for (index, row) in app.filter_panel_rows.iter().enumerate() {
        // Section header when the category changes.
        if current_category != Some(row.category) {
            if current_category.is_some() {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled(
                row.category.label().to_string(),
                emphasis_style(),
            )));
            current_category = Some(row.category);
        }

        let marker = if row.selected { "[x]" } else { "[ ]" };
        let cursor = if index == app.filter_panel_cursor {
            "> "
        } else {
            "  "
        };
        let style = if index == app.filter_panel_cursor {
            Style::default()
                .fg(ratatui::style::Color::Yellow)
                .add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!("{cursor}{marker} {}", row.value),
            style,
        )));
    }

    lines.push(Line::from(""));
    lines.push(key_hints(&[
        ("Space", "toggle"),
        ("Up/Down", "move"),
        ("C", "clear all"),
        ("F/Enter/Esc", "close"),
    ]));

    let widget = ratatui::widgets::Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(panel_title("Filter Candidates", true))
                .padding(Padding::new(1, 1, 1, 0)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, popup);
}

fn interactive_footer_text(app: &InteractiveReviewApp) -> Text<'static> {
    if let Some(error) = app.error.as_deref() {
        return Text::from(vec![
            Line::from(Span::styled(
                match app.command {
                    ReviewCommandKind::Review => "The review flow failed.",
                    ReviewCommandKind::Retro => "The retro flow failed.",
                },
                emphasis_style(),
            )),
            Line::from(""),
            Line::from(error.to_string()),
            Line::from(""),
            key_hints(&[("Esc", "back"), ("q", "exit")]),
        ]);
    }

    match app.stage {
        InteractiveReviewStage::Loading => Text::from(vec![
            Line::from(match app.command {
                ReviewCommandKind::Review => {
                    "The dashboard is gathering discovery and prerequisite state."
                }
                ReviewCommandKind::Retro => {
                    "The dashboard is gathering retro candidate and prerequisite state."
                }
            }),
            Line::from(""),
            Line::from(
                "Stay in this screen while MetaStack verifies auth, loads PR metadata, and prepares review previews.",
            ),
            Line::from(""),
            key_hints(&interactive_key_hints(app)),
        ]),
        InteractiveReviewStage::Select => Text::from(match app.command {
            ReviewCommandKind::Review => vec![
                Line::from(
                    "Search candidates, mark PRs with Space, then press Enter to queue reviews.",
                ),
                Line::from(
                    "After a review finishes, switch to Sessions and press `A` to launch the remediation agent PR.",
                ),
                interactive_action_line(app),
                Line::from("Use the Navigation strip to track the active view and focused pane."),
                Line::from(""),
                key_hints(&interactive_key_hints(app)),
            ],
            ReviewCommandKind::Retro => vec![
                Line::from(
                    "Search candidates, mark PRs with Space, then press Enter to queue retro ticket analysis.",
                ),
                interactive_action_line(app),
                Line::from("Use the Navigation strip to track the active view and focused pane."),
                Line::from(""),
                key_hints(&interactive_key_hints(app)),
            ],
        }),
        InteractiveReviewStage::Confirm => {
            let detail = match app.dialog {
                Some(InteractiveReviewDialog::LaunchReviews(_)) => {
                    "Press Enter to start the selected reviews."
                }
                Some(InteractiveReviewDialog::LaunchFollowUpTickets(_)) => {
                    "Press Enter to analyze the selected PRs for follow-up Linear ticket recommendations."
                }
                Some(InteractiveReviewDialog::StartRemediation(_)) => {
                    "Press Enter to create the remediation PR from the review report."
                }
                Some(InteractiveReviewDialog::SkipRemediation(_)) => {
                    "Press Enter to keep the review report without opening a remediation PR."
                }
                Some(InteractiveReviewDialog::DeleteSession(_, _)) => {
                    "Press Enter to delete the selected stored session."
                }
                Some(InteractiveReviewDialog::CancelSession(_, _)) => {
                    "Press Enter to cancel the selected session at the next checkpoint."
                }
                None => "Press Enter to continue, or Esc to go back.",
            };
            Text::from(vec![
                Line::from(detail),
                Line::from(""),
                key_hints(&interactive_key_hints(app)),
            ])
        }
        InteractiveReviewStage::TicketReview => Text::from(""),
        InteractiveReviewStage::TicketLoading => Text::from(vec![
            Line::from(
                "MetaStack is rebuilding the curated follow-up ticket batch or creating the selected issues in Linear.",
            ),
            Line::from(""),
            key_hints(&[("q", "exit after load")]),
        ]),
        InteractiveReviewStage::Empty => Text::from(vec![
            Line::from(match app.command {
                ReviewCommandKind::Review => "No review candidates were found.",
                ReviewCommandKind::Retro => "No retro candidates were found.",
            }),
            Line::from(""),
            Line::from(
                "Press `R` to refresh discovery, or `q` to exit and return when additional PRs are labeled for review.",
            ),
            Line::from(""),
            key_hints(&interactive_key_hints(app)),
        ]),
    }
}

fn interactive_key_hints(app: &InteractiveReviewApp) -> Vec<(&'static str, &'static str)> {
    match app.stage {
        InteractiveReviewStage::Loading => vec![("q", "exit after load")],
        InteractiveReviewStage::Select => match app.tab {
            InteractiveReviewTab::Candidates => match app.command {
                ReviewCommandKind::Review => vec![
                    ("Type", "search"),
                    ("F", "filter"),
                    ("Up/Down", "move"),
                    ("Space", "select"),
                    ("Tab", "focus"),
                    ("PgUp/PgDn", "scroll"),
                    ("Enter", "queue review"),
                    ("R", "refresh"),
                    ("q", "exit"),
                ],
                ReviewCommandKind::Retro => vec![
                    ("Type", "search"),
                    ("Up/Down", "move"),
                    ("Space", "select"),
                    ("F", "filter"),
                    ("Tab", "focus"),
                    ("PgUp/PgDn", "scroll"),
                    ("Enter", "queue retro"),
                    ("R", "refresh"),
                    ("q", "exit"),
                ],
            },
            InteractiveReviewTab::Sessions => session_key_hints(app),
        },
        InteractiveReviewStage::Confirm => {
            vec![("Enter", "confirm"), ("Esc", "back"), ("q", "exit")]
        }
        InteractiveReviewStage::TicketReview => vec![("Esc", "sessions"), ("q", "exit")],
        InteractiveReviewStage::TicketLoading => vec![("q", "exit after load")],
        InteractiveReviewStage::Empty => vec![("R", "refresh"), ("q", "exit")],
    }
}

fn session_key_hints(app: &InteractiveReviewApp) -> Vec<(&'static str, &'static str)> {
    let mut hints = vec![
        ("Up/Down", "move"),
        ("Tab", "focus"),
        ("PgUp/PgDn", "scroll"),
    ];

    if let Some(session) = app.selected_session() {
        if InteractiveReviewApp::session_has_ticket_review(session) {
            hints.push(("Enter", "review tickets"));
        }
        if InteractiveReviewApp::session_needs_remediation_decision(session) {
            hints.push(("A", "create PR"));
            hints.push(("N", "keep report"));
        }
        hints.push(("D", "delete"));
        if InteractiveReviewApp::session_can_cancel(session) {
            hints.push(("C", "cancel"));
        }
    }

    hints.push(("Esc", "candidates"));
    hints.push(("q", "exit"));
    hints
}

fn interactive_action_line(app: &InteractiveReviewApp) -> Line<'static> {
    if app.tab != InteractiveReviewTab::Sessions {
        return Line::from(vec![
            badge("Enter", Tone::Accent),
            Span::raw(" queue selected work  "),
            badge("F", Tone::Info),
            Span::raw(" filter candidates  "),
            badge("R", Tone::Info),
            Span::raw(" refresh"),
        ]);
    }

    let mut spans = vec![badge("D", Tone::Muted), Span::raw(" delete stored session")];
    if let Some(session) = app.selected_session() {
        if InteractiveReviewApp::session_needs_remediation_decision(session) {
            spans = vec![
                badge("A", Tone::Success),
                Span::raw(" launch remediation PR  "),
                badge("N", Tone::Muted),
                Span::raw(" keep report  "),
                badge("D", Tone::Muted),
                Span::raw(" delete"),
            ];
        }
        if InteractiveReviewApp::session_can_cancel(session) {
            spans.push(Span::raw("  "));
            spans.push(badge("C", Tone::Danger));
            spans.push(Span::raw(" cancel active work"));
        }
    }
    Line::from(spans)
}

fn tab_label(label: &str, active: bool, count: usize) -> Span<'static> {
    if active {
        Span::styled(format!("[{} {}]", label, count), emphasis_style())
    } else {
        Span::styled(format!("{} {}", label, count), muted_style())
    }
}

fn interactive_focus_label(focus: InteractiveReviewFocus) -> &'static str {
    match focus {
        InteractiveReviewFocus::CandidateList => "candidate list",
        InteractiveReviewFocus::CandidatePreview => "candidate detail",
        InteractiveReviewFocus::SessionList => "session list",
        InteractiveReviewFocus::SessionPreview => "session detail",
    }
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
            "number,title,url,body,author,headRefName,baseRefName,changedFiles,additions,deletions,state,labels,assignees,reviewDecision",
        ],
    )
    .with_context(|| format!("failed to fetch PR #{pr_number} metadata — does the PR exist?"))
}

fn resolve_linear_identifier(pr: &GhPrMetadata) -> Result<String> {
    for identifiers in [
        collect_linear_identifiers_from_labels(&pr.labels),
        collect_linear_identifiers_from_single_source(&pr.title),
        collect_linear_identifiers_from_single_source(&pr.head_ref_name),
        collect_linear_identifiers_from_single_source(pr.body.as_deref().unwrap_or("")),
    ] {
        match identifiers.as_slice() {
            [identifier] => return Ok(identifier.clone()),
            [] => continue,
            _ => {
                bail!(
                    "multiple Linear ticket identifiers found in PR #{}: {}. \
                     Link exactly one ticket in the PR label, title, branch, or body.",
                    pr.number,
                    identifiers.join(", ")
                )
            }
        }
    }

    bail!(
        "no Linear ticket identifier found in PR #{} label, title, branch, or body. \
         Expected a pattern like `MET-42` or `ENG-1234`.",
        pr.number
    )
}

fn collect_linear_identifiers_from_labels(labels: &[GhPrLabel]) -> Vec<String> {
    let mut identifiers = BTreeSet::new();
    for label in labels {
        let Some(raw_identifier) = label.name.strip_prefix("id-") else {
            continue;
        };
        if is_linear_identifier(raw_identifier) {
            identifiers.insert(raw_identifier.to_uppercase());
        }
    }
    identifiers.into_iter().collect()
}

fn collect_linear_identifiers_from_single_source(text: &str) -> Vec<String> {
    let mut identifiers = BTreeSet::new();
    collect_linear_identifiers_from_text(text, &mut identifiers);
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

fn assemble_follow_up_linear_prompt(
    pr: &GhPrMetadata,
    linear_identifier: &str,
    diff: &str,
    context_bundle: &str,
    workflow_contract: &str,
    repo_map: &str,
    ticket_context: &str,
) -> String {
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
        r#"# Follow-Up Linear Ticket Recommendation Request

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
- Labels: {labels}
- Linked Linear Ticket: {linear_identifier}

## Linked Linear Ticket
{ticket_context}

## PR Description
{body}

## Diff
```diff
{diff_display}
```

## Recommendation Instructions
{instructions}

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
        labels = pr
            .labels
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        linear_identifier = linear_identifier,
        ticket_context = ticket_context,
        body = pr.body.as_deref().unwrap_or("(no description)"),
        diff_display = diff_display,
        instructions = VIEW_LINEAR_INSTRUCTIONS,
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

    println!(
        "--- dry-run: {} agents review #{} ---",
        crate::branding::COMMAND_NAME,
        pr.number
    );
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

struct RemediationContext<'a> {
    root: &'a Path,
    pr: &'a GhPrMetadata,
    linear_identifier: &'a str,
    review_output: &'a str,
    config: &'a AppConfig,
    planning_meta: &'a PlanningMeta,
    args: &'a ReviewRunArgs,
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
    let context = RemediationContext {
        root,
        pr,
        linear_identifier,
        review_output,
        config,
        planning_meta,
        args,
    };
    run_remediation_with_retry(&context, None, |_attempt, _error| {})
}

fn run_remediation_with_retry(
    context: &RemediationContext<'_>,
    cancel: Option<&AtomicBool>,
    mut on_retry: impl FnMut(usize, &str),
) -> Result<RemediationOutcome> {
    let mut attempt = 1usize;
    let mut previous_error: Option<String> = None;
    loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            bail!("remediation cancelled before PR creation completed");
        }

        match run_remediation_attempt(context, attempt, previous_error.as_deref()) {
            Ok(outcome) => return Ok(outcome),
            Err(error) => {
                let message = error.to_string();
                on_retry(attempt, &message);
                previous_error = Some(message);
                attempt += 1;
                thread::sleep(Duration::from_secs(REMEDIATION_RETRY_DELAY_SECONDS));
            }
        }
    }
}

fn run_remediation_attempt(
    context: &RemediationContext<'_>,
    attempt: usize,
    previous_error: Option<&str>,
) -> Result<RemediationOutcome> {
    let gh = GhCli;
    let remediation_branch = format!("review/remediation-pr-{}", context.pr.number);
    let remediation_base_branch = remediation_target_branch(context.pr);
    let workspace_path = prepare_remediation_workspace(context.root, context.pr.number)?;
    let remediation_branch_exists =
        materialize_pull_request_head(&workspace_path, context.pr.number, &remediation_branch)?;
    let starting_head = git_stdout(&workspace_path, &["rev-parse", "HEAD"])?;

    let fix_prompt = remediation_fix_prompt(
        context.review_output,
        attempt,
        previous_error,
        remediation_branch_exists,
    );

    let fix_args = RunAgentArgs {
        root: Some(workspace_path.clone()),
        route_key: Some(AGENT_ROUTE_AGENTS_REVIEW.to_string()),
        agent: context.args.agent.clone(),
        prompt: fix_prompt,
        instructions: None,
        model: context.args.model.clone(),
        reasoning: context.args.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    };

    let report = run_agent_capture(&fix_args)?;
    eprintln!("{}", report.stdout.trim());

    ensure_remediation_commits_created(&workspace_path, &starting_head, remediation_branch_exists)?;

    run_git(
        &workspace_path,
        &[
            "push",
            "--force-with-lease",
            "-u",
            "origin",
            &remediation_branch,
        ],
    )
    .map_err(|e| {
        anyhow!(
            "failed to push remediation branch `{remediation_branch}`: {e}. \
             Check repository write permissions."
        )
    })?;

    let pr_title = format!("review: remediation for PR #{}", context.pr.number);
    let pr_body = remediation_pull_request_body(
        context.pr.number,
        remediation_base_branch,
        context.review_output,
        context.linear_identifier,
    );
    let body_path = workspace_path
        .join(crate::branding::PROJECT_DIR)
        .join("review-pr-body.md");
    ensure_dir(&workspace_path.join(crate::branding::PROJECT_DIR))?;
    std::fs::write(&body_path, &pr_body).context("failed to write remediation PR body")?;

    let result = gh.publish_branch_pull_request(
        &workspace_path,
        crate::github_pr::PullRequestPublishRequest {
            head_branch: &remediation_branch,
            base_branch: remediation_base_branch,
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
        context.root,
        context.config,
        context.planning_meta,
        context.linear_identifier,
        &result.url,
        context.pr.number,
    )?;

    let _ = std::fs::remove_file(&body_path);

    println!("{}", context.review_output);
    println!(
        "\nRemediation PR #{} opened against `{}` from `{}`: {}",
        result.number,
        remediation_base_branch,
        workspace_path.display(),
        result.url
    );

    Ok(RemediationOutcome {
        pr_number: result.number,
        pr_url: result.url,
    })
}

fn remediation_fix_prompt(
    review_output: &str,
    attempt: usize,
    previous_error: Option<&str>,
    remediation_branch_exists: bool,
) -> String {
    let retry_context = previous_error
        .map(|error| {
            format!(
                "\n## Retry Context\n\
                 This is remediation attempt #{attempt}.\n\
                 The previous attempt failed with:\n\
                 {error}\n"
            )
        })
        .unwrap_or_default();
    let existing_branch_context = if remediation_branch_exists {
        "\n## Existing Remediation Branch\n\
         A remediation branch already exists remotely. Reuse and update it instead of starting over from scratch.\n"
    } else {
        ""
    };

    format!(
        "You are applying required fixes from a code review to this branch.\n\n\
         ## Review Output\n{review_output}\n\
         {retry_context}\
         {existing_branch_context}\
         ## Instructions\n\
         Apply ONLY the required fixes identified in the review above. Do not apply optional recommendations.\n\
         Make minimal, targeted changes. Commit each logical fix separately with clear commit messages.\n\
         If the branch already contains prior remediation work, continue from it instead of undoing it.\n\
         After applying all fixes, verify the changes compile and pass basic checks.\n"
    )
}

fn remediation_target_branch(pr: &GhPrMetadata) -> &str {
    &pr.head_ref_name
}

fn remediation_pull_request_body(
    original_pr_number: u64,
    target_branch: &str,
    review_output: &str,
    linear_identifier: &str,
) -> String {
    format!(
        "## Summary\n\n\
         Automated remediation PR for #{original_pr_number} based on `{} agents review` audit.\n\
         This follow-up PR targets the reviewed branch `{target_branch}` so it can merge into the original PR.\n\n\
         ## Review Findings\n\n\
         {review_output}\n\n\
         ## Linear Ticket\n\n\
         {linear_identifier}\n",
        crate::branding::COMMAND_NAME,
    )
}

fn resolve_remediation_clone_source(root: &Path) -> Result<String> {
    let gh = GhCli;
    match gh.run_json::<GhRepoView>(root, &["repo", "view", "--json", "url"]) {
        Ok(repo) => Ok(repo.url),
        Err(_) => store::resolve_origin_remote(root),
    }
}

fn prepare_remediation_workspace(root: &Path, pr_number: u64) -> Result<std::path::PathBuf> {
    let workspace_root = sibling_workspace_root(root)?.join("review-runs");
    ensure_dir(&workspace_root)?;
    let workspace_path = workspace_root.join(format!("pr-{pr_number}"));
    let clone_source = resolve_remediation_clone_source(root)
        .context("failed to resolve a GitHub clone source for the remediation workspace")?;

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
            &clone_source,
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
) -> Result<bool> {
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
    let branch_exists = remote_branch_exists(workspace_path, remediation_branch)?;
    if branch_exists {
        run_git(workspace_path, &["fetch", "origin", remediation_branch])?;
        run_git(
            workspace_path,
            &[
                "checkout",
                "-B",
                remediation_branch,
                &format!("origin/{remediation_branch}"),
            ],
        )?;
    } else {
        run_git(
            workspace_path,
            &["checkout", "-B", remediation_branch, "HEAD"],
        )?;
    }
    Ok(branch_exists)
}

fn remote_branch_exists(workspace_path: &Path, branch: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["ls-remote", "--heads", "origin", branch])
        .current_dir(workspace_path)
        .output()
        .context("failed to inspect existing remediation branch")?;
    if !output.status.success() {
        bail!(
            "failed to inspect remediation branch `{branch}`: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

fn ensure_remediation_commits_created(
    workspace_path: &Path,
    starting_head: &str,
    remediation_branch_exists: bool,
) -> Result<()> {
    let commit_count = git_stdout(
        workspace_path,
        &["rev-list", "--count", &format!("{starting_head}..HEAD")],
    )?;
    if commit_count.trim() == "0" && !remediation_branch_exists {
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
        bail!(
            "`--json` requires `--once` for `{} agents review`",
            crate::branding::COMMAND_NAME
        );
    }

    let root = canonicalize_existing_dir(&args.root)?;

    if args.check {
        return run_review_check(&root, args);
    }

    verify_gh_auth(&root)?;
    let store = ReviewProjectStore::resolve(&root)?;
    reset_review_store(&store)?;

    if args.once || args.json {
        return run_review_once(&root, &store, args);
    }

    if args.render_once {
        return run_review_render_once(&root, &store, args);
    }

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!(
            "the interactive review dashboard requires a TTY; use `{} agents review <PR_NUMBER> --dry-run`, `{} agents review --once`, or `{} agents review --once --json` for scripted runs",
            crate::branding::COMMAND_NAME,
            crate::branding::COMMAND_NAME,
            crate::branding::COMMAND_NAME,
        );
    }

    run_review_daemon(&root, &store, args).await
}

fn run_review_check(root: &Path, args: &ReviewRunArgs) -> Result<()> {
    let config = AppConfig::load()?;
    let planning_meta = crate::config::load_required_planning_meta(
        root,
        &format!("{} agents review", crate::branding::COMMAND_NAME),
    )?;

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

fn reset_review_store(store: &ReviewProjectStore) -> Result<()> {
    store.save_state(&state::ReviewState::default())
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

    let eligible_prs = discover_eligible_prs(&gh, root, ReviewCommandKind::Review)?;
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
            review_output: None,
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
                session.review_output = result.review_output;
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
    review_output: Option<String>,
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
    let planning_meta = crate::config::load_required_planning_meta(
        root,
        &format!("{} agents review", crate::branding::COMMAND_NAME),
    )?;
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
        review_output: Some(review_output.clone()),
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

/// Discover PRs with the `metastack` label.
///
/// The retro flow loads both open and closed PRs so users can filter by state in
/// the dashboard. The review flow keeps the original open-only behaviour.
fn discover_eligible_prs(
    gh: &GhCli,
    root: &Path,
    command: ReviewCommandKind,
) -> Result<Vec<GhPrListEntry>> {
    let state = match command {
        ReviewCommandKind::Retro => "all",
        ReviewCommandKind::Review => "open",
    };
    gh.run_json(
        root,
        &[
            "pr",
            "list",
            "--state",
            state,
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
        && args.fix_pr.is_none()
        && args.skip_pr.is_none()
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

fn push_note(data: &mut ReviewDashboardData, note: impl Into<String>) {
    data.now_epoch_seconds = now_epoch_seconds();
    data.notes.insert(0, note.into());
    data.notes.truncate(6);
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

    fn draw_interactive(&mut self, app: &InteractiveReviewApp) -> Result<()> {
        self.terminal
            .draw(|frame| render_interactive_review(frame, app))?;
        Ok(())
    }

    fn size(&self) -> Result<Rect> {
        Ok(self.terminal.size()?.into())
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
    fn resolve_linear_identifier_prefers_title_over_body_mentions() {
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
            assignees: Vec::new(),
            review_decision: None,
        };

        assert_eq!(
            resolve_linear_identifier(&pr).expect("title identifier should win"),
            "MET-74".to_string()
        );
    }

    #[test]
    fn resolve_linear_identifier_prefers_linear_id_label() {
        let pr = GhPrMetadata {
            number: 17,
            title: "Technical: Promote listen-managed PRs and dashboard state".to_string(),
            url: "https://example.test/pull/17".to_string(),
            body: Some("Parent MET-48\nChild MET-53".to_string()),
            author: GhPrAuthor {
                login: "metasudo".to_string(),
            },
            head_ref_name: "technical-review-flow".to_string(),
            base_ref_name: "main".to_string(),
            changed_files: 1,
            additions: 1,
            deletions: 0,
            state: "OPEN".to_string(),
            labels: vec![GhPrLabel {
                name: "id-MET-53".to_string(),
            }],
            assignees: Vec::new(),
            review_decision: None,
        };

        assert_eq!(
            resolve_linear_identifier(&pr).expect("label identifier should win"),
            "MET-53".to_string()
        );
    }

    #[test]
    fn resolve_linear_identifier_rejects_ambiguous_ticket_labels() {
        let pr = GhPrMetadata {
            number: 42,
            title: "Review flow".to_string(),
            url: "https://example.test/pull/42".to_string(),
            body: Some("Also references MET-99".to_string()),
            author: GhPrAuthor {
                login: "metasudo".to_string(),
            },
            head_ref_name: "review-flow".to_string(),
            base_ref_name: "main".to_string(),
            changed_files: 1,
            additions: 1,
            deletions: 0,
            state: "OPEN".to_string(),
            labels: vec![
                GhPrLabel {
                    name: "id-MET-74".to_string(),
                },
                GhPrLabel {
                    name: "id-MET-99".to_string(),
                },
            ],
            assignees: Vec::new(),
            review_decision: None,
        };

        let error = resolve_linear_identifier(&pr).expect_err("multiple labels should fail");
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
    fn remediation_targets_reviewed_pr_branch() {
        let pr = GhPrMetadata {
            number: 42,
            title: "Review flow".to_string(),
            url: "https://example.test/pull/42".to_string(),
            body: Some("Implements MET-42".to_string()),
            author: GhPrAuthor {
                login: "metasudo".to_string(),
            },
            head_ref_name: "met-42-review".to_string(),
            base_ref_name: "main".to_string(),
            changed_files: 1,
            additions: 10,
            deletions: 2,
            state: "OPEN".to_string(),
            assignees: vec![],
            labels: vec![GhPrLabel {
                name: "id-MET-42".to_string(),
            }],
            review_decision: None,
        };

        assert_eq!(remediation_target_branch(&pr), "met-42-review");
    }

    #[test]
    fn remediation_pull_request_body_mentions_original_target_branch() {
        let body = remediation_pull_request_body(
            42,
            "met-42-review",
            "### Remediation Required\nYES",
            "MET-42",
        );

        assert!(body.contains("Automated remediation PR for #42"));
        assert!(body.contains("targets the reviewed branch `met-42-review`"));
        assert!(body.contains("### Remediation Required"));
        assert!(body.contains("MET-42"));
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

    #[test]
    fn interactive_dashboard_copy_mentions_explicit_approval() -> Result<()> {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Review);
        app.load_candidates(vec![ReviewLaunchCandidate {
            pr_number: 42,
            title: "MET-74 Improve review UX".to_string(),
            url: "https://example.test/pull/42".to_string(),
            author: "metasudo".to_string(),
            head_ref: "met-74-review".to_string(),
            base_ref: "main".to_string(),
            review_state: "PENDING".to_string(),
            changed_files: 7,
            additions: 120,
            deletions: 18,
            linear_identifier: Some("MET-74".to_string()),
            linear_error: None,
            candidate_state: "open".to_string(),
            candidate_labels: Vec::new(),
            candidate_assignees: Vec::new(),
        }]);

        let backend = ratatui::backend::TestBackend::new(120, 36);
        let mut terminal = ratatui::Terminal::new(backend)?;
        terminal.draw(|frame| render_interactive_review(frame, &app))?;
        let snapshot = format!("{}", terminal.backend());

        assert!(snapshot.contains("Navigation"));
        assert!(snapshot.contains("candidate list"));
        Ok(())
    }

    fn make_test_candidate(pr_number: u64) -> ReviewLaunchCandidate {
        ReviewLaunchCandidate {
            pr_number,
            title: format!("MET-{pr_number} Test PR"),
            url: format!("https://example.test/pull/{pr_number}"),
            author: "metasudo".to_string(),
            head_ref: format!("met-{pr_number}-test"),
            base_ref: "main".to_string(),
            review_state: "PENDING".to_string(),
            changed_files: 3,
            additions: 40,
            deletions: 10,
            linear_identifier: Some(format!("MET-{pr_number}")),
            linear_error: None,
            candidate_state: "open".to_string(),
            candidate_labels: Vec::new(),
            candidate_assignees: Vec::new(),
        }
    }

    /// Build a candidate with explicit filter-relevant fields for filter tests.
    fn make_filter_candidate(
        pr_number: u64,
        state: &str,
        author: &str,
        labels: &[&str],
        assignees: &[&str],
    ) -> ReviewLaunchCandidate {
        ReviewLaunchCandidate {
            pr_number,
            title: format!("PR #{pr_number}"),
            url: format!("https://example.test/pull/{pr_number}"),
            author: author.to_string(),
            head_ref: format!("branch-{pr_number}"),
            base_ref: "main".to_string(),
            review_state: "PENDING".to_string(),
            changed_files: 1,
            additions: 1,
            deletions: 0,
            linear_identifier: None,
            linear_error: None,
            candidate_state: state.to_string(),
            candidate_labels: labels.iter().map(|l| l.to_string()).collect(),
            candidate_assignees: assignees.iter().map(|a| a.to_string()).collect(),
        }
    }

    fn type_query(app: &mut InteractiveReviewApp, query: &str) {
        for ch in query.chars() {
            assert!(app.handle_query_key(crossterm::event::KeyEvent::new(
                KeyCode::Char(ch),
                KeyModifiers::NONE,
            )));
        }
    }

    fn make_test_session(
        pr_number: u64,
        kind: InteractiveSessionKind,
        phase: ReviewPhase,
        summary: &str,
    ) -> InteractiveReviewSession {
        InteractiveReviewSession {
            kind,
            candidate: make_test_candidate(pr_number),
            phase,
            summary: summary.to_string(),
            notes: Vec::new(),
            review_output: Some("Test review output".to_string()),
            follow_up_ticket_set: None,
            created_follow_up_issues: Vec::new(),
            remediation_required: Some(true),
            remediation_pr_number: None,
            remediation_pr_url: None,
            remediation_declined: false,
            cancel_requested: false,
            error: None,
            updated_at_epoch_seconds: now_epoch_seconds(),
        }
    }

    #[test]
    fn snapshot_shows_fix_agent_pending_session() -> Result<()> {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Review);
        app.load_candidates(vec![make_test_candidate(42)]);
        app.upsert_session(make_test_session(
            42,
            InteractiveSessionKind::Review,
            ReviewPhase::FixAgentPending,
            "Fix agent pending for PR #42",
        ));
        app.tab = InteractiveReviewTab::Sessions;
        app.focus = InteractiveReviewFocus::SessionList;

        let backend = ratatui::backend::TestBackend::new(120, 36);
        let mut terminal = ratatui::Terminal::new(backend)?;
        terminal.draw(|frame| render_interactive_review(frame, &app))?;
        let snapshot = format!("{}", terminal.backend());

        assert!(
            snapshot.contains("Fix Agent Pending"),
            "snapshot should show 'Fix Agent Pending'"
        );
        Ok(())
    }

    #[test]
    fn snapshot_shows_fix_agent_complete_with_pr_url() -> Result<()> {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Review);
        app.load_candidates(vec![make_test_candidate(42)]);
        let mut session = make_test_session(
            42,
            InteractiveSessionKind::Review,
            ReviewPhase::FixAgentComplete,
            "Remediation PR #99 created",
        );
        session.remediation_pr_number = Some(99);
        session.remediation_pr_url = Some("https://example.test/pull/99".to_string());
        app.upsert_session(session);
        app.tab = InteractiveReviewTab::Sessions;
        app.focus = InteractiveReviewFocus::SessionList;

        let backend = ratatui::backend::TestBackend::new(120, 36);
        let mut terminal = ratatui::Terminal::new(backend)?;
        terminal.draw(|frame| render_interactive_review(frame, &app))?;
        let snapshot = format!("{}", terminal.backend());

        assert!(
            snapshot.contains("Fix Agent Complete"),
            "snapshot should show 'Fix Agent Complete'"
        );
        Ok(())
    }

    #[test]
    fn snapshot_shows_skipped_session() -> Result<()> {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Review);
        app.load_candidates(vec![make_test_candidate(42)]);
        let mut session = make_test_session(
            42,
            InteractiveSessionKind::Review,
            ReviewPhase::Skipped,
            "Remediation skipped for PR #42",
        );
        session.remediation_declined = true;
        app.upsert_session(session);
        app.tab = InteractiveReviewTab::Sessions;
        app.focus = InteractiveReviewFocus::SessionList;

        let backend = ratatui::backend::TestBackend::new(120, 36);
        let mut terminal = ratatui::Terminal::new(backend)?;
        terminal.draw(|frame| render_interactive_review(frame, &app))?;
        let snapshot = format!("{}", terminal.backend());

        assert!(
            snapshot.contains("Skipped"),
            "snapshot should show 'Skipped'"
        );
        Ok(())
    }

    #[test]
    fn snapshot_shows_review_complete_with_remediation_hints() -> Result<()> {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Review);
        app.load_candidates(vec![make_test_candidate(42)]);
        app.upsert_session(make_test_session(
            42,
            InteractiveSessionKind::Review,
            ReviewPhase::ReviewComplete,
            "Review report ready for PR #42",
        ));
        app.tab = InteractiveReviewTab::Sessions;
        app.focus = InteractiveReviewFocus::SessionList;

        let backend = ratatui::backend::TestBackend::new(120, 36);
        let mut terminal = ratatui::Terminal::new(backend)?;
        terminal.draw(|frame| render_interactive_review(frame, &app))?;
        let snapshot = format!("{}", terminal.backend());

        assert!(
            snapshot.contains("Review Complete"),
            "snapshot should show 'Review Complete'"
        );
        Ok(())
    }

    #[test]
    fn session_needs_remediation_decision_for_review_complete() {
        let session = make_test_session(
            42,
            InteractiveSessionKind::Review,
            ReviewPhase::ReviewComplete,
            "Review report ready",
        );
        assert!(InteractiveReviewApp::session_needs_remediation_decision(
            &session
        ));
    }

    #[test]
    fn session_no_remediation_decision_after_skip() {
        let mut session = make_test_session(
            42,
            InteractiveSessionKind::Review,
            ReviewPhase::Skipped,
            "Skipped",
        );
        session.remediation_declined = true;
        assert!(!InteractiveReviewApp::session_needs_remediation_decision(
            &session
        ));
    }

    #[test]
    fn session_no_remediation_decision_with_fix_pr() {
        let mut session = make_test_session(
            42,
            InteractiveSessionKind::Review,
            ReviewPhase::FixAgentComplete,
            "Fix agent complete",
        );
        session.remediation_pr_url = Some("https://example.test/pull/99".to_string());
        assert!(!InteractiveReviewApp::session_needs_remediation_decision(
            &session
        ));
    }

    #[test]
    fn multiple_sessions_maintain_independent_state() {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Review);
        app.load_candidates(vec![make_test_candidate(42), make_test_candidate(99)]);

        app.upsert_session(make_test_session(
            42,
            InteractiveSessionKind::Review,
            ReviewPhase::ReviewComplete,
            "Review complete for PR #42",
        ));
        app.upsert_session(make_test_session(
            99,
            InteractiveSessionKind::Review,
            ReviewPhase::FixAgentInProgress,
            "Fix agent running for PR #99",
        ));

        assert_eq!(app.sessions.len(), 2);
        assert_eq!(app.sessions[0].phase, ReviewPhase::ReviewComplete);
        assert_eq!(app.sessions[1].phase, ReviewPhase::FixAgentInProgress);
        assert_eq!(app.active_session_count(), 2);
    }

    #[test]
    fn query_filter_preserves_selected_candidate_when_still_visible() {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Review);
        let mut first = make_test_candidate(42);
        first.title = "Alpha change".to_string();
        let mut second = make_test_candidate(99);
        second.title = "Beta cleanup".to_string();
        let mut third = make_test_candidate(123);
        third.title = "Beta release".to_string();
        app.load_candidates(vec![first, second, third]);
        app.selected_index = 2;

        type_query(&mut app, "beta");

        assert_eq!(
            app.selected_candidate()
                .map(|candidate| candidate.pr_number),
            Some(123),
            "filtering in place should keep the cursor on the previously selected PR"
        );
        assert_eq!(app.selected_index, 1);

        for _ in 0..4 {
            assert!(app.handle_query_key(crossterm::event::KeyEvent::new(
                KeyCode::Backspace,
                KeyModifiers::NONE,
            )));
        }

        assert_eq!(
            app.selected_candidate()
                .map(|candidate| candidate.pr_number),
            Some(123),
            "clearing the filter should restore the same selected PR when it is still visible"
        );
        assert_eq!(app.selected_index, 2);
    }

    #[test]
    fn restore_from_persistent_state_creates_sessions() {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Review);
        app.load_candidates(vec![make_test_candidate(42)]);

        let persistent = vec![ReviewSession {
            pr_number: 42,
            pr_title: "Test PR #42".to_string(),
            pr_url: Some("https://example.test/pull/42".to_string()),
            pr_author: Some("metasudo".to_string()),
            head_branch: Some("met-42-test".to_string()),
            base_branch: Some("main".to_string()),
            linear_identifier: Some("MET-42".to_string()),
            phase: ReviewPhase::ReviewComplete,
            summary: "Review complete".to_string(),
            updated_at_epoch_seconds: 1000,
            review_output: Some("### Remediation Required\nYES".to_string()),
            remediation_required: Some(true),
            remediation_pr_number: None,
            remediation_pr_url: None,
        }];

        app.restore_from_persistent_state(&persistent);
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].phase, ReviewPhase::ReviewComplete);
        assert!(app.tab == InteractiveReviewTab::Sessions);
    }

    #[test]
    fn restore_from_persistent_state_preserves_loaded_candidate_filter_metadata() {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Review);
        app.load_candidates(vec![make_filter_candidate(
            42,
            "closed",
            "metasudo",
            &["bug", "metastack"],
            &["alice"],
        )]);

        let persistent = vec![ReviewSession {
            pr_number: 42,
            pr_title: "Restored title".to_string(),
            pr_url: Some("https://example.test/pull/42".to_string()),
            pr_author: Some("restored-user".to_string()),
            head_branch: Some("restored-head".to_string()),
            base_branch: Some("main".to_string()),
            linear_identifier: Some("MET-42".to_string()),
            phase: ReviewPhase::ReviewComplete,
            summary: "Review complete".to_string(),
            updated_at_epoch_seconds: 1000,
            review_output: Some("### Remediation Required\nYES".to_string()),
            remediation_required: Some(true),
            remediation_pr_number: None,
            remediation_pr_url: None,
        }];

        app.restore_from_persistent_state(&persistent);

        let candidate = app
            .candidates
            .iter()
            .find(|candidate| candidate.pr_number == 42)
            .expect("candidate should remain loaded");
        assert_eq!(candidate.candidate_state, "closed");
        assert_eq!(
            candidate.candidate_labels,
            vec!["bug".to_string(), "metastack".to_string()]
        );
        assert_eq!(candidate.candidate_assignees, vec!["alice".to_string()]);

        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].candidate.candidate_state, "closed");
        assert_eq!(
            app.sessions[0].candidate.candidate_labels,
            vec!["bug".to_string(), "metastack".to_string()]
        );
        assert_eq!(
            app.sessions[0].candidate.candidate_assignees,
            vec!["alice".to_string()]
        );
    }

    #[test]
    fn restore_skips_fully_completed_sessions() {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Review);
        app.load_candidates(vec![make_test_candidate(42)]);

        let persistent = vec![ReviewSession {
            pr_number: 42,
            pr_title: "Test PR #42".to_string(),
            pr_url: Some("https://example.test/pull/42".to_string()),
            pr_author: Some("metasudo".to_string()),
            head_branch: Some("met-42-test".to_string()),
            base_branch: Some("main".to_string()),
            linear_identifier: Some("MET-42".to_string()),
            phase: ReviewPhase::Completed,
            summary: "No remediation required".to_string(),
            updated_at_epoch_seconds: 1000,
            review_output: Some("### Remediation Required\nNO".to_string()),
            remediation_required: Some(false),
            remediation_pr_number: None,
            remediation_pr_url: None,
        }];

        app.restore_from_persistent_state(&persistent);
        assert_eq!(
            app.sessions.len(),
            0,
            "fully completed sessions should not be restored"
        );
    }

    #[test]
    fn delete_session_removes_selected_entry() {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Review);
        app.load_candidates(vec![make_test_candidate(42)]);
        app.upsert_session(make_test_session(
            42,
            InteractiveSessionKind::Review,
            ReviewPhase::ReviewComplete,
            "Review report ready",
        ));
        app.session_index = 0;
        app.tab = InteractiveReviewTab::Sessions;
        app.focus = InteractiveReviewFocus::SessionList;

        app.delete_session(42, InteractiveSessionKind::Review);

        assert!(app.sessions.is_empty());
        assert_eq!(app.tab, InteractiveReviewTab::Candidates);
    }

    #[test]
    fn listener_dashboard_snapshot_shows_fix_agent_states() {
        let data = ReviewDashboardData {
            scope: "origin/main".to_string(),
            cycle_summary: "Review cycle complete".to_string(),
            eligible_prs: 2,
            sessions: vec![
                ReviewSession {
                    pr_number: 42,
                    pr_title: "MET-42 fix agent test".to_string(),
                    pr_url: Some("https://example.test/pull/42".to_string()),
                    pr_author: Some("metasudo".to_string()),
                    head_branch: Some("met-42-test".to_string()),
                    base_branch: Some("main".to_string()),
                    linear_identifier: Some("MET-42".to_string()),
                    phase: ReviewPhase::FixAgentInProgress,
                    summary: "Fix agent running".to_string(),
                    updated_at_epoch_seconds: 1,
                    review_output: Some("### Remediation Required\nYES".to_string()),
                    remediation_required: Some(true),
                    remediation_pr_number: None,
                    remediation_pr_url: None,
                },
                ReviewSession {
                    pr_number: 99,
                    pr_title: "MET-99 skipped test".to_string(),
                    pr_url: Some("https://example.test/pull/99".to_string()),
                    pr_author: Some("metasudo".to_string()),
                    head_branch: Some("met-99-test".to_string()),
                    base_branch: Some("main".to_string()),
                    linear_identifier: Some("MET-99".to_string()),
                    phase: ReviewPhase::Skipped,
                    summary: "Remediation skipped".to_string(),
                    updated_at_epoch_seconds: 1,
                    review_output: Some("### Remediation Required\nYES".to_string()),
                    remediation_required: Some(true),
                    remediation_pr_number: None,
                    remediation_pr_url: None,
                },
            ],
            now_epoch_seconds: 5,
            notes: vec![],
            state_file: "/tmp/review-session.json".to_string(),
        };

        let snapshot = render_review_dashboard_snapshot(
            120,
            32,
            &data,
            &dashboard::ReviewBrowserState::default(),
        )
        .expect("snapshot should render");

        assert!(
            snapshot.contains("Fix Agent Running"),
            "snapshot should show 'Fix Agent Running'"
        );
    }

    // -----------------------------------------------------------------------
    // CandidateFilter unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn filter_default_matches_everything() {
        let filter = CandidateFilter::default();
        assert!(!filter.is_active());
        let candidate = make_filter_candidate(1, "open", "alice", &["bug"], &["alice"]);
        assert!(filter.matches(&candidate));
    }

    #[test]
    fn filter_state_open_only() {
        let mut filter = CandidateFilter::default();
        filter.states.insert("open".to_string());
        assert!(filter.matches(&make_filter_candidate(1, "open", "alice", &[], &[])));
        assert!(!filter.matches(&make_filter_candidate(2, "closed", "alice", &[], &[])));
    }

    #[test]
    fn filter_state_closed_only() {
        let mut filter = CandidateFilter::default();
        filter.states.insert("closed".to_string());
        assert!(!filter.matches(&make_filter_candidate(1, "open", "alice", &[], &[])));
        assert!(filter.matches(&make_filter_candidate(2, "closed", "alice", &[], &[])));
    }

    #[test]
    fn filter_state_both_open_and_closed() {
        let mut filter = CandidateFilter::default();
        filter.states.insert("open".to_string());
        filter.states.insert("closed".to_string());
        assert!(filter.matches(&make_filter_candidate(1, "open", "alice", &[], &[])));
        assert!(filter.matches(&make_filter_candidate(2, "closed", "bob", &[], &[])));
    }

    #[test]
    fn filter_author() {
        let mut filter = CandidateFilter::default();
        filter.authors.insert("alice".to_string());
        assert!(filter.matches(&make_filter_candidate(1, "open", "alice", &[], &[])));
        assert!(!filter.matches(&make_filter_candidate(2, "open", "bob", &[], &[])));
    }

    #[test]
    fn filter_labels_requires_all_selected() {
        let mut filter = CandidateFilter::default();
        filter.labels.insert("bug".to_string());
        filter.labels.insert("urgent".to_string());
        // Has both labels.
        assert!(filter.matches(&make_filter_candidate(
            1,
            "open",
            "alice",
            &["bug", "urgent", "extra"],
            &[]
        )));
        // Missing "urgent".
        assert!(!filter.matches(&make_filter_candidate(2, "open", "alice", &["bug"], &[])));
        // Missing both.
        assert!(!filter.matches(&make_filter_candidate(3, "open", "alice", &[], &[])));
    }

    #[test]
    fn filter_assignee_matches_any() {
        let mut filter = CandidateFilter::default();
        filter.assignees.insert("alice".to_string());
        filter.assignees.insert("bob".to_string());
        assert!(filter.matches(&make_filter_candidate(1, "open", "eve", &[], &["alice"])));
        assert!(filter.matches(&make_filter_candidate(
            2,
            "open",
            "eve",
            &[],
            &["bob", "charlie"]
        )));
        assert!(!filter.matches(&make_filter_candidate(3, "open", "eve", &[], &["charlie"])));
    }

    #[test]
    fn filter_assignee_unassigned() {
        let mut filter = CandidateFilter::default();
        filter.assignees.insert(UNASSIGNED_FILTER_VALUE.to_string());
        // Unassigned candidate (empty assignees).
        assert!(filter.matches(&make_filter_candidate(1, "open", "alice", &[], &[])));
        // Assigned candidate.
        assert!(!filter.matches(&make_filter_candidate(2, "open", "alice", &[], &["bob"])));
    }

    #[test]
    fn filter_assignee_multi_assigned() {
        let mut filter = CandidateFilter::default();
        filter.assignees.insert("alice".to_string());
        // PR assigned to both alice and bob — should match because alice is in filter.
        assert!(filter.matches(&make_filter_candidate(
            1,
            "open",
            "eve",
            &[],
            &["alice", "bob"]
        )));
    }

    #[test]
    fn filter_conjunctive_combination() {
        let mut filter = CandidateFilter::default();
        filter.states.insert("open".to_string());
        filter.authors.insert("alice".to_string());
        filter.labels.insert("bug".to_string());
        filter.assignees.insert("alice".to_string());

        // Passes all categories.
        assert!(filter.matches(&make_filter_candidate(
            1,
            "open",
            "alice",
            &["bug"],
            &["alice"]
        )));
        // Wrong state.
        assert!(!filter.matches(&make_filter_candidate(
            2,
            "closed",
            "alice",
            &["bug"],
            &["alice"]
        )));
        // Wrong author.
        assert!(!filter.matches(&make_filter_candidate(
            3,
            "open",
            "bob",
            &["bug"],
            &["alice"]
        )));
        // Missing label.
        assert!(!filter.matches(&make_filter_candidate(4, "open", "alice", &[], &["alice"])));
        // Wrong assignee.
        assert!(!filter.matches(&make_filter_candidate(
            5,
            "open",
            "alice",
            &["bug"],
            &["bob"]
        )));
    }

    #[test]
    fn filter_clear_resets_to_default() {
        let mut filter = CandidateFilter::default();
        filter.states.insert("open".to_string());
        filter.authors.insert("alice".to_string());
        filter.labels.insert("bug".to_string());
        filter.assignees.insert("bob".to_string());
        assert!(filter.is_active());
        filter.clear();
        assert!(!filter.is_active());
        // After clear, matches everything.
        assert!(filter.matches(&make_filter_candidate(1, "closed", "eve", &[], &[])));
    }

    #[test]
    fn filter_summary_describes_active_filters() {
        let mut filter = CandidateFilter::default();
        assert!(filter.summary().is_empty());
        filter.states.insert("open".to_string());
        filter.authors.insert("alice".to_string());
        let summary = filter.summary();
        assert!(summary.contains("state=open"));
        assert!(summary.contains("author=alice"));
    }

    #[test]
    fn normalize_pr_state_maps_open_closed_merged() {
        assert_eq!(normalize_pr_state("OPEN"), "open");
        assert_eq!(normalize_pr_state("CLOSED"), "closed");
        assert_eq!(normalize_pr_state("MERGED"), "closed");
    }

    // -----------------------------------------------------------------------
    // Filter panel interaction tests
    // -----------------------------------------------------------------------

    #[test]
    fn filter_panel_f_key_opens_panel() {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Retro);
        app.load_candidates(vec![
            make_filter_candidate(1, "open", "alice", &["bug"], &["alice"]),
            make_filter_candidate(2, "closed", "bob", &["feature"], &[]),
        ]);
        assert!(!app.filter_panel_open);

        // Simulate pressing 'F'.
        let f_key = crossterm::event::KeyEvent::new(KeyCode::Char('F'), KeyModifiers::NONE);
        let _ = app.handle_key(f_key, Rect::default());
        assert!(app.filter_panel_open);
        assert!(!app.filter_panel_rows.is_empty());
    }

    #[test]
    fn filter_panel_space_toggles_selection() {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Retro);
        app.load_candidates(vec![
            make_filter_candidate(1, "open", "alice", &[], &[]),
            make_filter_candidate(2, "closed", "bob", &[], &[]),
        ]);
        app.open_filter_panel();
        // First row should be "open" state.
        assert!(!app.filter_panel_rows[0].selected);

        let space = crossterm::event::KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        app.handle_filter_panel_key(space);
        assert!(app.filter_panel_rows[0].selected);

        // Toggle off.
        app.handle_filter_panel_key(space);
        assert!(!app.filter_panel_rows[0].selected);
    }

    #[test]
    fn filter_panel_close_applies_filter() {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Retro);
        app.load_candidates(vec![
            make_filter_candidate(1, "open", "alice", &[], &[]),
            make_filter_candidate(2, "closed", "bob", &[], &[]),
        ]);
        assert_eq!(app.visible_candidate_indices().len(), 2);

        app.open_filter_panel();
        // Select "open" state (first row).
        let space = crossterm::event::KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        app.handle_filter_panel_key(space);
        // Close with Enter.
        let enter = crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        app.handle_filter_panel_key(enter);

        assert!(!app.filter_panel_open);
        assert!(app.filter.is_active());
        // Only the open candidate should be visible.
        assert_eq!(app.visible_candidate_indices().len(), 1);
    }

    #[test]
    fn filter_panel_clear_removes_all_selections() {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Retro);
        app.load_candidates(vec![
            make_filter_candidate(1, "open", "alice", &[], &[]),
            make_filter_candidate(2, "closed", "bob", &[], &[]),
        ]);
        app.open_filter_panel();
        // Select some options.
        let space = crossterm::event::KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        app.handle_filter_panel_key(space);
        assert!(app.filter_panel_rows[0].selected);

        // Clear all.
        let c_key = crossterm::event::KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE);
        app.handle_filter_panel_key(c_key);
        assert!(app.filter_panel_rows.iter().all(|row| !row.selected));
    }

    #[test]
    fn filter_panel_navigation_up_down() {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Retro);
        app.load_candidates(vec![
            make_filter_candidate(1, "open", "alice", &[], &[]),
            make_filter_candidate(2, "closed", "bob", &[], &[]),
        ]);
        app.open_filter_panel();
        assert_eq!(app.filter_panel_cursor, 0);

        let down = crossterm::event::KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        app.handle_filter_panel_key(down);
        assert_eq!(app.filter_panel_cursor, 1);

        let up = crossterm::event::KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        app.handle_filter_panel_key(up);
        assert_eq!(app.filter_panel_cursor, 0);
    }

    #[test]
    fn filter_preserves_selection_when_candidate_still_visible() {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Retro);
        app.load_candidates(vec![
            make_filter_candidate(1, "open", "alice", &[], &[]),
            make_filter_candidate(2, "open", "bob", &[], &[]),
            make_filter_candidate(3, "closed", "eve", &[], &[]),
        ]);
        app.selected_prs.insert(2);

        // Filter to open only.
        app.filter.states.insert("open".to_string());
        let visible = app.visible_candidate_indices();
        // PR #2 is still in the visible set.
        assert!(visible.iter().any(|&i| app.candidates[i].pr_number == 2));
        assert!(app.selected_prs.contains(&2));
    }

    #[test]
    fn filter_panel_rows_include_unassigned_when_applicable() {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Retro);
        app.load_candidates(vec![
            make_filter_candidate(1, "open", "alice", &[], &[]), // unassigned
            make_filter_candidate(2, "open", "bob", &[], &["charlie"]), // assigned
        ]);
        let rows = app.build_filter_panel_rows();
        let assignee_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.category == FilterCategory::Assignee)
            .collect();
        assert!(
            assignee_rows
                .iter()
                .any(|r| r.value == UNASSIGNED_FILTER_VALUE),
            "panel should include an unassigned option"
        );
        assert!(
            assignee_rows.iter().any(|r| r.value == "charlie"),
            "panel should include the assigned login"
        );
    }

    #[test]
    fn mixed_open_closed_dataset_filter_test() {
        let candidates = vec![
            make_filter_candidate(1, "open", "alice", &["bug"], &["alice"]),
            make_filter_candidate(2, "closed", "bob", &["feature"], &["bob"]),
            make_filter_candidate(3, "open", "alice", &["bug", "urgent"], &[]),
            make_filter_candidate(4, "closed", "eve", &[], &["alice", "bob"]),
        ];
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Retro);
        app.load_candidates(candidates);
        assert_eq!(app.visible_candidate_indices().len(), 4);

        // Filter: open only.
        app.filter.states.insert("open".to_string());
        assert_eq!(app.visible_candidate_indices().len(), 2);

        // Further narrow: author=alice.
        app.filter.authors.insert("alice".to_string());
        assert_eq!(app.visible_candidate_indices().len(), 2);

        // Further narrow: label=urgent.
        app.filter.labels.insert("urgent".to_string());
        assert_eq!(app.visible_candidate_indices().len(), 1);

        // Clear filters.
        app.filter.clear();
        assert_eq!(app.visible_candidate_indices().len(), 4);
    }

    #[test]
    fn f_key_opens_filter_panel_in_review_mode() {
        let mut app =
            InteractiveReviewApp::new(InteractiveReviewMode::Discovery, ReviewCommandKind::Review);
        app.load_candidates(vec![make_test_candidate(1)]);
        let f_key = crossterm::event::KeyEvent::new(KeyCode::Char('F'), KeyModifiers::NONE);
        let _ = app.handle_key(f_key, Rect::default());
        assert!(
            app.filter_panel_open,
            "filter panel should open in review mode"
        );
    }

    #[test]
    fn candidate_from_metadata_populates_filter_fields() {
        let pr = GhPrMetadata {
            number: 10,
            title: "MET-10: Test".to_string(),
            url: "https://example.test/pull/10".to_string(),
            body: None,
            author: GhPrAuthor {
                login: "alice".to_string(),
            },
            head_ref_name: "met-10-test".to_string(),
            base_ref_name: "main".to_string(),
            changed_files: 1,
            additions: 1,
            deletions: 0,
            state: "MERGED".to_string(),
            labels: vec![
                GhPrLabel {
                    name: "bug".to_string(),
                },
                GhPrLabel {
                    name: "metastack".to_string(),
                },
            ],
            assignees: vec![
                GhPrAuthor {
                    login: "alice".to_string(),
                },
                GhPrAuthor {
                    login: "bob".to_string(),
                },
            ],
            review_decision: Some("APPROVED".to_string()),
        };
        let candidate = candidate_from_metadata(&pr);
        assert_eq!(candidate.candidate_state, "closed"); // MERGED -> closed
        assert_eq!(candidate.candidate_labels, vec!["bug", "metastack"]);
        assert_eq!(candidate.candidate_assignees, vec!["alice", "bob"]);
        assert_eq!(candidate.author, "alice");
    }

    const FOLLOW_UP_JSON: &str = r#"{"summary":"summary","tickets":[{"title":"T","why_now":"W","outcome":"O","scope":"S","acceptance_criteria":["A"],"priority":1}],"notes":["N"]}"#;

    #[test]
    fn parse_follow_up_ticket_set_bare_json() {
        let parsed = parse_follow_up_ticket_set(FOLLOW_UP_JSON).unwrap();
        assert_eq!(parsed.summary, "summary");
        assert_eq!(parsed.tickets.len(), 1);
        assert_eq!(parsed.tickets[0].title, "T");
    }

    #[test]
    fn parse_follow_up_ticket_set_code_fenced() {
        let input = format!("```json\n{}\n```", FOLLOW_UP_JSON);
        let parsed = parse_follow_up_ticket_set(&input).unwrap();
        assert_eq!(parsed.summary, "summary");
        assert_eq!(parsed.tickets[0].title, "T");
    }

    #[test]
    fn parse_follow_up_ticket_set_surrounded_by_text() {
        let input = format!("Here is the analysis:\n{}\nDone.", FOLLOW_UP_JSON);
        let parsed = parse_follow_up_ticket_set(&input).unwrap();
        assert_eq!(parsed.summary, "summary");
        assert_eq!(parsed.tickets[0].title, "T");
    }
}
