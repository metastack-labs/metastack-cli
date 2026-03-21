use std::io::IsTerminal;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, anyhow};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde::Serialize;

use crate::backlog::template_seed_conflicts;
use crate::cli::{ConfigEventArg, SetupArgs};
use crate::config::{
    AppConfig, DEFAULT_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT,
    DEFAULT_LISTEN_POLL_INTERVAL_SECONDS, LinearConfig, LinearConfigOverrides,
    ListenAssignmentScope, ListenRefreshPolicy, PlanDefaultMode, PlanningMeta, VelocityAutoAssign,
    ensure_saved_issue_labels, normalize_agent_name, parse_listen_required_labels_csv,
    supported_agent_models, supported_agent_names, supported_reasoning_options,
    validate_agent_model, validate_agent_name, validate_agent_reasoning,
    validate_backlog_default_priority, validate_backlog_labels, validate_fast_plan_question_limit,
    validate_interactive_plan_follow_up_question_limit, validate_listen_poll_interval_seconds,
};
use crate::fs::{PlanningPaths, canonicalize_existing_dir};
use crate::linear::{LinearService, ReqwestLinearClient};
use crate::scaffold::{ensure_backlog_templates, ensure_planning_layout};
use crate::tui::fields::{InputFieldState, SelectFieldState};
use crate::tui::keybindings::KeybindingPolicy;
use crate::tui::scroll::{ScrollState, plain_text, scrollable_paragraph_with_block, wrapped_rows};

#[derive(Debug, Clone)]
struct SetupViewData {
    root: PathBuf,
    config_path: PathBuf,
    metastack_meta_path: PathBuf,
    app_config: AppConfig,
    app_config_changed: bool,
    planning_meta: PlanningMeta,
    detected_agents: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SerializableSetupView<'a> {
    config_path: String,
    metastack_meta_path: String,
    detected_agents: &'a [String],
    repo: &'a PlanningMeta,
}

#[derive(Debug, Clone)]
struct SetupReport {
    metastack_meta_path: PathBuf,
    changed: bool,
}

