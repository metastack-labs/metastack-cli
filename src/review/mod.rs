pub(crate) mod dashboard;
mod state;
pub(crate) mod store;

use std::collections::BTreeSet;
use std::io;
use std::io::IsTerminal;
use std::path::Path;
use std::process::Command;
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
use ratatui::widgets::{ListItem, ListState, Wrap};
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
}

#[derive(Debug, Clone)]
struct InteractiveReviewOutcome {
    candidate: ReviewLaunchCandidate,
    summary: String,
    review_output: String,
    remediation_required: bool,
    remediation_pr_number: Option<u64>,
    remediation_pr_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveReviewStage {
    Loading,
    Select,
    Confirm,
    Running,
    Completed,
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveReviewMode {
    Direct,
    Discovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveReviewFocus {
    Candidates,
    Preview,
}

#[derive(Debug, Clone)]
struct InteractiveReviewApp {
    mode: InteractiveReviewMode,
    stage: InteractiveReviewStage,
    focus: InteractiveReviewFocus,
    candidates: Vec<ReviewLaunchCandidate>,
    selected_index: usize,
    preview_scroll: ScrollState,
    status: String,
    notes: Vec<String>,
    outcome: Option<InteractiveReviewOutcome>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
enum ReviewExecutionEvent {
    Progress {
        candidate: ReviewLaunchCandidate,
        phase: ReviewPhase,
        summary: String,
        note: Option<String>,
        remediation_required: Option<bool>,
    },
    Completed(InteractiveReviewOutcome),
    Failed {
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
    if let Some(pr_number) = args.pr_number {
        if should_launch_interactive_review_dashboard(&args.run) {
            run_review_interactive(&args.run, Some(pr_number))
        } else {
            run_review_one_shot(&args.run, pr_number)
        }
    } else if should_launch_interactive_review_dashboard(&args.run) {
        run_review_interactive(&args.run, None)
    } else {
        run_review_listener(&args.run).await
    }
}

// ---------------------------------------------------------------------------
// One-shot PR review
// ---------------------------------------------------------------------------

fn run_review_interactive(args: &ReviewRunArgs, pr_number: Option<u64>) -> Result<()> {
    let root = canonicalize_existing_dir(&args.root)?;
    let config = AppConfig::load()?;
    let planning_meta = crate::config::load_required_planning_meta(&root, "meta agents review")?;
    let gh = GhCli;
    let store = ReviewProjectStore::resolve(&root).ok();
    let mode = if pr_number.is_some() {
        InteractiveReviewMode::Direct
    } else {
        InteractiveReviewMode::Discovery
    };

    let mut terminal = ReviewTerminalDashboard::open()?;
    let mut app = InteractiveReviewApp::new(mode);
    app.set_loading(
        "Preparing review dashboard".to_string(),
        "Opening the review workflow and resolving prerequisites.".to_string(),
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
            Some(number) => format!("Loading PR #{number} and linked review context."),
            None => "Finding open `metastack` pull requests that are ready for explicit review approval."
                .to_string(),
        },
    );
    terminal.draw_interactive(&app)?;

    let candidates = discover_review_candidates(&root, &gh, pr_number, &mut app, &mut terminal)?;
    app.load_candidates(candidates);
    terminal.draw_interactive(&app)?;

    let mut worker_rx: Option<Receiver<ReviewExecutionEvent>> = None;
    let mut next_pulse_at = Instant::now() + Duration::from_millis(150);

    loop {
        if next_pulse_at <= Instant::now() {
            app.tick();
            terminal.draw_interactive(&app)?;
            next_pulse_at = Instant::now() + Duration::from_millis(150);
        }

        if let Some(ref receiver) = worker_rx {
            match receiver.try_recv() {
                Ok(event) => {
                    app.apply_worker_event(event);
                    terminal.draw_interactive(&app)?;
                    if app.stage != InteractiveReviewStage::Running {
                        worker_rx = None;
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    app.fail("review worker disconnected unexpectedly".to_string());
                    worker_rx = None;
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
                if let Some(selection) = app.handle_key(key.code, preview)? {
                    let receiver = spawn_review_execution(
                        root.clone(),
                        config.clone(),
                        planning_meta.clone(),
                        args.clone(),
                        selection.clone(),
                        store.clone(),
                    );
                    worker_rx = Some(receiver);
                    app.begin_running(selection);
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
                let viewport = interactive_preview_viewport(terminal.size()?);
                let _ = app.handle_preview_mouse(mouse, viewport);
            }
            _ => {}
        }
    }

    terminal.close()?;
    if let Some(outcome) = app.outcome {
        println!("{}", outcome.review_output);
        if let Some(pr_url) = outcome.remediation_pr_url {
            println!(
                "\nRemediation PR #{} opened: {}",
                outcome.remediation_pr_number.unwrap_or_default(),
                pr_url
            );
        }
    }

    Ok(())
}

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

impl InteractiveReviewApp {
    fn new(mode: InteractiveReviewMode) -> Self {
        Self {
            mode,
            stage: InteractiveReviewStage::Loading,
            focus: InteractiveReviewFocus::Candidates,
            candidates: Vec::new(),
            selected_index: 0,
            preview_scroll: ScrollState::default(),
            status: "Preparing review dashboard".to_string(),
            notes: Vec::new(),
            outcome: None,
            error: None,
        }
    }

    fn set_loading(&mut self, status: String, note: String) {
        self.stage = InteractiveReviewStage::Loading;
        self.status = status;
        self.push_note(note);
        self.error = None;
    }

    fn load_candidates(&mut self, candidates: Vec<ReviewLaunchCandidate>) {
        self.candidates = candidates;
        self.selected_index = 0;
        self.preview_scroll.reset();
        self.error = None;
        self.outcome = None;
        if self.candidates.is_empty() {
            self.stage = InteractiveReviewStage::Empty;
            self.status = "No review candidates found".to_string();
        } else {
            self.stage = InteractiveReviewStage::Select;
            self.status = match self.mode {
                InteractiveReviewMode::Direct => {
                    "Review candidate loaded. Confirm when you want to start the audit.".to_string()
                }
                InteractiveReviewMode::Discovery => {
                    "Select a PR to review, inspect the preview, then explicitly start the audit."
                        .to_string()
                }
            };
        }
    }

    fn begin_running(&mut self, candidate: ReviewLaunchCandidate) {
        self.stage = InteractiveReviewStage::Running;
        self.status = format!(
            "Reviewing PR #{} — {}",
            candidate.pr_number, candidate.title
        );
        self.outcome = None;
        self.error = None;
        self.replace_candidate(candidate);
        self.preview_scroll.reset();
    }

    fn apply_worker_event(&mut self, event: ReviewExecutionEvent) {
        match event {
            ReviewExecutionEvent::Progress {
                candidate,
                phase,
                summary,
                note,
                remediation_required,
            } => {
                self.stage = InteractiveReviewStage::Running;
                self.status = summary.clone();
                self.error = None;
                self.replace_candidate(candidate.clone());
                if let Some(candidate) = self.candidates.get_mut(self.selected_index) {
                    candidate.linear_identifier = candidate.linear_identifier.clone();
                }
                if let Some(session) = self
                    .candidates
                    .iter_mut()
                    .find(|entry| entry.pr_number == candidate.pr_number)
                {
                    session.linear_identifier = candidate.linear_identifier.clone();
                    if remediation_required.is_some() {
                        session.linear_error = None;
                    }
                }
                if let Some(note) = note {
                    self.push_note(note);
                }
                self.push_note(format!(
                    "PR #{} is now in `{}`.",
                    candidate.pr_number,
                    phase.display_label()
                ));
            }
            ReviewExecutionEvent::Completed(outcome) => {
                self.stage = InteractiveReviewStage::Completed;
                self.status = outcome.summary.clone();
                self.error = None;
                self.replace_candidate(outcome.candidate.clone());
                self.outcome = Some(outcome.clone());
                self.push_note(match outcome.remediation_pr_url.as_deref() {
                    Some(url) => format!(
                        "Remediation PR #{} opened at {}.",
                        outcome.remediation_pr_number.unwrap_or_default(),
                        url
                    ),
                    None => format!(
                        "Review finished for PR #{} without remediation.",
                        outcome.candidate.pr_number
                    ),
                });
                self.preview_scroll.reset();
            }
            ReviewExecutionEvent::Failed { candidate, error } => {
                self.stage = InteractiveReviewStage::Completed;
                self.status = format!("Review failed for PR #{}", candidate.pr_number);
                self.error = Some(error.clone());
                self.outcome = None;
                self.replace_candidate(candidate);
                self.push_note(error);
                self.preview_scroll.reset();
            }
        }
    }

    fn fail(&mut self, error: String) {
        self.stage = InteractiveReviewStage::Completed;
        self.status = "Review dashboard failed".to_string();
        self.error = Some(error.clone());
        self.push_note(error);
    }

    fn tick(&mut self) {}

    fn handle_key(
        &mut self,
        code: KeyCode,
        preview: Rect,
    ) -> Result<Option<ReviewLaunchCandidate>> {
        match self.stage {
            InteractiveReviewStage::Loading => Ok(None),
            InteractiveReviewStage::Empty => {
                if matches!(code, KeyCode::Char('r') | KeyCode::Char('R')) {
                    self.stage = InteractiveReviewStage::Loading;
                    self.status = "Refreshing review candidates".to_string();
                    self.notes.clear();
                    self.push_note("Refreshing candidate discovery.".to_string());
                }
                Ok(None)
            }
            InteractiveReviewStage::Select => {
                match code {
                    KeyCode::Up => {
                        if self.focus == InteractiveReviewFocus::Candidates {
                            self.selected_index = self.selected_index.saturating_sub(1);
                            self.preview_scroll.reset();
                        } else {
                            let _ = self.preview_scroll.apply_key_code_in_viewport(
                                KeyCode::Up,
                                preview,
                                self.preview_rows(preview.width),
                            );
                        }
                    }
                    KeyCode::Down => {
                        if self.focus == InteractiveReviewFocus::Candidates {
                            if self.selected_index + 1 < self.candidates.len() {
                                self.selected_index += 1;
                                self.preview_scroll.reset();
                            }
                        } else {
                            let _ = self.preview_scroll.apply_key_code_in_viewport(
                                KeyCode::Down,
                                preview,
                                self.preview_rows(preview.width),
                            );
                        }
                    }
                    KeyCode::Tab => {
                        self.focus = match self.focus {
                            InteractiveReviewFocus::Candidates => InteractiveReviewFocus::Preview,
                            InteractiveReviewFocus::Preview => InteractiveReviewFocus::Candidates,
                        };
                    }
                    KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
                        if self.focus == InteractiveReviewFocus::Preview =>
                    {
                        let _ = self.preview_scroll.apply_key_code_in_viewport(
                            code,
                            preview,
                            self.preview_rows(preview.width),
                        );
                    }
                    KeyCode::Enter if self.selected_candidate().is_some() => {
                        self.stage = InteractiveReviewStage::Confirm;
                        self.status = format!(
                            "Confirm review start for PR #{}.",
                            self.selected_candidate()
                                .map(|candidate| candidate.pr_number)
                                .unwrap_or_default()
                        );
                    }
                    _ => {}
                }
                Ok(None)
            }
            InteractiveReviewStage::Confirm => match code {
                KeyCode::Enter => Ok(self.selected_candidate().cloned()),
                KeyCode::Esc | KeyCode::Backspace => {
                    self.stage = InteractiveReviewStage::Select;
                    Ok(None)
                }
                _ => Ok(None),
            },
            InteractiveReviewStage::Running => Ok(None),
            InteractiveReviewStage::Completed => {
                if matches!(code, KeyCode::Esc | KeyCode::Backspace)
                    && self.mode == InteractiveReviewMode::Discovery
                    && !self.candidates.is_empty()
                {
                    self.stage = InteractiveReviewStage::Select;
                    self.status =
                        "Select another PR to review, or exit when you are done.".to_string();
                    self.error = None;
                    self.outcome = None;
                    self.preview_scroll.reset();
                }
                Ok(None)
            }
        }
    }

    fn should_exit(&self, code: KeyCode) -> bool {
        match self.stage {
            InteractiveReviewStage::Running => false,
            InteractiveReviewStage::Completed if self.mode == InteractiveReviewMode::Direct => {
                matches!(code, KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter)
            }
            InteractiveReviewStage::Completed => matches!(code, KeyCode::Char('q')),
            _ => matches!(code, KeyCode::Char('q') | KeyCode::Esc),
        }
    }

    fn handle_preview_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        viewport: Rect,
    ) -> bool {
        self.preview_scroll.apply_mouse_in_viewport(
            mouse,
            viewport,
            self.preview_rows(viewport.width),
        )
    }

    fn selected_candidate(&self) -> Option<&ReviewLaunchCandidate> {
        self.candidates.get(self.selected_index)
    }

    fn selected_candidate_text(&self) -> Text<'static> {
        if let Some(outcome) = &self.outcome {
            return Text::from(outcome.review_output.clone());
        }

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
                ]),
            ];

            if let Some(error) = candidate.linear_error.as_deref() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Linear linkage needs attention before remediation can proceed.",
                    Style::default().fg(ratatui::style::Color::Red),
                )));
                lines.push(Line::from(error.to_string()));
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

