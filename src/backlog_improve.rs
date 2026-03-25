use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
#[cfg(test)]
use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{ListItem, ListState, Wrap};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::macros::format_description;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::agents::{AgentContinuation, run_agent_capture, run_agent_capture_with_continuation};
use crate::backlog::{
    BacklogIssueMetadata, INDEX_FILE_NAME, ManagedFileRecord, save_issue_metadata,
    write_issue_description,
};
use crate::branding;
use crate::cli::{BacklogImproveArgs, BacklogImproveModeArg, RunAgentArgs};
use crate::config::AGENT_ROUTE_BACKLOG_IMPROVE;
use crate::fs::{
    PlanningPaths, canonicalize_existing_dir, display_path, ensure_dir, write_text_file,
};
use crate::linear::browser::{
    IssueSearchResult, render_issue_preview, render_issue_row, search_issues,
};
use crate::linear::{
    IssueEditSpec, IssueListFilters, IssueSummary, LinearService, ReqwestLinearClient,
};
use crate::progress::{LoadingPanelData, SPINNER_FRAMES, render_loading_panel};
use crate::repo_target::RepoTarget;
use crate::scaffold::ensure_planning_layout;
use crate::tui::fields::InputFieldState;
use crate::tui::markdown::render_markdown;
use crate::tui::scroll::{ScrollState, plain_text, scrollable_content_paragraph, wrapped_rows};
use crate::tui::spaced_list::spaced_list;
use crate::tui::theme::{
    Tone, badge, emphasis_style, empty_state, key_hints, label_style, muted_style, panel,
    panel_title, paragraph, tone_style,
};
use crate::{LinearCommandContext, load_linear_command_context};

const ORIGINAL_SNAPSHOT_FILE: &str = "original.md";
const ISSUE_SNAPSHOT_FILE: &str = "issue.json";
const LOCAL_INDEX_SNAPSHOT_FILE: &str = "local-index.md";
const PROPOSAL_JSON_FILE: &str = "proposal.json";
const PROPOSAL_MARKDOWN_FILE: &str = "proposal.md";
const FOLLOW_UP_ANSWERS_FILE: &str = "follow-up-answers.md";
const SUMMARY_FILE: &str = "summary.json";
const MAX_FOLLOW_UP_QUESTION_ROUNDS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum ImprovementRoute {
    #[default]
    NoUpdateNeeded,
    ReadyForUpdate,
    NeedsPlanning,
    NeedsQuestions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ImprovementOutput {
    summary: String,
    #[serde(default)]
    needs_improvement: bool,
    #[serde(default)]
    route: Option<ImprovementRoute>,
    #[serde(default)]
    recommendation: Option<String>,
    #[serde(default)]
    findings: ImprovementFindings,
    #[serde(default)]
    context_requirements: Vec<String>,
    #[serde(default)]
    follow_up_questions: Vec<String>,
    #[serde(default)]
    proposal: ImprovementProposal,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ImprovementFindings {
    #[serde(default)]
    title_gaps: Vec<String>,
    #[serde(default)]
    description_gaps: Vec<String>,
    #[serde(default)]
    acceptance_criteria_gaps: Vec<String>,
    #[serde(default)]
    metadata_gaps: Vec<String>,
    #[serde(default)]
    structure_opportunities: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ImprovementProposal {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    priority: Option<u8>,
    #[serde(default)]
    estimate: Option<f64>,
    #[serde(default)]
    labels: Option<Vec<String>>,
    #[serde(default)]
    parent_issue_identifier: Option<String>,
    #[serde(default)]
    acceptance_criteria: Vec<String>,
}

/// Classifies the kind of error encountered during the apply-back flow so that
/// downstream error messages can distinguish a Linear permission/auth failure from
/// generic operational errors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ImprovementErrorKind {
    /// Linear API returned a permission or authentication error (HTTP 401/403 or
    /// GraphQL error containing auth/permission keywords).
    LinearPermission,
    /// A network-level failure prevented the Linear request from completing.
    LinearNetwork,
    /// Any other error during the apply-back flow.
    Other,
}

#[derive(Debug, Clone, Serialize)]
struct ImprovementApplyRecord {
    requested: bool,
    local_updated: bool,
    remote_updated: bool,
    local_before_path: Option<String>,
    local_after_path: Option<String>,
    remote_before_path: Option<String>,
    remote_after_path: Option<String>,
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error_kind: Option<ImprovementErrorKind>,
}

#[derive(Debug, Clone, Serialize)]
struct ImprovementRunSummary {
    run_id: String,
    issue_identifier: String,
    issue_title: String,
    mode: String,
    route: String,
    decision: String,
    started_at: String,
    completed_at: String,
    needs_improvement: bool,
    original_snapshot_path: String,
    issue_snapshot_path: String,
    local_index_snapshot_path: Option<String>,
    proposal_json_path: String,
    proposal_markdown_path: String,
    apply: ImprovementApplyRecord,
}

#[derive(Debug, Clone)]
struct ImprovementReport {
    issue_identifier: String,
    run_dir: PathBuf,
    status_label: String,
}

#[derive(Debug, Clone)]
struct ImprovementIssueRun {
    issue: IssueSummary,
    mode: BacklogImproveModeArg,
    run_id: String,
    run_dir: PathBuf,
    original_description: String,
    local_index_before: Option<String>,
    local_index_snapshot_path: Option<PathBuf>,
    original_snapshot_path: PathBuf,
    issue_snapshot_path: PathBuf,
    proposal_json_path: PathBuf,
    proposal_markdown_path: PathBuf,
    started_at: String,
    output: ImprovementOutput,
}

#[derive(Debug, Clone)]
struct ImprovementQuestionAnswer {
    question: String,
    answer: InputFieldState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImprovementReviewFocus {
    Recommendation,
    Proposal,
}

#[derive(Debug, Clone)]
struct ImprovementReviewApp {
    issue_position: usize,
    issue_total: usize,
    issue: IssueSummary,
    output: ImprovementOutput,
    questions: Vec<ImprovementQuestionAnswer>,
    selected_question: usize,
    question_round: usize,
    review_focus: ImprovementReviewFocus,
    recommendation_scroll: ScrollState,
    proposal_scroll: ScrollState,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct ImprovementReviewProgress {
    issue_position: usize,
    issue_total: usize,
    question_round: usize,
}

#[derive(Debug, Clone)]
enum ImprovementReviewExit {
    Cancelled,
    Accepted {
        decision: String,
        apply_requested: bool,
    },
    FollowUp {
        answers: Vec<(String, String)>,
        question_round: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImprovementPrimaryAction {
    ApplyUpdate,
    KeepUnchanged,
    AcceptPlanning,
    StartQuestions,
    ContinueQuestions,
}

impl ImprovementRoute {
    fn as_str(self) -> &'static str {
        match self {
            Self::NoUpdateNeeded => "no_update_needed",
            Self::ReadyForUpdate => "ready_for_update",
            Self::NeedsPlanning => "needs_planning",
            Self::NeedsQuestions => "needs_questions",
        }
    }

    fn badge(self) -> (&'static str, Tone) {
        match self {
            Self::NoUpdateNeeded => ("no update", Tone::Info),
            Self::ReadyForUpdate => ("ready", Tone::Accent),
            Self::NeedsPlanning => ("planning", Tone::Info),
            Self::NeedsQuestions => ("questions", Tone::Info),
        }
    }
}

impl ImprovementOutput {
    fn route(&self) -> ImprovementRoute {
        self.route.unwrap_or(ImprovementRoute::NoUpdateNeeded)
    }
}

#[derive(Debug, Clone)]
struct ImprovementLoadingState {
    message: String,
    detail: String,
    spinner_index: usize,
}

enum ImprovementLoadingDisplay {
    Tui(Terminal<CrosstermBackend<io::Stdout>>),
    Text {
        last_message: Option<String>,
        last_detail: Option<String>,
    },
}

impl ImprovementLoadingDisplay {
    fn start() -> Result<Self> {
        Ok(if io::stdout().is_terminal() {
            let mut stdout = io::stdout();
            execute!(stdout, EnterAlternateScreen, Hide)
                .context("failed to enter the backlog improvement loading dashboard")?;
            Self::Tui(Terminal::new(CrosstermBackend::new(stdout))?)
        } else {
            Self::Text {
                last_message: None,
                last_detail: None,
            }
        })
    }

    fn render(&mut self, state: &ImprovementLoadingState) -> Result<()> {
        match self {
            Self::Tui(terminal) => {
                terminal.draw(|frame| {
                    render_loading_panel(
                        frame,
                        frame.area(),
                        &LoadingPanelData {
                            title: "Agent Working [loading]".to_string(),
                            message: state.message.clone(),
                            detail: state.detail.clone(),
                            spinner_index: state.spinner_index,
                            status_line: "State: loading. The dashboard advances automatically as Linear and the agent respond.".to_string(),
                        },
                    );
                })?;
            }
            Self::Text {
                last_message,
                last_detail,
            } => {
                if last_message.as_ref() != Some(&state.message) {
                    println!("==> {}", state.message);
                    *last_message = Some(state.message.clone());
                }
                if last_detail.as_ref() != Some(&state.detail) {
                    println!("    {}", state.detail);
                    *last_detail = Some(state.detail.clone());
                }
            }
        }
        Ok(())
    }
}

impl Drop for ImprovementLoadingDisplay {
    fn drop(&mut self) {
        let Self::Tui(terminal) = self else {
            return;
        };

        let _ = execute!(terminal.backend_mut(), Show, LeaveAlternateScreen);
    }
}

#[derive(Debug, Clone)]
struct ImprovementProgressUpdate {
    message: String,
    detail: String,
}

#[derive(Debug, Clone)]
struct ImprovementIssueSelection {
    issues: Vec<IssueSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstructionPromptFocus {
    Editor,
    Preview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InstructionPromptExit {
    /// User pressed Escape or triggered quit — exit the TUI without continuing.
    Cancelled,
    /// User pressed Enter or Ctrl+S — continue with optional instructions.
    Continue(Option<String>),
}

#[derive(Debug, Clone)]
struct InstructionPromptApp {
    input: InputFieldState,
    issues: Vec<IssueSummary>,
    issue_cursor: usize,
    focus: InstructionPromptFocus,
    preview_scroll: ScrollState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImprovementPickerFocus {
    List,
    Preview,
}

#[derive(Debug, Clone)]
enum ImprovementDashboardExit {
    Cancelled,
    Selected(ImprovementIssueSelection),
}

#[derive(Debug, Clone)]
struct ImprovementDashboardApp {
    query: InputFieldState,
    issues: Vec<IssueSummary>,
    cursor: usize,
    selected: Vec<bool>,
    focus: ImprovementPickerFocus,
    preview_scroll: ScrollState,
    completed: Option<ImprovementIssueSelection>,
}

/// Review repo-scoped backlog issues for hygiene gaps and optionally apply improvements.
///
/// Returns an error when repo planning metadata is missing, scoped issue discovery fails, the
/// configured agent cannot produce a valid proposal, or the requested Linear mutations fail.
pub async fn run_backlog_improve(args: &BacklogImproveArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&args.client.root)?;
    ensure_planning_layout(&root, false)?;
    let command_context = load_linear_command_context(&args.client, None)?;
    let issues = load_backlog_improve_issues_with_loading(&command_context, args).await?;

    if issues.is_empty() {
        println!(
            "No repo-scoped issues matched state `{}` under the configured backlog scope.",
            args.state
        );
        return Ok(());
    }

    let selected_issues =
        if args.issues.is_empty() && io::stdin().is_terminal() && io::stdout().is_terminal() {
            match run_improvement_dashboard(issues.clone())? {
                ImprovementDashboardExit::Cancelled => {
                    println!("Backlog improvement cancelled.");
                    return Ok(());
                }
                ImprovementDashboardExit::Selected(selection) => selection.issues,
            }
        } else {
            issues.clone()
        };

    let related_backlog_issues =
        load_related_backlog_issues(&command_context, args, issues.len()).await?;

    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        let summary = run_interactive_improvement_session(
            &root,
            &command_context.service,
            selected_issues,
            related_backlog_issues,
            args,
        )
        .await?;
        println!("{summary}");
        return Ok(());
    }

    let (sender, mut receiver) = unbounded_channel();
    let worker_args = args.clone();
    let mut worker = tokio::spawn(async move {
        run_backlog_improve_job(worker_args, selected_issues, related_backlog_issues, sender).await
    });
    let mut display = ImprovementLoadingDisplay::start()?;
    let mut loading = ImprovementLoadingState {
        message: "Preparing backlog improvement review".to_string(),
        detail: "Starting the selected issue reviews and waiting for the first agent response."
            .to_string(),
        spinner_index: 0,
    };
    display.render(&loading)?;

    let summary = loop {
        tokio::select! {
            update = receiver.recv() => {
                if let Some(update) = update {
                    loading.message = update.message;
                    loading.detail = update.detail;
                    display.render(&loading)?;
                }
            }
            result = &mut worker => {
                break result
                    .context("backlog improvement worker exited unexpectedly")??;
            }
            _ = tokio::time::sleep(Duration::from_millis(120)) => {
                loading.spinner_index = (loading.spinner_index + 1) % SPINNER_FRAMES.len();
                display.render(&loading)?;
            }
        }
    };

    drop(display);
    println!("{summary}");
    Ok(())
}

async fn run_interactive_improvement_session(
    root: &Path,
    service: &LinearService<ReqwestLinearClient>,
    issues: Vec<IssueSummary>,
    related_backlog_issues: Vec<IssueSummary>,
    args: &BacklogImproveArgs,
) -> Result<String> {
    let mut stdout = io::stdout();
    enable_raw_mode().context("failed to enable raw mode for backlog improve review session")?;
    execute!(
        stdout,
        EnterAlternateScreen,
        Hide,
        EnableMouseCapture,
        EnableBracketedPaste
    )
    .context("failed to enter alternate screen for backlog improve review session")?;
    let _cleanup = ImprovementReviewCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let instructions = match run_instruction_prompt(&mut terminal, &issues)? {
        InstructionPromptExit::Cancelled => {
            return Ok(render_improvement_reports(root, &[]));
        }
        InstructionPromptExit::Continue(text) => text,
    };

    let mut reports = Vec::with_capacity(issues.len());

    for (index, issue) in issues.iter().enumerate() {
        let mut continuation = None;
        let mut question_round = 0usize;
        let mut issue_run = analyze_issue_with_loading(
            &mut terminal,
            root,
            issue,
            &related_backlog_issues,
            args,
            &mut continuation,
            instructions.as_deref(),
            ImprovementReviewProgress {
                issue_position: index + 1,
                issue_total: issues.len(),
                question_round,
            },
        )
        .await?;

        loop {
            match run_improvement_review_dashboard(
                &mut terminal,
                index + 1,
                issues.len(),
                issue,
                &issue_run.output,
                question_round,
            )? {
                ImprovementReviewExit::Cancelled => {
                    return Ok(render_improvement_reports(root, &reports));
                }
                ImprovementReviewExit::Accepted {
                    decision,
                    apply_requested,
                } => {
                    let apply = if apply_requested {
                        apply_improvement(root, service, &issue_run).await?
                    } else {
                        ImprovementApplyRecord {
                            requested: false,
                            local_updated: false,
                            remote_updated: false,
                            local_before_path: None,
                            local_after_path: None,
                            remote_before_path: None,
                            remote_after_path: None,
                            error: None,
                            error_kind: None,
                        }
                    };
                    let report = finalize_issue_run(
                        root,
                        &issue_run,
                        apply,
                        decision.clone(),
                        match (apply_requested, decision.as_str()) {
                            (true, _) => format!("{} applied", render_mode(args.mode)),
                            (false, "accepted_no_update_needed") => {
                                "accepted no-update recommendation".to_string()
                            }
                            (false, "accepted_needs_planning") => {
                                "accepted planning/context recommendation".to_string()
                            }
                            (false, "accepted_needs_questions") => {
                                "accepted follow-up-question recommendation".to_string()
                            }
                            (false, "skipped_no_update_needed")
                            | (false, "skipped_ready_for_update")
                            | (false, "skipped_needs_planning")
                            | (false, "skipped_needs_questions") => {
                                "skipped without changes".to_string()
                            }
                            (false, "rejected_no_update_needed")
                            | (false, "rejected_ready_for_update")
                            | (false, "rejected_needs_planning")
                            | (false, "rejected_needs_questions") => {
                                "rejected recommendation".to_string()
                            }
                            _ => "reviewed".to_string(),
                        },
                    )?;
                    reports.push(report);
                    break;
                }
                ImprovementReviewExit::FollowUp {
                    answers,
                    question_round: next_question_round,
                } => {
                    question_round = next_question_round;
                    issue_run = continue_issue_with_follow_up_loading(
                        &mut terminal,
                        root,
                        issue_run,
                        args,
                        &answers,
                        &mut continuation,
                        instructions.as_deref(),
                        ImprovementReviewProgress {
                            issue_position: index + 1,
                            issue_total: issues.len(),
                            question_round: next_question_round,
                        },
                    )?;
                }
            }
        }
    }

    Ok(render_improvement_reports(root, &reports))
}

async fn load_backlog_improve_issues_with_loading(
    command_context: &LinearCommandContext,
    args: &BacklogImproveArgs,
) -> Result<Vec<IssueSummary>> {
    let mut display = ImprovementLoadingDisplay::start()?;
    let loading = ImprovementLoadingState {
        message: "Reading Linear backlog tickets".to_string(),
        detail: format!(
            "Loading repo-scoped issues in state `{}` with limit {}.",
            args.state, args.limit
        ),
        spinner_index: 0,
    };
    display.render(&loading)?;
    let issues = load_target_issues(command_context, args).await?;
    drop(display);
    Ok(issues)
}

async fn run_backlog_improve_job(
    args: BacklogImproveArgs,
    issues: Vec<IssueSummary>,
    related_backlog_issues: Vec<IssueSummary>,
    sender: UnboundedSender<ImprovementProgressUpdate>,
) -> Result<String> {
    let root = canonicalize_existing_dir(&args.client.root)?;
    ensure_planning_layout(&root, false)?;
    let command_context = load_linear_command_context(&args.client, None)?;

    let mut reports = Vec::with_capacity(issues.len());
    for (index, issue) in issues.iter().enumerate() {
        send_improvement_progress(
            &sender,
            format!(
                "Improving {} ({}/{})",
                issue.identifier,
                index + 1,
                issues.len()
            ),
            format!(
                "Saving snapshots, reading local backlog context, and generating the follow-up proposal for {}.",
                issue.title
            ),
        );
        let report = improve_issue(
            &root,
            &command_context.service,
            issue,
            &related_backlog_issues,
            &args,
            Some(&sender),
        )
        .await?;
        reports.push(report);
    }

    Ok(render_improvement_reports(&root, &reports))
}

async fn load_related_backlog_issues(
    command_context: &LinearCommandContext,
    args: &BacklogImproveArgs,
    target_issue_count: usize,
) -> Result<Vec<IssueSummary>> {
    command_context
        .service
        .list_issues(IssueListFilters {
            team: command_context.default_team.clone(),
            project_id: command_context.default_project_id.clone(),
            state: Some(args.state.clone()),
            limit: args.limit.max(target_issue_count).max(25),
            ..IssueListFilters::default()
        })
        .await
}

fn send_improvement_progress(
    sender: &UnboundedSender<ImprovementProgressUpdate>,
    message: impl Into<String>,
    detail: impl Into<String>,
) {
    let _ = sender.send(ImprovementProgressUpdate {
        message: message.into(),
        detail: detail.into(),
    });
}

fn run_improvement_dashboard(issues: Vec<IssueSummary>) -> Result<ImprovementDashboardExit> {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        bail!(
            "the interactive backlog improvement dashboard requires a TTY; pass explicit issue identifiers for scripted runs"
        );
    }

    let mut stdout = io::stdout();
    enable_raw_mode().context("failed to enable raw mode for backlog improve dashboard")?;
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )
    .context("failed to enter alternate screen for backlog improve dashboard")?;
    let _cleanup = ImprovementDashboardCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = ImprovementDashboardApp::new(issues);
    let mut preview_viewport = Rect::default();

    loop {
        terminal.draw(|frame| {
            preview_viewport = render_improvement_dashboard(frame, &app);
        })?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    return Ok(ImprovementDashboardExit::Cancelled);
                }
                KeyCode::Tab => {
                    app.focus = match app.focus {
                        ImprovementPickerFocus::List => ImprovementPickerFocus::Preview,
                        ImprovementPickerFocus::Preview => ImprovementPickerFocus::List,
                    };
                }
                KeyCode::Up => {
                    if app.focus == ImprovementPickerFocus::Preview {
                        let _ = app.preview_scroll.apply_key_code_in_viewport(
                            KeyCode::Up,
                            preview_viewport,
                            app.preview_content_rows(preview_viewport.width.max(1)),
                        );
                    } else {
                        app.move_up();
                    }
                }
                KeyCode::Down => {
                    if app.focus == ImprovementPickerFocus::Preview {
                        let _ = app.preview_scroll.apply_key_code_in_viewport(
                            KeyCode::Down,
                            preview_viewport,
                            app.preview_content_rows(preview_viewport.width.max(1)),
                        );
                    } else {
                        app.move_down();
                    }
                }
                KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
                    if app.focus == ImprovementPickerFocus::Preview =>
                {
                    let _ = app.preview_scroll.apply_key_in_viewport(
                        key,
                        preview_viewport,
                        app.preview_content_rows(preview_viewport.width.max(1)),
                    );
                }
                KeyCode::Char(' ') if app.focus == ImprovementPickerFocus::List => {
                    app.toggle();
                }
                KeyCode::Enter => {
                    let selection = app.select();
                    return Ok(ImprovementDashboardExit::Selected(selection));
                }
                _ => {
                    if app.focus == ImprovementPickerFocus::List && app.query.handle_key(key) {
                        app.cursor = 0;
                        app.preview_scroll.reset();
                    }
                }
            },
            Event::Paste(text) => {
                if app.focus == ImprovementPickerFocus::List && app.query.paste(&text) {
                    app.cursor = 0;
                    app.preview_scroll.reset();
                }
            }
            Event::Mouse(mouse) => {
                let _ = app.preview_scroll.apply_mouse_in_viewport(
                    mouse,
                    preview_viewport,
                    app.preview_content_rows(preview_viewport.width.max(1)),
                );
            }
            _ => {}
        }
    }
}

#[cfg(test)]
fn render_improvement_dashboard_once(
    issues: Vec<IssueSummary>,
    width: u16,
    height: u16,
) -> Result<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    let app = ImprovementDashboardApp::new(issues);
    terminal.draw(|frame| {
        render_improvement_dashboard(frame, &app);
    })?;
    Ok(improvement_dashboard_snapshot(terminal.backend()))
}