#[derive(Debug, Clone)]
struct SetupApp {
    keybindings: KeybindingPolicy,
    step: SetupStep,
    profile: Option<String>,
    repo_auth_field: SelectFieldState,
    api_key: InputFieldState,
    team: InputFieldState,
    project: InputFieldState,
    provider_field: SelectFieldState,
    model_field: SelectFieldState,
    reasoning: SelectFieldState,
    listen_label: InputFieldState,
    assignment_field: SelectFieldState,
    refresh_policy_field: SelectFieldState,
    instructions_path: InputFieldState,
    listen_poll_interval: InputFieldState,
    interactive_plan_limit: InputFieldState,
    plan_default_mode: SelectFieldState,
    plan_fast_single_ticket: SelectFieldState,
    plan_fast_questions: InputFieldState,
    plan_label: InputFieldState,
    technical_label: InputFieldState,
    summary_scroll: ScrollState,
    detected_agents: Vec<String>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct SubmittedSetup {
    api_key: Option<String>,
    profile: Option<String>,
    team: Option<String>,
    project_selector: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
    listen_labels: Option<Vec<String>>,
    assignment_scope: ListenAssignmentScope,
    refresh_policy: ListenRefreshPolicy,
    instructions_path: Option<String>,
    listen_poll_interval: Option<u64>,
    interactive_plan_limit: Option<usize>,
    plan_default_mode: Option<PlanDefaultMode>,
    fast_single_ticket: Option<bool>,
    fast_questions: Option<usize>,
    plan_label: Option<String>,
    technical_label: Option<String>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
enum SetupExit {
    Cancelled,
    Submitted(SubmittedSetup),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupStep {
    LinearAuth,
    LinearApiKey,
    Team,
    Project,
    Provider,
    Model,
    Reasoning,
    ListenLabel,
    AssignmentScope,
    RefreshPolicy,
    InstructionsPath,
    ListenPollInterval,
    InteractivePlanLimit,
    PlanDefaultMode,
    PlanFastSingleTicket,
    PlanFastQuestions,
    PlanLabel,
    TechnicalLabel,
    Save,
}

impl SetupStep {
    fn all() -> [Self; 19] {
        [
            Self::LinearAuth,
            Self::LinearApiKey,
            Self::Team,
            Self::Project,
            Self::Provider,
            Self::Model,
            Self::Reasoning,
            Self::ListenLabel,
            Self::AssignmentScope,
            Self::RefreshPolicy,
            Self::InstructionsPath,
            Self::ListenPollInterval,
            Self::InteractivePlanLimit,
            Self::PlanDefaultMode,
            Self::PlanFastSingleTicket,
            Self::PlanFastQuestions,
            Self::PlanLabel,
            Self::TechnicalLabel,
            Self::Save,
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
            Self::LinearAuth => "Linear auth",
            Self::LinearApiKey => "Project Linear API key",
            Self::Team => "Default team",
            Self::Project => "Default project",
            Self::Provider => "Repo agent",
            Self::Model => "Repo model",
            Self::Reasoning => "Repo reasoning",
            Self::ListenLabel => "Listen labels",
            Self::AssignmentScope => "Assignee filter",
            Self::RefreshPolicy => "Workspace refresh",
            Self::InstructionsPath => "Instructions file",
            Self::ListenPollInterval => "Listen poll interval",
            Self::InteractivePlanLimit => "Plan follow-up limit",
            Self::PlanDefaultMode => "Plan mode",
            Self::PlanFastSingleTicket => "Fast plan shape",
            Self::PlanFastQuestions => "Fast plan questions",
            Self::PlanLabel => "Plan issue label",
            Self::TechnicalLabel => "Technical issue label",
            Self::Save => "Save",
        }
    }

    fn compact_label(self) -> &'static str {
        match self {
            Self::LinearAuth => "Linear auth",
            Self::LinearApiKey => "API key",
            Self::Team => "Team",
            Self::Project => "Project",
            Self::Provider => "Agent",
            Self::Model => "Model",
            Self::Reasoning => "Reasoning",
            Self::ListenLabel => "Listen labels",
            Self::AssignmentScope => "Assignee",
            Self::RefreshPolicy => "Refresh",
            Self::InstructionsPath => "Instructions",
            Self::ListenPollInterval => "Poll interval",
            Self::InteractivePlanLimit => "Plan limit",
            Self::PlanDefaultMode => "Plan mode",
            Self::PlanFastSingleTicket => "Fast shape",
            Self::PlanFastQuestions => "Fast questions",
            Self::PlanLabel => "Plan label",
            Self::TechnicalLabel => "Tech label",
            Self::Save => "Save",
        }
    }

    fn panel_label(self) -> &'static str {
        match self {
            Self::LinearAuth => "Project-specific Linear auth",
            Self::LinearApiKey => "Project Linear API key (CLI config)",
            Self::Team => "Repo default Linear team",
            Self::Project => "Repo default Linear project",
            Self::Provider => "Repo default agent/provider",
            Self::Model => "Repo default model",
            Self::Reasoning => "Repo default reasoning effort",
            Self::ListenLabel => "Listen required labels",
            Self::AssignmentScope => "Listen assignee filter",
            Self::RefreshPolicy => "Listen workspace refresh policy",
            Self::InstructionsPath => "Listen instructions file",
            Self::ListenPollInterval => "Listen poll interval in seconds",
            Self::InteractivePlanLimit => "Interactive plan follow-up question limit",
            Self::PlanDefaultMode => "Default plan mode",
            Self::PlanFastSingleTicket => "Default fast ticket shape",
            Self::PlanFastQuestions => "Default fast follow-up batch size",
            Self::PlanLabel => "Default plan issue label",
            Self::TechnicalLabel => "Default technical issue label",
            Self::Save => "Save repo setup",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum SetupAction {
    Up,
    Down,
    Tab,
    BackTab,
    Enter,
    Esc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BacklogTemplateConflictAction {
    Overwrite,
    Skip,
    Cancel,
}

pub async fn run_setup(args: &SetupArgs) -> Result<String> {
    let root = canonicalize_existing_dir(&args.root)?;
    let can_launch_tui = io::stdin().is_terminal() && io::stdout().is_terminal();
    let template_conflict_action = resolve_backlog_template_conflicts(&root, args, can_launch_tui)?;
    if template_conflict_action == BacklogTemplateConflictAction::Cancel {
        return Ok("Repo setup cancelled.".to_string());
    }
    ensure_planning_layout(&root, false)?;
    if template_conflict_action == BacklogTemplateConflictAction::Overwrite {
        ensure_backlog_templates(&root, true)?;
    }
    let mut view = load_view(&root)?;

    if args.json {
        return render_json(&view);
    }

    if has_direct_updates(args) {
        let changed = apply_direct_updates(&mut view, args).await?;
        save_view(&mut view).await?;
        return Ok(SetupReport {
            metastack_meta_path: view.metastack_meta_path.clone(),
            changed,
        }
        .render());
    }

    if args.render_once {
        return render_once(SetupApp::new(&view), args);
    }

    if can_launch_tui {
        return match run_setup_dashboard(SetupApp::new(&view))? {
            SetupExit::Cancelled => Ok("Repo setup cancelled.".to_string()),
            SetupExit::Submitted(submitted) => {
                submitted.apply(&mut view).await?;
                save_view(&mut view).await?;
                Ok(SetupReport {
                    metastack_meta_path: view.metastack_meta_path.clone(),
                    changed: true,
                }
                .render())
            }
        };
    }

    Ok(render_summary(&view, false))
}

fn resolve_backlog_template_conflicts(
    root: &std::path::Path,
    args: &SetupArgs,
    can_launch_tui: bool,
) -> Result<BacklogTemplateConflictAction> {
    let conflicts = template_seed_conflicts(&PlanningPaths::new(root).backlog_template_dir)?;
    if conflicts.is_empty() {
        return Ok(BacklogTemplateConflictAction::Skip);
    }

    let can_prompt = can_launch_tui && !args.json && !args.render_once && !has_direct_updates(args);
    if can_prompt {
        return prompt_backlog_template_conflicts(&conflicts);
    }

    Err(anyhow!(
        "repo setup found existing canonical backlog template files with local changes:\n{}\n\
rerun `meta setup --root {}` in an interactive terminal to choose overwrite, skip, or cancel.",
        conflicts
            .iter()
            .map(|path| format!("- .metastack/backlog/_TEMPLATE/{path}"))
            .collect::<Vec<_>>()
            .join("\n"),
        root.display()
    ))
}

fn prompt_backlog_template_conflicts(
    conflicts: &[String],
) -> Result<BacklogTemplateConflictAction> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    prompt_backlog_template_conflicts_with_io(conflicts, &mut reader, &mut writer)
}

fn parse_backlog_template_conflict_action(input: &str) -> Option<BacklogTemplateConflictAction> {
    match input.trim().to_ascii_lowercase().as_str() {
        "o" | "overwrite" => Some(BacklogTemplateConflictAction::Overwrite),
        "s" | "skip" => Some(BacklogTemplateConflictAction::Skip),
        "c" | "cancel" => Some(BacklogTemplateConflictAction::Cancel),
        _ => None,
    }
}

fn prompt_backlog_template_conflicts_with_io(
    conflicts: &[String],
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> Result<BacklogTemplateConflictAction> {
    writeln!(
        writer,
        "Canonical backlog template files already exist with local changes:"
    )?;
    for path in conflicts {
        writeln!(writer, "  - .metastack/backlog/_TEMPLATE/{path}")?;
    }
    writeln!(writer, "Choose [o]verwrite, [s]kip, or [c]ancel:")?;
    writer.flush()?;

    let mut input = String::new();
    loop {
        input.clear();
        reader.read_line(&mut input)?;
        if let Some(action) = parse_backlog_template_conflict_action(&input) {
            return Ok(action);
        }

        writeln!(writer, "Enter `o`, `s`, or `c`:")?;
        writer.flush()?;
    }
}

impl SetupReport {
    fn render(&self) -> String {
        let verb = if self.changed { "saved" } else { "unchanged" };
        format!(
            "Repo setup {verb}. Repo defaults: {}.\n{}",
            self.metastack_meta_path.display(),
            listen_prerequisites_summary()
        )
    }
}

fn load_view(root: &std::path::Path) -> Result<SetupViewData> {
    Ok(SetupViewData {
        root: root.to_path_buf(),
        config_path: crate::config::resolve_config_path()?,
        metastack_meta_path: PlanningPaths::new(root).meta_path(),
        app_config: AppConfig::load()?,
        app_config_changed: false,
        planning_meta: PlanningMeta::load(root)?,
        detected_agents: crate::config::detect_supported_agents(),
    })
}

async fn save_view(view: &mut SetupViewData) -> Result<()> {
    ensure_saved_issue_labels(&view.root, &view.app_config, &view.planning_meta).await?;
    if view.app_config_changed {
        view.app_config.save()?;
    }
    view.planning_meta.save(&view.root)?;
    Ok(())
}

fn render_json(view: &SetupViewData) -> Result<String> {
    Ok(serde_json::to_string_pretty(&SerializableSetupView {
        config_path: view.config_path.display().to_string(),
        metastack_meta_path: view.metastack_meta_path.display().to_string(),
        detected_agents: &view.detected_agents,
        repo: &view.planning_meta,
    })?)
}

fn render_summary(view: &SetupViewData, include_paths: bool) -> String {
    let mut lines = Vec::new();
    if include_paths {
        lines.push(format!(
            "Repo defaults path: {}",
            view.metastack_meta_path.display()
        ));
    }
    lines.push(format!(
        "Repo Linear auth: {}",
        display_repo_auth(
            view.app_config.repo_linear_api_key(&view.root).as_deref(),
            view.planning_meta.linear.profile.as_deref()
        )
    ));
    lines.push(format!(
        "Repo default team: {}",
        display_optional(view.planning_meta.linear.team.as_deref())
    ));
    lines.push(format!(
        "Repo default project ID: {}",
        display_optional(view.planning_meta.linear.project_id.as_deref())
    ));
    lines.push(format!(
        "Repo provider: {}",
        display_optional(view.planning_meta.agent.provider.as_deref())
    ));
    lines.push(format!(
        "Repo model: {}",
        display_optional(view.planning_meta.agent.model.as_deref())
    ));
    lines.push(format!(
        "Repo reasoning: {}",
        display_optional(view.planning_meta.agent.reasoning.as_deref())
    ));
    lines.push(format!(
        "Listen labels: {}",
        display_listen_labels(view.planning_meta.listen.required_label_names())
    ));
    lines.push(format!(
        "Assignee filter: {}",
        assignment_scope_label(view.planning_meta.listen.assignment_scope())
    ));
    lines.push(format!(
        "Workspace refresh: {}",
        refresh_policy_label(view.planning_meta.listen.refresh_policy())
    ));
    lines.push(format!(
        "Instructions file: {}",
        display_optional(view.planning_meta.listen.instructions_path.as_deref())
    ));
    lines.push(format!(
        "Listen poll interval: {}",
        display_poll_interval(view.planning_meta.listen.poll_interval_seconds)
    ));
    lines.push(format!(
        "Interactive plan limit: {}",
        display_plan_limit(view.planning_meta.plan.interactive_follow_up_questions)
    ));
    lines.push(format!(
        "Default plan mode: {}",
        display_plan_default_mode(view.planning_meta.plan.default_mode)
    ));
    lines.push(format!(
        "Fast single-ticket default: {}",
        display_fast_single_ticket(view.planning_meta.plan.fast_single_ticket)
    ));
    lines.push(format!(
        "Fast plan question limit: {}",
        display_fast_question_limit(view.planning_meta.plan.fast_questions)
    ));
    lines.push(format!(
        "Plan issue label: {}",
        effective_label(view.planning_meta.issue_labels.plan.as_deref(), "plan")
    ));
    lines.push(format!(
        "Technical issue label: {}",
        effective_label(
            view.planning_meta.issue_labels.technical.as_deref(),
            "technical"
        )
    ));
    lines.push(format!(
        "Backlog default assignee: {}",
        display_optional(view.planning_meta.backlog.default_assignee.as_deref())
    ));
    lines.push(format!(
        "Backlog default state: {}",
        display_optional(view.planning_meta.backlog.default_state.as_deref())
    ));
    lines.push(format!(
        "Backlog default priority: {}",
        view.planning_meta
            .backlog
            .default_priority
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unset".to_string())
    ));
    lines.push(format!(
        "Backlog default labels: {}",
        render_label_summary(&view.planning_meta.backlog.default_labels)
    ));
    lines.push(format!(
        "Zero-prompt velocity project: {}",
        display_optional(
            view.planning_meta
                .backlog
                .velocity_defaults
                .project
                .as_deref()
        )
    ));
    lines.push(format!(
        "Zero-prompt velocity state: {}",
        display_optional(
            view.planning_meta
                .backlog
                .velocity_defaults
                .state
                .as_deref()
        )
    ));
    lines.push(format!(
        "Zero-prompt auto-assign: {}",
        view.planning_meta
            .backlog
            .velocity_defaults
            .auto_assign
            .map(render_velocity_auto_assign)
            .unwrap_or_else(|| "unset".to_string())
    ));
    lines.push(format!(
        "Listen prerequisites: {}",
        listen_prerequisites_summary()
    ));
    lines.join("\n")
}

fn listen_prerequisites_summary() -> &'static str {
    "Built-in Codex listen runs require `~/.codex/config.toml` with `approval_policy = \"never\"` and `sandbox_mode = \"danger-full-access\"`, plus `[mcp_servers.linear]` removed or disabled. Built-in Claude listen runs require `claude` on PATH and no `ANTHROPIC_API_KEY` override. Use `meta agents listen --check --root .` to verify."
}

fn has_direct_updates(args: &SetupArgs) -> bool {
    args.api_key.is_some()
        || args.profile.is_some()
        || args.team.is_some()
        || args.project.is_some()
        || args.project_id.is_some()
        || args.provider.is_some()
        || args.model.is_some()
        || args.reasoning.is_some()
        || args.listen_label.is_some()
        || args.assignment_scope.is_some()
        || args.refresh_policy.is_some()
        || args.instructions_path.is_some()
        || args.listen_poll_interval.is_some()
        || args.interactive_plan_follow_up_question_limit.is_some()
        || args.plan_default_mode.is_some()
        || args.plan_fast_single_ticket.is_some()
        || args.plan_fast_questions.is_some()
        || args.plan_label.is_some()
        || args.technical_label.is_some()
        || args.default_assignee.is_some()
        || args.default_state.is_some()
        || args.default_priority.is_some()
        || !args.default_labels.is_empty()
        || args.velocity_project.is_some()
        || args.velocity_state.is_some()
        || args.velocity_auto_assign.is_some()
}

async fn apply_direct_updates(view: &mut SetupViewData, args: &SetupArgs) -> Result<bool> {
    let before_meta = serde_json::to_value(&view.planning_meta)?;
    let before_app = serde_json::to_value(&view.app_config)?;

    if args.project.is_some() && args.project_id.is_some() {
        return Err(anyhow!(
            "`--project` and `--project-id` both set the repo default project; choose one."
        ));
    }

    if let Some(profile) = &args.profile {
        let profile = normalize_optional(profile);
        validate_profile(&view.app_config, profile.as_deref())?;
        view.planning_meta.linear.profile = profile;
    }
    if let Some(api_key) = &args.api_key {
        let api_key = normalize_optional(api_key);
        if view.app_config.repo_linear_api_key(&view.root) != api_key {
            view.app_config.set_repo_linear_api_key(&view.root, api_key);
            view.app_config_changed = true;
        }
    }
    if let Some(team) = &args.team {
        view.planning_meta.linear.team = normalize_optional(team);
    }
    if let Some(project_selector) = args.project.as_deref().or(args.project_id.as_deref()) {
        view.planning_meta.linear.project_id =
            resolve_project_selector(view, normalize_optional(project_selector)).await?;
    }
    if let Some(provider) = &args.provider {
        let normalized = normalize_optional(provider).map(|value| normalize_agent_name(&value));
        if let Some(provider) = normalized.as_deref() {
            validate_agent_name(&view.app_config, provider)?;
        }
        view.planning_meta.agent.provider = normalized;
        if validate_agent_model(
            &selected_provider(view).unwrap_or_else(|| supported_agent_names()[0].to_string()),
            view.planning_meta.agent.model.as_deref(),
        )
        .is_err()
        {
            view.planning_meta.agent.model = None;
        }
        if validate_agent_reasoning(
            &selected_provider(view).unwrap_or_else(|| supported_agent_names()[0].to_string()),
            view.planning_meta.agent.model.as_deref(),
            view.planning_meta.agent.reasoning.as_deref(),
        )
        .is_err()
        {
            view.planning_meta.agent.reasoning = None;
        }
    }
    if let Some(model) = &args.model {
        let provider =
            selected_provider(view).unwrap_or_else(|| supported_agent_names()[0].to_string());
        let model = normalize_optional(model);
        validate_agent_model(&provider, model.as_deref())?;
        view.planning_meta.agent.model = model;
        if validate_agent_reasoning(
            &provider,
            view.planning_meta.agent.model.as_deref(),
            view.planning_meta.agent.reasoning.as_deref(),
        )
        .is_err()
        {
            view.planning_meta.agent.reasoning = None;
        }
    }
    if let Some(reasoning) = &args.reasoning {
        let provider = view
            .planning_meta
            .agent
            .provider
            .clone()
            .or_else(|| view.app_config.agents.default_agent.clone())
            .ok_or_else(|| anyhow!("repo reasoning requires a selected provider"))?;
        let normalized = normalize_optional(reasoning);
        validate_agent_reasoning(
            &provider,
            view.planning_meta.agent.model.as_deref(),
            normalized.as_deref(),
        )?;
        view.planning_meta.agent.reasoning = normalized;
    }
    if let Some(label) = &args.listen_label {
        view.planning_meta.listen.required_labels = parse_optional_listen_labels_input(label);
    }
    if let Some(scope) = args.assignment_scope {
        view.planning_meta.listen.assignment_scope = Some(scope.into());
    }
    if let Some(policy) = args.refresh_policy {
        view.planning_meta.listen.refresh_policy = Some(policy.into());
    }
    if let Some(path) = &args.instructions_path {
        view.planning_meta.listen.instructions_path = normalize_optional(path);
    }
    if let Some(interval) = &args.listen_poll_interval {
        view.planning_meta.listen.poll_interval_seconds = parse_poll_interval(interval)?;
    }
    if let Some(limit) = &args.interactive_plan_follow_up_question_limit {
        view.planning_meta.plan.interactive_follow_up_questions = parse_plan_limit(limit)?;
    }
    if let Some(mode) = &args.plan_default_mode {
        view.planning_meta.plan.default_mode =
            parse_optional_plan_default_mode(mode, "plan default mode")?;
    }
    if let Some(single_ticket) = &args.plan_fast_single_ticket {
        view.planning_meta.plan.fast_single_ticket =
            parse_optional_bool(single_ticket, "fast single-ticket default")?;
    }
    if let Some(limit) = &args.plan_fast_questions {
        view.planning_meta.plan.fast_questions =
            parse_fast_plan_limit(limit, "fast plan question limit")?;
    }
    if let Some(label) = &args.plan_label {
        view.planning_meta.issue_labels.plan = normalize_optional(label);
    }
    if let Some(label) = &args.technical_label {
        view.planning_meta.issue_labels.technical = normalize_optional(label);
    }
    if let Some(assignee) = &args.default_assignee {
        view.planning_meta.backlog.default_assignee = normalize_optional(assignee);
    }
    if let Some(state) = &args.default_state {
        view.planning_meta.backlog.default_state = normalize_optional(state);
    }
    if let Some(priority) = &args.default_priority {
        view.planning_meta.backlog.default_priority = parse_optional_priority(priority)?;
    }
    if !args.default_labels.is_empty() {
        view.planning_meta.backlog.default_labels = parse_default_labels(&args.default_labels)?;
    }
    if let Some(project) = &args.velocity_project {
        view.planning_meta.backlog.velocity_defaults.project = normalize_optional(project);
    }
    if let Some(state) = &args.velocity_state {
        view.planning_meta.backlog.velocity_defaults.state = normalize_optional(state);
    }
    if let Some(auto_assign) = &args.velocity_auto_assign {
        view.planning_meta.backlog.velocity_defaults.auto_assign =
            parse_velocity_auto_assign(auto_assign)?;
    }

    let after_meta = serde_json::to_value(&view.planning_meta)?;
    let after_app = serde_json::to_value(&view.app_config)?;
    Ok(before_meta != after_meta || before_app != after_app)
}

fn validate_profile(app_config: &AppConfig, profile: Option<&str>) -> Result<()> {
    let Some(profile) = profile else {
        return Ok(());
    };
    if app_config.linear.profiles.contains_key(profile) {
        return Ok(());
    }
    Err(anyhow!(
        "Linear profile `{profile}` is not configured. Add it under `[linear.profiles.{profile}]` or clear the repo profile binding."
    ))
}

fn selected_provider(view: &SetupViewData) -> Option<String> {
    view.planning_meta
        .agent
        .provider
        .clone()
        .or_else(|| view.app_config.agents.default_agent.clone())
}

async fn resolve_project_selector(
    view: &SetupViewData,
    project_selector: Option<String>,
) -> Result<Option<String>> {
    let Some(project_selector) = project_selector else {
        return Ok(None);
    };
    let config = LinearConfig::from_sources(
        &view.app_config,
        &view.planning_meta,
        Some(&view.root),
        LinearConfigOverrides::default(),
    )?;
    let default_team = config.default_team.clone();
    let service = LinearService::new(ReqwestLinearClient::new(config)?, default_team.clone());
    Ok(Some(
        service
            .resolve_project_selector_strict(
                &project_selector,
                view.planning_meta
                    .linear
                    .team
                    .as_deref()
                    .or(default_team.as_deref()),
            )
            .await?,
    ))
}

impl SetupApp {
    fn new(view: &SetupViewData) -> Self {
        let provider_options = supported_agent_names()
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        let plan_mode_options = vec![
            "Leave unset".to_string(),
            "Normal".to_string(),
            "Fast".to_string(),
        ];
        let fast_single_ticket_options = vec![
            "Leave unset".to_string(),
            "Single ticket by default".to_string(),
            "Multiple tickets by default".to_string(),
        ];
        let selected_provider = view
            .planning_meta
            .agent
            .provider
            .as_deref()
            .map(normalize_agent_name)
            .or_else(|| view.app_config.agents.default_agent.clone())
            .unwrap_or_else(|| supported_agent_names()[0].to_string());
        let provider_index = provider_options
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(&selected_provider))
            .unwrap_or(0);

        let mut app = Self {
            keybindings: KeybindingPolicy::new(view.app_config.vim_mode_enabled()),
            step: SetupStep::LinearAuth,
            profile: view.planning_meta.linear.profile.clone(),
            repo_auth_field: SelectFieldState::new(
                vec![
                    "Inherit shared or configured Linear auth".to_string(),
                    "Set a Linear API key for this project".to_string(),
                ],
                usize::from(view.app_config.repo_linear_api_key(&view.root).is_some()),
            ),
            api_key: InputFieldState::new(
                view.app_config
                    .repo_linear_api_key(&view.root)
                    .unwrap_or_default(),
            ),
            team: InputFieldState::new(view.planning_meta.linear.team.clone().unwrap_or_default()),
            project: InputFieldState::new(
                view.planning_meta
                    .linear
                    .project_id
                    .clone()
                    .unwrap_or_default(),
            ),
            provider_field: SelectFieldState::new(provider_options, provider_index),
            model_field: SelectFieldState::new(vec!["Leave unset".to_string()], 0),
            reasoning: SelectFieldState::new(vec!["Leave unset".to_string()], 0),
            listen_label: InputFieldState::new(
                view.planning_meta.listen.required_label_names().join(", "),
            ),
            assignment_field: SelectFieldState::new(
                vec![
                    "Any eligible issue".to_string(),
                    "Only issues assigned to the authenticated viewer".to_string(),
                    "Viewer-assigned issues plus unassigned issues".to_string(),
                ],
                match view.planning_meta.listen.assignment_scope() {
                    ListenAssignmentScope::Any => 0,
                    ListenAssignmentScope::ViewerOnly => 1,
                    ListenAssignmentScope::ViewerOrUnassigned => 2,
                },
            ),
            refresh_policy_field: SelectFieldState::new(
                vec![
                    "Reuse the clone and hard-refresh it from origin/main".to_string(),
                    "Delete the clone and recreate it from origin/main".to_string(),
                ],
                match view.planning_meta.listen.refresh_policy() {
                    ListenRefreshPolicy::ReuseAndRefresh => 0,
                    ListenRefreshPolicy::RecreateFromOriginMain => 1,
                },
            ),
            instructions_path: InputFieldState::new(
                view.planning_meta
                    .listen
                    .instructions_path
                    .clone()
                    .unwrap_or_default(),
            ),
            listen_poll_interval: InputFieldState::new(
                view.planning_meta
                    .listen
                    .poll_interval_seconds
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            interactive_plan_limit: InputFieldState::new(
                view.planning_meta
                    .plan
                    .interactive_follow_up_questions
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            plan_default_mode: SelectFieldState::new(
                plan_mode_options,
                match view.planning_meta.plan.default_mode {
                    Some(PlanDefaultMode::Normal) => 1,
                    Some(PlanDefaultMode::Fast) => 2,
                    None => 0,
                },
            ),
            plan_fast_single_ticket: SelectFieldState::new(
                fast_single_ticket_options,
                match view.planning_meta.plan.fast_single_ticket {
                    Some(true) => 1,
                    Some(false) => 2,
                    None => 0,
                },
            ),
            plan_fast_questions: InputFieldState::new(
                view.planning_meta
                    .plan
                    .fast_questions
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            plan_label: InputFieldState::new(
                view.planning_meta
                    .issue_labels
                    .plan
                    .clone()
                    .unwrap_or_default(),
            ),
            technical_label: InputFieldState::new(
                view.planning_meta
                    .issue_labels
                    .technical
                    .clone()
                    .unwrap_or_default(),
            ),
            summary_scroll: ScrollState::default(),
            detected_agents: view.detected_agents.clone(),
            error: None,
        };
        app.sync_models(view.planning_meta.agent.model.as_deref());
        app.sync_reasoning(view.planning_meta.agent.reasoning.as_deref());
        app
    }

    fn current_provider(&self) -> &str {
        self.provider_field.selected_label().unwrap_or("codex")
    }

    fn sync_models(&mut self, preferred: Option<&str>) {
        let current = preferred
            .map(str::to_string)
            .or_else(|| self.model_field.selected_label().map(str::to_string))
            .filter(|value| value != "Leave unset");
        let mut options = vec!["Leave unset".to_string()];
        options.extend(
            supported_agent_models(self.current_provider())
                .iter()
                .map(|value| (*value).to_string()),
        );
        let selected = current
            .as_deref()
            .and_then(|value| {
                options
                    .iter()
                    .position(|candidate| candidate.eq_ignore_ascii_case(value))
            })
            .unwrap_or(0);
        self.model_field = SelectFieldState::new(options, selected);
        self.sync_reasoning(None);
    }

    fn sync_reasoning(&mut self, preferred: Option<&str>) {
        let current = preferred.map(str::to_string).or_else(|| {
            self.reasoning
                .selected_label()
                .map(str::to_string)
                .filter(|value| value != "Leave unset")
        });
        let mut options = vec!["Leave unset".to_string()];
        options.extend(
            supported_reasoning_options(
                self.current_provider(),
                match self.model_field.selected() {
                    0 => None,
                    _ => self.model_field.selected_label(),
                },
            )
            .iter()
            .map(|value| (*value).to_string()),
        );
        let selected = current
            .as_deref()
            .and_then(|value| {
                options
                    .iter()
                    .position(|candidate| candidate.eq_ignore_ascii_case(value))
            })
            .unwrap_or(0);
        self.reasoning = SelectFieldState::new(options, selected);
    }

    fn apply_action(&mut self, action: SetupAction) -> Option<SetupExit> {
        self.error = None;
        match action {
            SetupAction::Tab => {
                self.step = self.step.next();
                None
            }
            SetupAction::BackTab => {
                self.step = self.step.previous();
                None
            }
            SetupAction::Enter => {
                if self.step == SetupStep::Save {
                    match self.submit() {
                        Ok(submitted) => Some(SetupExit::Submitted(submitted)),
                        Err(error) => {
                            self.error = Some(error.to_string());
                            None
                        }
                    }
                } else {
                    self.step = self.step.next();
                    None
                }
            }
            SetupAction::Esc => Some(SetupExit::Cancelled),
            SetupAction::Up => {
                if matches!(
                    self.step,
                    SetupStep::LinearAuth
                        | SetupStep::Provider
                        | SetupStep::Model
                        | SetupStep::Reasoning
                        | SetupStep::AssignmentScope
                        | SetupStep::RefreshPolicy
                        | SetupStep::PlanDefaultMode
                        | SetupStep::PlanFastSingleTicket
                ) {
                    self.move_selection(-1);
                } else {
                    self.step = self.step.previous();
                }
                None
            }
            SetupAction::Down => {
                if matches!(
                    self.step,
                    SetupStep::LinearAuth
                        | SetupStep::Provider
                        | SetupStep::Model
                        | SetupStep::Reasoning
                        | SetupStep::AssignmentScope
                        | SetupStep::RefreshPolicy
                        | SetupStep::PlanDefaultMode
                        | SetupStep::PlanFastSingleTicket
                ) {
                    self.move_selection(1);
                } else {
                    self.step = self.step.next();
                }
                None
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent, summary_viewport: Rect) -> Option<SetupExit> {
        if self.select_step_active()
            && let Some(delta) = self.keybindings.vertical_delta(key)
        {
            return self.apply_action(if delta < 0 {
                SetupAction::Up
            } else {
                SetupAction::Down
            });
        }

        match key.code {
            KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
                if self.step == SetupStep::Save =>
            {
                let _ = self.summary_scroll.apply_key_code_in_viewport(
                    key.code,
                    summary_viewport,
                    self.summary_content_rows(summary_viewport.width),
                );
                None
            }
            KeyCode::Char(_)
            | KeyCode::Backspace
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End => {
                self.apply_text_key(key);
                None
            }
            KeyCode::Up => self.apply_action(SetupAction::Up),
            KeyCode::Down => self.apply_action(SetupAction::Down),
            KeyCode::Tab => self.apply_action(SetupAction::Tab),
            KeyCode::BackTab => self.apply_action(SetupAction::BackTab),
            KeyCode::Enter => self.apply_action(SetupAction::Enter),
            KeyCode::Esc => self.apply_action(SetupAction::Esc),
            _ => None,
        }
    }

    fn handle_summary_mouse(&mut self, mouse: MouseEvent, summary_viewport: Rect) -> bool {
        if self.step != SetupStep::Save {
            return false;
        }
        self.summary_scroll.apply_mouse_in_viewport(
            mouse,
            summary_viewport,
            self.summary_content_rows(summary_viewport.width),
        )
    }

    fn select_step_active(&self) -> bool {
        matches!(
            self.step,
            SetupStep::LinearAuth
                | SetupStep::Provider
                | SetupStep::Model
                | SetupStep::Reasoning
                | SetupStep::AssignmentScope
                | SetupStep::RefreshPolicy
                | SetupStep::PlanDefaultMode
                | SetupStep::PlanFastSingleTicket
        )
    }

    fn handle_paste(&mut self, text: &str) {
        self.error = None;
        match self.step {
            SetupStep::LinearApiKey => {
                let _ = self.api_key.paste(text);
            }
            SetupStep::Team => {
                let _ = self.team.paste(text);
            }
            SetupStep::Project => {
                let _ = self.project.paste(text);
            }
            SetupStep::ListenLabel => {
                let _ = self.listen_label.paste(text);
            }
            SetupStep::InstructionsPath => {
                let _ = self.instructions_path.paste(text);
            }
            SetupStep::ListenPollInterval => {
                let _ = self.listen_poll_interval.paste(text);
            }
            SetupStep::InteractivePlanLimit => {
                let _ = self.interactive_plan_limit.paste(text);
            }
            SetupStep::PlanFastQuestions => {
                let _ = self.plan_fast_questions.paste(text);
            }
            SetupStep::PlanLabel => {
                let _ = self.plan_label.paste(text);
            }
            SetupStep::TechnicalLabel => {
                let _ = self.technical_label.paste(text);
            }
            SetupStep::LinearAuth
            | SetupStep::Provider
            | SetupStep::Model
            | SetupStep::Reasoning
            | SetupStep::AssignmentScope
            | SetupStep::RefreshPolicy
            | SetupStep::PlanDefaultMode
            | SetupStep::PlanFastSingleTicket
            | SetupStep::Save => {}
        }
    }

    fn apply_text_key(&mut self, key: KeyEvent) {
        self.error = None;
        match self.step {
            SetupStep::LinearApiKey => {
                let _ = self.api_key.handle_key(key);
            }
            SetupStep::Team => {
                let _ = self.team.handle_key(key);
            }
            SetupStep::Project => {
                let _ = self.project.handle_key(key);
            }
            SetupStep::ListenLabel => {
                let _ = self.listen_label.handle_key(key);
            }
            SetupStep::InstructionsPath => {
                let _ = self.instructions_path.handle_key(key);
            }
            SetupStep::ListenPollInterval => {
                let _ = self.listen_poll_interval.handle_key(key);
            }
            SetupStep::InteractivePlanLimit => {
                let _ = self.interactive_plan_limit.handle_key(key);
            }
            SetupStep::PlanFastQuestions => {
                let _ = self.plan_fast_questions.handle_key(key);
            }
            SetupStep::PlanLabel => {
                let _ = self.plan_label.handle_key(key);
            }
            SetupStep::TechnicalLabel => {
                let _ = self.technical_label.handle_key(key);
            }
            SetupStep::LinearAuth
            | SetupStep::Provider
            | SetupStep::Model
            | SetupStep::Reasoning
            | SetupStep::AssignmentScope
            | SetupStep::RefreshPolicy
            | SetupStep::PlanDefaultMode
            | SetupStep::PlanFastSingleTicket
            | SetupStep::Save => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.step {
            SetupStep::LinearAuth => {
                self.repo_auth_field.move_by(delta);
                if self.repo_auth_field.selected() == 0 {
                    self.api_key = InputFieldState::new(String::new());
                }
            }
            SetupStep::Provider => {
                self.provider_field.move_by(delta);
                self.sync_models(None);
            }
            SetupStep::Model => {
                self.model_field.move_by(delta);
                self.sync_reasoning(None);
            }
            SetupStep::Reasoning => self.reasoning.move_by(delta),
            SetupStep::AssignmentScope => self.assignment_field.move_by(delta),
            SetupStep::RefreshPolicy => self.refresh_policy_field.move_by(delta),
            SetupStep::PlanDefaultMode => self.plan_default_mode.move_by(delta),
            SetupStep::PlanFastSingleTicket => self.plan_fast_single_ticket.move_by(delta),
            SetupStep::Save => {
                let key = if delta.is_negative() {
                    KeyCode::Up
                } else {
                    KeyCode::Down
                };
                let viewport = summary_viewport(Rect::new(0, 0, 120, 32));
                let _ = self.summary_scroll.apply_key_code_in_viewport(
                    key,
                    viewport,
                    self.summary_content_rows(viewport.width),
                );
            }
            SetupStep::LinearApiKey
            | SetupStep::Team
            | SetupStep::Project
            | SetupStep::ListenLabel
            | SetupStep::InstructionsPath
            | SetupStep::ListenPollInterval
            | SetupStep::InteractivePlanLimit
            | SetupStep::PlanFastQuestions
            | SetupStep::PlanLabel
            | SetupStep::TechnicalLabel => {}
        }
    }

    fn summary_text(&self, width: u16) -> Text<'static> {
        Text::from(summary_lines(
            width,
            &[
                (
                    "Linear auth",
                    summarize_repo_auth(&self.repo_auth_field, &self.api_key),
                ),
                ("Default team", summarize_optional(&self.team)),
                ("Project selector", summarize_optional(&self.project)),
                (
                    "Repo provider",
                    self.provider_field
                        .selected_label()
                        .unwrap_or("unset")
                        .to_string(),
                ),
                (
                    "Repo model",
                    self.model_field
                        .selected_label()
                        .unwrap_or("Leave unset")
                        .to_string(),
                ),
                (
                    "Repo reasoning",
                    summarize_optional_select(&self.reasoning, "Leave unset"),
                ),
                ("Listen labels", summarize_listen_labels(&self.listen_label)),
                (
                    "Assignee filter",
                    assignment_scope_label(match self.assignment_field.selected() {
                        1 => ListenAssignmentScope::ViewerOnly,
                        2 => ListenAssignmentScope::ViewerOrUnassigned,
                        _ => ListenAssignmentScope::Any,
                    })
                    .to_string(),
                ),
                (
                    "Workspace refresh",
                    refresh_policy_label(match self.refresh_policy_field.selected() {
                        1 => ListenRefreshPolicy::RecreateFromOriginMain,
                        _ => ListenRefreshPolicy::ReuseAndRefresh,
                    })
                    .to_string(),
                ),
                (
                    "Instructions file",
                    summarize_optional(&self.instructions_path),
                ),
                (
                    "Listen poll interval",
                    summarize_optional(&self.listen_poll_interval),
                ),
                (
                    "Interactive plan limit",
                    summarize_optional(&self.interactive_plan_limit),
                ),
                (
                    "Plan mode",
                    summarize_optional_select(&self.plan_default_mode, "Leave unset"),
                ),
                (
                    "Fast single-ticket",
                    summarize_optional_select(&self.plan_fast_single_ticket, "Leave unset"),
                ),
                (
                    "Fast questions",
                    summarize_optional(&self.plan_fast_questions),
                ),
                ("Plan label", summarize_optional(&self.plan_label)),
                ("Technical label", summarize_optional(&self.technical_label)),
            ],
        ))
    }

    fn summary_content_rows(&self, width: u16) -> usize {
        wrapped_rows(&plain_text(&self.summary_text(width.max(1))), width.max(1))
    }

    fn submit(&self) -> Result<SubmittedSetup> {
        let provider =
            normalize_optional(self.current_provider()).map(|value| normalize_agent_name(&value));
        if let Some(provider) = provider.as_deref() {
            validate_agent_name(&AppConfig::load()?, provider)?;
        }
        let model = match self.model_field.selected() {
            0 => None,
            _ => self.model_field.selected_label().map(str::to_string),
        };
        if let Some(provider) = provider.as_deref() {
            validate_agent_model(provider, model.as_deref())?;
        }
        let reasoning = match self.reasoning.selected() {
            0 => None,
            _ => self.reasoning.selected_label().map(str::to_string),
        };
        if let Some(provider) = provider.as_deref() {
            validate_agent_reasoning(provider, model.as_deref(), reasoning.as_deref())?;
        }
        Ok(SubmittedSetup {
            api_key: match self.repo_auth_field.selected() {
                1 => normalize_optional(self.api_key.value()),
                _ => None,
            },
            profile: self.profile.clone(),
            team: normalize_optional(self.team.value()),
            project_selector: normalize_optional(self.project.value()),
            provider,
            model,
            reasoning,
            listen_labels: parse_optional_listen_labels_input(self.listen_label.value()),
            assignment_scope: match self.assignment_field.selected() {
                1 => ListenAssignmentScope::ViewerOnly,
                2 => ListenAssignmentScope::ViewerOrUnassigned,
                _ => ListenAssignmentScope::Any,
            },
            refresh_policy: match self.refresh_policy_field.selected() {
                1 => ListenRefreshPolicy::RecreateFromOriginMain,
                _ => ListenRefreshPolicy::ReuseAndRefresh,
            },
            instructions_path: normalize_optional(self.instructions_path.value()),
            listen_poll_interval: parse_poll_interval(self.listen_poll_interval.value())?,
            interactive_plan_limit: parse_plan_limit(self.interactive_plan_limit.value())?,
            plan_default_mode: match self.plan_default_mode.selected() {
                1 => Some(PlanDefaultMode::Normal),
                2 => Some(PlanDefaultMode::Fast),
                _ => None,
            },
            fast_single_ticket: match self.plan_fast_single_ticket.selected() {
                1 => Some(true),
                2 => Some(false),
                _ => None,
            },
            fast_questions: parse_fast_plan_limit(
                self.plan_fast_questions.value(),
                "fast plan question limit",
            )?,
            plan_label: normalize_optional(self.plan_label.value()),
            technical_label: normalize_optional(self.technical_label.value()),
        })
    }
}

impl SubmittedSetup {
    async fn apply(&self, view: &mut SetupViewData) -> Result<()> {
        validate_profile(&view.app_config, self.profile.as_deref())?;
        if let Some(provider) = self.provider.as_deref() {
            validate_agent_name(&view.app_config, provider)?;
            validate_agent_model(provider, self.model.as_deref())?;
            validate_agent_reasoning(provider, self.model.as_deref(), self.reasoning.as_deref())?;
        }
        if view.app_config.repo_linear_api_key(&view.root) != self.api_key {
            view.app_config
                .set_repo_linear_api_key(&view.root, self.api_key.clone());
            view.app_config_changed = true;
        }
        view.planning_meta.linear.profile = self.profile.clone();
        view.planning_meta.linear.team = self.team.clone();
        view.planning_meta.linear.project_id =
            resolve_project_selector(view, self.project_selector.clone()).await?;
        view.planning_meta.agent.provider = self.provider.clone();
        view.planning_meta.agent.model = self.model.clone();
        view.planning_meta.agent.reasoning = self.reasoning.clone();
        view.planning_meta.listen.required_labels = self.listen_labels.clone();
        view.planning_meta.listen.assignment_scope = Some(self.assignment_scope);
        view.planning_meta.listen.refresh_policy = Some(self.refresh_policy);
        view.planning_meta.listen.instructions_path = self.instructions_path.clone();
        view.planning_meta.listen.poll_interval_seconds = self.listen_poll_interval;
        view.planning_meta.plan.interactive_follow_up_questions = self.interactive_plan_limit;
        view.planning_meta.plan.default_mode = self.plan_default_mode;
        view.planning_meta.plan.fast_single_ticket = self.fast_single_ticket;
        view.planning_meta.plan.fast_questions = self.fast_questions;
        view.planning_meta.issue_labels.plan = self.plan_label.clone();
        view.planning_meta.issue_labels.technical = self.technical_label.clone();
        Ok(())
    }
}

fn render_once(app: SetupApp, args: &SetupArgs) -> Result<String> {
    let backend = TestBackend::new(args.width, args.height);
    let mut terminal = Terminal::new(backend)?;
    let mut app = app;

    for action in args.events.iter().copied().map(setup_action_from_event) {
        if app.apply_action(action).is_some() {
            break;
        }
    }

    terminal.draw(|frame| render_setup_dashboard(frame, &app))?;
    Ok(snapshot(terminal.backend()))
}

fn run_setup_dashboard(app: SetupApp) -> Result<SetupExit> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = app;

    loop {
        terminal.draw(|frame| render_setup_dashboard(frame, &app))?;

        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let viewport = summary_viewport(terminal.size()?.into());
                    if let Some(exit) = app.handle_key(key, viewport) {
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
                    let viewport = summary_viewport(terminal.size()?.into());
                    let _ = app.handle_summary_mouse(mouse, viewport);
                }
                _ => {}
            }
        }
    }
}

/// Minimum column width to show every step label without wrapping (single-column mode).
/// Accounts for `"> XX. Label"` prefix plus two border characters.
fn setup_step_column_width() -> u16 {
    let steps = SetupStep::all();
    let max_label = steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let digits = if i + 1 >= 10 { 2 } else { 1 };
            2 + digits + 2 + s.label().len()
        })
        .max()
        .unwrap_or(20);
    (max_label + 2) as u16
}

fn render_setup_dashboard(frame: &mut Frame<'_>, app: &SetupApp) {
    let area = frame.area();
    let header_height = if area.width >= 110 { 5 } else { 6 };
    let footer_height = if area.width >= 100 { 4 } else { 5 };
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(0),
            Constraint::Length(footer_height),
        ])
        .split(area);

    let header = Paragraph::new(Text::from(vec![
        Line::from("Meta Setup"),
        Line::from(
            "Configure repo-scoped defaults stored in `.metastack/meta.json` after install onboarding is complete.",
        ),
        Line::from(format!(
            "Detected supported agents on PATH: {}",
            if app.detected_agents.is_empty() {
                "none".to_string()
            } else {
                app.detected_agents.join(", ")
            }
        )),
        Line::from(
            "Project names are resolved to canonical Linear project IDs before setup is saved.",
        ),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Repo setup"))
    .wrap(Wrap { trim: false });
    frame.render_widget(header, layout[0]);

    let step_col = setup_step_column_width();
    let body_area = layout[1];
    if body_area.width >= 118 {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(step_col),
                Constraint::Min(38),
                Constraint::Length(44),
            ])
            .split(body_area);
        render_step_list(frame, app, body[0], 1);
        render_step_panel(frame, app, body[1]);
        render_summary_panel(frame, app, body[2]);
    } else if body_area.width >= 90 {
        let sidebar_width = step_col.max(40);
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar_width), Constraint::Min(40)])
            .split(body_area);
        let sidebar = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(12), Constraint::Min(10)])
            .split(body[0]);
        render_step_list(frame, app, sidebar[0], 2);
        render_summary_panel(frame, app, sidebar[1]);
        render_step_panel(frame, app, body[1]);
    } else {
        let stacked = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(10),
                Constraint::Min(8),
                Constraint::Length(14),
            ])
            .split(body_area);
        render_step_list(frame, app, stacked[0], 2);
        render_step_panel(frame, app, stacked[1]);
        render_summary_panel(frame, app, stacked[2]);
    }
    render_footer(frame, app, layout[2]);
}