    fn preview_rows(&self, width: u16) -> usize {
        wrapped_rows(&self.selected_candidate_text().to_string(), width.max(1))
    }

    fn push_note(&mut self, note: String) {
        if self.notes.first().is_some_and(|existing| existing == &note) {
            return;
        }
        self.notes.insert(0, note);
        self.notes.truncate(10);
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
}

fn discover_review_candidates(
    root: &Path,
    gh: &GhCli,
    pr_number: Option<u64>,
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
        discover_eligible_prs(gh, root)?
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
    }
}

fn spawn_review_execution(
    root: std::path::PathBuf,
    config: AppConfig,
    planning_meta: PlanningMeta,
    args: ReviewRunArgs,
    candidate: ReviewLaunchCandidate,
    store: Option<ReviewProjectStore>,
) -> Receiver<ReviewExecutionEvent> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = execute_review_with_progress(
            &root,
            &config,
            &planning_meta,
            &args,
            &candidate,
            store.as_ref(),
            |event| {
                let _ = tx.send(event);
            },
        );

        if let Err(error) = result {
            let _ = tx.send(ReviewExecutionEvent::Failed {
                candidate,
                error: error.to_string(),
            });
        }
    });
    rx
}

fn execute_review_with_progress(
    root: &Path,
    config: &AppConfig,
    planning_meta: &PlanningMeta,
    args: &ReviewRunArgs,
    initial_candidate: &ReviewLaunchCandidate,
    store: Option<&ReviewProjectStore>,
    mut emit: impl FnMut(ReviewExecutionEvent),
) -> Result<()> {
    let gh = GhCli;
    let mut candidate = initial_candidate.clone();
    let mut session =
        review_session_from_candidate(&candidate, ReviewPhase::Claimed, "Queued for review");
    persist_review_session(store, &session)?;

    emit(ReviewExecutionEvent::Progress {
        candidate: candidate.clone(),
        phase: ReviewPhase::Claimed,
        summary: format!("Queued review for PR #{}", candidate.pr_number),
        note: Some("Human approval received. Starting review workflow.".to_string()),
        remediation_required: None,
    });

    let pr = fetch_pr_metadata(&gh, root, candidate.pr_number)?;
    candidate = candidate_from_metadata(&pr);
    session = review_session_from_candidate(
        &candidate,
        ReviewPhase::ReviewStarted,
        "Loading pull request and Linear context",
    );
    persist_review_session(store, &session)?;
    emit(ReviewExecutionEvent::Progress {
        candidate: candidate.clone(),
        phase: ReviewPhase::ReviewStarted,
        summary: format!("Loading review context for PR #{}", candidate.pr_number),
        note: Some("Resolving PR metadata, diff scope, and linked Linear context.".to_string()),
        remediation_required: None,
    });

    let linear_identifier = resolve_linear_identifier(&pr)?;
    let diff = fetch_pr_diff(root, candidate.pr_number)?;
    let context_bundle = load_codebase_context_bundle(root).unwrap_or_default();
    let workflow_contract = load_workflow_contract(root).unwrap_or_default();
    let repo_map = render_repo_map(root).unwrap_or_default();
    let ticket_context =
        gather_linear_ticket_context(root, config, planning_meta, &linear_identifier)?;

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
    let invocation = resolve_agent_invocation_for_planning(config, planning_meta, &agent_args)?;

    candidate.linear_identifier = Some(linear_identifier.clone());
    candidate.linear_error = None;
    session = review_session_from_candidate(
        &candidate,
        ReviewPhase::Running,
        format!("Running agent review with {}", invocation.agent),
    );
    persist_review_session(store, &session)?;
    emit(ReviewExecutionEvent::Progress {
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

    if remediation_required {
        emit(ReviewExecutionEvent::Progress {
            candidate: candidate.clone(),
            phase: ReviewPhase::Running,
            summary: format!("Creating remediation PR for #{}", candidate.pr_number),
            note: Some(
                "Required fixes were found. Preparing the remediation branch and follow-up PR."
                    .to_string(),
            ),
            remediation_required: Some(true),
        });
        let remediation = run_remediation(
            root,
            &pr,
            &linear_identifier,
            &review_output,
            config,
            planning_meta,
            args,
        )?;
        let outcome = InteractiveReviewOutcome {
            candidate: candidate.clone(),
            summary: "Remediation PR created".to_string(),
            review_output,
            remediation_required: true,
            remediation_pr_number: Some(remediation.pr_number),
            remediation_pr_url: Some(remediation.pr_url),
        };
        session = review_session_from_candidate(
            &candidate,
            ReviewPhase::Completed,
            "Remediation PR created",
        );
        session.remediation_required = Some(true);
        session.remediation_pr_number = outcome.remediation_pr_number;
        session.remediation_pr_url = outcome.remediation_pr_url.clone();
        session.linear_identifier = Some(linear_identifier);
        persist_review_session(store, &session)?;
        emit(ReviewExecutionEvent::Completed(outcome));
    } else {
        let outcome = InteractiveReviewOutcome {
            candidate: candidate.clone(),
            summary: "No remediation required".to_string(),
            review_output,
            remediation_required: false,
            remediation_pr_number: None,
            remediation_pr_url: None,
        };
        session = review_session_from_candidate(
            &candidate,
            ReviewPhase::Completed,
            "No remediation required",
        );
        session.remediation_required = Some(false);
        session.linear_identifier = Some(linear_identifier);
        persist_review_session(store, &session)?;
        emit(ReviewExecutionEvent::Completed(outcome));
    }

    Ok(())
}

fn persist_review_session(
    store: Option<&ReviewProjectStore>,
    session: &ReviewSession,
) -> Result<()> {
    if let Some(store) = store {
        let mut state = store.load_state()?;
        state.upsert(session.clone());
        store.save_state(&state)?;
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
            Constraint::Min(0),
            Constraint::Length(7),
        ])
        .split(area);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(outer[1]);
    body[1]
}

fn render_interactive_review(frame: &mut ratatui::Frame<'_>, app: &InteractiveReviewApp) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Min(0),
            Constraint::Length(7),
        ])
        .split(frame.area());
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(outer[1]);

    let status_badge = match app.stage {
        InteractiveReviewStage::Loading => badge("loading", Tone::Info),
        InteractiveReviewStage::Select => badge("select", Tone::Accent),
        InteractiveReviewStage::Confirm => badge("confirm", Tone::Accent),
        InteractiveReviewStage::Running => badge("running", Tone::Info),
        InteractiveReviewStage::Completed => {
            if app.error.is_some() {
                badge("failed", Tone::Danger)
            } else {
                badge("complete", Tone::Success)
            }
        }
        InteractiveReviewStage::Empty => badge("empty", Tone::Muted),
    };
    let header = paragraph(
        Text::from(vec![
            Line::from(vec![
                status_badge,
                Span::raw(" "),
                Span::styled("meta agents review", emphasis_style()),
            ]),
            Line::from(app.status.clone()),
            Line::from(vec![
                Span::styled("Mode ", label_style()),
                Span::raw(match app.mode {
                    InteractiveReviewMode::Direct => "single PR".to_string(),
                    InteractiveReviewMode::Discovery => "guided queue".to_string(),
                }),
                Span::styled("  Candidates ", label_style()),
                Span::raw(app.candidates.len().to_string()),
            ]),
            key_hints(&interactive_key_hints(app)),
        ]),
        panel_title("Review Flow", false),
    );
    frame.render_widget(header, outer[0]);

    render_interactive_candidate_list(frame, body[0], app);
    let preview = scrollable_content_paragraph(
        app.selected_candidate_text(),
        panel_title(
            match app.outcome {
                Some(_) => "Review Output",
                None => "Selected PR Preview",
            },
            app.focus == InteractiveReviewFocus::Preview,
        ),
        &app.preview_scroll,
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(preview, body[1]);

    let footer = paragraph(
        interactive_footer_text(app),
        panel_title("Next Step", false),
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(footer, outer[2]);
}

fn render_interactive_candidate_list(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &InteractiveReviewApp,
) {
    let items = if app.candidates.is_empty() {
        vec![ListItem::new(empty_state(
            "No pull requests are currently queued for review.",
            "Add the `metastack` label to a PR or press `q` to exit.",
        ))]
    } else {
        app.candidates
            .iter()
            .map(|candidate| {
                let linear = candidate
                    .linear_identifier
                    .clone()
                    .unwrap_or_else(|| "unresolved".to_string());
                ListItem::new(Text::from(vec![
                    Line::from(vec![
                        badge(format!("#{}", candidate.pr_number), Tone::Accent),
                        Span::raw(" "),
                        Span::styled(candidate.title.clone(), emphasis_style()),
                    ]),
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
                ]))
            })
            .collect()
    };

    let mut state = ListState::default();
    if !app.candidates.is_empty() {
        state.select(Some(
            app.selected_index
                .min(app.candidates.len().saturating_sub(1)),
        ));
    }

    let widget = list(
        items,
        panel_title(
            match app.mode {
                InteractiveReviewMode::Direct => "Review Candidate",
                InteractiveReviewMode::Discovery => "Candidate PRs",
            },
            app.focus == InteractiveReviewFocus::Candidates,
        ),
    );
    frame.render_stateful_widget(widget, area, &mut state);
}

fn interactive_footer_text(app: &InteractiveReviewApp) -> Text<'static> {
    if let Some(error) = app.error.as_deref() {
        return Text::from(vec![
            Line::from(Span::styled("The review flow failed.", emphasis_style())),
            Line::from(""),
            Line::from(error.to_string()),
            Line::from(""),
            Line::from(
                "Press `q` to exit, or `Esc` to return to the candidate list when available.",
            ),
        ]);
    }

    match app.stage {
        InteractiveReviewStage::Loading => Text::from(vec![
            Line::from("The dashboard is gathering discovery and prerequisite state."),
            Line::from(""),
            Line::from(
                "Stay in this screen while MetaStack verifies auth, loads PR metadata, and prepares review previews.",
            ),
        ]),
        InteractiveReviewStage::Select => Text::from(vec![
            Line::from("Review is human-gated."),
            Line::from(""),
            Line::from(
                "Use Up/Down to choose a PR. Tab moves into the preview pane. Enter opens the approval screen; no review work starts until you confirm.",
            ),
        ]),
        InteractiveReviewStage::Confirm => Text::from(vec![
            Line::from("This PR will be reviewed only after explicit approval."),
            Line::from(""),
            Line::from(
                "Press Enter to start the audit and possible remediation flow, or Esc to go back without launching anything.",
            ),
        ]),
        InteractiveReviewStage::Running => Text::from(vec![
            Line::from("A review session is active."),
            Line::from(""),
            Line::from(
                "The dashboard will stay on this screen until the review completes so the current phase and remediation status remain visible.",
            ),
        ]),
        InteractiveReviewStage::Completed => Text::from(vec![
            Line::from("The review session finished."),
            Line::from(""),
            Line::from(match app.outcome.as_ref() {
                Some(outcome) if outcome.remediation_required => {
                    "Required fixes were found and a remediation PR was opened."
                }
                Some(_) => "No remediation PR was needed for the selected review.",
                None => "The session ended without a completed review artifact.",
            }),
            Line::from(""),
            Line::from(match app.mode {
                InteractiveReviewMode::Direct => "Press Enter or q to exit this review session.",
                InteractiveReviewMode::Discovery => {
                    "Press Esc to return to the candidate list for another review, or q to exit."
                }
            }),
        ]),
        InteractiveReviewStage::Empty => Text::from(vec![
            Line::from("No review candidates were found."),
            Line::from(""),
            Line::from("Press q to exit. Run the command again after PRs are labeled for review."),
        ]),
    }
}

