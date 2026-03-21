use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::Backend;
use ratatui::backend::CrosstermBackend;
use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde::Deserialize;

use crate::agents::run_agent_capture;
use crate::cli::{BacklogSpecArgs, RunAgentArgs};
use crate::config::AGENT_ROUTE_BACKLOG_SPEC;
use crate::context::load_workflow_contract;
use crate::fs::{PlanningPaths, canonicalize_existing_dir, display_path, write_text_file};
use crate::progress::{LoadingPanelData, SPINNER_FRAMES, render_loading_panel};
use crate::tui::fields::InputFieldState;
use crate::tui::scroll::{ScrollState, scrollable_content_paragraph, wrapped_rows};
use crate::tui::theme::{Tone, badge, key_hints, panel_title};

const SPEC_INSTRUCTIONS: &str = include_str!("artifacts/SPEC_INSTRUCTIONS.md");
const MAX_FOLLOW_UP_QUESTIONS: usize = 3;

#[derive(Debug, Clone)]
pub(crate) enum BacklogSpecOutput {
    Report(BacklogSpecReport),
    Snapshot(String),
}

#[derive(Debug, Clone)]
pub(crate) struct BacklogSpecReport {
    mode: SpecMode,
    path: String,
    cancelled: bool,
}

impl BacklogSpecReport {
    pub(crate) fn render(&self) -> String {
        if self.cancelled {
            return "SPEC update cancelled.".to_string();
        }

        match self.mode {
            SpecMode::Create => format!("Created repo-local spec at {}.", self.path),
            SpecMode::Improve => format!("Updated repo-local spec at {}.", self.path),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpecMode {
    Create,
    Improve,
}

impl SpecMode {
    fn title(self) -> &'static str {
        match self {
            Self::Create => "Create SPEC",
            Self::Improve => "Improve SPEC",
        }
    }

    fn request_title(self) -> &'static str {
        match self {
            Self::Create => "Step 1 of 3: What should this repository build? [editing]",
            Self::Improve => "Step 1 of 3: What should be updated or improved? [editing]",
        }
    }

    fn request_placeholder(self) -> &'static str {
        match self {
            Self::Create => "Describe what you want to build for this repository...",
            Self::Improve => "Describe what should change in the existing SPEC...",
        }
    }