/// Renders the search-first improvement dashboard and returns the preview pane viewport rect.
fn render_improvement_dashboard(frame: &mut Frame<'_>, app: &ImprovementDashboardApp) -> Rect {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(0)])
        .split(frame.area());
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(outer[1]);
    let left_split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(body[0]);

    let header = paragraph(
        Text::from(vec![
            Line::from(vec![
                badge("select", Tone::Info),
                Span::raw(" "),
                Span::styled(
                    format!(
                        "Review {} issue(s) in `{}`",
                        app.issues.len(),
                        app.state_label()
                    ),
                    emphasis_style(),
                ),
            ]),
            Line::from(Span::styled(app.summary_line(), emphasis_style())),
            key_hints(&[
                ("Tab", "focus"),
                ("Space", "select"),
                ("Enter", "start review"),
                ("Esc", "cancel"),
            ]),
        ]),
        panel_title(format!("{} backlog improve", branding::COMMAND_NAME), false),
    );
    frame.render_widget(header, outer[0]);

    let list_focused = app.focus == ImprovementPickerFocus::List;
    let query_block = panel(panel_title("Search", list_focused));
    let query_inner = query_block.inner(left_split[0]);
    let rendered_query = app.query.render_with_width(
        "Search by identifier, title, or description...",
        list_focused,
        query_inner.width,
    );
    let query_widget = rendered_query.paragraph(query_block);
    frame.render_widget(query_widget, left_split[0]);
    if list_focused {
        rendered_query.set_cursor(frame, query_inner);
    }

    let preview_area = body[1];
    render_improvement_issue_list(frame, left_split[1], app);
    render_improvement_issue_preview(frame, preview_area, app);
    preview_area
}

fn render_improvement_issue_list(frame: &mut Frame<'_>, area: Rect, app: &ImprovementDashboardApp) {
    let filtered = dashboard_search_results(app);
    let title = panel_title(
        format!("Issues ({}/{})", filtered.len(), app.issues.len()),
        app.focus == ImprovementPickerFocus::List,
    );
    let items = if filtered.is_empty() {
        vec![ListItem::new(empty_state(
            "No issues match the current search.",
            "Adjust the search query or clear it to see all issues.",
        ))]
    } else {
        filtered
            .iter()
            .filter_map(|result| {
                app.issues.get(result.issue_index).map(|issue| {
                    let explicitly_selected = app
                        .selected
                        .get(result.issue_index)
                        .copied()
                        .unwrap_or(false);
                    let status_label = if app.any_selected() {
                        if explicitly_selected {
                            Some("selected")
                        } else {
                            Some("skipped")
                        }
                    } else {
                        None
                    };
                    render_issue_row(issue, Some(result), status_label)
                })
            })
            .collect::<Vec<_>>()
    };

    let mut state = ListState::default();
    state.select(Some(app.cursor.min(filtered.len().saturating_sub(1))));
    let list_widget = spaced_list(items, title);
    frame.render_stateful_widget(list_widget, area, &mut state);
}