fn render_step_list(frame: &mut Frame<'_>, app: &SetupApp, area: Rect, columns: usize) {
    let block = Block::default().borders(Borders::ALL).title("Questions");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let columns = columns.max(1);
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints((0..columns).map(|_| Constraint::Ratio(1, columns as u32)))
        .split(inner);
    let steps = SetupStep::all();
    let per_column = steps.len().div_ceil(columns);

    for (column, chunk) in chunks.iter().enumerate() {
        let start = column * per_column;
        let end = (start + per_column).min(steps.len());
        if start >= end {
            continue;
        }

        let lines = steps[start..end]
            .iter()
            .enumerate()
            .map(|(offset, step)| {
                let index = start + offset;
                let selected = index == app.step.index();
                let marker = if selected { "> " } else { "  " };
                let style = if selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let label = if columns > 1 || chunk.width < 20 {
                    step.compact_label()
                } else {
                    step.label()
                };
                Line::from(Span::styled(
                    format!("{marker}{}. {label}", index + 1),
                    style,
                ))
            })
            .collect::<Vec<_>>();
        let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, *chunk);
    }
}

fn render_step_panel(frame: &mut Frame<'_>, app: &SetupApp, area: Rect) {
    let title = if area.width < 60 {
        app.step.panel_label().to_string()
    } else {
        format!(
            "Question {} of {}: {}",
            app.step.index() + 1,
            SetupStep::all().len(),
            app.step.panel_label()
        )
    };

    match app.step {
        SetupStep::LinearAuth => render_select_panel(frame, area, &title, &app.repo_auth_field),
        SetupStep::LinearApiKey => render_input_panel(
            frame,
            area,
            &title,
            &app.api_key,
            "Paste a project-specific Linear API key stored in local CLI config, or leave blank to inherit shared auth.",
        ),
        SetupStep::Team => render_input_panel(
            frame,
            area,
            &title,
            &app.team,
            "Optional repo-scoped default Linear team key.",
        ),
        SetupStep::Project => render_input_panel(
            frame,
            area,
            &title,
            &app.project,
            "Enter a Linear project name or ID. Names resolve to canonical project IDs on save.",
        ),
        SetupStep::Provider => render_select_panel(frame, area, &title, &app.provider_field),
        SetupStep::Model => render_select_panel(frame, area, &title, &app.model_field),
        SetupStep::Reasoning => render_select_panel(frame, area, &title, &app.reasoning),
        SetupStep::ListenLabel => render_input_panel(
            frame,
            area,
            &title,
            &app.listen_label,
            "Only tickets carrying any of these comma-separated labels will be picked up automatically. Leave blank to unset.",
        ),
        SetupStep::AssignmentScope => {
            render_select_panel(frame, area, &title, &app.assignment_field)
        }
        SetupStep::RefreshPolicy => {
            render_select_panel(frame, area, &title, &app.refresh_policy_field)
        }
        SetupStep::InstructionsPath => render_input_panel(
            frame,
            area,
            &title,
            &app.instructions_path,
            "Optional markdown file appended to launched-agent instructions.",
        ),
        SetupStep::ListenPollInterval => render_input_panel(
            frame,
            area,
            &title,
            &app.listen_poll_interval,
            "Optional poll interval in seconds. Leave blank to keep the default.",
        ),
        SetupStep::InteractivePlanLimit => render_input_panel(
            frame,
            area,
            &title,
            &app.interactive_plan_limit,
            "Optional `meta backlog plan` interactive follow-up limit between 1 and 10.",
        ),
        SetupStep::PlanDefaultMode => {
            render_select_panel(frame, area, &title, &app.plan_default_mode)
        }
        SetupStep::PlanFastSingleTicket => {
            render_select_panel(frame, area, &title, &app.plan_fast_single_ticket)
        }
        SetupStep::PlanFastQuestions => render_input_panel(
            frame,
            area,
            &title,
            &app.plan_fast_questions,
            "Optional fast planning follow-up batch size between 0 and 10.",
        ),
        SetupStep::PlanLabel => render_input_panel(
            frame,
            area,
            &title,
            &app.plan_label,
            "Optional repo default label for `meta backlog plan` issues. Leave blank for `plan`.",
        ),
        SetupStep::TechnicalLabel => render_input_panel(
            frame,
            area,
            &title,
            &app.technical_label,
            "Optional repo default label for `meta backlog tech` issues. Leave blank for `technical`.",
        ),
        SetupStep::Save => render_save_panel(frame, area),
    }
}