fn interactive_key_hints(app: &InteractiveReviewApp) -> Vec<(&'static str, &'static str)> {
    match app.stage {
        InteractiveReviewStage::Loading => vec![("q", "exit after load")],
        InteractiveReviewStage::Select => vec![
            ("Up/Down", "move"),
            ("Tab", "focus"),
            ("PgUp/PgDn", "scroll preview"),
            ("Enter", "approve screen"),
            ("q", "exit"),
        ],
        InteractiveReviewStage::Confirm => {
            vec![("Enter", "start review"), ("Esc", "back"), ("q", "exit")]
        }
        InteractiveReviewStage::Running => vec![("q", "disabled while running")],
        InteractiveReviewStage::Completed => match app.mode {
            InteractiveReviewMode::Direct => vec![("Enter", "exit"), ("q", "exit")],
            InteractiveReviewMode::Discovery => {
                vec![("Esc", "back to list"), ("q", "exit")]
            }
        },
        InteractiveReviewStage::Empty => vec![("q", "exit")],
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

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!(
            "the interactive review dashboard requires a TTY; use `meta agents review <PR_NUMBER> --dry-run`, `meta agents review --once`, or `meta agents review --once --json` for scripted runs"
        );
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

    #[test]
    fn interactive_dashboard_copy_mentions_explicit_approval() -> Result<()> {
        let mut app = InteractiveReviewApp::new(InteractiveReviewMode::Discovery);
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
        }]);

        let backend = ratatui::backend::TestBackend::new(120, 36);
        let mut terminal = ratatui::Terminal::new(backend)?;
        terminal.draw(|frame| render_interactive_review(frame, &app))?;
        let snapshot = format!("{}", terminal.backend());

        assert!(snapshot.contains("Review is human-gated"));
        assert!(snapshot.contains("Enter opens the approval screen"));
        Ok(())
    }
}