fn render_improvement_issue_preview(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &ImprovementDashboardApp,
) {
    let filtered = dashboard_search_results(app);
    let preview_focused = app.focus == ImprovementPickerFocus::Preview;
    let preview = filtered
        .get(app.cursor)
        .and_then(|result| {
            app.issues.get(result.issue_index).map(|issue| {
                let status_label = if app
                    .selected
                    .get(result.issue_index)
                    .copied()
                    .unwrap_or(false)
                {
                    Some("selected")
                } else {
                    None
                };
                render_issue_preview(
                    issue,
                    Some(result),
                    status_label,
                    "_No description provided._",
                )
            })
        })
        .unwrap_or_else(|| {
            empty_state(
                "No issue is available to preview.",
                "Type to search or clear the query to see all issues.",
            )
        });
    let widget = scrollable_content_paragraph(
        preview,
        panel_title("Issue Preview", preview_focused),
        &app.preview_scroll,
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(widget, area);
}

#[allow(clippy::too_many_arguments)]
async fn analyze_issue_with_loading(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    root: &Path,
    issue: &IssueSummary,
    related_backlog_issues: &[IssueSummary],
    args: &BacklogImproveArgs,
    continuation: &mut Option<AgentContinuation>,
    instructions: Option<&str>,
    progress: ImprovementReviewProgress,
) -> Result<ImprovementIssueRun> {
    let detail = if progress.question_round == 0 {
        format!(
            "Issue {}/{}: analyzing {} and preparing the guided recommendation.",
            progress.issue_position, progress.issue_total, issue.identifier
        )
    } else {
        format!(
            "Issue {}/{}: incorporating follow-up answers for {} (round {}).",
            progress.issue_position,
            progress.issue_total,
            issue.identifier,
            progress.question_round
        )
    };
    terminal.draw(|frame| {
        render_loading_panel(
            frame,
            frame.area(),
            &LoadingPanelData {
                title: "Backlog Improve [analysis]".to_string(),
                message: format!("Reviewing {}", issue.identifier),
                detail,
                spinner_index: progress.question_round % SPINNER_FRAMES.len(),
                status_line:
                    "State: agent analysis in progress. The dashboard stays in review mode once the turn completes."
                        .to_string(),
            },
        );
    })?;
    analyze_issue(
        root,
        issue,
        related_backlog_issues,
        args,
        Some(continuation),
        instructions,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn continue_issue_with_follow_up_loading(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    root: &Path,
    mut issue_run: ImprovementIssueRun,
    args: &BacklogImproveArgs,
    answers: &[(String, String)],
    continuation: &mut Option<AgentContinuation>,
    instructions: Option<&str>,
    progress: ImprovementReviewProgress,
) -> Result<ImprovementIssueRun> {
    terminal.draw(|frame| {
        render_loading_panel(
            frame,
            frame.area(),
            &LoadingPanelData {
                title: "Backlog Improve [follow-up]".to_string(),
                message: format!("Continuing {}", issue_run.issue.identifier),
                detail: format!(
                    "Issue {}/{}: rerunning the recommendation with {} answered follow-up question(s).",
                    progress.issue_position,
                    progress.issue_total,
                    answers.len()
                ),
                spinner_index: progress.question_round % SPINNER_FRAMES.len(),
                status_line:
                    "State: waiting for the agent to turn answers into a concrete recommendation."
                        .to_string(),
            },
        );
    })?;

    let answers_path = issue_run.run_dir.join(FOLLOW_UP_ANSWERS_FILE);
    write_text_file(
        &answers_path,
        &render_follow_up_answers_markdown(answers),
        true,
    )?;
    let prompt = render_follow_up_prompt(&issue_run.issue, &issue_run.output, answers);
    let report = run_agent_capture_with_continuation(
        &RunAgentArgs {
            root: Some(root.to_path_buf()),
            route_key: Some(AGENT_ROUTE_BACKLOG_IMPROVE.to_string()),
            agent: args.agent.clone(),
            prompt,
            instructions: instructions.map(str::to_string),
            model: args.model.clone(),
            reasoning: args.reasoning.clone(),
            transport: None,
            attachments: Vec::new(),
        },
        continuation,
    )
    .with_context(|| {
        format!(
            "{} backlog improve requires a configured local agent to continue backlog issue review",
            branding::COMMAND_NAME
        )
    })?;
    let parsed: ImprovementOutput =
        parse_agent_json(&report.stdout, "backlog improvement follow-up")?;
    let mut normalized = normalize_improvement_output(&issue_run.issue, parsed)?;
    if normalized.route() == ImprovementRoute::NeedsQuestions
        && progress.question_round >= MAX_FOLLOW_UP_QUESTION_ROUNDS
    {
        normalized.route = Some(ImprovementRoute::NeedsPlanning);
        normalized.follow_up_questions.clear();
        normalized.context_requirements.push(
            "The agent still needed direct answers after multiple rounds. Gather the missing planning context offline before retrying."
                .to_string(),
        );
        normalized.recommendation = Some(
            "Stop the improve flow for now and gather the missing planning context before proposing an update."
                .to_string(),
        );
    }
    issue_run.output = normalized;
    write_text_file(
        &issue_run.proposal_json_path,
        &serde_json::to_string_pretty(&issue_run.output)
            .context("failed to encode follow-up backlog improvement proposal")?,
        true,
    )?;
    write_text_file(
        &issue_run.proposal_markdown_path,
        &render_proposal_markdown(args.mode, &issue_run.output),
        true,
    )?;

    Ok(issue_run)
}

fn run_improvement_review_dashboard(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    issue_position: usize,
    issue_total: usize,
    issue: &IssueSummary,
    output: &ImprovementOutput,
    question_round: usize,
) -> Result<ImprovementReviewExit> {
    let mut app = ImprovementReviewApp::new(
        issue_position,
        issue_total,
        issue.clone(),
        output.clone(),
        question_round,
    );
    let mut review_viewports = ReviewViewports::default();

    loop {
        terminal.draw(|frame| {
            review_viewports = render_improvement_review(frame, &app);
        })?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('q') | KeyCode::Esc if app.questions.is_empty() => {
                    return Ok(ImprovementReviewExit::Cancelled);
                }
                KeyCode::Esc => {
                    app.error = None;
                    app.questions.clear();
                    app.selected_question = 0;
                }
                KeyCode::Tab if app.questions.is_empty() => {
                    app.review_focus = match app.review_focus {
                        ImprovementReviewFocus::Recommendation => ImprovementReviewFocus::Proposal,
                        ImprovementReviewFocus::Proposal => ImprovementReviewFocus::Recommendation,
                    };
                }
                KeyCode::Tab if !app.questions.is_empty() => {
                    app.selected_question = (app.selected_question + 1) % app.questions.len();
                }
                KeyCode::BackTab if !app.questions.is_empty() => {
                    app.selected_question = app
                        .selected_question
                        .checked_sub(1)
                        .unwrap_or(app.questions.len().saturating_sub(1));
                }
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
                    if app.questions.is_empty() =>
                {
                    match app.review_focus {
                        ImprovementReviewFocus::Recommendation => {
                            let _ = app.recommendation_scroll.apply_key_in_viewport(
                                key,
                                review_viewports.left,
                                app.recommendation_content_rows(review_viewports.left.width),
                            );
                        }
                        ImprovementReviewFocus::Proposal => {
                            let _ = app.proposal_scroll.apply_key_in_viewport(
                                key,
                                review_viewports.right,
                                app.proposal_content_rows(review_viewports.right.width),
                            );
                        }
                    }
                }
                KeyCode::Enter
                    if !app.questions.is_empty()
                        && key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if let Some(answers) = app.collected_answers() {
                        return Ok(ImprovementReviewExit::FollowUp {
                            answers,
                            question_round: app.question_round + 1,
                        });
                    }
                    app.error = Some(
                        "Every follow-up question needs an answer before the agent can continue."
                            .to_string(),
                    );
                }
                KeyCode::Enter
                    if app.questions.is_empty()
                        && !key.modifiers.contains(KeyModifiers::SHIFT)
                        && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if app.primary_action() == ImprovementPrimaryAction::StartQuestions {
                        app.begin_questions();
                    } else {
                        return app.activate_primary_action();
                    }
                }
                KeyCode::Enter
                    if !app.questions.is_empty()
                        && !key.modifiers.contains(KeyModifiers::SHIFT)
                        && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if app.record_or_submit_question_answers()? {
                        return Ok(ImprovementReviewExit::FollowUp {
                            answers: app.collected_answers().unwrap_or_default(),
                            question_round: app.question_round + 1,
                        });
                    }
                }
                KeyCode::Char('a')
                    if app.questions.is_empty()
                        && app.output.route() == ImprovementRoute::ReadyForUpdate =>
                {
                    return Ok(ImprovementReviewExit::Accepted {
                        decision: "accepted_update".to_string(),
                        apply_requested: true,
                    });
                }
                KeyCode::Char('a')
                    if app.questions.is_empty()
                        && app.output.route() == ImprovementRoute::NeedsQuestions =>
                {
                    app.begin_questions();
                }
                KeyCode::Char('a') if app.questions.is_empty() => {
                    return Ok(ImprovementReviewExit::Accepted {
                        decision: format!("accepted_{}", app.output.route().as_str()),
                        apply_requested: false,
                    });
                }
                KeyCode::Char('r') | KeyCode::Char('s') if app.questions.is_empty() => {
                    return Ok(ImprovementReviewExit::Accepted {
                        decision: if key.code == KeyCode::Char('s') {
                            format!("skipped_{}", app.output.route().as_str())
                        } else {
                            format!("rejected_{}", app.output.route().as_str())
                        },
                        apply_requested: false,
                    });
                }
                _ if !app.questions.is_empty() => {
                    let input_width = 60;
                    if let Some(selected) = app.questions.get_mut(app.selected_question) {
                        let _ = selected.answer.handle_key_with_width(key, input_width);
                    }
                }
                _ => {}
            },
            Event::Mouse(mouse) => {
                if app.questions.is_empty() {
                    match app.review_focus {
                        ImprovementReviewFocus::Recommendation => {
                            let _ = app.recommendation_scroll.apply_mouse_in_viewport(
                                mouse,
                                review_viewports.left,
                                app.recommendation_content_rows(review_viewports.left.width),
                            );
                        }
                        ImprovementReviewFocus::Proposal => {
                            let _ = app.proposal_scroll.apply_mouse_in_viewport(
                                mouse,
                                review_viewports.right,
                                app.proposal_content_rows(review_viewports.right.width),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ReviewViewports {
    left: Rect,
    right: Rect,
}

/// Renders the improvement review dashboard and returns viewport rects for scroll handling.
fn render_improvement_review(frame: &mut Frame<'_>, app: &ImprovementReviewApp) -> ReviewViewports {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(0),
            Constraint::Length(7),
        ])
        .split(frame.area());
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(outer[1]);
    let route = app.output.route();
    let (route_label, tone) = route.badge();
    let header = paragraph(
        Text::from(vec![
            Line::from(vec![
                badge(route_label, tone),
                Span::raw(" "),
                Span::styled(
                    format!(
                        "Issue {}/{}: {} {}",
                        app.issue_position, app.issue_total, app.issue.identifier, app.issue.title
                    ),
                    emphasis_style(),
                ),
            ]),
            Line::from(app.output.summary.clone()),
            key_hints(&review_key_hints(route, !app.questions.is_empty())),
        ]),
        panel_title(format!("{} backlog improve", branding::COMMAND_NAME), false),
    );
    frame.render_widget(header, outer[0]);

    let left_focused =
        app.questions.is_empty() && app.review_focus == ImprovementReviewFocus::Recommendation;
    let left = scrollable_content_paragraph(
        render_review_overview(app),
        panel_title("Findings & Recommendation", left_focused),
        &app.recommendation_scroll,
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(left, body[0]);

    let right_focused =
        app.questions.is_empty() && app.review_focus == ImprovementReviewFocus::Proposal;
    let right = if app.questions.is_empty() {
        scrollable_content_paragraph(
            render_review_comparison(app),
            panel_title("Before / After", right_focused),
            &app.proposal_scroll,
        )
        .wrap(Wrap { trim: false })
    } else {
        paragraph(
            render_questions_panel(app),
            panel_title("Follow-up Questions", true),
        )
        .wrap(Wrap { trim: false })
    };
    frame.render_widget(right, body[1]);

    let footer = paragraph(render_decision_panel(app), panel_title("Decision", false))
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, outer[2]);

    ReviewViewports {
        left: body[0],
        right: body[1],
    }
}

fn review_key_hints(
    route: ImprovementRoute,
    answering_questions: bool,
) -> Vec<(&'static str, &'static str)> {
    if answering_questions {
        vec![
            ("Type", "answer"),
            ("Enter", "save/next"),
            ("Tab", "next question"),
            ("s", "skip issue"),
            ("r", "reject"),
        ]
    } else {
        match route {
            ImprovementRoute::ReadyForUpdate => {
                vec![
                    ("Tab", "focus"),
                    ("Enter", "apply"),
                    ("s", "skip"),
                    ("r", "reject"),
                    ("q", "exit"),
                ]
            }
            ImprovementRoute::NeedsQuestions => {
                vec![
                    ("Tab", "focus"),
                    ("Enter", "answer questions"),
                    ("s", "skip"),
                    ("r", "reject"),
                    ("q", "exit"),
                ]
            }
            _ => vec![
                ("Tab", "focus"),
                ("Enter", "accept"),
                ("s", "skip"),
                ("r", "reject"),
                ("q", "exit"),
            ],
        }
    }
}

fn render_decision_panel(app: &ImprovementReviewApp) -> Text<'static> {
    let primary = primary_action_copy(app);
    let status = if let Some(error) = app.error.as_deref() {
        error.to_string()
    } else if app.questions.is_empty() {
        format!("Enter will {}.", primary.verb)
    } else {
        format!(
            "Question {}/{}. Enter saves the current answer and advances.",
            app.selected_question + 1,
            app.questions.len()
        )
    };

    Text::from(vec![
        Line::from(vec![
            badge("enter", Tone::Accent),
            Span::raw(" "),
            Span::styled(primary.title.to_string(), emphasis_style()),
            Span::raw("  "),
            badge("s", Tone::Muted),
            Span::raw(" Skip  "),
            badge("r", Tone::Danger),
            Span::raw(" Reject"),
        ]),
        Line::from(primary.detail.to_string()),
        Line::from(""),
        Line::from(Span::styled(status, muted_style())),
    ])
}

struct ImprovementPrimaryActionCopy {
    title: &'static str,
    verb: &'static str,
    detail: &'static str,
}

fn primary_action_copy(app: &ImprovementReviewApp) -> ImprovementPrimaryActionCopy {
    match app.primary_action() {
        ImprovementPrimaryAction::ApplyUpdate => ImprovementPrimaryActionCopy {
            title: "Apply Update",
            verb: "apply the proposed update",
            detail: "Saves the approved local backlog description first, then updates the Linear issue metadata and description when needed.",
        },
        ImprovementPrimaryAction::KeepUnchanged => ImprovementPrimaryActionCopy {
            title: "Keep Unchanged",
            verb: "accept the keep-unchanged recommendation",
            detail: "Confirms that no ticket update is needed and advances without changing local backlog files or Linear.",
        },
        ImprovementPrimaryAction::AcceptPlanning => ImprovementPrimaryActionCopy {
            title: "Needs Planning",
            verb: "accept the planning/context recommendation",
            detail: "Confirms that more planning or repo context is needed before proposing any issue rewrite.",
        },
        ImprovementPrimaryAction::StartQuestions => ImprovementPrimaryActionCopy {
            title: "Answer Questions",
            verb: "start the follow-up question step",
            detail: "Opens the agent's blocking questions inside the dashboard so you can answer them before another recommendation pass.",
        },
        ImprovementPrimaryAction::ContinueQuestions => ImprovementPrimaryActionCopy {
            title: "Continue Review",
            verb: "continue with the answered questions",
            detail: "Uses your answers to rerun the same issue review and produce the next recommendation without leaving the dashboard.",
        },
    }
}

fn render_review_overview(app: &ImprovementReviewApp) -> Text<'static> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Recommendation ", label_style()),
            Span::raw(
                app.output
                    .recommendation
                    .clone()
                    .unwrap_or_else(|| app.output.summary.clone()),
            ),
        ]),
        Line::from(""),
    ];

    for (title, values) in [
        ("Title gaps", &app.output.findings.title_gaps),
        ("Description gaps", &app.output.findings.description_gaps),
        (
            "Acceptance criteria gaps",
            &app.output.findings.acceptance_criteria_gaps,
        ),
        ("Metadata gaps", &app.output.findings.metadata_gaps),
        (
            "Structure opportunities",
            &app.output.findings.structure_opportunities,
        ),
        ("Context still needed", &app.output.context_requirements),
        ("Follow-up questions", &app.output.follow_up_questions),
    ] {
        lines.push(Line::from(Span::styled(title, label_style())));
        if values.is_empty() {
            lines.push(Line::from(Span::styled("- None", muted_style())));
        } else {
            lines.extend(values.iter().map(|value| Line::from(format!("- {value}"))));
        }
        lines.push(Line::from(""));
    }

    Text::from(lines)
}

