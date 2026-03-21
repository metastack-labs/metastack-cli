use std::io::IsTerminal;
use std::io::{self};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::{Backend, CrosstermBackend, TestBackend};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::config::{
    AppConfig, DEFAULT_LINEAR_API_URL, ListenAssignmentScope, ListenRefreshPolicy,
    validate_interactive_plan_follow_up_question_limit, validate_listen_poll_interval_seconds,
};
use crate::linear::{
    LinearClient, LinearService, ProjectSummary, ReqwestLinearClient, TeamSummary, UserRef,
};
use crate::progress::{LoadingPanelData, SPINNER_FRAMES, render_loading_panel};
use crate::tui::fields::{
    FilterableSelectFieldState, InputFieldRender, InputFieldState, SelectFieldState,
};
use crate::tui::scroll::{ScrollState, plain_text, scrollable_paragraph_with_block, wrapped_rows};
use crate::tui::theme::{Tone, badge, emphasis_style, label_style, tone_style};

const PROJECT_FETCH_LIMIT: usize = 100;

/// Entry mode for the shared onboarding wizard.
#[derive(Debug, Clone)]
pub enum OnboardingLaunchMode {
    Intercepted { command_label: String },
    Replay,
}

/// Options controlling how onboarding should run.
#[derive(Debug, Clone)]
pub struct OnboardingOptions {
    pub mode: OnboardingLaunchMode,
    pub render_once: bool,
    pub width: u16,
    pub height: u16,
}

/// Result returned by the onboarding launcher.
#[derive(Debug, Clone)]
pub enum OnboardingResult {
    Completed,
    Rendered(String),
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnboardingStep {
    Welcome,
    ApiKey,
    Team,
    Project,
    ListenLabel,
    AssignmentScope,
    RefreshPolicy,
    PollInterval,
    PlanFollowUpLimit,
    PlanLabel,
    TechnicalLabel,
    Review,
}

impl OnboardingStep {
    fn all() -> [Self; 12] {
        [
            Self::Welcome,
            Self::ApiKey,
            Self::Team,
            Self::Project,
            Self::ListenLabel,
            Self::AssignmentScope,
            Self::RefreshPolicy,
            Self::PollInterval,
            Self::PlanFollowUpLimit,
            Self::PlanLabel,
            Self::TechnicalLabel,
            Self::Review,
        ]
    }

    fn index(self) -> usize {
        Self::all()
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0)
    }

    fn next(self) -> Self {
        let index = (self.index() + 1).min(Self::all().len() - 1);
        Self::all()[index]
    }

    fn previous(self) -> Self {
        let index = self.index().saturating_sub(1);
        Self::all()[index]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Welcome => "Welcome",
            Self::ApiKey => "Linear key",
            Self::Team => "Default team",
            Self::Project => "Default project",
            Self::ListenLabel => "Listen label",
            Self::AssignmentScope => "Assignee scope",
            Self::RefreshPolicy => "Refresh policy",
            Self::PollInterval => "Poll interval",
            Self::PlanFollowUpLimit => "Plan follow-ups",
            Self::PlanLabel => "Plan label",
            Self::TechnicalLabel => "Tech label",
            Self::Review => "Save",
        }
    }
}

#[derive(Debug, Clone)]
enum ValidationState {
    Idle,
    Loading { spinner_index: usize },
    Failed(String),
    Succeeded { viewer: UserRef },
}

#[derive(Debug, Clone)]
struct LoadedCatalog {
    viewer: UserRef,
    teams: Vec<TeamSummary>,
    projects: Vec<ProjectSummary>,
}

struct PendingCatalogJob {
    receiver: Receiver<Result<LoadedCatalog>>,
}