fn render_summary_panel(frame: &mut Frame<'_>, app: &SetupApp, area: Rect) {
    let active = app.step == SetupStep::Save;
    let paragraph = scrollable_paragraph_with_block(
        app.summary_text(area.width.saturating_sub(2)),
        Block::default()
            .borders(Borders::ALL)
            .title(if active {
                "Summary [scroll]"
            } else {
                "Summary"
            })
            .border_style(if active {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            }),
        &app.summary_scroll,
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame<'_>, app: &SetupApp, area: Rect) {
    let controls = match app.step {
        SetupStep::LinearAuth
        | SetupStep::Provider
        | SetupStep::Model
        | SetupStep::Reasoning
        | SetupStep::AssignmentScope
        | SetupStep::RefreshPolicy
        | SetupStep::PlanDefaultMode
        | SetupStep::PlanFastSingleTicket => {
            if app.keybindings.vim_mode_enabled() {
                "Use Up/Down/j/k to choose. Enter or Tab advances. Shift+Tab goes back. Esc cancels."
            } else {
                "Use Up/Down to choose. Enter or Tab advances. Shift+Tab goes back. Esc cancels."
            }
        }
        SetupStep::Save => {
            "Review the summary. Up/Down and PgUp/PgDn/Home/End or the mouse wheel scroll. Enter saves. Shift+Tab goes back. Esc cancels."
        }
        _ => "Type or paste the value. Enter or Tab advances. Shift+Tab goes back. Esc cancels.",
    };
    let status = app.error.as_deref().unwrap_or("Ready.");
    let footer = Paragraph::new(Text::from(vec![Line::from(controls), Line::from(status)]))
        .block(Block::default().borders(Borders::ALL).title("Controls"))
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, area);
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
        .border_style(Style::default().add_modifier(Modifier::BOLD));
    let inner = block.inner(area);
    let rendered = field.render_with_width(placeholder, true, inner.width);
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
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    frame.render_widget(list, area);
}