fn render_review_comparison(app: &ImprovementReviewApp) -> Text<'static> {
    let original_title = app.issue.title.clone();
    let proposed_title = app
        .output
        .proposal
        .title
        .clone()
        .unwrap_or_else(|| original_title.clone());
    let original_description = app
        .issue
        .description
        .clone()
        .unwrap_or_else(|| "_No description provided._".to_string());
    let proposed_description = app
        .output
        .proposal
        .description
        .clone()
        .unwrap_or_else(|| original_description.clone());
    let original_priority = app
        .issue
        .priority
        .map(|v| v.to_string())
        .unwrap_or_else(|| "None".to_string());
    let proposed_priority = app
        .output
        .proposal
        .priority
        .map(|v| v.to_string())
        .unwrap_or_else(|| original_priority.clone());
    let original_estimate = app
        .issue
        .estimate
        .map(|v| v.to_string())
        .unwrap_or_else(|| "None".to_string());
    let proposed_estimate = app
        .output
        .proposal
        .estimate
        .map(|v| v.to_string())
        .unwrap_or_else(|| original_estimate.clone());
    let original_labels = render_labels(&app.issue);
    let proposed_labels = app
        .output
        .proposal
        .labels
        .as_ref()
        .map(|l| l.join(", "))
        .unwrap_or_else(|| original_labels.clone());

    let changed = tone_style(Tone::Success);
    let muted = muted_style();

    let mut lines = vec![
        Line::from(vec![
            badge("original", Tone::Muted),
            Span::raw(" "),
            Span::styled("Current issue content", label_style()),
        ]),
        Line::from(vec![
            Span::styled("Title ", label_style()),
            Span::styled(original_title.clone(), muted),
        ]),
        Line::from(vec![
            Span::styled("Priority ", label_style()),
            Span::styled(original_priority.clone(), muted),
            Span::styled("  Estimate ", label_style()),
            Span::styled(original_estimate.clone(), muted),
        ]),
        Line::from(vec![
            Span::styled("Labels ", label_style()),
            Span::styled(original_labels.clone(), muted),
        ]),
        Line::from(""),
    ];
    lines.extend(render_markdown(&original_description, muted, &[]).lines);
    lines.push(Line::from(""));
    lines.push(Line::from(""));

    lines.push(Line::from(vec![
        badge("proposed", Tone::Accent),
        Span::raw(" "),
        Span::styled("Proposed changes", label_style()),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Title ", label_style()),
        Span::styled(
            proposed_title.clone(),
            if proposed_title != original_title {
                changed
            } else {
                Style::default()
            },
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Priority ", label_style()),
        Span::styled(
            proposed_priority.clone(),
            if proposed_priority != original_priority {
                changed
            } else {
                Style::default()
            },
        ),
        Span::styled("  Estimate ", label_style()),
        Span::styled(
            proposed_estimate.clone(),
            if proposed_estimate != original_estimate {
                changed
            } else {
                Style::default()
            },
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Labels ", label_style()),
        Span::styled(
            proposed_labels.clone(),
            if proposed_labels != original_labels {
                changed
            } else {
                Style::default()
            },
        ),
    ]));
    lines.push(Line::from(""));
    let desc_changed = proposed_description != original_description;
    let desc_base_style = if desc_changed {
        changed
    } else {
        Style::default()
    };
    lines.extend(render_markdown(&proposed_description, desc_base_style, &[]).lines);

    if !app.output.proposal.acceptance_criteria.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Acceptance Criteria",
            label_style(),
        )));
        for criterion in &app.output.proposal.acceptance_criteria {
            lines.push(Line::from(Span::styled(format!("- {criterion}"), changed)));
        }
    }

    Text::from(lines)
}

fn render_questions_panel(app: &ImprovementReviewApp) -> Text<'static> {
    let mut lines = Vec::new();
    for (index, item) in app.questions.iter().enumerate() {
        let marker = if index == app.selected_question {
            ">"
        } else {
            " "
        };
        let answer = item.answer.display_value();
        lines.push(Line::from(format!(
            "{marker} Q{}: {}",
            index + 1,
            item.question
        )));
        if answer.trim().is_empty() {
            lines.push(Line::from(Span::styled(
                "   _No answer yet_",
                muted_style(),
            )));
        } else {
            lines.push(Line::from(format!("   {}", answer.replace('\n', "\n   "))));
        }
        lines.push(Line::from(""));
    }
    Text::from(lines)
}

fn render_follow_up_answers_markdown(answers: &[(String, String)]) -> String {
    answers
        .iter()
        .enumerate()
        .flat_map(|(index, (question, answer))| {
            [
                format!("## Question {}", index + 1),
                String::new(),
                format!("Q: {question}"),
                String::new(),
                "Answer:".to_string(),
                answer.clone(),
                String::new(),
            ]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_follow_up_prompt(
    issue: &IssueSummary,
    output: &ImprovementOutput,
    answers: &[(String, String)],
) -> String {
    let answers_block = answers
        .iter()
        .enumerate()
        .map(|(index, (question, answer))| format!("{}. Q: {}\nA: {}", index + 1, question, answer))
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "Continue the backlog improvement review for `{}`.\n\nPrevious route: `{}`\nSummary: {}\nRecommendation: {}\n\nFollow-up answers:\n{}\n\nReassess the issue using the same JSON schema as before. If the answers are sufficient, prefer `ready_for_update` with a concrete proposal. Only return `needs_questions` again when a remaining blocker is material.",
        issue.identifier,
        output.route().as_str(),
        output.summary,
        output
            .recommendation
            .as_deref()
            .unwrap_or("No extra recommendation text provided."),
        answers_block
    )
}

struct ImprovementDashboardCleanup;

impl Drop for ImprovementDashboardCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
}

struct ImprovementReviewCleanup;

impl Drop for ImprovementReviewCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            Show,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
}

/// Collects optional free-form instructions before the improvement analysis begins.
///
/// Renders a side-by-side layout with a multiline editor on the left and a ticket preview
/// on the right so the user can draft improvement instructions while reading issue context.
/// Returns `None` when the user presses Enter with an empty field or Esc to skip.
fn run_instruction_prompt(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    issues: &[IssueSummary],
) -> Result<InstructionPromptExit> {
    let mut app = InstructionPromptApp::new(issues.to_vec());
    let mut preview_viewport = Rect::default();

    loop {
        terminal.draw(|frame| {
            preview_viewport = render_instruction_prompt(frame, &app);
        })?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Enter
                    if !key.modifiers.contains(KeyModifiers::SHIFT)
                        && app.focus == InstructionPromptFocus::Editor =>
                {
                    let text = app.input.display_value().trim().to_string();
                    return Ok(InstructionPromptExit::Continue(if text.is_empty() {
                        None
                    } else {
                        Some(text)
                    }));
                }
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let text = app.input.display_value().trim().to_string();
                    return Ok(InstructionPromptExit::Continue(if text.is_empty() {
                        None
                    } else {
                        Some(text)
                    }));
                }
                KeyCode::Esc => return Ok(InstructionPromptExit::Cancelled),
                KeyCode::Char('q') if app.focus == InstructionPromptFocus::Preview => {
                    return Ok(InstructionPromptExit::Cancelled);
                }
                KeyCode::Tab => {
                    app.focus = match app.focus {
                        InstructionPromptFocus::Editor => InstructionPromptFocus::Preview,
                        InstructionPromptFocus::Preview => InstructionPromptFocus::Editor,
                    };
                }
                KeyCode::Left
                    if app.focus == InstructionPromptFocus::Preview && app.issues.len() > 1 =>
                {
                    app.prev_issue();
                }
                KeyCode::Right
                    if app.focus == InstructionPromptFocus::Preview && app.issues.len() > 1 =>
                {
                    app.next_issue();
                }
                KeyCode::Up if app.focus == InstructionPromptFocus::Preview => {
                    let _ = app.preview_scroll.apply_key_code_in_viewport(
                        KeyCode::Up,
                        preview_viewport,
                        app.preview_content_rows(preview_viewport.width.max(1)),
                    );
                }
                KeyCode::Down if app.focus == InstructionPromptFocus::Preview => {
                    let _ = app.preview_scroll.apply_key_code_in_viewport(
                        KeyCode::Down,
                        preview_viewport,
                        app.preview_content_rows(preview_viewport.width.max(1)),
                    );
                }
                KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
                    if app.focus == InstructionPromptFocus::Preview =>
                {
                    let _ = app.preview_scroll.apply_key_in_viewport(
                        key,
                        preview_viewport,
                        app.preview_content_rows(preview_viewport.width.max(1)),
                    );
                }
                _ if app.focus == InstructionPromptFocus::Editor => {
                    app.input.handle_key(key);
                }
                _ => {}
            },
            Event::Paste(text) if app.focus == InstructionPromptFocus::Editor => {
                app.input.paste(&text);
            }
            Event::Mouse(mouse) => {
                let _ = app.preview_scroll.apply_mouse_in_viewport(
                    mouse,
                    preview_viewport,
                    app.preview_content_rows(preview_viewport.width.max(1)),
                );
            }
            _ => {}
        }
    }
}

/// Renders the instruction prompt as a side-by-side layout and returns the preview pane viewport.
fn render_instruction_prompt(frame: &mut Frame<'_>, app: &InstructionPromptApp) -> Rect {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(0)])
        .split(frame.area());

    let issue_count_label = if app.issues.len() == 1 {
        format!("1 issue selected ({})", app.issues[0].identifier)
    } else {
        format!("{} issues selected", app.issues.len())
    };

    let mut hints = vec![
        ("Tab", "focus"),
        ("Enter", "continue"),
        ("Shift+Enter", "newline"),
        ("Ctrl+S", "submit"),
        ("Esc", "cancel"),
    ];
    if app.focus == InstructionPromptFocus::Preview {
        hints.push(("q", "cancel"));
        if app.issues.len() > 1 {
            hints.insert(1, ("\u{2190}/\u{2192}", "prev/next issue"));
        }
    }

    let header = paragraph(
        Text::from(vec![
            Line::from(vec![
                badge("improve", Tone::Accent),
                Span::raw(" "),
                Span::styled("Improvement Instructions (optional)", emphasis_style()),
            ]),
            Line::from(format!(
                "{issue_count_label}. Add free-form guidance for the analysis or leave empty for default behavior.",
            )),
            key_hints(&hints),
        ]),
        panel_title(format!("{} backlog improve", branding::COMMAND_NAME), false),
    );
    frame.render_widget(header, outer[0]);

    let body = if outer[1].width >= 60 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(outer[1])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(outer[1])
    };

    let editor_focused = app.focus == InstructionPromptFocus::Editor;
    let input_block = panel(panel_title("Instructions", editor_focused));
    let input_inner = input_block.inner(body[0]);
    let rendered = app.input.render_with_viewport(
        "Type optional improvement instructions for the agent...",
        editor_focused,
        input_inner.width,
        input_inner.height,
    );
    let widget = rendered.paragraph(input_block);
    frame.render_widget(widget, body[0]);
    if editor_focused {
        rendered.set_cursor(frame, input_inner);
    }

    let preview_area = body[1];
    render_instruction_issue_preview(frame, preview_area, app);
    preview_area
}

#[cfg(test)]
fn render_instruction_prompt_snapshot(
    issues: Vec<IssueSummary>,
    width: u16,
    height: u16,
) -> Result<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    let app = InstructionPromptApp::new(issues);
    terminal.draw(|frame| {
        render_instruction_prompt(frame, &app);
    })?;
    Ok(improvement_dashboard_snapshot(terminal.backend()))
}

impl InstructionPromptApp {
    fn new(issues: Vec<IssueSummary>) -> Self {
        Self {
            input: InputFieldState::multiline(String::new()),
            issues,
            issue_cursor: 0,
            focus: InstructionPromptFocus::Editor,
            preview_scroll: ScrollState::default(),
        }
    }

    fn next_issue(&mut self) {
        if !self.issues.is_empty() {
            self.issue_cursor = (self.issue_cursor + 1) % self.issues.len();
            self.preview_scroll.reset();
        }
    }

    fn prev_issue(&mut self) {
        if !self.issues.is_empty() {
            self.issue_cursor = if self.issue_cursor == 0 {
                self.issues.len() - 1
            } else {
                self.issue_cursor - 1
            };
            self.preview_scroll.reset();
        }
    }

    fn preview_content_rows(&self, width: u16) -> usize {
        let preview = self
            .issues
            .get(self.issue_cursor)
            .map(|issue| render_issue_preview(issue, None, None, "_No description provided._"))
            .unwrap_or_else(|| {
                empty_state(
                    "No issues selected.",
                    "Select issues from the dashboard first.",
                )
            });
        wrapped_rows(&plain_text(&preview), width.max(1))
    }
}