#[derive(Debug, Clone)]
struct OnboardingApp {
    mode: OnboardingLaunchMode,
    step: OnboardingStep,
    api_key: InputFieldState,
    listen_label: InputFieldState,
    poll_interval: InputFieldState,
    plan_follow_up_limit: InputFieldState,
    plan_label: InputFieldState,
    technical_label: InputFieldState,
    assignment_scope: SelectFieldState,
    refresh_policy: SelectFieldState,
    team: SelectFieldState,
    project: FilterableSelectFieldState,
    team_ids: Vec<String>,
    project_ids: Vec<String>,
    all_projects: Vec<ProjectSummary>,
    validation_state: ValidationState,
    review_scroll: ScrollState,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct OnboardingSubmission {
    api_key: String,
    team_key: String,
    project_id: Option<String>,
    listen_label: Option<String>,
    assignment_scope: ListenAssignmentScope,
    refresh_policy: ListenRefreshPolicy,
    poll_interval_seconds: Option<u64>,
    interactive_follow_up_questions: Option<usize>,
    plan_label: Option<String>,
    technical_label: Option<String>,
}

enum DashboardExit {
    Cancelled,
    Submitted(OnboardingSubmission),
}

/// Runs the install-scoped onboarding wizard.
///
/// Returns an error when config loading or saving fails, when Linear validation fails during a
/// required save, or when the terminal UI cannot be initialized.
pub async fn run_onboarding(options: OnboardingOptions) -> Result<OnboardingResult> {
    let app_config = AppConfig::load()?;
    let can_launch_tui = io::stdin().is_terminal() && io::stdout().is_terminal();

    if options.render_once || !can_launch_tui {
        return Ok(OnboardingResult::Rendered(render_once(
            OnboardingApp::new(&app_config, options.mode),
            options.width,
            options.height,
        )?));
    }

    match run_dashboard(OnboardingApp::new(&app_config, options.mode), &app_config)? {
        DashboardExit::Cancelled => Ok(OnboardingResult::Cancelled),
        DashboardExit::Submitted(submitted) => {
            save_submission(&mut AppConfig::load()?, submitted).await?;
            Ok(OnboardingResult::Completed)
        }
    }
}

impl OnboardingApp {
    fn new(app_config: &AppConfig, mode: OnboardingLaunchMode) -> Self {
        let selected_team = app_config.linear.team.clone().unwrap_or_default();
        let assignment_scope_options = vec![
            "Any eligible issue".to_string(),
            "Viewer-assigned plus unassigned".to_string(),
        ];
        let refresh_options = vec![
            "Reuse workspace and refresh from origin/main".to_string(),
            "Recreate workspace from origin/main".to_string(),
        ];
        let mut app = Self {
            mode,
            step: OnboardingStep::Welcome,
            api_key: InputFieldState::new(app_config.linear.api_key.clone().unwrap_or_default()),
            listen_label: InputFieldState::new(
                app_config
                    .defaults
                    .listen
                    .required_label
                    .clone()
                    .unwrap_or_default(),
            ),
            poll_interval: InputFieldState::new(
                app_config
                    .defaults
                    .listen
                    .poll_interval_seconds
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            plan_follow_up_limit: InputFieldState::new(
                app_config
                    .defaults
                    .plan
                    .interactive_follow_up_questions
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            plan_label: InputFieldState::new(
                app_config
                    .defaults
                    .issue_labels
                    .plan
                    .clone()
                    .unwrap_or_default(),
            ),
            technical_label: InputFieldState::new(
                app_config
                    .defaults
                    .issue_labels
                    .technical
                    .clone()
                    .unwrap_or_default(),
            ),
            assignment_scope: SelectFieldState::new(
                assignment_scope_options,
                match app_config
                    .defaults
                    .listen
                    .assignment_scope
                    .unwrap_or_default()
                {
                    ListenAssignmentScope::Any => 0,
                    ListenAssignmentScope::ViewerOnly => 1,
                    ListenAssignmentScope::ViewerOrUnassigned => 1,
                },
            ),
            refresh_policy: SelectFieldState::new(
                refresh_options,
                match app_config
                    .defaults
                    .listen
                    .refresh_policy
                    .unwrap_or_default()
                {
                    ListenRefreshPolicy::ReuseAndRefresh => 0,
                    ListenRefreshPolicy::RecreateFromOriginMain => 1,
                },
            ),
            team: SelectFieldState::new(vec!["Validate Linear auth first".to_string()], 0),
            project: FilterableSelectFieldState::new(vec!["Choose a team first".to_string()]),
            team_ids: Vec::new(),
            project_ids: Vec::new(),
            all_projects: Vec::new(),
            validation_state: ValidationState::Idle,
            review_scroll: ScrollState::default(),
            error: None,
        };
        if !selected_team.is_empty() {
            app.team = SelectFieldState::new(vec![selected_team], 0);
        }
        app
    }

    fn handle_key(&mut self, key: KeyEvent, review_viewport: Rect) -> bool {
        if self.step == OnboardingStep::Review
            && matches!(
                key.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End
            )
        {
            let _ = self.review_scroll.apply_key_code_in_viewport(
                key.code,
                review_viewport,
                self.review_content_rows(review_viewport.width),
            );
            return true;
        }
        match self.step {
            OnboardingStep::Welcome => false,
            OnboardingStep::ApiKey => self.api_key.handle_key(key),
            OnboardingStep::Team => self.team.handle_key(key),
            OnboardingStep::Project => self.project.handle_key(key),
            OnboardingStep::ListenLabel => self.listen_label.handle_key(key),
            OnboardingStep::AssignmentScope => self.assignment_scope.handle_key(key),
            OnboardingStep::RefreshPolicy => self.refresh_policy.handle_key(key),
            OnboardingStep::PollInterval => self.poll_interval.handle_key(key),
            OnboardingStep::PlanFollowUpLimit => self.plan_follow_up_limit.handle_key(key),
            OnboardingStep::PlanLabel => self.plan_label.handle_key(key),
            OnboardingStep::TechnicalLabel => self.technical_label.handle_key(key),
            OnboardingStep::Review => false,
        }
    }

    fn handle_review_mouse(&mut self, mouse: MouseEvent, review_viewport: Rect) -> bool {
        if self.step != OnboardingStep::Review {
            return false;
        }
        self.review_scroll.apply_mouse_in_viewport(
            mouse,
            review_viewport,
            self.review_content_rows(review_viewport.width),
        )
    }

    fn review_text(&self) -> Text<'static> {
        Text::from(review_lines(self))
    }

    fn review_content_rows(&self, width: u16) -> usize {
        wrapped_rows(&plain_text(&self.review_text()), width.max(1))
    }

    fn sync_project_options(&mut self) {
        let Some(team_id) = self.team_ids.get(self.team.selected()).cloned() else {
            self.project = FilterableSelectFieldState::new(vec!["Choose a team first".to_string()]);
            self.project_ids.clear();
            return;
        };

        let mut project_rows = Vec::new();
        let mut project_ids = Vec::new();
        for project in &self.all_projects {
            if project.teams.iter().any(|team| team.key == team_id) {
                project_rows.push(format!("{} ({})", project.name, project.id));
                project_ids.push(project.id.clone());
            }
        }

        self.project = FilterableSelectFieldState::new(project_rows);
        self.project_ids = project_ids;
    }

    fn spinner_tick(&mut self) {
        if let ValidationState::Loading { spinner_index } = &mut self.validation_state {
            *spinner_index = spinner_index.wrapping_add(1);
        }
    }

    fn submission(&self) -> Result<OnboardingSubmission> {
        let api_key = normalize_required(self.api_key.value(), "Linear API key")?;
        let team_key = self
            .team_ids
            .get(self.team.selected())
            .cloned()
            .ok_or_else(|| anyhow!("select one Linear team before saving"))?;
        let project_id = self
            .project
            .selected_original_index()
            .and_then(|i| self.project_ids.get(i).cloned());
        let poll_interval_seconds = parse_optional_u64(
            self.poll_interval.value(),
            "listen poll interval",
            validate_listen_poll_interval_seconds,
        )?;
        let interactive_follow_up_questions = parse_optional_usize(
            self.plan_follow_up_limit.value(),
            "interactive plan follow-up question limit",
            validate_interactive_plan_follow_up_question_limit,
        )?;

        if !matches!(self.validation_state, ValidationState::Succeeded { .. }) {
            bail!("Linear authentication must succeed before onboarding can finish");
        }

        Ok(OnboardingSubmission {
            api_key,
            team_key,
            project_id,
            listen_label: normalize_optional(self.listen_label.value()),
            assignment_scope: match self.assignment_scope.selected() {
                1 => ListenAssignmentScope::ViewerOrUnassigned,
                _ => ListenAssignmentScope::Any,
            },
            refresh_policy: match self.refresh_policy.selected() {
                1 => ListenRefreshPolicy::RecreateFromOriginMain,
                _ => ListenRefreshPolicy::ReuseAndRefresh,
            },
            poll_interval_seconds,
            interactive_follow_up_questions,
            plan_label: normalize_optional(self.plan_label.value()),
            technical_label: normalize_optional(self.technical_label.value()),
        })
    }
}

fn run_dashboard(mut app: OnboardingApp, app_config: &AppConfig) -> Result<DashboardExit> {
    let mut pending_job: Option<PendingCatalogJob> = None;
    let mut stdout = io::stdout();
    enable_raw_mode().context("failed to enable raw mode for onboarding")?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("failed to enter alternate screen")?;
    let _cleanup = OnboardingTerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend).context("failed to initialize onboarding terminal")?;