fn render_save_panel(frame: &mut Frame<'_>, area: Rect) {
    let paragraph = Paragraph::new(Text::from(vec![
        Line::from("Press Enter to save repo-scoped defaults to `.metastack/meta.json`."),
        Line::from("Project names are resolved before setup is persisted."),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Save"))
    .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn assignment_scope_label(scope: ListenAssignmentScope) -> &'static str {
    match scope {
        ListenAssignmentScope::Any => "Any eligible issue",
        ListenAssignmentScope::ViewerOnly => "Only issues assigned to the authenticated viewer",
        ListenAssignmentScope::ViewerOrUnassigned => {
            "Viewer-assigned issues plus unassigned issues"
        }
    }
}

fn refresh_policy_label(policy: ListenRefreshPolicy) -> &'static str {
    match policy {
        ListenRefreshPolicy::ReuseAndRefresh => {
            "Reuse the clone and hard-refresh it from origin/main"
        }
        ListenRefreshPolicy::RecreateFromOriginMain => {
            "Delete the clone and recreate it from origin/main"
        }
    }
}

fn summary_lines(width: u16, entries: &[(&str, String)]) -> Vec<Line<'static>> {
    let compact = width < 42;
    let mut lines = Vec::new();

    for (label, value) in entries {
        if compact {
            lines.push(Line::from(Span::styled(
                (*label).to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(format!("  {value}")));
        } else {
            lines.push(Line::from(format!("{label}: {value}")));
        }
    }

    lines
}

fn display_optional(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unset")
        .to_string()
}

fn display_listen_labels(labels: &[String]) -> String {
    if labels.is_empty() {
        "unset".to_string()
    } else {
        labels.join(", ")
    }
}

fn render_label_summary(labels: &[String]) -> String {
    display_listen_labels(labels)
}

fn render_velocity_auto_assign(value: VelocityAutoAssign) -> String {
    match value {
        VelocityAutoAssign::Viewer => "viewer".to_string(),
    }
}
fn display_repo_auth(api_key: Option<&str>, profile: Option<&str>) -> String {
    if api_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
    {
        "project-specific API key".to_string()
    } else if let Some(profile) = profile.map(str::trim).filter(|value| !value.is_empty()) {
        format!("inherited ({profile})")
    } else {
        "inherited".to_string()
    }
}

fn display_poll_interval(interval: Option<u64>) -> String {
    match interval {
        Some(interval) => format!("{interval}s"),
        None => format!("{DEFAULT_LISTEN_POLL_INTERVAL_SECONDS}s (default)"),
    }
}

fn display_plan_limit(limit: Option<usize>) -> String {
    match limit {
        Some(limit) => limit.to_string(),
        None => format!("{DEFAULT_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT} (default)"),
    }
}

fn display_plan_default_mode(mode: Option<PlanDefaultMode>) -> String {
    match mode {
        Some(PlanDefaultMode::Normal) => "normal".to_string(),
        Some(PlanDefaultMode::Fast) => "fast".to_string(),
        None => "unset".to_string(),
    }
}

fn display_fast_single_ticket(value: Option<bool>) -> String {
    match value {
        Some(true) => "single ticket".to_string(),
        Some(false) => "multiple tickets".to_string(),
        None => "unset".to_string(),
    }
}

fn display_fast_question_limit(limit: Option<usize>) -> String {
    match limit {
        Some(limit) => limit.to_string(),
        None => "unset".to_string(),
    }
}

fn effective_label(value: Option<&str>, default: &str) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
        .to_string()
}

fn normalize_optional(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_optional_priority(value: &str) -> Result<Option<u8>> {
    let Some(value) = normalize_optional(value) else {
        return Ok(None);
    };
    let priority = value
        .parse::<u8>()
        .map_err(|_| anyhow!("backlog default priority must be an integer between 1 and 4"))?;
    validate_backlog_default_priority(priority)?;
    Ok(Some(priority))
}

fn parse_default_labels(values: &[String]) -> Result<Vec<String>> {
    if values.len() == 1 && values[0].trim().eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }

    let labels = values
        .iter()
        .map(|value| value.trim().to_string())
        .collect::<Vec<_>>();
    validate_backlog_labels(&labels)?;
    Ok(labels)
}

fn parse_velocity_auto_assign(value: &str) -> Result<Option<VelocityAutoAssign>> {
    let Some(value) = normalize_optional(value) else {
        return Ok(None);
    };
    match value.as_str() {
        "viewer" => Ok(Some(VelocityAutoAssign::Viewer)),
        _ => Err(anyhow!(
            "velocity auto-assign must be `viewer` or empty to clear it"
        )),
    }
}

fn parse_optional_listen_labels_input(value: &str) -> Option<Vec<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        None
    } else {
        parse_listen_required_labels_csv(trimmed)
    }
}