/// Renders the issue preview pane for the instruction prompt.
fn render_instruction_issue_preview(frame: &mut Frame<'_>, area: Rect, app: &InstructionPromptApp) {
    let preview_focused = app.focus == InstructionPromptFocus::Preview;
    let title = if app.issues.len() > 1 {
        format!(
            "Ticket Preview ({}/{})",
            app.issue_cursor + 1,
            app.issues.len()
        )
    } else {
        "Ticket Preview".to_string()
    };
    let preview = app
        .issues
        .get(app.issue_cursor)
        .map(|issue| render_issue_preview(issue, None, None, "_No description provided._"))
        .unwrap_or_else(|| {
            empty_state(
                "No issues selected.",
                "Select issues from the dashboard first.",
            )
        });
    let widget = scrollable_content_paragraph(
        preview,
        panel_title(title, preview_focused),
        &app.preview_scroll,
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(widget, area);
}

impl ImprovementDashboardApp {
    fn new(issues: Vec<IssueSummary>) -> Self {
        let issue_count = issues.len();
        Self {
            query: InputFieldState::new(String::new()),
            issues,
            cursor: 0,
            selected: vec![false; issue_count],
            focus: ImprovementPickerFocus::List,
            preview_scroll: ScrollState::default(),
            completed: None,
        }
    }

    fn move_up(&mut self) {
        let filtered = dashboard_search_results(self);
        if filtered.is_empty() {
            self.cursor = 0;
        } else if self.cursor == 0 {
            self.cursor = filtered.len().saturating_sub(1);
        } else {
            self.cursor -= 1;
        }
        self.preview_scroll.reset();
    }

    fn move_down(&mut self) {
        let filtered = dashboard_search_results(self);
        if filtered.is_empty() {
            self.cursor = 0;
        } else {
            self.cursor = (self.cursor + 1) % filtered.len();
        }
        self.preview_scroll.reset();
    }

    fn toggle(&mut self) {
        let filtered = dashboard_search_results(self);
        if let Some(result) = filtered.get(self.cursor) {
            if let Some(selected) = self.selected.get_mut(result.issue_index) {
                *selected = !*selected;
            }
        }
    }

    fn select(&mut self) -> ImprovementIssueSelection {
        let selection = ImprovementIssueSelection {
            issues: self.selected_issues(),
        };
        self.completed = Some(selection.clone());
        selection
    }

    fn any_selected(&self) -> bool {
        self.selected.iter().any(|selected| *selected)
    }

    fn selected_issues(&self) -> Vec<IssueSummary> {
        if self.any_selected() {
            self.issues
                .iter()
                .zip(self.selected.iter())
                .filter_map(|(issue, selected)| (*selected).then_some(issue.clone()))
                .collect()
        } else {
            self.issues.clone()
        }
    }

    fn preview_content_rows(&self, width: u16) -> usize {
        let filtered = dashboard_search_results(self);
        let preview = filtered
            .get(self.cursor)
            .and_then(|result| {
                self.issues.get(result.issue_index).map(|issue| {
                    render_issue_preview(issue, Some(result), None, "_No description provided._")
                })
            })
            .unwrap_or_else(|| {
                empty_state(
                    "No issue is available to preview.",
                    "Type to search or clear the query to see all issues.",
                )
            });
        wrapped_rows(&plain_text(&preview), width.max(1))
    }

    fn summary_line(&self) -> String {
        let explicit = self.selected.iter().filter(|selected| **selected).count();
        if explicit == 0 {
            format!(
                "No explicit subset selected. Enter will review all {} listed issues.",
                self.issues.len()
            )
        } else {
            format!("{explicit} issue(s) selected explicitly. Enter will review only that subset.")
        }
    }

    fn state_label(&self) -> &str {
        self.issues
            .first()
            .and_then(|issue| issue.state.as_ref().map(|state| state.name.as_str()))
            .unwrap_or("Unknown")
    }

    #[cfg(test)]
    fn set_search_query(&mut self, query: &str) {
        self.query = InputFieldState::new(query.to_string());
        self.cursor = 0;
        self.preview_scroll.reset();
    }
}

fn dashboard_search_results(app: &ImprovementDashboardApp) -> Vec<IssueSearchResult> {
    search_issues(&app.issues, &app.query.display_value())
}

impl ImprovementReviewApp {
    fn new(
        issue_position: usize,
        issue_total: usize,
        issue: IssueSummary,
        mut output: ImprovementOutput,
        question_round: usize,
    ) -> Self {
        if output.route() == ImprovementRoute::NeedsQuestions
            && output.follow_up_questions.len() > 5
        {
            output.follow_up_questions.truncate(5);
        }
        Self {
            issue_position,
            issue_total,
            issue,
            output,
            questions: Vec::new(),
            selected_question: 0,
            question_round,
            review_focus: ImprovementReviewFocus::Proposal,
            recommendation_scroll: ScrollState::default(),
            proposal_scroll: ScrollState::default(),
            error: None,
        }
    }

    fn recommendation_content_rows(&self, width: u16) -> usize {
        let text = render_review_overview(self);
        wrapped_rows(&plain_text(&text), width.max(1))
    }

    fn proposal_content_rows(&self, width: u16) -> usize {
        let text = render_review_comparison(self);
        wrapped_rows(&plain_text(&text), width.max(1))
    }

    fn collected_answers(&self) -> Option<Vec<(String, String)>> {
        let answers = self
            .questions
            .iter()
            .map(|item| {
                (
                    item.question.clone(),
                    item.answer.display_value().trim().to_string(),
                )
            })
            .collect::<Vec<_>>();
        if answers.iter().any(|(_, answer)| answer.is_empty()) {
            None
        } else {
            Some(answers)
        }
    }

    fn primary_action(&self) -> ImprovementPrimaryAction {
        if !self.questions.is_empty() {
            ImprovementPrimaryAction::ContinueQuestions
        } else {
            match self.output.route() {
                ImprovementRoute::ReadyForUpdate => ImprovementPrimaryAction::ApplyUpdate,
                ImprovementRoute::NoUpdateNeeded => ImprovementPrimaryAction::KeepUnchanged,
                ImprovementRoute::NeedsPlanning => ImprovementPrimaryAction::AcceptPlanning,
                ImprovementRoute::NeedsQuestions => ImprovementPrimaryAction::StartQuestions,
            }
        }
    }

    fn activate_primary_action(&self) -> Result<ImprovementReviewExit> {
        Ok(match self.primary_action() {
            ImprovementPrimaryAction::ApplyUpdate => ImprovementReviewExit::Accepted {
                decision: "accepted_update".to_string(),
                apply_requested: true,
            },
            ImprovementPrimaryAction::KeepUnchanged => ImprovementReviewExit::Accepted {
                decision: "accepted_no_update_needed".to_string(),
                apply_requested: false,
            },
            ImprovementPrimaryAction::AcceptPlanning => ImprovementReviewExit::Accepted {
                decision: "accepted_needs_planning".to_string(),
                apply_requested: false,
            },
            ImprovementPrimaryAction::StartQuestions => {
                bail!("primary follow-up action must be handled by the dashboard state machine")
            }
            ImprovementPrimaryAction::ContinueQuestions => {
                bail!("question continuation must be handled by the dashboard state machine")
            }
        })
    }

    fn record_or_submit_question_answers(&mut self) -> Result<bool> {
        let Some(selected) = self.questions.get(self.selected_question) else {
            return Ok(false);
        };
        if selected.answer.display_value().trim().is_empty() {
            self.error = Some("Answer the active question before continuing.".to_string());
            return Ok(false);
        }
        self.error = None;
        if self.selected_question + 1 < self.questions.len() {
            self.selected_question += 1;
            Ok(false)
        } else if self.collected_answers().is_some() {
            Ok(true)
        } else {
            self.error = Some(
                "Every follow-up question needs an answer before the agent can continue."
                    .to_string(),
            );
            Ok(false)
        }
    }

    fn begin_questions(&mut self) {
        if self.questions.is_empty() {
            self.questions = self
                .output
                .follow_up_questions
                .iter()
                .map(|question| ImprovementQuestionAnswer {
                    question: question.clone(),
                    answer: InputFieldState::multiline(String::new()),
                })
                .collect();
            self.selected_question = 0;
            self.error = None;
        }
    }
}

#[cfg(test)]
fn improvement_dashboard_snapshot(backend: &TestBackend) -> String {
    let buffer = backend.buffer();
    let mut lines = Vec::new();

    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        lines.push(line.trim_end().to_string());
    }

    lines.join("\n")
}

#[cfg(test)]
fn review_dashboard_snapshot(app: &ImprovementReviewApp, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            render_improvement_review(frame, app);
        })
        .expect("review should render");
    improvement_dashboard_snapshot(terminal.backend())
}

async fn load_target_issues(
    command_context: &LinearCommandContext,
    args: &BacklogImproveArgs,
) -> Result<Vec<IssueSummary>> {
    if !args.issues.is_empty() {
        let mut issues = Vec::with_capacity(args.issues.len());
        for identifier in &args.issues {
            let issue = command_context.service.load_issue(identifier).await?;
            validate_issue_scope(
                &issue,
                command_context.default_team.as_deref(),
                command_context.default_project_id.as_deref(),
            )?;
            issues.push(issue);
        }
        return Ok(issues);
    }

    command_context
        .service
        .list_issues(IssueListFilters {
            team: command_context.default_team.clone(),
            project_id: command_context.default_project_id.clone(),
            state: Some(args.state.clone()),
            limit: args.limit.max(1),
            ..IssueListFilters::default()
        })
        .await
}

async fn improve_issue(
    root: &Path,
    service: &LinearService<ReqwestLinearClient>,
    issue: &IssueSummary,
    related_backlog_issues: &[IssueSummary],
    args: &BacklogImproveArgs,
    progress: Option<&UnboundedSender<ImprovementProgressUpdate>>,
) -> Result<ImprovementReport> {
    let issue_run = analyze_issue(
        root,
        issue,
        related_backlog_issues,
        args,
        None,
        None,
        progress,
    )?;
    let apply = if args.apply && issue_run.output.route() == ImprovementRoute::ReadyForUpdate {
        if let Some(progress) = progress {
            send_improvement_progress(
                progress,
                format!("Applying updates for {}", issue.identifier),
                "Persisting the artifact trail and pushing the approved metadata changes back to Linear."
                    .to_string(),
            );
        }
        apply_improvement(root, service, &issue_run).await?
    } else {
        ImprovementApplyRecord {
            requested: args.apply,
            local_updated: false,
            remote_updated: false,
            local_before_path: None,
            local_after_path: None,
            remote_before_path: None,
            remote_after_path: None,
            error: None,
            error_kind: None,
        }
    };

    let status_label = if !issue_run.output.needs_improvement {
        format!("{} no changes needed", render_mode(args.mode))
    } else if args.apply && issue_run.output.route() == ImprovementRoute::ReadyForUpdate {
        format!("{} applied", render_mode(args.mode))
    } else {
        format!("{} proposal only", render_mode(args.mode))
    };

    finalize_issue_run(
        root,
        &issue_run,
        apply,
        if args.apply && issue_run.output.route() == ImprovementRoute::ReadyForUpdate {
            "applied_update".to_string()
        } else {
            format!("reviewed_{}", issue_run.output.route().as_str())
        },
        status_label,
    )
}

fn analyze_issue(
    root: &Path,
    issue: &IssueSummary,
    related_backlog_issues: &[IssueSummary],
    args: &BacklogImproveArgs,
    continuation: Option<&mut Option<AgentContinuation>>,
    instructions: Option<&str>,
    progress: Option<&UnboundedSender<ImprovementProgressUpdate>>,
) -> Result<ImprovementIssueRun> {
    let started_at = now_rfc3339()?;
    let run_id = improvement_run_id()?;
    let paths = PlanningPaths::new(root);
    let issue_dir = paths.backlog_issue_dir(&issue.identifier);
    ensure_dir(&issue_dir)?;
    save_issue_metadata(&issue_dir, &build_issue_metadata(issue))?;

    let run_dir = issue_dir
        .join("artifacts")
        .join("improvement")
        .join(&run_id);
    ensure_dir(&run_dir)?;

    let original_description = issue.description.clone().unwrap_or_default();
    let original_snapshot_path = run_dir.join(ORIGINAL_SNAPSHOT_FILE);
    write_text_file(&original_snapshot_path, &original_description, true)?;

    let issue_snapshot_path = run_dir.join(ISSUE_SNAPSHOT_FILE);
    write_text_file(
        &issue_snapshot_path,
        &serde_json::to_string_pretty(issue).context("failed to encode issue snapshot")?,
        true,
    )?;

    let local_index_path = issue_dir.join(INDEX_FILE_NAME);
    let local_index_before = match fs::read_to_string(&local_index_path) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read `{}`", local_index_path.display()));
        }
    };
    let local_index_snapshot_path = if let Some(contents) = local_index_before.as_deref() {
        let path = run_dir.join(LOCAL_INDEX_SNAPSHOT_FILE);
        write_text_file(&path, contents, true)?;
        Some(path)
    } else {
        None
    };

    let prompt = render_improvement_prompt(
        root,
        issue,
        local_index_before.as_deref(),
        related_backlog_issues,
        args.mode,
    )?;
    if let Some(progress) = progress {
        send_improvement_progress(
            progress,
            format!("Analyzing {}", issue.identifier),
            "The configured agent is reviewing the issue body, backlog packet, and related tickets."
                .to_string(),
        );
    }
    let agent_args = RunAgentArgs {
        root: Some(root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_BACKLOG_IMPROVE.to_string()),
        agent: args.agent.clone(),
        prompt,
        instructions: instructions.map(str::to_string),
        model: args.model.clone(),
        reasoning: args.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    };
    let output = if let Some(continuation) = continuation {
        run_agent_capture_with_continuation(&agent_args, continuation)
    } else {
        run_agent_capture(&agent_args)
    }
    .with_context(|| {
        format!("{} backlog improve requires a configured local agent to review repo-scoped backlog issues", branding::COMMAND_NAME)
    })?;
    let parsed: ImprovementOutput =
        parse_agent_json(&output.stdout, "backlog improvement proposal")?;
    let normalized = normalize_improvement_output(issue, parsed)?;

    let proposal_json_path = run_dir.join(PROPOSAL_JSON_FILE);
    write_text_file(
        &proposal_json_path,
        &serde_json::to_string_pretty(&normalized)
            .context("failed to encode backlog improvement proposal")?,
        true,
    )?;
    let proposal_markdown_path = run_dir.join(PROPOSAL_MARKDOWN_FILE);
    write_text_file(
        &proposal_markdown_path,
        &render_proposal_markdown(args.mode, &normalized),
        true,
    )?;

    Ok(ImprovementIssueRun {
        issue: issue.clone(),
        mode: args.mode,
        run_id,
        run_dir,
        original_description,
        local_index_before,
        local_index_snapshot_path,
        original_snapshot_path,
        issue_snapshot_path,
        proposal_json_path,
        proposal_markdown_path,
        started_at,
        output: normalized,
    })
}

/// Inspects the error chain from a failed Linear API call and classifies it as a
/// permission/auth error, a network error, or a generic error.
fn classify_linear_error(error: &anyhow::Error) -> ImprovementErrorKind {
    let message = format!("{error:#}").to_ascii_lowercase();
    let permission_signals = [
        "permission",
        "unauthorized",
        "forbidden",
        "authentication required",
        "not authorized",
        "access denied",
        "status 401",
        "status 403",
    ];
    if permission_signals
        .iter()
        .any(|signal| message.contains(signal))
    {
        return ImprovementErrorKind::LinearPermission;
    }
    let network_signals = [
        "failed to reach",
        "connection refused",
        "dns error",
        "timed out",
        "connection reset",
    ];
    if network_signals
        .iter()
        .any(|signal| message.contains(signal))
    {
        return ImprovementErrorKind::LinearNetwork;
    }
    ImprovementErrorKind::Other
}