    loop {
        if let Some(job) = &pending_job {
            match job.receiver.try_recv() {
                Ok(result) => {
                    pending_job = None;
                    match result {
                        Ok(catalog) => {
                            let viewer = catalog.viewer.clone();
                            app.validation_state = ValidationState::Succeeded { viewer };
                            app.team = SelectFieldState::new(
                                catalog
                                    .teams
                                    .iter()
                                    .map(|team| format!("{} ({})", team.name, team.key))
                                    .collect(),
                                0,
                            );
                            app.team_ids =
                                catalog.teams.iter().map(|team| team.key.clone()).collect();
                            app.all_projects = catalog.projects;
                            app.sync_project_options();
                            app.error = None;
                            app.step = OnboardingStep::Team;
                        }
                        Err(error) => {
                            app.validation_state = ValidationState::Failed(format!("{error:#}"));
                            app.error = Some("Linear validation failed.".to_string());
                            app.step = OnboardingStep::ApiKey;
                        }
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    pending_job = None;
                    app.validation_state =
                        ValidationState::Failed("validation worker disconnected".to_string());
                    app.step = OnboardingStep::ApiKey;
                }
                Err(TryRecvError::Empty) => {
                    app.spinner_tick();
                }
            }
        }

        terminal.draw(|frame| render_onboarding(frame, &app))?;

        if pending_job.is_some() {
            if event::poll(Duration::from_millis(90))? {
                let _ = event::read()?;
            }
            continue;
        }

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let read = event::read()?;
        let review_viewport = review_viewport(terminal.size()?.into());
        let Event::Key(key) = read else {
            if let Event::Mouse(mouse) = read
                && matches!(
                    mouse.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                )
            {
                let _ = app.handle_review_mouse(mouse, review_viewport);
            }
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if matches!(key.code, KeyCode::Esc) {
            return Ok(DashboardExit::Cancelled);
        }

        app.error = None;
        if matches!(key.code, KeyCode::BackTab) {
            app.step = app.step.previous();
            continue;
        }

        let handled = app.handle_key(key, review_viewport);
        if handled {
            if app.step == OnboardingStep::Team {
                app.sync_project_options();
            }
            continue;
        }

        if matches!(key.code, KeyCode::Tab | KeyCode::Enter) {
            match app.step {
                OnboardingStep::Welcome => {
                    app.step = OnboardingStep::ApiKey;
                }
                OnboardingStep::ApiKey => {
                    let api_key = match normalize_required(app.api_key.value(), "Linear API key") {
                        Ok(value) => value,
                        Err(error) => {
                            app.error = Some(error.to_string());
                            continue;
                        }
                    };
                    app.validation_state = ValidationState::Loading { spinner_index: 0 };
                    pending_job = Some(spawn_catalog_job(
                        api_key,
                        app_config.linear.api_url.clone(),
                    ));
                }
                OnboardingStep::Team => {
                    if app.team_ids.is_empty() {
                        app.error = Some("Select a validated Linear team first.".to_string());
                    } else {
                        app.sync_project_options();
                        app.step = OnboardingStep::Project;
                    }
                }
                OnboardingStep::Project => {
                    app.step = app.step.next();
                }
                OnboardingStep::Review => {
                    return Ok(DashboardExit::Submitted(app.submission()?));
                }
                _ => {
                    app.step = app.step.next();
                }
            }
        }
    }
}

fn spawn_catalog_job(api_key: String, api_url: String) -> PendingCatalogJob {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = (|| -> Result<LoadedCatalog> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("failed to create onboarding runtime")?;
            runtime.block_on(load_catalog(api_key, api_url))
        })();
        let _ = sender.send(result);
    });
    PendingCatalogJob { receiver }
}