fn parse_poll_interval(value: &str) -> Result<Option<u64>> {
    let Some(value) = normalize_optional(value) else {
        return Ok(None);
    };
    let interval = value
        .parse::<u64>()
        .map_err(|_| anyhow!("listen poll interval must be a whole number of seconds"))?;
    validate_listen_poll_interval_seconds(interval)?;
    Ok(Some(interval))
}

fn parse_plan_limit(value: &str) -> Result<Option<usize>> {
    let Some(value) = normalize_optional(value) else {
        return Ok(None);
    };
    let limit = value.parse::<usize>().map_err(|_| {
        anyhow!("interactive plan follow-up question limit must be a whole number between 1 and 10")
    })?;
    validate_interactive_plan_follow_up_question_limit(limit)?;
    Ok(Some(limit))
}

fn parse_fast_plan_limit(value: &str, label: &str) -> Result<Option<usize>> {
    let Some(value) = normalize_optional(value) else {
        return Ok(None);
    };
    let limit = value
        .parse::<usize>()
        .map_err(|_| anyhow!("{label} must be a whole number between 0 and 10"))?;
    validate_fast_plan_question_limit(limit)?;
    Ok(Some(limit))
}

fn parse_optional_bool(value: &str, label: &str) -> Result<Option<bool>> {
    let Some(value) = normalize_optional(value) else {
        return Ok(None);
    };
    match value.to_ascii_lowercase().as_str() {
        "true" => Ok(Some(true)),
        "false" => Ok(Some(false)),
        _ => Err(anyhow!("{label} must be `true`, `false`, or `none`")),
    }
}