async fn apply_improvement(
    root: &Path,
    service: &LinearService<ReqwestLinearClient>,
    issue_run: &ImprovementIssueRun,
) -> Result<ImprovementApplyRecord> {
    let mut apply = ImprovementApplyRecord {
        requested: true,
        local_updated: false,
        remote_updated: false,
        local_before_path: None,
        local_after_path: None,
        remote_before_path: None,
        remote_after_path: None,
        error: None,
        error_kind: None,
    };

    let local_before_path = issue_run.run_dir.join("applied-local-before.md");
    let local_after_path = issue_run.run_dir.join("applied-local-after.md");
    let remote_before_path = issue_run.run_dir.join("applied-remote-before.md");
    let remote_after_path = issue_run.run_dir.join("applied-remote-after.md");

    write_text_file(
        &local_before_path,
        issue_run.local_index_before.as_deref().unwrap_or_default(),
        true,
    )?;
    let proposed_description = issue_run
        .output
        .proposal
        .description
        .clone()
        .unwrap_or_else(|| issue_run.original_description.clone());
    write_text_file(&local_after_path, &proposed_description, true)?;
    write_text_file(&remote_before_path, &issue_run.original_description, true)?;
    write_text_file(&remote_after_path, &proposed_description, true)?;

    apply.local_before_path = Some(display_path(&local_before_path, root));
    apply.local_after_path = Some(display_path(&local_after_path, root));
    apply.remote_before_path = Some(display_path(&remote_before_path, root));
    apply.remote_after_path = Some(display_path(&remote_after_path, root));

    if let Some(description) = issue_run.output.proposal.description.as_deref() {
        write_issue_description(root, &issue_run.issue.identifier, description)?;
        apply.local_updated = true;
    }

    if proposal_has_remote_mutation(&issue_run.output.proposal) {
        let updated_issue = service
            .edit_issue(IssueEditSpec {
                identifier: issue_run.issue.identifier.clone(),
                title: issue_run.output.proposal.title.clone(),
                description: issue_run.output.proposal.description.clone(),
                project: None,
                state: None,
                priority: issue_run.output.proposal.priority,
                estimate: issue_run.output.proposal.estimate,
                labels: issue_run.output.proposal.labels.clone(),
                parent_identifier: issue_run.output.proposal.parent_issue_identifier.clone(),
            })
            .await;

        match updated_issue {
            Ok(updated_issue) => {
                let issue_dir =
                    PlanningPaths::new(root).backlog_issue_dir(&issue_run.issue.identifier);
                save_issue_metadata(&issue_dir, &build_issue_metadata(&updated_issue))?;
                apply.remote_updated = true;
            }
            Err(error) => {
                let kind = classify_linear_error(&error);
                apply.error_kind = Some(kind);
                apply.error = Some(error.to_string());
            }
        }
    }

    Ok(apply)
}

fn finalize_issue_run(
    root: &Path,
    issue_run: &ImprovementIssueRun,
    apply: ImprovementApplyRecord,
    decision: String,
    status_label: String,
) -> Result<ImprovementReport> {
    let completed_at = now_rfc3339()?;
    let summary = ImprovementRunSummary {
        run_id: issue_run.run_id.clone(),
        issue_identifier: issue_run.issue.identifier.clone(),
        issue_title: issue_run.issue.title.clone(),
        mode: render_mode(issue_run.mode).to_string(),
        route: issue_run.output.route().as_str().to_string(),
        decision,
        started_at: issue_run.started_at.clone(),
        completed_at,
        needs_improvement: issue_run.output.needs_improvement,
        original_snapshot_path: display_path(&issue_run.original_snapshot_path, root),
        issue_snapshot_path: display_path(&issue_run.issue_snapshot_path, root),
        local_index_snapshot_path: issue_run
            .local_index_snapshot_path
            .as_ref()
            .map(|path| display_path(path, root)),
        proposal_json_path: display_path(&issue_run.proposal_json_path, root),
        proposal_markdown_path: display_path(&issue_run.proposal_markdown_path, root),
        apply,
    };
    let summary_path = issue_run.run_dir.join(SUMMARY_FILE);
    write_text_file(
        &summary_path,
        &serde_json::to_string_pretty(&summary)
            .context("failed to encode backlog improvement summary")?,
        true,
    )?;

    if let Some(error) = summary.apply.error.as_deref() {
        let run_dir_display = display_path(&issue_run.run_dir, root);
        match summary.apply.error_kind.as_ref() {
            Some(ImprovementErrorKind::LinearPermission) => {
                bail!(
                    "Local proposal saved to {}. \
                     Linear update failed: permission denied — check LINEAR_API_KEY scopes. \
                     ({})",
                    run_dir_display,
                    error,
                );
            }
            _ => {
                bail!(
                    "improved `{}` but failed to finish the apply-back flow: {}",
                    issue_run.issue.identifier,
                    error
                );
            }
        }
    }

    Ok(ImprovementReport {
        issue_identifier: issue_run.issue.identifier.clone(),
        run_dir: issue_run.run_dir.clone(),
        status_label,
    })
}

fn render_improvement_reports(root: &Path, reports: &[ImprovementReport]) -> String {
    let mut lines = vec![format!("Improved {} issue(s):", reports.len())];

    for report in reports {
        lines.push(format!(
            "- {}: {} ({})",
            report.issue_identifier,
            report.status_label,
            display_path(&report.run_dir, root)
        ));
    }

    lines.join("\n")
}

fn normalize_improvement_output(
    issue: &IssueSummary,
    output: ImprovementOutput,
) -> Result<ImprovementOutput> {
    let priority = if let Some(priority) = output.proposal.priority {
        if priority > 4 {
            bail!("backlog improvement proposed invalid priority `{priority}`");
        }
        Some(priority)
    } else {
        None
    };
    let estimate = match output.proposal.estimate {
        Some(estimate) if !estimate.is_finite() || estimate.is_sign_negative() => {
            bail!("backlog improvement proposed invalid estimate `{estimate}`");
        }
        other => other,
    };
    let parent_issue_identifier = output
        .proposal
        .parent_issue_identifier
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if parent_issue_identifier
        .as_deref()
        .is_some_and(|identifier| issue.identifier.eq_ignore_ascii_case(identifier))
    {
        bail!("backlog improvement proposed the issue as its own parent");
    }

    let labels = output
        .proposal
        .labels
        .map(normalize_string_list)
        .filter(|labels| !labels.is_empty());
    let acceptance_criteria = normalize_string_list(output.proposal.acceptance_criteria);
    let title = normalize_optional_text(output.proposal.title);
    let description = normalize_optional_text(output.proposal.description);
    let context_requirements = normalize_string_list(output.context_requirements);
    let follow_up_questions = normalize_string_list(output.follow_up_questions);

    let needs_improvement = output.needs_improvement
        || title.is_some()
        || description.is_some()
        || priority.is_some()
        || estimate.is_some()
        || labels.is_some()
        || parent_issue_identifier.is_some()
        || !acceptance_criteria.is_empty()
        || findings_present(&output.findings)
        || !context_requirements.is_empty()
        || !follow_up_questions.is_empty();

    let route = match output.route {
        Some(route) => route,
        None if !follow_up_questions.is_empty() => ImprovementRoute::NeedsQuestions,
        None if !context_requirements.is_empty() => ImprovementRoute::NeedsPlanning,
        None if needs_improvement => ImprovementRoute::ReadyForUpdate,
        None => ImprovementRoute::NoUpdateNeeded,
    };

    Ok(ImprovementOutput {
        summary: trimmed_or_default(output.summary, "No summary provided."),
        needs_improvement,
        route: Some(route),
        recommendation: normalize_optional_text(output.recommendation),
        findings: ImprovementFindings {
            title_gaps: normalize_string_list(output.findings.title_gaps),
            description_gaps: normalize_string_list(output.findings.description_gaps),
            acceptance_criteria_gaps: normalize_string_list(
                output.findings.acceptance_criteria_gaps,
            ),
            metadata_gaps: normalize_string_list(output.findings.metadata_gaps),
            structure_opportunities: normalize_string_list(output.findings.structure_opportunities),
        },
        context_requirements,
        follow_up_questions,
        proposal: ImprovementProposal {
            title,
            description,
            priority,
            estimate,
            labels,
            parent_issue_identifier,
            acceptance_criteria,
        },
    })
}

fn findings_present(findings: &ImprovementFindings) -> bool {
    !findings.title_gaps.is_empty()
        || !findings.description_gaps.is_empty()
        || !findings.acceptance_criteria_gaps.is_empty()
        || !findings.metadata_gaps.is_empty()
        || !findings.structure_opportunities.is_empty()
}

fn proposal_has_remote_mutation(proposal: &ImprovementProposal) -> bool {
    proposal.title.is_some()
        || proposal.description.is_some()
        || proposal.priority.is_some()
        || proposal.estimate.is_some()
        || proposal.labels.is_some()
        || proposal.parent_issue_identifier.is_some()
}

fn normalize_string_list(values: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty()
            || normalized
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(trimmed))
        {
            continue;
        }
        normalized.push(trimmed.to_string());
    }
    normalized
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn render_mode(mode: BacklogImproveModeArg) -> &'static str {
    match mode {
        BacklogImproveModeArg::Basic => "basic",
        BacklogImproveModeArg::Advanced => "advanced",
    }
}

fn render_proposal_markdown(mode: BacklogImproveModeArg, output: &ImprovementOutput) -> String {
    let mut lines = vec![
        "# Backlog Improvement Proposal".to_string(),
        String::new(),
        format!("- Mode: `{}`", render_mode(mode)),
        format!("- Route: `{}`", output.route().as_str()),
        format!("- Needs improvement: `{}`", output.needs_improvement),
        String::new(),
        "## Summary".to_string(),
        String::new(),
        output.summary.clone(),
        String::new(),
    ];
    if let Some(recommendation) = output.recommendation.as_deref() {
        lines.push("## Recommendation".to_string());
        lines.push(String::new());
        lines.push(recommendation.to_string());
        lines.push(String::new());
    }

    for (title, values) in [
        ("Title Gaps", &output.findings.title_gaps),
        ("Description Gaps", &output.findings.description_gaps),
        (
            "Acceptance Criteria Gaps",
            &output.findings.acceptance_criteria_gaps,
        ),
        ("Metadata Gaps", &output.findings.metadata_gaps),
        (
            "Structure Opportunities",
            &output.findings.structure_opportunities,
        ),
        ("Context Requirements", &output.context_requirements),
        ("Follow-up Questions", &output.follow_up_questions),
    ] {
        lines.push(format!("## {title}"));
        lines.push(String::new());
        if values.is_empty() {
            lines.push("- None identified.".to_string());
        } else {
            lines.extend(values.iter().map(|value| format!("- {value}")));
        }
        lines.push(String::new());
    }

    lines.push("## Proposed Changes".to_string());
    lines.push(String::new());
    lines.push(format!(
        "- Title: {}",
        output.proposal.title.as_deref().unwrap_or("_unchanged_")
    ));
    lines.push(format!(
        "- Priority: {}",
        output
            .proposal
            .priority
            .map(|value| value.to_string())
            .unwrap_or_else(|| "_unchanged_".to_string())
    ));
    lines.push(format!(
        "- Estimate: {}",
        output
            .proposal
            .estimate
            .map(|value| value.to_string())
            .unwrap_or_else(|| "_unchanged_".to_string())
    ));
    lines.push(format!(
        "- Parent issue: {}",
        output
            .proposal
            .parent_issue_identifier
            .as_deref()
            .unwrap_or("_unchanged_")
    ));
    lines.push(format!(
        "- Labels: {}",
        output
            .proposal
            .labels
            .as_ref()
            .map(|labels| labels.join(", "))
            .unwrap_or_else(|| "_unchanged_".to_string())
    ));
    lines.push(String::new());
    lines.push("### Acceptance Criteria".to_string());
    lines.push(String::new());
    if output.proposal.acceptance_criteria.is_empty() {
        lines.push("- _No explicit acceptance-criteria rewrite proposed._".to_string());
    } else {
        lines.extend(
            output
                .proposal
                .acceptance_criteria
                .iter()
                .map(|value| format!("- {value}")),
        );
    }
    lines.push(String::new());
    lines.push("### Description".to_string());
    lines.push(String::new());
    lines.push(
        output
            .proposal
            .description
            .clone()
            .unwrap_or_else(|| "_No description rewrite proposed._".to_string()),
    );
    lines.join("\n")
}