async fn load_catalog(api_key: String, api_url: String) -> Result<LoadedCatalog> {
    let client = ReqwestLinearClient::new(crate::config::LinearConfig {
        api_key,
        api_url,
        default_team: None,
    })?;
    let teams = client.list_teams().await?;
    let service = LinearService::new(client, None);
    let viewer = service.viewer().await?;
    let projects = service
        .list_projects(crate::linear::ProjectListFilters {
            team: None,
            limit: PROJECT_FETCH_LIMIT,
        })
        .await?;
    if teams.is_empty() {
        bail!("Linear returned no teams for this account");
    }
    if projects.is_empty() {
        bail!("Linear returned no visible projects for this account");
    }
    Ok(LoadedCatalog {
        viewer,
        teams,
        projects,
    })
}

async fn save_submission(config: &mut AppConfig, submitted: OnboardingSubmission) -> Result<()> {
    config.linear.api_key = Some(submitted.api_key.clone());
    config.linear.api_url = normalize_optional(&config.linear.api_url)
        .unwrap_or_else(|| DEFAULT_LINEAR_API_URL.to_string());
    config.linear.team = Some(submitted.team_key.clone());
    config.defaults.linear.project_id = submitted.project_id.clone();
    config.defaults.listen.required_label = submitted.listen_label.clone();
    config.defaults.listen.assignment_scope = Some(submitted.assignment_scope);
    config.defaults.listen.refresh_policy = Some(submitted.refresh_policy);
    config.defaults.listen.poll_interval_seconds = submitted.poll_interval_seconds;
    config.defaults.plan.interactive_follow_up_questions =
        submitted.interactive_follow_up_questions;
    config.defaults.issue_labels.plan = submitted.plan_label.clone();
    config.defaults.issue_labels.technical = submitted.technical_label.clone();
    config.mark_onboarding_complete();

    let client = ReqwestLinearClient::new(crate::config::LinearConfig {
        api_key: submitted.api_key,
        api_url: config.linear.api_url.clone(),
        default_team: Some(submitted.team_key.clone()),
    })?;
    let service = LinearService::new(client, Some(submitted.team_key));
    let mut labels = vec![
        submitted
            .plan_label
            .clone()
            .unwrap_or_else(|| "plan".to_string()),
        submitted
            .technical_label
            .clone()
            .unwrap_or_else(|| "technical".to_string()),
    ];
    if let Some(listen_label) = submitted.listen_label {
        labels.push(listen_label);
    }
    service.ensure_issue_labels_exist(None, &labels).await?;

    config.save()?;
    Ok(())
}

fn render_once(app: OnboardingApp, width: u16, height: u16) -> Result<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| render_onboarding(frame, &app))?;
    Ok(snapshot(terminal.backend()))
}