    fn request_help(self) -> &'static str {
        match self {
            Self::Create => {
                "Capture the core build intent first. The workflow will ask follow-up questions before drafting `.metastack/SPEC.md`."
            }
            Self::Improve => {
                "Focus on what should change. The workflow will load the current `.metastack/SPEC.md` and revise it in place."
            }
        }
    }

    fn loading_message(self, phase: PendingKind) -> &'static str {
        match (self, phase) {
            (_, PendingKind::Questions) => "Analyzing follow-up context",
            (Self::Create, PendingKind::Generate) => "Drafting repo-local SPEC",
            (Self::Improve, PendingKind::Generate) => "Revising repo-local SPEC",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct FollowUpQuestions {
    #[serde(default)]
    questions: Vec<String>,
}

#[derive(Debug, Clone)]
struct FollowUpResponse {
    question: String,
    answer: String,
}

#[derive(Debug, Clone)]
struct RequestApp {
    mode: SpecMode,
    request: InputFieldState,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct QuestionAnswer {
    question: String,
    answer: InputFieldState,
}

#[derive(Debug, Clone)]
struct QuestionsApp {
    mode: SpecMode,
    request: String,
    questions: Vec<QuestionAnswer>,
    selected: usize,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct ReviewApp {
    mode: SpecMode,
    request: String,
    follow_ups: Vec<FollowUpResponse>,
    spec_markdown: String,
    preview_scroll: ScrollState,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct LoadingApp {
    mode: SpecMode,
    phase: PendingKind,
    detail: String,
    spinner_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingKind {
    Questions,
    Generate,
}

#[derive(Debug, Clone)]
enum SpecStage {
    Request(RequestApp),
    Questions(QuestionsApp),
    Loading(LoadingApp),
    Review(ReviewApp),
}

#[derive(Debug, Clone)]
enum RecoveryStage {
    Request(RequestApp),
    Questions(QuestionsApp),
}

struct BacklogSpecApp {
    mode: SpecMode,
    spec_path: PathBuf,
    existing_spec: Option<String>,
    prefilled_answers: Vec<String>,
    stage: SpecStage,
    pending: Option<PendingJob>,
}

struct PendingJob {
    receiver: Receiver<Result<PendingResult>>,
    recovery_stage: RecoveryStage,
}

enum PendingResult {
    Questions(Vec<String>),
    Generated(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpecAction {
    Up,
    Down,
    Tab,
    Enter,
    Back,
    Wait,
}

#[derive(Debug, Clone)]
enum InteractiveExit {
    Cancelled,
    Confirmed(String),
}

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(
            stdout,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
}

/// Run the repo-local SPEC lifecycle flow for the active repository.
///
/// Returns an error when the repository root cannot be resolved, the SPEC worker flow fails, or
/// the resulting `.metastack/SPEC.md` cannot be written.
pub async fn run_backlog_spec(args: &BacklogSpecArgs) -> Result<BacklogSpecOutput> {
    let root = canonicalize_existing_dir(&args.root.root)?;
    let paths = PlanningPaths::new(&root);
    let spec_path = paths.spec_path();
    let existing_spec = read_optional_spec(&spec_path)?;
    let mode = if existing_spec.is_some() {
        SpecMode::Improve
    } else {
        SpecMode::Create
    };

    if args.render_once {
        let snapshot = render_once_snapshot(
            &root,
            mode,
            &spec_path,
            existing_spec,
            args.request.clone(),
            args.answers.clone(),
            args.events.iter().copied().map(SpecAction::from).collect(),
            args.width,
            args.height,
        )?;
        return Ok(BacklogSpecOutput::Snapshot(snapshot));
    }

    let can_launch_tui = io::stdin().is_terminal() && io::stdout().is_terminal();
    if args.no_interactive || !can_launch_tui {
        let request = args
            .request
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let Some(request) = request else {
            bail!(
                "`meta backlog spec` requires `--request <TEXT>` when `--no-interactive` is used or when the command runs without a TTY"
            );
        };
        let spec_markdown = build_spec_markdown(
            &root,
            mode,
            request,
            &build_noninteractive_follow_ups(&args.answers),
            existing_spec.as_deref(),
            args.agent.clone(),
            args.model.clone(),
            args.reasoning.clone(),
        )?;
        persist_spec(&spec_path, &spec_markdown)?;
        return Ok(BacklogSpecOutput::Report(BacklogSpecReport {
            mode,
            path: display_path(&spec_path, &root),
            cancelled: false,
        }));
    }

    match run_interactive_spec_flow(
        &root,
        mode,
        spec_path.clone(),
        existing_spec,
        args.request.clone(),
        args.answers.clone(),
        args.agent.clone(),
        args.model.clone(),
        args.reasoning.clone(),
    )? {
        InteractiveExit::Cancelled => Ok(BacklogSpecOutput::Report(BacklogSpecReport {
            mode,
            path: display_path(&spec_path, &root),
            cancelled: true,
        })),
        InteractiveExit::Confirmed(spec_markdown) => {
            persist_spec(&spec_path, &spec_markdown)?;
            Ok(BacklogSpecOutput::Report(BacklogSpecReport {
                mode,
                path: display_path(&spec_path, &root),
                cancelled: false,
            }))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_interactive_spec_flow(
    root: &Path,
    mode: SpecMode,
    spec_path: PathBuf,
    existing_spec: Option<String>,
    initial_request: Option<String>,
    answers: Vec<String>,
    agent: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
) -> Result<InteractiveExit> {
    let mut app = BacklogSpecApp::new(mode, spec_path, existing_spec, initial_request, answers);

    let mut stdout = io::stdout();
    enable_raw_mode().context("failed to enable raw mode for SPEC workflow")?;
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )
    .context("failed to enter alternate screen for SPEC workflow")?;
    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to initialize SPEC terminal")?;

    loop {
        process_pending(&mut app)?;
        terminal.draw(|frame| render_frame(frame, &app))?;

        if event::poll(Duration::from_millis(120)).context("failed to poll SPEC terminal events")? {
            match event::read().context("failed to read SPEC terminal event")? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(exit) = app.handle_key(root, key, &agent, &model, &reasoning)? {
                        return Ok(exit);
                    }
                }
                Event::Paste(text) => app.handle_paste(&text),
                Event::Mouse(mouse)
                    if matches!(
                        mouse.kind,
                        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                    ) =>
                {
                    let viewport = review_viewport(terminal.size()?.into());
                    app.handle_review_mouse(mouse, viewport);
                }
                _ => {}
            }
        } else {
            advance_loading_spinner(&mut app);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_once_snapshot(
    root: &Path,
    mode: SpecMode,
    spec_path: &Path,
    existing_spec: Option<String>,
    initial_request: Option<String>,
    answers: Vec<String>,
    actions: Vec<SpecAction>,
    width: u16,
    height: u16,
) -> Result<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal =
        Terminal::new(backend).context("failed to initialize render-once backend")?;
    let mut app = BacklogSpecApp::new(
        mode,
        spec_path.to_path_buf(),
        existing_spec,
        initial_request,
        answers,
    );

    for action in actions {
        match action {
            SpecAction::Wait => {
                process_pending_blocking(&mut app)?;
                advance_loading_spinner(&mut app);
            }
            SpecAction::Up => {
                let _ = app.handle_key(
                    root,
                    KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
                    &None,
                    &None,
                    &None,
                )?;
            }
            SpecAction::Down => {
                let _ = app.handle_key(
                    root,
                    KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                    &None,
                    &None,
                    &None,
                )?;
            }
            SpecAction::Tab => {
                let _ = app.handle_key(
                    root,
                    KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
                    &None,
                    &None,
                    &None,
                )?;
            }
            SpecAction::Enter => {
                let _ = app.handle_key(
                    root,
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                    &None,
                    &None,
                    &None,
                )?;
            }
            SpecAction::Back => {
                let _ = app.handle_key(
                    root,
                    KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                    &None,
                    &None,
                    &None,
                )?;
            }
        }
    }

    terminal.draw(|frame| render_frame(frame, &app))?;
    Ok(snapshot(terminal.backend()))
}

impl BacklogSpecApp {
    fn new(
        mode: SpecMode,
        spec_path: PathBuf,
        existing_spec: Option<String>,
        initial_request: Option<String>,
        prefilled_answers: Vec<String>,
    ) -> Self {
        Self {
            mode,
            spec_path,
            existing_spec,
            prefilled_answers,
            stage: SpecStage::Request(RequestApp {
                mode,
                request: InputFieldState::multiline(initial_request.unwrap_or_default()),
                error: None,
            }),
            pending: None,
        }
    }

    fn handle_key(
        &mut self,
        root: &Path,
        key: KeyEvent,
        agent: &Option<String>,
        model: &Option<String>,
        reasoning: &Option<String>,
    ) -> Result<Option<InteractiveExit>> {
        enum NextStep {
            None,
            StartQuestions(String),
            StartGeneration(String, Vec<FollowUpResponse>),
            ShowRequest {
                mode: SpecMode,
                request: String,
            },
            ShowQuestions {
                mode: SpecMode,
                request: String,
                follow_ups: Vec<FollowUpResponse>,
            },
            Exit(InteractiveExit),
        }

        let mut next = NextStep::None;
        match &mut self.stage {
            SpecStage::Request(app) => match key.code {
                KeyCode::Esc => return Ok(Some(InteractiveExit::Cancelled)),
                KeyCode::Enter => {
                    let request = app.request.value().trim();
                    if request.is_empty() {
                        app.error = Some(
                            "Enter what this repository should build before continuing."
                                .to_string(),
                        );
                    } else {
                        app.error = None;
                        next = NextStep::StartQuestions(request.to_string());
                    }
                }
                _ => {
                    if app.request.handle_key(key) {
                        app.error = None;
                    }
                }
            },
            SpecStage::Questions(app) => match key.code {
                KeyCode::Esc => {
                    next = NextStep::ShowRequest {
                        mode: app.mode,
                        request: app.request.clone(),
                    };
                }
                KeyCode::Up => {
                    if !app.questions.is_empty() {
                        if app.selected == 0 {
                            app.selected = app.questions.len().saturating_sub(1);
                        } else {
                            app.selected -= 1;
                        }
                        app.error = None;
                    }
                }
                KeyCode::Down | KeyCode::Tab => {
                    if !app.questions.is_empty() {
                        app.selected = (app.selected + 1) % app.questions.len();
                        app.error = None;
                    }
                }
                KeyCode::Enter => {
                    let follow_ups = collect_follow_up_answers(&app.questions)?;
                    next = NextStep::StartGeneration(app.request.clone(), follow_ups);
                }
                _ => {
                    if let Some(current) = app.questions.get_mut(app.selected) {
                        if current.answer.handle_key(key) {
                            app.error = None;
                        }
                    }
                }
            },
            SpecStage::Loading(_) => {
                if key.code == KeyCode::Esc {
                    return Ok(Some(InteractiveExit::Cancelled));
                }
            }
            SpecStage::Review(app) => match key.code {
                KeyCode::Esc => {
                    next = if app.follow_ups.is_empty() {
                        NextStep::ShowRequest {
                            mode: app.mode,
                            request: app.request.clone(),
                        }
                    } else {
                        NextStep::ShowQuestions {
                            mode: app.mode,
                            request: app.request.clone(),
                            follow_ups: app.follow_ups.clone(),
                        }
                    };
                }
                KeyCode::Enter => {
                    next = NextStep::Exit(InteractiveExit::Confirmed(app.spec_markdown.clone()));
                }
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End => {
                    let viewport = review_viewport(Rect::new(0, 0, 120, 32));
                    let _ = app.preview_scroll.apply_key_in_viewport(
                        key,
                        viewport,
                        app.preview_rows(viewport.width.max(1)),
                    );
                }
                _ => {}
            },
        }

        match next {
            NextStep::None => Ok(None),
            NextStep::StartQuestions(request) => {
                start_questions(
                    self,
                    root,
                    request,
                    agent.clone(),
                    model.clone(),
                    reasoning.clone(),
                );
                Ok(None)
            }
            NextStep::StartGeneration(request, follow_ups) => {
                start_generation(
                    self,
                    root,
                    request,
                    follow_ups,
                    agent.clone(),
                    model.clone(),
                    reasoning.clone(),
                );
                Ok(None)
            }
            NextStep::ShowRequest { mode, request } => {
                self.stage = SpecStage::Request(RequestApp {
                    mode,
                    request: InputFieldState::multiline(request),
                    error: None,
                });
                Ok(None)
            }
            NextStep::ShowQuestions {
                mode,
                request,
                follow_ups,
            } => {
                self.stage = SpecStage::Questions(QuestionsApp {
                    mode,
                    request,
                    questions: follow_ups
                        .into_iter()
                        .map(|follow_up| QuestionAnswer {
                            question: follow_up.question,
                            answer: InputFieldState::multiline(follow_up.answer),
                        })
                        .collect(),
                    selected: 0,
                    error: None,
                });
                Ok(None)
            }
            NextStep::Exit(exit) => Ok(Some(exit)),
        }
    }

    fn handle_paste(&mut self, text: &str) {
        match &mut self.stage {
            SpecStage::Request(app) => {
                if app.request.paste(text) {
                    app.error = None;
                }
            }
            SpecStage::Questions(app) => {
                if let Some(current) = app.questions.get_mut(app.selected)
                    && current.answer.paste(text)
                {
                    app.error = None;
                }
            }
            _ => {}
        }
    }

    fn handle_review_mouse(&mut self, mouse: crossterm::event::MouseEvent, viewport: Rect) {
        if let SpecStage::Review(app) = &mut self.stage {
            let _ = app.preview_scroll.apply_mouse_in_viewport(
                mouse,
                viewport,
                app.preview_rows(viewport.width.max(1)),
            );
        }
    }
}

impl ReviewApp {
    fn preview_rows(&self, width: u16) -> usize {
        wrapped_rows(&self.spec_markdown, width.max(1))
    }
}

fn start_questions(
    app: &mut BacklogSpecApp,
    root: &Path,
    request: String,
    agent: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
) {
    let recovery_stage = RecoveryStage::Request(RequestApp {
        mode: app.mode,
        request: InputFieldState::multiline(request.clone()),
        error: None,
    });
    app.stage = SpecStage::Loading(LoadingApp {
        mode: app.mode,
        phase: PendingKind::Questions,
        detail: "Reviewing the repository context and drafting concise follow-up questions."
            .to_string(),
        spinner_index: 0,
    });
    app.pending = Some(PendingJob {
        receiver: spawn_question_job(
            root.to_path_buf(),
            app.mode,
            request,
            agent,
            model,
            reasoning,
        ),
        recovery_stage,
    });
}

fn start_generation(
    app: &mut BacklogSpecApp,
    root: &Path,
    request: String,
    follow_ups: Vec<FollowUpResponse>,
    agent: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
) {
    let recovery_stage = RecoveryStage::Questions(QuestionsApp {
        mode: app.mode,
        request: request.clone(),
        questions: follow_ups
            .iter()
            .map(|follow_up| QuestionAnswer {
                question: follow_up.question.clone(),
                answer: InputFieldState::multiline(follow_up.answer.clone()),
            })
            .collect(),
        selected: 0,
        error: None,
    });
    let existing_spec = app.existing_spec.clone();
    app.stage = SpecStage::Loading(LoadingApp {
        mode: app.mode,
        phase: PendingKind::Generate,
        detail: format!(
            "Preparing `{}` without touching Linear or backlog packets.",
            app.spec_path.display()
        ),
        spinner_index: 0,
    });
    app.pending = Some(PendingJob {
        receiver: spawn_generation_job(
            root.to_path_buf(),
            app.mode,
            request,
            follow_ups,
            existing_spec,
            agent,
            model,
            reasoning,
        ),
        recovery_stage,
    });
}

fn process_pending(app: &mut BacklogSpecApp) -> Result<()> {
    let Some(pending) = app.pending.as_ref() else {
        return Ok(());
    };

    match pending.receiver.try_recv() {
        Ok(result) => finish_pending(app, result),
        Err(TryRecvError::Empty) => Ok(()),
        Err(TryRecvError::Disconnected) => restore_from_error(
            app,
            "SPEC worker exited before returning a result".to_string(),
        ),
    }
}

fn process_pending_blocking(app: &mut BacklogSpecApp) -> Result<()> {
    let Some(pending) = app.pending.as_ref() else {
        return Ok(());
    };

    let result = pending
        .receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|error| {
            anyhow!("SPEC worker did not finish before render-once timeout: {error}")
        })?;
    finish_pending(app, result)
}

fn finish_pending(app: &mut BacklogSpecApp, result: Result<PendingResult>) -> Result<()> {
    let pending = app
        .pending
        .take()
        .ok_or_else(|| anyhow!("SPEC pending job disappeared unexpectedly"))?;

    match result {
        Ok(PendingResult::Questions(questions)) => {
            let question_answers = questions
                .into_iter()
                .enumerate()
                .map(|(index, question)| QuestionAnswer {
                    question,
                    answer: InputFieldState::multiline(
                        app.prefilled_answers
                            .get(index)
                            .cloned()
                            .unwrap_or_default(),
                    ),
                })
                .collect::<Vec<_>>();
            let request = match pending.recovery_stage {
                RecoveryStage::Request(request) => request.request.value().trim().to_string(),
                RecoveryStage::Questions(questions) => questions.request,
            };
            app.stage = SpecStage::Questions(QuestionsApp {
                mode: app.mode,
                request,
                questions: question_answers,
                selected: 0,
                error: None,
            });
            Ok(())
        }
        Ok(PendingResult::Generated(spec_markdown)) => {
            let request = match &pending.recovery_stage {
                RecoveryStage::Request(request) => request.request.value().trim().to_string(),
                RecoveryStage::Questions(questions) => questions.request.clone(),
            };
            let follow_ups = match pending.recovery_stage {
                RecoveryStage::Request(_) => Vec::new(),
                RecoveryStage::Questions(questions) => {
                    collect_follow_up_answers(&questions.questions)?
                }
            };
            app.stage = SpecStage::Review(ReviewApp {
                mode: app.mode,
                request,
                follow_ups,
                spec_markdown,
                preview_scroll: ScrollState::default(),
                error: None,
            });
            Ok(())
        }
        Err(error) => restore_from_error(app, error.to_string()),
    }
}

fn restore_from_error(app: &mut BacklogSpecApp, error: String) -> Result<()> {
    let pending = app
        .pending
        .take()
        .ok_or_else(|| anyhow!("SPEC pending job disappeared unexpectedly"))?;
    match pending.recovery_stage {
        RecoveryStage::Request(mut request) => {
            request.error = Some(error);
            app.stage = SpecStage::Request(request);
        }
        RecoveryStage::Questions(mut questions) => {
            questions.error = Some(error);
            app.stage = SpecStage::Questions(questions);
        }
    }
    Ok(())
}

fn spawn_question_job(
    root: PathBuf,
    mode: SpecMode,
    request: String,
    agent: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
) -> Receiver<Result<PendingResult>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = build_follow_up_questions(&root, mode, &request, agent, model, reasoning)
            .map(PendingResult::Questions);
        let _ = sender.send(result);
    });
    receiver
}

#[allow(clippy::too_many_arguments)]
fn spawn_generation_job(
    root: PathBuf,
    mode: SpecMode,
    request: String,
    follow_ups: Vec<FollowUpResponse>,
    existing_spec: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
) -> Receiver<Result<PendingResult>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = build_spec_markdown(
            &root,
            mode,
            &request,
            &follow_ups,
            existing_spec.as_deref(),
            agent,
            model,
            reasoning,
        )
        .map(PendingResult::Generated);
        let _ = sender.send(result);
    });
    receiver
}

fn build_follow_up_questions(
    root: &Path,
    mode: SpecMode,
    request: &str,
    agent: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
) -> Result<Vec<String>> {
    let output = run_agent_capture(&RunAgentArgs {
        root: Some(root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_BACKLOG_SPEC.to_string()),
        agent,
        prompt: render_question_prompt(root, mode, request)?,
        instructions: Some(SPEC_INSTRUCTIONS.to_string()),
        model,
        reasoning,
        transport: None,
        attachments: Vec::new(),
    })?;
    let parsed: FollowUpQuestions = parse_agent_json(&output.stdout, "SPEC follow-up questions")?;
    Ok(parsed
        .questions
        .into_iter()
        .map(|question| question.trim().to_string())
        .filter(|question| !question.is_empty())
        .take(MAX_FOLLOW_UP_QUESTIONS)
        .collect())
}

#[allow(clippy::too_many_arguments)]
fn build_spec_markdown(
    root: &Path,
    mode: SpecMode,
    request: &str,
    follow_ups: &[FollowUpResponse],
    existing_spec: Option<&str>,
    agent: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
) -> Result<String> {
    let output = run_agent_capture(&RunAgentArgs {
        root: Some(root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_BACKLOG_SPEC.to_string()),
        agent,
        prompt: render_spec_prompt(root, mode, request, follow_ups, existing_spec)?,
        instructions: Some(SPEC_INSTRUCTIONS.to_string()),
        model,
        reasoning,
        transport: None,
        attachments: Vec::new(),
    })?;
    let markdown = normalize_markdown_response(&output.stdout);
    ensure_required_headings(&markdown)?;
    Ok(markdown)
}

fn render_question_prompt(root: &Path, mode: SpecMode, request: &str) -> Result<String> {
    let workflow_contract = load_workflow_contract(root)?;
    let context = load_context_bundle(root)?;
    let existing_spec = read_optional_spec(&PlanningPaths::new(root).spec_path())?
        .unwrap_or_else(|| "_No existing `.metastack/SPEC.md` is present._".to_string());
    Ok(format!(
        "You are preparing a staged SPEC interview for the active repository.\n\n\
Mode: {}\n\n\
Injected workflow contract:\n{}\n\n\
SPEC authoring contract:\n{}\n\n\
User request:\n{}\n\n\
Existing SPEC:\n{}\n\n\
Repository context:\n{}\n\n\
Ask at most {} concise follow-up questions that would materially improve the repository-local SPEC. Return JSON only using this exact shape:\n{{\"questions\":[\"Question 1\",\"Question 2\"]}}",
        mode.title(),
        workflow_contract,
        SPEC_INSTRUCTIONS,
        request,
        existing_spec,
        context,
        MAX_FOLLOW_UP_QUESTIONS,
    ))
}

fn render_spec_prompt(
    root: &Path,
    mode: SpecMode,
    request: &str,
    follow_ups: &[FollowUpResponse],
    existing_spec: Option<&str>,
) -> Result<String> {
    let workflow_contract = load_workflow_contract(root)?;
    let context = load_context_bundle(root)?;
    let repository_snapshot = render_repository_snapshot(root)?;
    let follow_up_block = render_follow_up_block(follow_ups);
    let existing_spec_block =
        existing_spec.unwrap_or("_No existing `.metastack/SPEC.md` is present._");
    Ok(format!(
        "You are writing `.metastack/SPEC.md` for the active repository.\n\n\
Mode: {}\n\n\
Injected workflow contract:\n{}\n\n\
SPEC authoring contract:\n{}\n\n\
Primary request:\n{}\n\n\
Follow-up answers:\n{}\n\n\
Existing SPEC content:\n{}\n\n\
Repository context bundle:\n{}\n\n\
Repository snapshot:\n{}\n\n\
Return the complete markdown document for `.metastack/SPEC.md` only.",
        mode.title(),
        workflow_contract,
        SPEC_INSTRUCTIONS,
        request,
        follow_up_block,
        existing_spec_block,
        context,
        repository_snapshot,
    ))
}

fn render_follow_up_block(follow_ups: &[FollowUpResponse]) -> String {
    if follow_ups.is_empty() {
        return "_No follow-up answers were provided._".to_string();
    }

    follow_ups
        .iter()
        .map(|follow_up| format!("- {}: {}", follow_up.question, follow_up.answer))
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_noninteractive_follow_ups(answers: &[String]) -> Vec<FollowUpResponse> {
    answers
        .iter()
        .enumerate()
        .map(|(index, answer)| FollowUpResponse {
            question: format!("Additional context {}", index + 1),
            answer: answer.trim().to_string(),
        })
        .filter(|follow_up| !follow_up.answer.is_empty())
        .collect()
}

fn collect_follow_up_answers(questions: &[QuestionAnswer]) -> Result<Vec<FollowUpResponse>> {
    let follow_ups = questions
        .iter()
        .map(|question| FollowUpResponse {
            question: question.question.clone(),
            answer: question.answer.value().trim().to_string(),
        })
        .collect::<Vec<_>>();

    if follow_ups
        .iter()
        .any(|follow_up| follow_up.answer.is_empty())
    {
        bail!("Answer each follow-up question before generating the SPEC");
    }

    Ok(follow_ups)
}

fn read_optional_spec(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read `{}`", path.display())),
    }
}

fn persist_spec(path: &Path, contents: &str) -> Result<()> {
    let _ = write_text_file(path, contents, true)?;
    Ok(())
}

fn normalize_markdown_response(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(stripped) = strip_code_fence(trimmed) {
        stripped.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

fn strip_code_fence(raw: &str) -> Option<String> {
    if !raw.starts_with("```") {
        return None;
    }
    let mut lines = raw.lines();
    let _ = lines.next()?;
    let body = lines.collect::<Vec<_>>();
    let end = body
        .iter()
        .rposition(|line| line.trim_start().starts_with("```"))?;
    Some(body[..end].join("\n"))
}

fn parse_agent_json<T>(raw: &str, phase: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let trimmed = raw.trim();
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
        "agent returned invalid JSON during {phase}: {}",
        preview_text(trimmed)
    )
}

fn preview_text(raw: &str) -> String {
    let raw = raw.replace('\n', "\\n");
    if raw.len() > 180 {
        format!("{}...", &raw[..180])
    } else {
        raw
    }
}

fn ensure_required_headings(markdown: &str) -> Result<()> {
    for heading in ["OVERVIEW", "GOALS", "FEATURES", "NON-GOALS"] {
        let found = markdown
            .lines()
            .any(|line| line.trim_start_matches('#').trim() == heading);
        if !found {
            bail!("generated SPEC is missing the required `{heading}` heading");
        }
    }
    Ok(())
}

fn load_context_bundle(root: &Path) -> Result<String> {
    let paths = PlanningPaths::new(root);
    let sections = [
        ("SCAN.md", paths.scan_path()),
        ("ARCHITECTURE.md", paths.architecture_path()),
        ("CONCERNS.md", paths.concerns_path()),
        ("CONVENTIONS.md", paths.conventions_path()),
        ("INTEGRATIONS.md", paths.integrations_path()),
        ("STACK.md", paths.stack_path()),
        ("STRUCTURE.md", paths.structure_path()),
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
            "_Missing `{}`. Run `meta context scan --root .` to generate it._",
            path.file_name()
                .map(|value| value.to_string_lossy())
                .unwrap_or_default()
        )),
        Err(error) => Err(error).with_context(|| format!("failed to read `{}`", path.display())),
    }
}

fn render_repository_snapshot(root: &Path) -> Result<String> {
    let mut lines = Vec::new();
    let mut remaining = 60usize;
    collect_directory_snapshot(root, root, 0, 2, &mut remaining, &mut lines)?;
    if lines.is_empty() {
        Ok("_Repository snapshot is empty._".to_string())
    } else {
        Ok(lines.join("\n"))
    }
}

fn collect_directory_snapshot(
    root: &Path,
    current: &Path,
    depth: usize,
    max_depth: usize,
    remaining: &mut usize,
    lines: &mut Vec<String>,
) -> Result<()> {
    if *remaining == 0 || depth > max_depth {
        return Ok(());
    }

    let mut entries = fs::read_dir(current)
        .with_context(|| format!("failed to read directory `{}`", current.display()))?
        .filter_map(|entry| entry.ok())
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        if *remaining == 0 {
            break;
        }
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if matches!(file_name.as_ref(), ".git" | "target" | "node_modules") {
            continue;
        }

        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect `{}`", path.display()))?;
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let indent = "  ".repeat(depth);
        lines.push(if file_type.is_dir() {
            format!("{indent}- {}/", relative.display())
        } else {
            format!("{indent}- {}", relative.display())
        });
        *remaining = remaining.saturating_sub(1);

        if file_type.is_dir() {
            collect_directory_snapshot(root, &path, depth + 1, max_depth, remaining, lines)?;
        }
    }

    Ok(())
}