fn render_improvement_prompt(
    root: &Path,
    issue: &IssueSummary,
    local_index_snapshot: Option<&str>,
    related_backlog_issues: &[IssueSummary],
    mode: BacklogImproveModeArg,
) -> Result<String> {
    let repo_target = RepoTarget::from_root(root);
    let planning_context = load_context_bundle(root)?;
    let current_description = issue
        .description
        .as_deref()
        .unwrap_or("_No Linear description was provided._");
    let local_backlog_block = local_index_snapshot
        .map(str::trim)
        .filter(|contents| !contents.is_empty())
        .map(|contents| render_fenced_block("md", contents))
        .unwrap_or_else(|| "_No local backlog packet exists yet for this issue._".to_string());
    let related_backlog_block = render_related_backlog_catalog(issue, related_backlog_issues);

    Ok(format!(
        "You are improving the quality of an existing repo-scoped backlog issue.\n\n\
Repository scope:\n{repo_scope}\n\n\
Improvement mode: `{mode}`\n\
- `basic`: keep edits conservative and focus on labels, title hygiene, missing acceptance criteria, priority, estimate, and small description cleanups.\n\
- `advanced`: you may rewrite title/description more deeply and recommend or assign an existing parent issue when the work clearly belongs in a parent-child structure.\n\n\
Issue metadata:\n\
- Identifier: `{identifier}`\n\
- Title: {title}\n\
- Team: {team}\n\
- Project: {project}\n\
- State: {state}\n\
- Priority: {priority}\n\
- Estimate: {estimate}\n\
- Labels: {labels}\n\
- Parent: {parent}\n\
- Children: {children}\n\
- URL: {url}\n\n\
Current Linear description:\n{current_description_block}\n\n\
Current local backlog index snapshot:\n{local_backlog_block}\n\n\
Related repo-scoped backlog issues:\n{related_backlog_block}\n\n\
Repository planning context:\n{planning_context}\n\n\
Instructions:\n\
1. Decide whether this issue needs improvement before execution.\n\
2. Inspect issue hygiene gaps: weak title, weak description, missing acceptance criteria, absent or unclear labels, missing priority/estimate, and opportunities to group work under an existing parent issue.\n\
3. Stay inside the provided repository scope. Do not invent cross-repo work or new storage models.\n\
4. When you propose a parent issue, choose only from the provided related backlog issue catalog and only when the relationship is strong.\n\
5. When you propose description changes, return the full Markdown description ready for `{project_dir}/backlog/<ISSUE>/index.md`.\n\
6. In `basic` mode, prefer modest rewrites and safe metadata cleanup. In `advanced` mode, you may rewrite more substantially and use structure changes when justified.\n\
7. First choose exactly one route:\n\
- `no_update_needed`: the issue is already strong enough. Do not propose changes.\n\
- `ready_for_update`: you have enough context to recommend a concrete update now.\n\
- `needs_planning`: the issue lacks planning or context that a human should gather before editing it.\n\
- `needs_questions`: you need direct follow-up answers before you can responsibly recommend an update.\n\
8. Return JSON only using this exact shape:\n\
{{\n\
  \"summary\": \"One paragraph explaining the main improvement judgment\",\n\
  \"needs_improvement\": true,\n\
  \"route\": \"ready_for_update\",\n\
  \"recommendation\": \"Short operator-facing recommendation\",\n\
  \"findings\": {{\n\
    \"title_gaps\": [\"...\"],\n\
    \"description_gaps\": [\"...\"],\n\
    \"acceptance_criteria_gaps\": [\"...\"],\n\
    \"metadata_gaps\": [\"...\"],\n\
    \"structure_opportunities\": [\"...\"]\n\
  }},\n\
  \"context_requirements\": [\"Planning artifact or context still required\"],\n\
  \"follow_up_questions\": [\"Question that blocks a safe recommendation\"],\n\
  \"proposal\": {{\n\
    \"title\": \"Optional replacement title\",\n\
    \"description\": \"Optional full Markdown rewrite\",\n\
    \"priority\": 2,\n\
    \"estimate\": 3,\n\
    \"labels\": [\"plan\", \"technical\"],\n\
    \"parent_issue_identifier\": \"ENG-10001\",\n\
    \"acceptance_criteria\": [\"...\"]\n\
  }}\n\
}}",
        repo_scope = repo_target.prompt_scope_block(),
        mode = render_mode(mode),
        identifier = issue.identifier,
        title = issue.title,
        team = issue.team.key,
        project = issue
            .project
            .as_ref()
            .map(|project| project.name.as_str())
            .unwrap_or("No project"),
        state = issue
            .state
            .as_ref()
            .map(|state| state.name.as_str())
            .unwrap_or("Unknown"),
        priority = issue
            .priority
            .map(|value| value.to_string())
            .unwrap_or_else(|| "None".to_string()),
        estimate = issue
            .estimate
            .map(|value| value.to_string())
            .unwrap_or_else(|| "None".to_string()),
        labels = render_labels(issue),
        parent = issue
            .parent
            .as_ref()
            .map(|parent| parent.identifier.as_str())
            .unwrap_or("None"),
        children = issue.children.len(),
        url = issue.url,
        current_description_block = render_fenced_block("md", current_description),
        project_dir = branding::PROJECT_DIR,
    ))
}

fn render_related_backlog_catalog(
    issue: &IssueSummary,
    related_backlog_issues: &[IssueSummary],
) -> String {
    let mut entries = related_backlog_issues
        .iter()
        .filter(|candidate| candidate.identifier != issue.identifier)
        .map(|candidate| {
            format!(
                "- `{}` | {} | parent={} | labels={} | title={}",
                candidate.identifier,
                candidate
                    .state
                    .as_ref()
                    .map(|state| state.name.as_str())
                    .unwrap_or("Unknown"),
                candidate
                    .parent
                    .as_ref()
                    .map(|parent| parent.identifier.as_str())
                    .unwrap_or("none"),
                render_labels(candidate),
                candidate.title
            )
        })
        .collect::<Vec<_>>();

    if entries.is_empty() {
        "_No other repo-scoped backlog issues were available for structure comparison._".to_string()
    } else {
        entries.truncate(25);
        entries.join("\n")
    }
}

fn render_labels(issue: &IssueSummary) -> String {
    if issue.labels.is_empty() {
        "none".to_string()
    } else {
        issue
            .labels
            .iter()
            .map(|label| label.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
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

fn build_issue_metadata(issue: &IssueSummary) -> BacklogIssueMetadata {
    BacklogIssueMetadata {
        issue_id: issue.id.clone(),
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        url: issue.url.clone(),
        team_key: issue.team.key.clone(),
        project_id: issue.project.as_ref().map(|project| project.id.clone()),
        project_name: issue.project.as_ref().map(|project| project.name.clone()),
        parent_id: issue.parent.as_ref().map(|parent| parent.id.clone()),
        parent_identifier: issue
            .parent
            .as_ref()
            .map(|parent| parent.identifier.clone()),
        local_hash: None,
        remote_hash: None,
        last_sync_at: None,
        last_pulled_comment_ids: Vec::new(),
        managed_files: Vec::<ManagedFileRecord>::new(),
    }
}

fn render_fenced_block(language: &str, contents: &str) -> String {
    let fence_len = max_backtick_run(contents).saturating_add(1).max(3);
    let fence = "`".repeat(fence_len);
    if language.is_empty() {
        format!("{fence}\n{contents}\n{fence}")
    } else {
        format!("{fence}{language}\n{contents}\n{fence}")
    }
}

fn max_backtick_run(value: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for ch in value.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn load_context_bundle(root: &Path) -> Result<String> {
    let paths = PlanningPaths::new(root);
    let sections = [
        ("SCAN.md", paths.scan_path()),
        ("ARCHITECTURE.md", paths.architecture_path()),
        ("CONVENTIONS.md", paths.conventions_path()),
        ("STACK.md", paths.stack_path()),
        ("TESTING.md", paths.testing_path()),
    ];
    let mut lines = Vec::new();
    for (title, path) in sections {
        lines.push(format!("## {title}"));
        lines.push(String::new());
        lines.push(read_context(&path)?);
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}

fn read_context(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(format!(
            "_Missing `{}`. Run `meta scan` to generate it._",
            path.file_name()
                .map(|value| value.to_string_lossy())
                .unwrap_or_default()
        )),
        Err(error) => Err(error).with_context(|| format!("failed to read `{}`", path.display())),
    }
}

fn parse_agent_json<T>(raw: &str, phase: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let sanitized = strip_ansi_escapes(raw);
    let trimmed = sanitized.trim();
    let mut candidates = vec![trimmed.to_string()];
    if let Some(stripped) = strip_code_fence(trimmed) {
        candidates.push(stripped);
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}'))
        && start <= end
    {
        candidates.push(trimmed[start..=end].to_string());
    }

    for candidate in candidates {
        if let Ok(parsed) = serde_json::from_str::<T>(&candidate) {
            return Ok(parsed);
        }
    }

    bail!(
        "backlog improvement agent returned invalid JSON during {phase}: {}",
        preview_text(trimmed)
    )
}

/// Strips ANSI escape sequences (CSI sequences and OSC sequences) from agent
/// subprocess output. This prevents terminal corruption when the agent emits
/// escape sequences despite `TERM=dumb` and `NO_COLOR=1`.
fn strip_ansi_escapes(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    // CSI sequence: ESC [ <params> <final byte>
                    chars.next();
                    for next in chars.by_ref() {
                        if next.is_ascii_alphabetic() || next == '~' {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC sequence: ESC ] ... ST (ST = ESC \ or BEL)
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\x07' {
                            break;
                        }
                        if next == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {
                    // Single-char escape — skip the escape and the next char.
                    chars.next();
                }
            }
        } else {
            output.push(ch);
        }
    }
    output
}

fn strip_code_fence(raw: &str) -> Option<String> {
    let stripped = raw.strip_prefix("```")?;
    let stripped = stripped
        .strip_prefix("json\n")
        .or_else(|| stripped.strip_prefix("JSON\n"))
        .or_else(|| stripped.strip_prefix('\n'))
        .unwrap_or(stripped);
    let stripped = stripped.strip_suffix("```")?;
    Some(stripped.trim().to_string())
}

fn preview_text(value: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 240;
    let Some((truncate_at, _)) = value.char_indices().nth(MAX_PREVIEW_CHARS) else {
        return value.to_string();
    };
    format!("{}...", &value[..truncate_at])
}

fn trimmed_or_default(value: String, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn now_rfc3339() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(&format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second]Z"
        ))
        .context("failed to format the backlog improvement timestamp")
}