fn render_onboarding(frame: &mut Frame<'_>, app: &OnboardingApp) {
    let area = frame.area();
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(16),
            Constraint::Length(4),
        ])
        .split(area);
    render_header(frame, app, vertical[0]);

    let main = if vertical[1].width < 96 {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(vertical[1])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
            .split(vertical[1])
    };
    render_left_panel(frame, app, main[0]);
    render_right_panel(frame, app, main[1]);
    render_footer(frame, app, vertical[2]);
}

fn render_header(frame: &mut Frame<'_>, app: &OnboardingApp, area: Rect) {
    let title = match &app.mode {
        OnboardingLaunchMode::Intercepted { command_label } => {
            format!("MetaStack first run before `{command_label}`")
        }
        OnboardingLaunchMode::Replay => "MetaStack onboarding replay".to_string(),
    };
    let progress = format!(
        "Step {} / {}",
        app.step.index() + 1,
        OnboardingStep::all().len()
    );
    let lines = vec![Line::from(vec![
        Span::styled(title, emphasis_style()),
        Span::raw(" "),
        badge(progress, Tone::Accent),
    ])];
    let paragraph = Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::ALL).title("MetaStack"))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_left_panel(frame: &mut Frame<'_>, app: &OnboardingApp, area: Rect) {
    let title = format!("Guide • {}", app.step.label());
    let mut lines = Vec::new();
    lines.extend(step_copy(app));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .padding(Padding::new(1, 1, 1, 0));
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn step_copy(app: &OnboardingApp) -> Vec<Line<'static>> {
    match app.step {
        OnboardingStep::Welcome => vec![
            Line::from(Span::styled("MetaStack", emphasis_style())),
            Line::from(
                "Linear-native planning and agent automation CLI for repo-scoped workflows.",
            ),
            Line::from(""),
            Line::from(Span::styled("What it does:", label_style())),
            Line::from("  Repo-scoped planning synced to Linear"),
            Line::from("  Automated agent supervision via `meta listen`"),
            Line::from("  Workflow automation with labels, teams, and projects"),
            Line::from(""),
            Line::from(
                "This wizard configures install-scoped defaults shared across all repositories.",
            ),
            Line::from("Repo-level overrides live in `.metastack/meta.json`."),
        ],
        OnboardingStep::ApiKey => vec![
            Line::from("Paste a personal or workspace Linear API key."),
            Line::from(""),
            Line::from(
                "On Enter, MetaStack will validate the key, fetch your viewer identity, and load available teams/projects.",
            ),
            Line::from(""),
            Line::from("Get your key here:"),
            Line::from("https://linear.app/0xintuition/settings/account/security"),
            Line::from(""),
            validation_lines(&app.validation_state),
        ],
        OnboardingStep::Team => vec![
            Line::from("Choose exactly one install default team."),
            Line::from("Repo-scoped `.metastack/meta.json` can still override this later."),
        ],
        OnboardingStep::Project => vec![
            Line::from("Choose an install default project, or press Enter to skip."),
            Line::from(
                "The saved value is the canonical Linear project ID, which matches existing project resolution behavior.",
            ),
        ],
        OnboardingStep::ListenLabel => vec![
            Line::from("This label gates which Todo tickets `meta listen` picks up by default."),
            Line::from(
                "Leave blank to keep the built-in fallback and avoid a global label requirement.",
            ),
        ],
        OnboardingStep::AssignmentScope => vec![
            Line::from(
                "`viewer` keeps listen scoped to work assigned to you plus unassigned issues.",
            ),
            Line::from("`any` watches every eligible ticket for the selected project/team."),
        ],
        OnboardingStep::RefreshPolicy => vec![Line::from(
            "Reuse is faster. Recreate is stricter and useful when clone drift matters more than speed.",
        )],
        OnboardingStep::PollInterval => vec![
            Line::from("This controls how often `meta listen` polls Linear."),
            Line::from("CLI flags still override it for the active run."),
        ],
        OnboardingStep::PlanFollowUpLimit => vec![
            Line::from("Interactive `meta backlog plan` follow-up questions are capped here."),
            Line::from(
                "Set a small limit for tighter loops or a larger one for deeper planning passes.",
            ),
        ],
        OnboardingStep::PlanLabel => vec![
            Line::from("Default label for issues created by `meta backlog plan`."),
            Line::from("Repo defaults still win when `.metastack/meta.json` sets one."),
        ],
        OnboardingStep::TechnicalLabel => vec![
            Line::from("Default label for issues created by `meta backlog tech`."),
            Line::from("MetaStack will ensure the selected team has the chosen labels on save."),
        ],
        OnboardingStep::Review => vec![
            Line::from("Save writes install-scoped config only."),
            Line::from(""),
            Line::from("Precedence after this change:"),
            Line::from("CLI override -> repo default -> install default"),
        ],
    }
}