fn advance_loading_spinner(app: &mut BacklogSpecApp) {
    if let SpecStage::Loading(loading) = &mut app.stage {
        loading.spinner_index = (loading.spinner_index + 1) % SPINNER_FRAMES.len();
    }
}

fn render_frame(frame: &mut Frame<'_>, app: &BacklogSpecApp) {
    match &app.stage {
        SpecStage::Request(request) => render_request_frame(frame, request),
        SpecStage::Questions(questions) => render_questions_frame(frame, questions),
        SpecStage::Loading(loading) => render_loading_frame(frame, loading),
        SpecStage::Review(review) => render_review_frame(frame, review, &app.spec_path),
    }
}

fn render_request_frame(frame: &mut Frame<'_>, app: &RequestApp) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(0),
            Constraint::Length(4),
        ])
        .split(frame.area());
    let header = Paragraph::new(Text::from(vec![
        Line::from(vec![
            badge("spec", Tone::Accent),
            " Repo-local SPEC lifecycle".into(),
        ]),
        Line::from(app.mode.request_help()),
        key_hints(&[
            ("Enter", "continue"),
            ("Shift+Enter", "newline"),
            ("Esc", "cancel"),
        ]),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(panel_title("meta backlog spec", false)),
    );
    frame.render_widget(header, layout[0]);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(app.mode.request_title())
        .border_style(Style::default().add_modifier(Modifier::BOLD));
    let inner = block.inner(layout[1]);
    let rendered = app.request.render_with_viewport(
        app.mode.request_placeholder(),
        true,
        inner.width.max(1),
        inner.height.max(1),
    );
    frame.render_widget(block, layout[1]);
    frame.render_widget(
        Paragraph::new(rendered.text)
            .wrap(Wrap { trim: false })
            .scroll((rendered.scroll_offset, 0)),
        inner,
    );

    render_footer(frame, app.error.as_deref(), layout[2]);
}

fn render_questions_frame(frame: &mut Frame<'_>, app: &QuestionsApp) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(0),
            Constraint::Length(4),
        ])
        .split(frame.area());
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(38), Constraint::Min(0)])
        .split(layout[1]);

    let header = Paragraph::new(Text::from(vec![
        Line::from(vec![
            badge("spec", Tone::Accent),
            format!(" {} follow-up interview", app.mode.title()).into(),
        ]),
        Line::from(format!("Request: {}", app.request)),
        key_hints(&[
            ("Up/Down", "select question"),
            ("Enter", "generate"),
            ("Esc", "back"),
        ]),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(panel_title("meta backlog spec", false)),
    );
    frame.render_widget(header, layout[0]);

    let mut list_state = ListState::default();
    list_state.select(Some(app.selected));
    let items = app
        .questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let summary = if question.answer.value().trim().is_empty() {
                "pending".to_string()
            } else {
                "answered".to_string()
            };
            ListItem::new(format!("{}. {} ({summary})", index + 1, question.question))
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Questions"))
        .highlight_symbol("> ")
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(list, body[0], &mut list_state);

    let answer = app.questions.get(app.selected);
    let answer_block = Block::default()
        .borders(Borders::ALL)
        .title("Answer [editing]")
        .border_style(Style::default().add_modifier(Modifier::BOLD));
    let inner = answer_block.inner(body[1]);
    frame.render_widget(answer_block, body[1]);
    if let Some(answer) = answer {
        let rendered = answer.answer.render_with_viewport(
            "Type the answer...",
            true,
            inner.width.max(1),
            inner.height.max(1),
        );
        frame.render_widget(
            Paragraph::new(Text::from(vec![
                Line::raw(answer.question.clone()),
                Line::raw(""),
                Line::raw(rendered.text.to_string()),
            ]))
            .wrap(Wrap { trim: false })
            .scroll((rendered.scroll_offset, 0)),
            inner,
        );
    }

    render_footer(frame, app.error.as_deref(), layout[2]);
}

fn render_loading_frame(frame: &mut Frame<'_>, app: &LoadingApp) {
    render_loading_panel(
        frame,
        frame.area(),
        &LoadingPanelData {
            title: "meta backlog spec".to_string(),
            message: app.mode.loading_message(app.phase).to_string(),
            detail: app.detail.clone(),
            spinner_index: app.spinner_index,
            status_line: "Waiting on the local SPEC worker...".to_string(),
        },
    );
}

fn render_review_frame(frame: &mut Frame<'_>, app: &ReviewApp, spec_path: &Path) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(0),
            Constraint::Length(4),
        ])
        .split(frame.area());
    let header = Paragraph::new(Text::from(vec![
        Line::from(vec![
            badge("spec", Tone::Accent),
            format!(" {} preview", app.mode.title()).into(),
        ]),
        Line::from(format!("Target: {}", spec_path.display())),
        key_hints(&[
            ("Enter", "save"),
            ("Esc", "revise"),
            ("mouse wheel", "scroll preview"),
        ]),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(panel_title("meta backlog spec", false)),
    );
    frame.render_widget(header, layout[0]);

    frame.render_widget(
        scrollable_content_paragraph(
            app.spec_markdown.clone(),
            "SPEC Preview",
            &app.preview_scroll,
        ),
        layout[1],
    );
    render_footer(frame, app.error.as_deref(), layout[2]);
}

fn render_footer(frame: &mut Frame<'_>, error: Option<&str>, area: Rect) {
    let text =
        error.unwrap_or("The SPEC flow stays repo-local and only targets `.metastack/SPEC.md`.");
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("Status"))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn review_viewport(area: Rect) -> Rect {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(0),
            Constraint::Length(4),
        ])
        .split(area);
    layout[1]
}