fn improvement_run_id() -> Result<String> {
    let now = OffsetDateTime::now_utc();
    let base = now
        .format(&format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        ))
        .context("failed to format the backlog improvement run id")?;
    Ok(format!("{}-{:09}", base, now.nanosecond()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::{IssueLink, LabelRef, ProjectRef, TeamRef, WorkflowState};
    use crossterm::event::KeyEvent;

    fn demo_issue(identifier: &str, title: &str) -> IssueSummary {
        IssueSummary {
            id: format!("id-{identifier}"),
            identifier: identifier.to_string(),
            title: title.to_string(),
            description: Some(format!("# {title}\n\nDetailed description.")),
            url: format!("https://linear.example/{identifier}"),
            priority: Some(2),
            estimate: Some(3.0),
            updated_at: "2026-03-20T00:00:00Z".to_string(),
            team: TeamRef {
                id: "team-1".to_string(),
                key: "ENG".to_string(),
                name: "Engineering".to_string(),
            },
            project: Some(ProjectRef {
                id: "project-1".to_string(),
                name: "MetaStack CLI".to_string(),
            }),
            assignee: None,
            labels: vec![LabelRef {
                id: "label-1".to_string(),
                name: "plan".to_string(),
            }],
            comments: Vec::new(),
            state: Some(WorkflowState {
                id: "state-1".to_string(),
                name: "Backlog".to_string(),
                kind: Some("unstarted".to_string()),
            }),
            attachments: Vec::new(),
            parent: None,
            children: Vec::new(),
        }
    }

    #[test]
    fn render_improvement_prompt_uses_safe_fences_for_existing_markdown_code_blocks() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        let paths = PlanningPaths::new(root);
        write_text_file(&paths.scan_path(), "scan", true).expect("scan context");
        write_text_file(&paths.architecture_path(), "architecture", true).expect("architecture");
        write_text_file(&paths.conventions_path(), "conventions", true).expect("conventions");
        write_text_file(&paths.stack_path(), "stack", true).expect("stack");
        write_text_file(&paths.testing_path(), "testing", true).expect("testing");

        let issue = IssueSummary {
            id: "issue-1".to_string(),
            identifier: "ENG-10170".to_string(),
            title: "Improve prompt fences".to_string(),
            description: Some("```bash\ncargo test\n```".to_string()),
            url: "https://linear.example/ENG-10170".to_string(),
            priority: None,
            estimate: None,
            updated_at: "2026-03-20T00:00:00Z".to_string(),
            team: TeamRef {
                id: "team-1".to_string(),
                key: "ENG".to_string(),
                name: "Engineering".to_string(),
            },
            project: Some(ProjectRef {
                id: "project-1".to_string(),
                name: "MetaStack CLI".to_string(),
            }),
            assignee: None,
            labels: Vec::new(),
            comments: Vec::new(),
            state: Some(WorkflowState {
                id: "state-1".to_string(),
                name: "Backlog".to_string(),
                kind: Some("unstarted".to_string()),
            }),
            attachments: Vec::new(),
            parent: Some(IssueLink {
                id: "issue-parent".to_string(),
                identifier: "ENG-10100".to_string(),
                title: "Parent".to_string(),
                url: "https://linear.example/ENG-10100".to_string(),
                description: None,
            }),
            children: Vec::new(),
        };

        let prompt = render_improvement_prompt(
            root,
            &issue,
            Some("```md\n## Local\n```"),
            &[],
            BacklogImproveModeArg::Advanced,
        )
        .expect("prompt");

        assert!(prompt.contains("Current Linear description:\n````md\n```bash"));
        assert!(prompt.contains("Current local backlog index snapshot:\n````md\n```md"));
    }

    #[test]
    fn normalize_improvement_output_dedupes_labels_and_marks_changes() {
        let issue = IssueSummary {
            id: "issue-1".to_string(),
            identifier: "ENG-10170".to_string(),
            title: "Improve".to_string(),
            description: None,
            url: "https://linear.example/ENG-10170".to_string(),
            priority: None,
            estimate: None,
            updated_at: "2026-03-20T00:00:00Z".to_string(),
            team: TeamRef {
                id: "team-1".to_string(),
                key: "ENG".to_string(),
                name: "Engineering".to_string(),
            },
            project: None,
            assignee: None,
            labels: Vec::new(),
            comments: Vec::new(),
            state: None,
            attachments: Vec::new(),
            parent: None,
            children: Vec::new(),
        };

        let normalized = normalize_improvement_output(
            &issue,
            ImprovementOutput {
                summary: "  summary  ".to_string(),
                needs_improvement: false,
                route: None,
                recommendation: None,
                findings: ImprovementFindings::default(),
                context_requirements: Vec::new(),
                follow_up_questions: Vec::new(),
                proposal: ImprovementProposal {
                    labels: Some(vec![
                        "plan".to_string(),
                        " Plan ".to_string(),
                        "technical".to_string(),
                    ]),
                    acceptance_criteria: vec![" first ".to_string(), "first".to_string()],
                    ..ImprovementProposal::default()
                },
            },
        )
        .expect("normalize");

        assert!(normalized.needs_improvement);
        assert_eq!(
            normalized.proposal.labels,
            Some(vec!["plan".to_string(), "technical".to_string()])
        );
        assert_eq!(
            normalized.proposal.acceptance_criteria,
            vec!["first".to_string()]
        );
        assert_eq!(normalized.summary, "summary");
    }

    #[test]
    fn improvement_dashboard_defaults_to_all_issues_when_none_are_toggled() {
        let issues = vec![
            demo_issue("ENG-10170", "First"),
            demo_issue("ENG-10171", "Second"),
        ];
        let mut app = ImprovementDashboardApp::new(issues.clone());

        let selection = app.select();

        assert_eq!(
            selection
                .issues
                .iter()
                .map(|issue| issue.identifier.as_str())
                .collect::<Vec<_>>(),
            issues
                .iter()
                .map(|issue| issue.identifier.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn improvement_dashboard_enter_returns_only_explicitly_selected_subset() {
        let issues = vec![
            demo_issue("ENG-10170", "First"),
            demo_issue("ENG-10171", "Second"),
            demo_issue("ENG-10172", "Third"),
        ];
        let mut app = ImprovementDashboardApp::new(issues.clone());
        app.toggle();
        app.move_down();
        app.move_down();
        app.toggle();

        let selection = app.select();

        assert_eq!(
            selection
                .issues
                .iter()
                .map(|issue| issue.identifier.as_str())
                .collect::<Vec<_>>(),
            vec!["ENG-10170", "ENG-10172"]
        );
    }

    #[test]
    fn improvement_dashboard_snapshot_shows_search_and_issue_list() {
        let snapshot = render_improvement_dashboard_once(
            vec![
                demo_issue("ENG-10170", "First"),
                demo_issue("ENG-10171", "Second"),
            ],
            120,
            36,
        )
        .expect("snapshot");

        assert!(
            snapshot
                .contains("No explicit subset selected. Enter will review all 2 listed issues.")
        );
        assert!(snapshot.contains("ENG-10170"));
        assert!(snapshot.contains("Search"));
        assert!(snapshot.contains("Issue Preview"));
    }

    #[test]
    fn improvement_dashboard_search_filters_issue_list() {
        let issues = vec![
            demo_issue("ENG-10170", "Fix authentication bug"),
            demo_issue("ENG-10171", "Add dark mode toggle"),
            demo_issue("ENG-10172", "Refactor auth middleware"),
        ];
        let mut app = ImprovementDashboardApp::new(issues);
        app.set_search_query("auth");

        let filtered = dashboard_search_results(&app);
        let identifiers = filtered
            .iter()
            .map(|r| app.issues[r.issue_index].identifier.as_str())
            .collect::<Vec<_>>();
        assert!(identifiers.contains(&"ENG-10170"));
        assert!(identifiers.contains(&"ENG-10172"));
        assert!(!identifiers.contains(&"ENG-10171"));
    }

    #[test]
    fn improvement_dashboard_toggle_respects_search_filtered_position() {
        let issues = vec![
            demo_issue("ENG-10170", "Fix authentication bug"),
            demo_issue("ENG-10171", "Add dark mode toggle"),
            demo_issue("ENG-10172", "Refactor auth middleware"),
        ];
        let mut app = ImprovementDashboardApp::new(issues);
        app.set_search_query("auth");

        let filtered = dashboard_search_results(&app);
        assert!(filtered.len() >= 2);

        app.toggle();

        let toggled_index = filtered[0].issue_index;
        assert!(app.selected[toggled_index]);
        assert!(!app.selected[1]); // ENG-10171 is never matched by "auth"
    }

    #[test]
    fn instruction_prompt_renders_side_by_side_with_ticket_preview() {
        let issues = vec![demo_issue("ENG-10170", "First issue title")];
        let snapshot = render_instruction_prompt_snapshot(issues, 120, 24)
            .expect("instruction prompt snapshot");
        assert!(
            snapshot.contains("Instructions"),
            "snapshot should show editor pane"
        );
        assert!(
            snapshot.contains("Ticket Preview"),
            "snapshot should show ticket preview pane"
        );
        assert!(
            snapshot.contains("ENG-10170"),
            "snapshot should show issue identifier in preview"
        );
        assert!(
            snapshot.contains("optional"),
            "snapshot should mention optional instructions"
        );
    }

    #[test]
    fn instruction_prompt_uses_multiline_editor() {
        let app = InstructionPromptApp::new(vec![demo_issue("ENG-10170", "First")]);
        let mut input = app.input.clone();
        assert!(
            input.insert_newline(),
            "multiline editor should accept newlines"
        );
    }

    #[test]
    fn instruction_prompt_cycles_through_multiple_issues() {
        let issues = vec![
            demo_issue("ENG-10170", "First"),
            demo_issue("ENG-10171", "Second"),
            demo_issue("ENG-10172", "Third"),
        ];
        let mut app = InstructionPromptApp::new(issues);
        assert_eq!(app.issue_cursor, 0);
        app.next_issue();
        assert_eq!(app.issue_cursor, 1);
        app.next_issue();
        assert_eq!(app.issue_cursor, 2);
        app.next_issue();
        assert_eq!(app.issue_cursor, 0);
        app.prev_issue();
        assert_eq!(app.issue_cursor, 2);
    }

    #[test]
    fn instruction_prompt_degrades_to_vertical_layout_when_narrow() {
        let issues = vec![demo_issue("ENG-10170", "First")];
        let snapshot = render_instruction_prompt_snapshot(issues, 50, 24).expect("narrow snapshot");
        assert!(snapshot.contains("Instructions"));
        assert!(snapshot.contains("Ticket Preview"));
    }

    #[test]
    fn normalize_improvement_output_uses_follow_up_questions_route_when_questions_are_present() {
        let issue = demo_issue("ENG-10170", "First");
        let normalized = normalize_improvement_output(
            &issue,
            ImprovementOutput {
                summary: "Need one clarification.".to_string(),
                needs_improvement: true,
                route: None,
                recommendation: Some("Ask the engineer one direct question first.".to_string()),
                findings: ImprovementFindings::default(),
                context_requirements: Vec::new(),
                follow_up_questions: vec![
                    "Which subcommand should own the final apply step?".to_string(),
                ],
                proposal: ImprovementProposal::default(),
            },
        )
        .expect("normalize");

        assert_eq!(normalized.route(), ImprovementRoute::NeedsQuestions);
        assert_eq!(normalized.follow_up_questions.len(), 1);
    }

    #[test]
    fn review_dashboard_shows_enter_skip_and_reject_actions() {
        let issue = demo_issue("ENG-10170", "First");
        let app = ImprovementReviewApp::new(
            1,
            3,
            issue,
            ImprovementOutput {
                summary: "The ticket can be improved safely.".to_string(),
                needs_improvement: true,
                route: Some(ImprovementRoute::ReadyForUpdate),
                recommendation: Some("Apply the metadata and description cleanup now.".to_string()),
                findings: ImprovementFindings::default(),
                context_requirements: Vec::new(),
                follow_up_questions: Vec::new(),
                proposal: ImprovementProposal {
                    title: Some("Improved title".to_string()),
                    ..ImprovementProposal::default()
                },
            },
            0,
        );

        let snapshot = review_dashboard_snapshot(&app, 140, 40);
        assert!(snapshot.contains("Apply Update"));
        assert!(snapshot.contains("Skip"));
        assert!(snapshot.contains("Reject"));
    }

    #[test]
    fn review_dashboard_shows_before_after_comparison() {
        let issue = demo_issue("ENG-10170", "Original title");
        let app = ImprovementReviewApp::new(
            1,
            1,
            issue,
            ImprovementOutput {
                summary: "Description needs improvement.".to_string(),
                needs_improvement: true,
                route: Some(ImprovementRoute::ReadyForUpdate),
                recommendation: Some("Rewrite the description.".to_string()),
                findings: ImprovementFindings::default(),
                context_requirements: Vec::new(),
                follow_up_questions: Vec::new(),
                proposal: ImprovementProposal {
                    title: Some("Improved title".to_string()),
                    description: Some("Better description content.".to_string()),
                    ..ImprovementProposal::default()
                },
            },
            0,
        );

        let snapshot = review_dashboard_snapshot(&app, 140, 40);
        assert!(snapshot.contains("[original]"));
        assert!(snapshot.contains("[proposed]"));
        assert!(snapshot.contains("Before / After"));
        assert!(snapshot.contains("Findings & Recommendation"));
    }

    #[test]
    fn review_scrollable_panes_initialize_at_zero() {
        let issue = demo_issue("ENG-10170", "First");
        let app = ImprovementReviewApp::new(
            1,
            1,
            issue,
            ImprovementOutput {
                summary: "Summary.".to_string(),
                needs_improvement: true,
                route: Some(ImprovementRoute::ReadyForUpdate),
                recommendation: None,
                findings: ImprovementFindings::default(),
                context_requirements: Vec::new(),
                follow_up_questions: Vec::new(),
                proposal: ImprovementProposal::default(),
            },
            0,
        );

        assert_eq!(app.recommendation_scroll.offset(), 0);
        assert_eq!(app.proposal_scroll.offset(), 0);
        assert_eq!(app.review_focus, ImprovementReviewFocus::Proposal);
    }

    /// Simulates the key-event decision logic from `run_instruction_prompt` for a single
    /// key press and returns the exit action when the key would cause the prompt to return.
    fn simulate_instruction_key(
        app: &mut InstructionPromptApp,
        key: KeyEvent,
    ) -> Option<InstructionPromptExit> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        match key.code {
            KeyCode::Enter
                if !key.modifiers.contains(KeyModifiers::SHIFT)
                    && app.focus == InstructionPromptFocus::Editor =>
            {
                let text = app.input.display_value().trim().to_string();
                Some(InstructionPromptExit::Continue(if text.is_empty() {
                    None
                } else {
                    Some(text)
                }))
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let text = app.input.display_value().trim().to_string();
                Some(InstructionPromptExit::Continue(if text.is_empty() {
                    None
                } else {
                    Some(text)
                }))
            }
            KeyCode::Esc => Some(InstructionPromptExit::Cancelled),
            KeyCode::Char('q') if app.focus == InstructionPromptFocus::Preview => {
                Some(InstructionPromptExit::Cancelled)
            }
            _ => {
                if app.focus == InstructionPromptFocus::Editor {
                    app.input.handle_key(key);
                }
                None
            }
        }
    }

    #[test]
    fn instruction_prompt_escape_returns_cancelled() {
        let mut app = InstructionPromptApp::new(vec![demo_issue("ENG-10170", "First")]);
        let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let exit = simulate_instruction_key(&mut app, key);
        assert_eq!(exit, Some(InstructionPromptExit::Cancelled));
    }

    #[test]
    fn instruction_prompt_q_in_preview_returns_cancelled() {
        let mut app = InstructionPromptApp::new(vec![demo_issue("ENG-10170", "First")]);
        app.focus = InstructionPromptFocus::Preview;
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let exit = simulate_instruction_key(&mut app, key);
        assert_eq!(exit, Some(InstructionPromptExit::Cancelled));
    }

    #[test]
    fn instruction_prompt_q_in_editor_types_character_not_cancel() {
        let mut app = InstructionPromptApp::new(vec![demo_issue("ENG-10170", "First")]);
        assert_eq!(app.focus, InstructionPromptFocus::Editor);
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        let exit = simulate_instruction_key(&mut app, key);
        assert_eq!(exit, None, "q in editor mode should type, not cancel");
        assert!(
            app.input.display_value().contains('q'),
            "q should be typed into the editor"
        );
    }

    #[test]
    fn instruction_prompt_empty_enter_returns_continue_none() {
        let mut app = InstructionPromptApp::new(vec![demo_issue("ENG-10170", "First")]);
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let exit = simulate_instruction_key(&mut app, key);
        assert_eq!(exit, Some(InstructionPromptExit::Continue(None)));
    }

    #[test]
    fn instruction_prompt_enter_with_text_returns_continue_some() {
        let mut app = InstructionPromptApp::new(vec![demo_issue("ENG-10170", "First")]);
        // Type some instruction text.
        for ch in "focus on labels".chars() {
            let key = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
            simulate_instruction_key(&mut app, key);
        }
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let exit = simulate_instruction_key(&mut app, key);
        assert_eq!(
            exit,
            Some(InstructionPromptExit::Continue(Some(
                "focus on labels".to_string()
            )))
        );
    }

    #[test]
    fn instruction_prompt_escape_cancels_regardless_of_focus() {
        // Escape should cancel from both editor and preview focus.
        for focus in [
            InstructionPromptFocus::Editor,
            InstructionPromptFocus::Preview,
        ] {
            let mut app = InstructionPromptApp::new(vec![demo_issue("ENG-10170", "First")]);
            app.focus = focus;
            let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
            let exit = simulate_instruction_key(&mut app, key);
            assert_eq!(
                exit,
                Some(InstructionPromptExit::Cancelled),
                "Esc should cancel in {focus:?} mode"
            );
        }
    }

    #[test]
    fn instruction_prompt_escape_exits_regardless_of_typed_text() {
        let mut app = InstructionPromptApp::new(vec![demo_issue("ENG-10170", "First")]);
        // Type some text first.
        for ch in "hello".chars() {
            simulate_instruction_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            );
        }
        assert!(!app.input.display_value().trim().is_empty());
        // Escape should still cancel, ignoring the typed text.
        let exit =
            simulate_instruction_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(exit, Some(InstructionPromptExit::Cancelled));
    }

    #[test]
    fn classify_linear_error_detects_permission_keywords() {
        let cases = [
            "Linear request failed: You do not have permission to update this issue",
            "Linear request failed with status 403: Forbidden",
            "Linear request failed with status 401: Unauthorized",
            "failed to reach the Linear GraphQL endpoint: Authentication required",
            "Not authorized to perform this action",
            "Access denied for this resource",
        ];
        for message in cases {
            let error = anyhow::anyhow!("{message}");
            assert_eq!(
                classify_linear_error(&error),
                ImprovementErrorKind::LinearPermission,
                "expected LinearPermission for: {message}"
            );
        }
    }

    #[test]
    fn classify_linear_error_detects_network_failures() {
        let cases = [
            "failed to reach the Linear GraphQL endpoint: connection refused",
            "dns error: could not resolve host",
            "request timed out after 30s",
        ];
        for message in cases {
            let error = anyhow::anyhow!("{message}");
            assert_eq!(
                classify_linear_error(&error),
                ImprovementErrorKind::LinearNetwork,
                "expected LinearNetwork for: {message}"
            );
        }
    }

    #[test]
    fn classify_linear_error_falls_back_to_other() {
        let error = anyhow::anyhow!("no issue fields were provided to edit");
        assert_eq!(classify_linear_error(&error), ImprovementErrorKind::Other);
    }

    #[test]
    fn strip_ansi_escapes_removes_csi_sequences() {
        assert_eq!(strip_ansi_escapes("\x1b[1mhello\x1b[0m"), "hello");
        assert_eq!(
            strip_ansi_escapes("\x1b[31;1mred bold\x1b[0m text"),
            "red bold text"
        );
    }

    #[test]
    fn strip_ansi_escapes_removes_osc_sequences() {
        assert_eq!(strip_ansi_escapes("\x1b]0;title\x07content"), "content");
        assert_eq!(
            strip_ansi_escapes("\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\"),
            "link"
        );
    }

    #[test]
    fn strip_ansi_escapes_preserves_plain_text() {
        let plain = r#"{"summary":"test","needs_improvement":false}"#;
        assert_eq!(strip_ansi_escapes(plain), plain);
    }

    #[test]
    fn strip_ansi_escapes_handles_mixed_content() {
        let input = "\x1b[1m{\"key\":\x1b[32m\"value\"\x1b[0m}\x1b[0m";
        assert_eq!(strip_ansi_escapes(input), "{\"key\":\"value\"}");
    }
}