fn validation_lines(state: &ValidationState) -> Line<'static> {
    match state {
        ValidationState::Idle => Line::from(vec![
            badge("idle", Tone::Muted),
            Span::raw(" Press Enter to validate."),
        ]),
        ValidationState::Loading { spinner_index } => Line::from(vec![
            badge("loading", Tone::Info),
            Span::raw(format!(
                " {} validating key and loading catalog",
                SPINNER_FRAMES[*spinner_index % SPINNER_FRAMES.len()]
            )),
        ]),
        ValidationState::Failed(message) => Line::from(vec![
            badge("failed", Tone::Danger),
            Span::raw(" "),
            Span::styled(message.clone(), tone_style(Tone::Danger)),
        ]),
        ValidationState::Succeeded { viewer } => Line::from(vec![
            badge("verified", Tone::Success),
            Span::raw(format!(" Connected as {}", viewer.name)),
        ]),
    }
}

fn render_right_panel(frame: &mut Frame<'_>, app: &OnboardingApp, area: Rect) {
    if let ValidationState::Loading { spinner_index } = app.validation_state {
        render_loading_panel(
            frame,
            area,
            &LoadingPanelData {
                title: "Validating Linear auth".to_string(),
                message: "Checking token".to_string(),
                detail: "Loading viewer identity, teams, and projects.".to_string(),
                spinner_index,
                status_line:
                    "This blocks completion because onboarding requires live Linear access."
                        .to_string(),
            },
        );
        return;
    }

    match app.step {
        OnboardingStep::Welcome => {
            let paragraph = Paragraph::new(Text::from(vec![
                Line::from(Span::styled("This wizard will configure:", label_style())),
                Line::from(""),
                Line::from("  Linear authentication (API key)"),
                Line::from("  Default team and project"),
                Line::from("  Listen settings (label, scope, refresh, poll)"),
                Line::from("  Planning defaults (follow-ups, labels)"),
                Line::from(""),
                Line::from("Press Enter to begin."),
            ]))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Start")
                    .padding(Padding::new(1, 1, 1, 0)),
            )
            .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
        }
        OnboardingStep::ApiKey => {
            render_input_panel(frame, area, "Linear API key", &app.api_key, "lin_api_...")
        }
        OnboardingStep::Team => render_select_panel(frame, area, "Default team", &app.team),
        OnboardingStep::Project => {
            render_filterable_select_panel(frame, area, "Default project", &app.project)
        }
        OnboardingStep::ListenLabel => render_input_panel(
            frame,
            area,
            "Default listen label",
            &app.listen_label,
            "agent",
        ),
        OnboardingStep::AssignmentScope => {
            render_select_panel(frame, area, "Listen assignee scope", &app.assignment_scope)
        }
        OnboardingStep::RefreshPolicy => {
            render_select_panel(frame, area, "Workspace refresh policy", &app.refresh_policy)
        }
        OnboardingStep::PollInterval => {
            render_input_panel(frame, area, "Listen poll interval", &app.poll_interval, "7")
        }
        OnboardingStep::PlanFollowUpLimit => render_input_panel(
            frame,
            area,
            "Plan follow-up question limit",
            &app.plan_follow_up_limit,
            "10",
        ),
        OnboardingStep::PlanLabel => {
            render_input_panel(frame, area, "Default plan label", &app.plan_label, "plan")
        }
        OnboardingStep::TechnicalLabel => render_input_panel(
            frame,
            area,
            "Default technical label",
            &app.technical_label,
            "technical",
        ),
        OnboardingStep::Review => {
            let summary = scrollable_paragraph_with_block(
                app.review_text(),
                Block::default()
                    .borders(Borders::ALL)
                    .title("Review [scroll]")
                    .border_style(Style::default().add_modifier(Modifier::BOLD))
                    .padding(Padding::new(1, 1, 1, 0)),
                &app.review_scroll,
            )
            .wrap(Wrap { trim: false });
            frame.render_widget(summary, area);
        }
    }
}

fn render_footer(frame: &mut Frame<'_>, app: &OnboardingApp, area: Rect) {
    let controls = match app.step {
        OnboardingStep::Team | OnboardingStep::AssignmentScope | OnboardingStep::RefreshPolicy => {
            "Up/Down move • Enter accepts • Shift+Tab goes back • Esc cancels"
        }
        OnboardingStep::Project => {
            "Type to search • Up/Down move • Enter accepts • Shift+Tab goes back • Esc cancels"
        }
        OnboardingStep::Review => {
            "Up/Down and PgUp/PgDn/Home/End or the mouse wheel scroll • Enter saves install defaults • Shift+Tab goes back • Esc cancels"
        }
        OnboardingStep::Welcome => "Enter starts • Esc cancels",
        _ => "Type or paste • Enter continues • Shift+Tab goes back • Esc cancels",
    };
    let status = app.error.as_deref().unwrap_or(match app.mode {
        OnboardingLaunchMode::Intercepted { .. } => {
            "Complete onboarding once, then your original command continues."
        }
        OnboardingLaunchMode::Replay => "Replay mode only updates install-scoped defaults.",
    });
    let footer = Paragraph::new(Text::from(vec![Line::from(controls), Line::from(status)]))
        .block(Block::default().borders(Borders::ALL).title("Hints"))
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, area);
}