fn snapshot(backend: &TestBackend) -> String {
    let width = backend.size().map(|size| size.width as usize).unwrap_or(1);
    backend
        .buffer()
        .content
        .chunks(width)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;

    fn render_request_snapshot(app: &RequestApp) -> String {
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");
        terminal
            .draw(|frame| render_request_frame(frame, app))
            .expect("request frame should render");
        snapshot(terminal.backend())
    }

    fn render_loading_snapshot(app: &LoadingApp) -> String {
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");
        terminal
            .draw(|frame| render_loading_frame(frame, app))
            .expect("loading frame should render");
        snapshot(terminal.backend())
    }

    fn render_review_snapshot(app: &ReviewApp) -> String {
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");
        terminal
            .draw(|frame| render_review_frame(frame, app, Path::new(".metastack/SPEC.md")))
            .expect("review frame should render");
        snapshot(terminal.backend())
    }

    #[test]
    fn request_snapshot_uses_create_prompt() {
        let snapshot = render_request_snapshot(&RequestApp {
            mode: SpecMode::Create,
            request: InputFieldState::multiline("Add a repo-local feature spec workflow"),
            error: None,
        });
        assert!(snapshot.contains("What should this repository build?"));
        assert!(snapshot.contains("Repo-local SPEC lifecycle"));
    }

    #[test]
    fn loading_snapshot_mentions_repo_local_worker() {
        let snapshot = render_loading_snapshot(&LoadingApp {
            mode: SpecMode::Improve,
            phase: PendingKind::Generate,
            detail: "Preparing `.metastack/SPEC.md` without touching Linear.".to_string(),
            spinner_index: 1,
        });
        assert!(snapshot.contains("Revising repo-local SPEC"));
        assert!(snapshot.contains("local SPEC worker"));
    }

    #[test]
    fn review_snapshot_shows_required_headings() {
        let snapshot = render_review_snapshot(&ReviewApp {
            mode: SpecMode::Create,
            request: "Add a repo-local spec flow".to_string(),
            follow_ups: Vec::new(),
            spec_markdown: "# OVERVIEW\n\n## GOALS\n\n## FEATURES\n\n## NON-GOALS".to_string(),
            preview_scroll: ScrollState::default(),
            error: None,
        });
        assert!(snapshot.contains("SPEC Preview"));
        assert!(snapshot.contains("NON-GOALS"));
    }

    #[test]
    fn heading_validation_requires_uppercase_sections() {
        assert!(
            ensure_required_headings("# OVERVIEW\n\n## GOALS\n\n## FEATURES\n\n## NON-GOALS")
                .is_ok()
        );
        assert!(ensure_required_headings("# Overview\n\n## GOALS").is_err());
    }
}