fn parse_optional_plan_default_mode(value: &str, label: &str) -> Result<Option<PlanDefaultMode>> {
    let Some(value) = normalize_optional(value) else {
        return Ok(None);
    };
    match value.to_ascii_lowercase().as_str() {
        "normal" => Ok(Some(PlanDefaultMode::Normal)),
        "fast" => Ok(Some(PlanDefaultMode::Fast)),
        _ => Err(anyhow!("{label} must be `normal`, `fast`, or `none`")),
    }
}

fn summarize_optional(field: &InputFieldState) -> String {
    normalize_optional(field.value()).unwrap_or_else(|| "unset".to_string())
}

fn summarize_listen_labels(field: &InputFieldState) -> String {
    parse_optional_listen_labels_input(field.value())
        .map(|labels| labels.join(", "))
        .unwrap_or_else(|| "unset".to_string())
}

fn summarize_optional_select(field: &SelectFieldState, unset_label: &str) -> String {
    match field.selected_label() {
        Some(value) if value != unset_label => value.to_string(),
        _ => "unset".to_string(),
    }
}

fn summarize_repo_auth(auth_field: &SelectFieldState, api_key: &InputFieldState) -> String {
    match auth_field.selected() {
        1 if normalize_optional(api_key.value()).is_some() => {
            "project-specific API key".to_string()
        }
        _ => "inherited".to_string(),
    }
}