fn review_lines(app: &OnboardingApp) -> Vec<Line<'static>> {
    let viewer = match &app.validation_state {
        ValidationState::Succeeded { viewer } => viewer.name.clone(),
        _ => "not validated".to_string(),
    };
    vec![
        summary_line("Linear viewer", &viewer),
        summary_line("Default team", app.team.selected_label().unwrap_or("unset")),
        summary_line(
            "Default project",
            app.project.selected_label().unwrap_or("unset"),
        ),
        summary_line("Listen label", summarize_input(&app.listen_label, "unset")),
        summary_line(
            "Assignee scope",
            app.assignment_scope.selected_label().unwrap_or("unset"),
        ),
        summary_line(
            "Refresh policy",
            app.refresh_policy.selected_label().unwrap_or("unset"),
        ),
        summary_line("Poll interval", summarize_input(&app.poll_interval, "7")),
        summary_line(
            "Plan follow-ups",
            summarize_input(&app.plan_follow_up_limit, "10"),
        ),
        summary_line("Plan label", summarize_input(&app.plan_label, "plan")),
        summary_line(
            "Technical label",
            summarize_input(&app.technical_label, "technical"),
        ),
    ]
}

fn summary_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), label_style()),
        Span::raw(value.to_string()),
    ])
}

fn summarize_input<'a>(field: &'a InputFieldState, fallback: &'a str) -> &'a str {
    let value = field.value().trim();
    if value.is_empty() { fallback } else { value }
}

fn render_input_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    field: &InputFieldState,
    placeholder: &str,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("{title} [editing]"))
        .border_style(Style::default().add_modifier(Modifier::BOLD))
        .padding(Padding::new(1, 1, 1, 0));
    let inner = block.inner(area);
    let rendered: InputFieldRender = field.render_with_width(placeholder, true, inner.width);
    let paragraph = rendered.paragraph(block);
    frame.render_widget(paragraph, area);
    rendered.set_cursor(frame, inner);
}

fn render_select_panel(frame: &mut Frame<'_>, area: Rect, title: &str, field: &SelectFieldState) {
    let lines = field
        .options()
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let selected = index == field.selected();
            let marker = if selected { "> " } else { "  " };
            let style = if selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(format!("{marker}{option}"), style))
        })
        .collect::<Vec<_>>();
    let list = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .padding(Padding::new(1, 1, 1, 0)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(list, area);
}

fn render_filterable_select_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    field: &FilterableSelectFieldState,
) {
    let filter_value = field.filter_value();
    let filter_line = if filter_value.is_empty() {
        Line::from(Span::styled(
            "Type to filter…",
            Style::default().add_modifier(Modifier::DIM),
        ))
    } else {
        Line::from(vec![
            Span::styled("Filter: ", label_style()),
            Span::raw(filter_value.to_string()),
        ])
    };

    let visible = field.visible_options();
    let mut lines = vec![filter_line, Line::from("")];
    if visible.is_empty() {
        lines.push(Line::from(Span::styled(
            "No matches — clear filter or press Enter to skip",
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else {
        for (index, option) in visible.iter().enumerate() {
            let selected = index == field.cursor_index();
            let marker = if selected { "> " } else { "  " };
            let style = if selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(format!("{marker}{option}"), style)));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("{title} [search]"))
        .border_style(Style::default().add_modifier(Modifier::BOLD))
        .padding(Padding::new(1, 1, 1, 0));
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn snapshot(backend: &TestBackend) -> String {
    let mut lines = Vec::new();
    let size = backend
        .size()
        .expect("test backend size should be available");
    for row in 0..size.height {
        let mut line = String::new();
        for column in 0..size.width {
            if let Some(cell) = backend.buffer().cell((column, row)) {
                line.push_str(cell.symbol());
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

fn normalize_optional(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn normalize_required(value: &str, label: &str) -> Result<String> {
    normalize_optional(value).ok_or_else(|| anyhow!("{label} is required"))
}

fn parse_optional_u64(
    value: &str,
    label: &str,
    validate: impl Fn(u64) -> Result<()>,
) -> Result<Option<u64>> {
    let Some(value) = normalize_optional(value) else {
        return Ok(None);
    };
    let parsed = value
        .parse::<u64>()
        .with_context(|| format!("{label} must be a whole number"))?;
    validate(parsed)?;
    Ok(Some(parsed))
}

fn parse_optional_usize(
    value: &str,
    label: &str,
    validate: impl Fn(usize) -> Result<()>,
) -> Result<Option<usize>> {
    let Some(value) = normalize_optional(value) else {
        return Ok(None);
    };
    let parsed = value
        .parse::<usize>()
        .with_context(|| format!("{label} must be a whole number"))?;
    validate(parsed)?;
    Ok(Some(parsed))
}

struct OnboardingTerminalCleanup;

impl Drop for OnboardingTerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
    }
}

fn review_viewport(area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(16),
            Constraint::Length(4),
        ])
        .split(area);
    let main = if vertical[1].width < 96 {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(vertical[1])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
            .split(vertical[1])
    };
    Block::default()
        .borders(Borders::ALL)
        .title("Review")
        .padding(Padding::new(1, 1, 1, 0))
        .inner(main[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers, MouseEvent, MouseEventKind};
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use ratatui::layout::Rect;

    #[test]
    fn onboarding_render_once_shows_two_column_shell() -> Result<()> {
        let app = OnboardingApp::new(&AppConfig::default(), OnboardingLaunchMode::Replay);
        let snapshot = render_once(app, 120, 32)?;
        assert!(snapshot.contains("MetaStack"));
        assert!(snapshot.contains("Guide"));
        assert!(snapshot.contains("Start"));
        assert!(snapshot.contains("Press Enter to begin"));
        Ok(())
    }

    #[test]
    fn onboarding_review_scrolls_when_summary_overflows() {
        let mut app = OnboardingApp::new(&AppConfig::default(), OnboardingLaunchMode::Replay);
        app.step = OnboardingStep::Review;
        let long = "repo-default-label-".repeat(10);
        let _ = app.listen_label.paste(&long);
        let _ = app.plan_label.paste(&long);
        let _ = app.technical_label.paste(&long);

        let viewport = review_viewport(Rect::new(0, 0, 72, 20));
        assert!(app.handle_key(KeyCode::End.into(), viewport));
        assert!(app.review_scroll.offset() > 0);
    }

    #[test]
    fn onboarding_review_mouse_wheel_scrolls_when_summary_overflows() {
        let mut app = OnboardingApp::new(&AppConfig::default(), OnboardingLaunchMode::Replay);
        app.step = OnboardingStep::Review;
        let long = "repo-default-label-".repeat(10);
        let _ = app.listen_label.paste(&long);
        let _ = app.plan_label.paste(&long);
        let _ = app.technical_label.paste(&long);

        let viewport = review_viewport(Rect::new(0, 0, 72, 20));
        let handled = app.handle_review_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: viewport.x,
                row: viewport.y,
                modifiers: KeyModifiers::NONE,
            },
            viewport,
        );

        assert!(handled);
        assert!(app.review_scroll.offset() > 0);
    }

    #[tokio::test]
    async fn load_catalog_fetches_viewer_teams_and_projects() -> Result<()> {
        let server = MockServer::start();
        let api_url = server.url("/graphql");

        let viewer_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .header("authorization", "lin_api_test")
                .body_includes("query Viewer");
            then.status(200).json_body(serde_json::json!({
                "data": {
                    "viewer": {
                        "id": "user-1",
                        "name": "Meta User",
                        "email": "meta@example.com"
                    }
                }
            }));
        });
        let teams_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .header("authorization", "lin_api_test")
                .body_includes("query Teams");
            then.status(200).json_body(serde_json::json!({
                "data": {
                    "teams": {
                        "nodes": [{
                            "id": "team-1",
                            "key": "ENG",
                            "name": "Engineering",
                            "states": { "nodes": [] }
                        }]
                    }
                }
            }));
        });
        let projects_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .header("authorization", "lin_api_test")
                .body_includes("query Projects");
            then.status(200).json_body(serde_json::json!({
                "data": {
                    "projects": {
                        "nodes": [{
                            "id": "project-1",
                            "name": "MetaStack CLI",
                            "description": "Primary",
                            "url": "https://linear.app/project/project-1",
                            "progress": 0.5,
                            "teams": {
                                "nodes": [{
                                    "id": "team-1",
                                    "key": "ENG",
                                    "name": "Engineering"
                                }]
                            }
                        }]
                    }
                }
            }));
        });

        let catalog = load_catalog("lin_api_test".to_string(), api_url).await?;
        assert_eq!(catalog.viewer.name, "Meta User");
        assert_eq!(catalog.teams.len(), 1);
        assert_eq!(catalog.projects.len(), 1);
        viewer_mock.assert();
        teams_mock.assert();
        projects_mock.assert();

        Ok(())
    }

    #[tokio::test]
    async fn load_catalog_fails_when_projects_are_missing() {
        let server = MockServer::start();
        let api_url = server.url("/graphql");

        server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("query Viewer");
            then.status(200).json_body(serde_json::json!({
                "data": {
                    "viewer": {
                        "id": "user-1",
                        "name": "Meta User",
                        "email": "meta@example.com"
                    }
                }
            }));
        });
        server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("query Teams");
            then.status(200).json_body(serde_json::json!({
                "data": {
                    "teams": {
                        "nodes": [{
                            "id": "team-1",
                            "key": "ENG",
                            "name": "Engineering",
                            "states": { "nodes": [] }
                        }]
                    }
                }
            }));
        });
        server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("query Projects");
            then.status(200).json_body(serde_json::json!({
                "data": {
                    "projects": {
                        "nodes": []
                    }
                }
            }));
        });

        let error = load_catalog("lin_api_test".to_string(), api_url)
            .await
            .expect_err("catalog loading should fail");
        assert!(
            error
                .to_string()
                .contains("Linear returned no visible projects")
        );
    }
}