fn snapshot(backend: &TestBackend) -> String {
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

fn setup_action_from_event(event: ConfigEventArg) -> SetupAction {
    match event {
        ConfigEventArg::Up => SetupAction::Up,
        ConfigEventArg::Down => SetupAction::Down,
        ConfigEventArg::Tab => SetupAction::Tab,
        ConfigEventArg::BackTab => SetupAction::BackTab,
        ConfigEventArg::Enter => SetupAction::Enter,
        ConfigEventArg::Esc => SetupAction::Esc,
    }
}

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
    }
}

fn summary_viewport(area: Rect) -> Rect {
    let header_height = if area.width >= 110 { 5 } else { 6 };
    let footer_height = if area.width >= 100 { 4 } else { 5 };
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(0),
            Constraint::Length(footer_height),
        ])
        .split(area);
    let step_col = setup_step_column_width();
    let body_area = layout[1];
    let summary_area = if body_area.width >= 118 {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(step_col),
                Constraint::Min(38),
                Constraint::Length(44),
            ])
            .split(body_area);
        body[2]
    } else if body_area.width >= 90 {
        let sidebar_width = step_col.max(40);
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar_width), Constraint::Min(40)])
            .split(body_area);
        let sidebar = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(12), Constraint::Min(10)])
            .split(body[0]);
        sidebar[1]
    } else {
        let stacked = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(10),
                Constraint::Min(8),
                Constraint::Length(14),
            ])
            .split(body_area);
        stacked[2]
    };
    Rect::new(
        summary_area.x.saturating_add(1),
        summary_area.y.saturating_add(1),
        summary_area.width.saturating_sub(2).max(1),
        summary_area.height.saturating_sub(2).max(1),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        BacklogTemplateConflictAction, SetupApp, SetupStep, SetupViewData,
        parse_backlog_template_conflict_action, parse_optional_listen_labels_input,
        prompt_backlog_template_conflicts_with_io, render_summary, summary_viewport,
    };
    use crate::config::{
        AgentSettings, AppConfig, ListenAssignmentScope, PlanningAgentSettings,
        PlanningListenSettings, PlanningMeta,
    };
    use anyhow::Result;
    use crossterm::event::{KeyCode, KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    use std::io::Cursor;

    #[test]
    fn setup_app_refreshes_reasoning_options_when_provider_changes() {
        let view = SetupViewData {
            root: PathBuf::from("/tmp/repo"),
            config_path: PathBuf::from("/tmp/metastack-config.toml"),
            metastack_meta_path: PathBuf::from("/tmp/repo/.metastack/meta.json"),
            app_config: AppConfig {
                agents: AgentSettings {
                    default_agent: Some("codex".to_string()),
                    ..AgentSettings::default()
                },
                ..AppConfig::default()
            },
            app_config_changed: false,
            planning_meta: PlanningMeta {
                agent: PlanningAgentSettings {
                    provider: Some("codex".to_string()),
                    model: Some("gpt-5.4".to_string()),
                    reasoning: Some("high".to_string()),
                },
                ..PlanningMeta::default()
            },
            detected_agents: Vec::new(),
        };

        let mut app = SetupApp::new(&view);
        assert_eq!(
            app.reasoning.options(),
            &[
                "Leave unset".to_string(),
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
            ]
        );
        assert_eq!(app.reasoning.selected_label(), Some("high"));

        let claude_index = app
            .provider_field
            .options()
            .iter()
            .position(|option| option == "claude")
            .expect("claude provider should be listed");
        app.provider_field
            .move_by(claude_index as isize - app.provider_field.selected() as isize);
        app.sync_models(Some("sonnet"));
        app.sync_reasoning(Some("max"));

        assert_eq!(
            app.reasoning.options(),
            &[
                "Leave unset".to_string(),
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
                "max".to_string(),
            ]
        );
        assert_eq!(app.reasoning.selected_label(), Some("max"));
    }

    #[test]
    fn setup_save_summary_scrolls_when_content_overflows() {
        let mut view = SetupViewData {
            root: PathBuf::from("/tmp/repo"),
            config_path: PathBuf::from("/tmp/metastack-config.toml"),
            metastack_meta_path: PathBuf::from("/tmp/repo/.metastack/meta.json"),
            app_config: AppConfig::default(),
            app_config_changed: false,
            planning_meta: PlanningMeta::default(),
            detected_agents: Vec::new(),
        };
        view.planning_meta.listen.instructions_path =
            Some("docs/".to_string() + &"very-long-review-value/".repeat(12));
        let mut app = SetupApp::new(&view);
        app.step = SetupStep::Save;

        let viewport = summary_viewport(Rect::new(0, 0, 72, 20));
        let _ = app.handle_key(KeyCode::End.into(), viewport);

        assert!(app.summary_scroll.offset() > 0);
    }

    #[test]
    fn setup_save_summary_mouse_wheel_scrolls_when_content_overflows() {
        let mut view = SetupViewData {
            root: PathBuf::from("/tmp/repo"),
            config_path: PathBuf::from("/tmp/metastack-config.toml"),
            metastack_meta_path: PathBuf::from("/tmp/repo/.metastack/meta.json"),
            app_config: AppConfig::default(),
            app_config_changed: false,
            planning_meta: PlanningMeta::default(),
            detected_agents: Vec::new(),
        };
        view.planning_meta.listen.instructions_path =
            Some("docs/".to_string() + &"very-long-review-value/".repeat(12));
        let mut app = SetupApp::new(&view);
        app.step = SetupStep::Save;

        let viewport = summary_viewport(Rect::new(0, 0, 72, 20));
        let handled = app.handle_summary_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: viewport.x,
                row: viewport.y,
                modifiers: KeyModifiers::NONE,
            },
            viewport,
        );

        assert!(handled);
        assert!(app.summary_scroll.offset() > 0);
    }

    #[test]
    fn setup_parse_optional_listen_labels_supports_comma_separated_values() {
        assert_eq!(
            parse_optional_listen_labels_input(" plan, urgent ,Plan "),
            Some(vec!["plan".to_string(), "urgent".to_string()])
        );
        assert_eq!(parse_optional_listen_labels_input("none"), None);
    }

    #[test]
    fn setup_render_summary_lists_all_required_labels() {
        let view = SetupViewData {
            root: PathBuf::from("/tmp/repo"),
            config_path: PathBuf::from("/tmp/metastack-config.toml"),
            metastack_meta_path: PathBuf::from("/tmp/repo/.metastack/meta.json"),
            app_config: AppConfig::default(),
            app_config_changed: false,
            planning_meta: PlanningMeta {
                listen: PlanningListenSettings {
                    required_labels: Some(vec!["plan".to_string(), "urgent".to_string()]),
                    ..PlanningListenSettings::default()
                },
                ..PlanningMeta::default()
            },
            detected_agents: Vec::new(),
        };

        let summary = render_summary(&view, false);
        assert!(summary.contains("Listen labels: plan, urgent"));
    }

    #[test]
    fn parse_backlog_template_conflict_action_accepts_supported_inputs() {
        assert_eq!(
            parse_backlog_template_conflict_action("o"),
            Some(BacklogTemplateConflictAction::Overwrite)
        );
        assert_eq!(
            parse_backlog_template_conflict_action("skip"),
            Some(BacklogTemplateConflictAction::Skip)
        );
        assert_eq!(
            parse_backlog_template_conflict_action("Cancel\n"),
            Some(BacklogTemplateConflictAction::Cancel)
        );
        assert_eq!(parse_backlog_template_conflict_action("later"), None);
    }

    #[test]
    fn prompt_backlog_template_conflicts_retries_until_it_gets_a_valid_choice() -> Result<()> {
        let conflicts = vec!["index.md".to_string(), "validation.md".to_string()];
        let mut reader = Cursor::new(b"later\noverwrite\n".to_vec());
        let mut writer = Vec::new();

        let action =
            prompt_backlog_template_conflicts_with_io(&conflicts, &mut reader, &mut writer)?;

        assert_eq!(action, BacklogTemplateConflictAction::Overwrite);
        let output = String::from_utf8(writer)?;
        assert!(output.contains(".metastack/backlog/_TEMPLATE/index.md"));
        assert!(output.contains(".metastack/backlog/_TEMPLATE/validation.md"));
        assert!(output.contains("Enter `o`, `s`, or `c`"));

        Ok(())
    }

    #[test]
    fn assignment_scope_labels_match_explicit_listen_semantics() {
        assert_eq!(
            crate::setup::assignment_scope_label(ListenAssignmentScope::Any),
            "Any eligible issue"
        );
        assert_eq!(
            crate::setup::assignment_scope_label(ListenAssignmentScope::ViewerOnly),
            "Only issues assigned to the authenticated viewer"
        );
        assert_eq!(
            crate::setup::assignment_scope_label(ListenAssignmentScope::ViewerOrUnassigned),
            "Viewer-assigned issues plus unassigned issues"
        );
    }
}
