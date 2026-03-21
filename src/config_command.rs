use std::collections::BTreeMap;
use std::io;
use std::io::IsTerminal;
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

use crate::cli::{ConfigArgs, VimModeArg};
use crate::config::{
    AgentConfigSource, AgentRouteConfig, AgentRouteScope, AppConfig, ListenAssignmentScope,
    ListenRefreshPolicy, PlanDefaultMode, VelocityAutoAssign, detect_supported_agents,
    normalize_agent_name, normalize_agent_route_key, resolve_agent_route, supported_agent_models,
    supported_agent_names, supported_agent_route_definitions, supported_agent_route_families,
    supported_reasoning_options, validate_agent_model, validate_agent_name,
    validate_agent_reasoning, validate_backlog_default_priority, validate_backlog_labels,
    validate_fast_plan_question_limit, validate_interactive_plan_follow_up_question_limit,
    validate_listen_poll_interval_seconds,
};
use crate::tui::fields::{InputFieldState, SelectFieldState};
use crate::tui::keybindings::KeybindingPolicy;
use crate::tui::scroll::{ScrollState, plain_text, scrollable_paragraph_with_block, wrapped_rows};

#[derive(Debug, Clone)]
pub struct ConfigReport {
    pub config_path: PathBuf,
    pub changed: bool,
}

#[derive(Debug, Clone)]
pub enum ConfigCommandOutput {
    Text(String),
    Json(String),
}

#[derive(Debug, Clone)]
struct ConfigViewData {
    config_path: PathBuf,
    app_config: AppConfig,
    detected_agents: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SerializableConfigView<'a> {
    config_path: String,
    detected_agents: &'a [String],
    app: &'a AppConfig,
}

pub async fn run_config(args: &ConfigArgs) -> Result<ConfigCommandOutput> {
    let mut view = load_view()?;

    if has_direct_updates(args) {
        let changed = apply_direct_updates(&mut view, args)?;
        save_view(&view)?;
        return if args.json {
            Ok(ConfigCommandOutput::Json(render_json(&view)?))
        } else {
            Ok(ConfigCommandOutput::Text(
                ConfigReport {
                    config_path: view.config_path.clone(),
                    changed,
                }
                .render(&view),
            ))
        };
    }

    if args.json {
        return Ok(ConfigCommandOutput::Json(render_json(&view)?));
    }

    if args.render_once {
        return Ok(ConfigCommandOutput::Text(if args.advanced_routing {
            render_advanced_once(AdvancedRoutingApp::new(&view)?, args)?
        } else {
            render_once(ConfigApp::new(&view), args)?
        }));
    }

    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        let exit = if args.advanced_routing {
            run_advanced_routing_dashboard(AdvancedRoutingApp::new(&view)?)?
        } else {
            run_config_dashboard(ConfigApp::new(&view))?
        };
        return match exit {
            ConfigDashboardExit::Cancelled => Ok(ConfigCommandOutput::Text(
                "Configuration dashboard cancelled.".to_string(),
            )),
            ConfigDashboardExit::Submitted(submitted) => {
                submitted.apply(&mut view)?;
                save_view(&view)?;
                Ok(ConfigCommandOutput::Text(
                    ConfigReport {
                        config_path: view.config_path.clone(),
                        changed: true,
                    }
                    .render(&view),
                ))
            }
            ConfigDashboardExit::SubmittedRoute(submitted) => {
                submitted.apply(&mut view)?;
                save_view(&view)?;
                Ok(ConfigCommandOutput::Text(
                    ConfigReport {
                        config_path: view.config_path.clone(),
                        changed: true,
                    }
                    .render(&view),
                ))
            }
        };
    }

    Ok(ConfigCommandOutput::Text(render_summary(&view, false)))
}

impl ConfigReport {
    fn render(&self, view: &ConfigViewData) -> String {
        let verb = if self.changed { "saved" } else { "unchanged" };
        format!(
            "Configuration {verb}. Config: {}.\n{}",
            self.config_path.display(),
            render_summary(view, true)
        )
    }
}

fn load_view() -> Result<ConfigViewData> {
    Ok(ConfigViewData {
        config_path: crate::config::resolve_config_path()?,
        app_config: AppConfig::load()?,
        detected_agents: detect_supported_agents(),
    })
}

fn save_view(view: &ConfigViewData) -> Result<()> {
    view.app_config.save()?;
    Ok(())
}

fn render_json(view: &ConfigViewData) -> Result<String> {
    Ok(serde_json::to_string_pretty(&SerializableConfigView {
        config_path: view.config_path.display().to_string(),
        detected_agents: &view.detected_agents,
        app: &view.app_config,
    })?)
}

fn render_summary(view: &ConfigViewData, include_path: bool) -> String {
    let mut lines = Vec::new();
    if include_path {
        lines.push(format!("Config path: {}", view.config_path.display()));
    }
    lines.push(format!(
        "Onboarding complete: {}",
        if view.app_config.onboarding.completed {
            "yes"
        } else {
            "no"
        }
    ));
    lines.push(format!(
        "Linear API key: {}",
        mask_secret(view.app_config.linear.api_key.as_deref())
    ));
    lines.push(format!(
        "Default Linear team: {}",
        display_optional(view.app_config.linear.team.as_deref())
    ));
    lines.push(format!(
        "Default Linear project ID: {}",
        display_optional(view.app_config.defaults.linear.project_id.as_deref())
    ));
    lines.push(format!(
        "Install listen label: {}",
        display_optional(view.app_config.defaults.listen.required_label.as_deref())
    ));
    lines.push(format!(
        "Install assignee scope: {}",
        view.app_config
            .defaults
            .listen
            .assignment_scope
            .map(|s| format!("{s:?}"))
            .unwrap_or_else(|| "unset".to_string())
    ));
    lines.push(format!(
        "Install refresh policy: {}",
        view.app_config
            .defaults
            .listen
            .refresh_policy
            .map(|p| format!("{p:?}"))
            .unwrap_or_else(|| "unset".to_string())
    ));
    lines.push(format!(
        "Install poll interval: {}",
        view.app_config
            .defaults
            .listen
            .poll_interval_seconds
            .map(|v| format!("{v}s"))
            .unwrap_or_else(|| "unset".to_string())
    ));
    lines.push(format!(
        "Install plan follow-up limit: {}",
        view.app_config
            .defaults
            .plan
            .interactive_follow_up_questions
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unset".to_string())
    ));
    lines.push(format!(
        "Install default plan mode: {}",
        display_plan_default_mode(view.app_config.defaults.plan.default_mode)
    ));
    lines.push(format!(
        "Install fast single-ticket default: {}",
        display_fast_single_ticket(view.app_config.defaults.plan.fast_single_ticket)
    ));
    lines.push(format!(
        "Install fast question limit: {}",
        view.app_config
            .defaults
            .plan
            .fast_questions
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unset".to_string())
    ));
    lines.push(format!(
        "Vim mode: {}",
        if view.app_config.vim_mode_enabled() {
            "enabled"
        } else {
            "disabled"
        }
    ));
    lines.push(format!(
        "Install plan label: {}",
        display_optional(view.app_config.defaults.issue_labels.plan.as_deref())
    ));
    lines.push(format!(
        "Install technical label: {}",
        display_optional(view.app_config.defaults.issue_labels.technical.as_deref())
    ));
    lines.push(format!(
        "Default Linear profile: {}",
        display_optional(view.app_config.linear.default_profile.as_deref())
    ));
    lines.push(format!(
        "Default agent: {}",
        display_optional(view.app_config.agents.default_agent.as_deref())
    ));
    lines.push(format!(
        "Default model: {}",
        display_optional(view.app_config.agents.default_model.as_deref())
    ));
    lines.push(format!(
        "Default reasoning: {}",
        display_optional(view.app_config.agents.default_reasoning.as_deref())
    ));
    lines.push(format!(
        "Backlog default assignee: {}",
        display_optional(view.app_config.backlog.default_assignee.as_deref())
    ));
    lines.push(format!(
        "Backlog default state: {}",
        display_optional(view.app_config.backlog.default_state.as_deref())
    ));
    lines.push(format!(
        "Backlog default priority: {}",
        view.app_config
            .backlog
            .default_priority
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unset".to_string())
    ));
    lines.push(format!(
        "Backlog default labels: {}",
        render_label_summary(&view.app_config.backlog.default_labels)
    ));
    lines.push(format!(
        "Zero-prompt velocity project: {}",
        display_optional(view.app_config.backlog.velocity_defaults.project.as_deref())
    ));
    lines.push(format!(
        "Zero-prompt velocity state: {}",
        display_optional(view.app_config.backlog.velocity_defaults.state.as_deref())
    ));
    lines.push(format!(
        "Zero-prompt auto-assign: {}",
        view.app_config
            .backlog
            .velocity_defaults
            .auto_assign
            .map(render_velocity_auto_assign)
            .unwrap_or_else(|| "unset".to_string())
    ));
    lines.push(format!(
        "Merge validation repair attempts: {}",
        view.app_config.merge.validation_repair_attempts()
    ));
    lines.push(format!(
        "Merge transient validation retries: {}",
        view.app_config.merge.validation_transient_retry_attempts()
    ));
    lines.push(format!(
        "Merge publication retries: {}",
        view.app_config.merge.publication_retry_attempts()
    ));
    lines.push(format!(
        "Advanced route overrides: {}",
        render_route_override_summary(&view.app_config)
    ));
    lines.push(format!(
        "Configured Linear profiles: {}",
        if view.app_config.linear.profiles.is_empty() {
            "none".to_string()
        } else {
            view.app_config
                .linear
                .profiles
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
    lines.push(format!(
        "Detected agents on PATH: {}",
        if view.detected_agents.is_empty() {
            "none".to_string()
        } else {
            view.detected_agents.join(", ")
        }
    ));
    lines.join("\n")
}

fn display_optional(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unset")
        .to_string()
}

fn display_plan_default_mode(value: Option<PlanDefaultMode>) -> String {
    match value {
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

fn render_label_summary(labels: &[String]) -> String {
    if labels.is_empty() {
        "unset".to_string()
    } else {
        labels.join(", ")
    }
}

fn render_velocity_auto_assign(value: VelocityAutoAssign) -> String {
    match value {
        VelocityAutoAssign::Viewer => "viewer".to_string(),
    }
}

fn mask_secret(value: Option<&str>) -> String {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) if value.len() <= 6 => "*".repeat(value.len()),
        Some(value) => format!(
            "{}{}",
            "*".repeat(value.len() - 4),
            &value[value.len() - 4..]
        ),
        None => "unset".to_string(),
    }
}

fn has_direct_updates(args: &ConfigArgs) -> bool {
    args.api_key.is_some()
        || args.team.is_some()
        || args.project_id.is_some()
        || args.listen_label.is_some()
        || args.assignment_scope.is_some()
        || args.refresh_policy.is_some()
        || args.poll_interval.is_some()
        || args.plan_follow_up_limit.is_some()
        || args.plan_default_mode.is_some()
        || args.plan_fast_single_ticket.is_some()
        || args.plan_fast_questions.is_some()
        || args.vim_mode.is_some()
        || args.plan_label.is_some()
        || args.technical_label.is_some()
        || args.default_profile.is_some()
        || args.default_agent.is_some()
        || args.default_model.is_some()
        || args.default_reasoning.is_some()
        || args.merge_validation_repair_attempts.is_some()
        || args.merge_validation_transient_retry_attempts.is_some()
        || args.merge_publication_retry_attempts.is_some()
        || args.default_assignee.is_some()
        || args.default_state.is_some()
        || args.default_priority.is_some()
        || !args.default_labels.is_empty()
        || args.velocity_project.is_some()
        || args.velocity_state.is_some()
        || args.velocity_auto_assign.is_some()
        || args.route.is_some()
        || args.clear_route.is_some()
        || args.route_agent.is_some()
        || args.route_model.is_some()
        || args.route_reasoning.is_some()
}

fn apply_direct_updates(view: &mut ConfigViewData, args: &ConfigArgs) -> Result<bool> {
    let before = serde_json::to_value(&view.app_config)?;

    if let Some(api_key) = &args.api_key {
        view.app_config.linear.api_key = normalize_optional(api_key);
    }
    if let Some(team) = &args.team {
        view.app_config.linear.team = normalize_optional(team);
    }
    if let Some(project_id) = &args.project_id {
        view.app_config.defaults.linear.project_id = normalize_optional(project_id);
    }
    if let Some(listen_label) = &args.listen_label {
        view.app_config.defaults.listen.required_label = normalize_optional(listen_label);
    }
    if let Some(scope) = &args.assignment_scope {
        view.app_config.defaults.listen.assignment_scope =
            Some(ListenAssignmentScope::from(*scope));
    }
    if let Some(policy) = &args.refresh_policy {
        view.app_config.defaults.listen.refresh_policy = Some(ListenRefreshPolicy::from(*policy));
    }
    if let Some(interval) = &args.poll_interval {
        view.app_config.defaults.listen.poll_interval_seconds = parse_optional_u64(
            interval,
            "listen poll interval",
            validate_listen_poll_interval_seconds,
        )?;
    }
    if let Some(limit) = &args.plan_follow_up_limit {
        view.app_config
            .defaults
            .plan
            .interactive_follow_up_questions = parse_optional_usize(
            limit,
            "plan follow-up question limit",
            validate_interactive_plan_follow_up_question_limit,
        )?;
    }
    if let Some(mode) = &args.plan_default_mode {
        view.app_config.defaults.plan.default_mode =
            parse_optional_plan_default_mode(mode, "plan default mode")?;
    }
    if let Some(single_ticket) = &args.plan_fast_single_ticket {
        view.app_config.defaults.plan.fast_single_ticket =
            parse_optional_bool(single_ticket, "fast single-ticket default")?;
    }
    if let Some(limit) = &args.plan_fast_questions {
        view.app_config.defaults.plan.fast_questions = parse_optional_usize(
            limit,
            "fast plan question limit",
            validate_fast_plan_question_limit,
        )?;
    }
    if let Some(vim_mode) = args.vim_mode {
        view.app_config.defaults.ui.vim_mode = matches!(vim_mode, VimModeArg::Enabled);
    }
    if let Some(plan_label) = &args.plan_label {
        view.app_config.defaults.issue_labels.plan = normalize_optional(plan_label);
    }
    if let Some(technical_label) = &args.technical_label {
        view.app_config.defaults.issue_labels.technical = normalize_optional(technical_label);
    }
    if let Some(default_profile) = &args.default_profile {
        let normalized = normalize_optional(default_profile);
        validate_default_profile(&view.app_config, normalized.as_deref())?;
        view.app_config.linear.default_profile = normalized;
    }
    if let Some(default_agent) = &args.default_agent {
        let normalized = normalize_agent_name(default_agent);
        validate_agent_name(&view.app_config, &normalized)?;
        view.app_config.agents.default_agent = Some(normalized.clone());
        if validate_agent_model(&normalized, view.app_config.agents.default_model.as_deref())
            .is_err()
        {
            view.app_config.agents.default_model = None;
        }
        if validate_agent_reasoning(
            &normalized,
            view.app_config.agents.default_model.as_deref(),
            view.app_config.agents.default_reasoning.as_deref(),
        )
        .is_err()
        {
            view.app_config.agents.default_reasoning = None;
        }
    }
    if let Some(default_model) = &args.default_model {
        let selected_agent = selected_global_agent(&view.app_config);
        let normalized = normalize_optional(default_model);
        validate_agent_model(&selected_agent, normalized.as_deref())?;
        view.app_config.agents.default_model = normalized;
    }
    if let Some(default_reasoning) = &args.default_reasoning {
        let selected_agent = selected_global_agent(&view.app_config);
        let normalized = normalize_optional(default_reasoning);
        validate_agent_reasoning(
            &selected_agent,
            view.app_config.agents.default_model.as_deref(),
            normalized.as_deref(),
        )?;
        view.app_config.agents.default_reasoning = normalized;
    }
    if let Some(limit) = &args.merge_validation_repair_attempts {
        view.app_config.merge.validation_repair_attempts = normalize_optional(limit)
            .map(|value| {
                value.parse::<usize>().map_err(|error| {
                    anyhow!(
                        "merge validation repair attempt limit must be a positive integer: {error}"
                    )
                })
            })
            .transpose()?;
    }
    if let Some(limit) = &args.merge_validation_transient_retry_attempts {
        view.app_config.merge.validation_transient_retry_attempts = normalize_optional(limit)
            .map(|value| {
                value.parse::<usize>().map_err(|error| {
                    anyhow!(
                        "merge transient validation retry attempt limit must be a non-negative integer: {error}"
                    )
                })
            })
            .transpose()?;
    }
    if let Some(limit) = &args.merge_publication_retry_attempts {
        view.app_config.merge.publication_retry_attempts = normalize_optional(limit)
            .map(|value| {
                value.parse::<usize>().map_err(|error| {
                    anyhow!("merge publication retry attempt limit must be at least 1: {error}")
                })
            })
            .transpose()?;
    }
    if let Some(default_assignee) = &args.default_assignee {
        view.app_config.backlog.default_assignee = normalize_optional(default_assignee);
    }
    if let Some(default_state) = &args.default_state {
        view.app_config.backlog.default_state = normalize_optional(default_state);
    }
    if let Some(default_priority) = &args.default_priority {
        view.app_config.backlog.default_priority = parse_optional_priority(default_priority)?;
    }
    if !args.default_labels.is_empty() {
        view.app_config.backlog.default_labels = parse_default_labels(&args.default_labels)?;
    }
    if let Some(project) = &args.velocity_project {
        view.app_config.backlog.velocity_defaults.project = normalize_optional(project);
    }
    if let Some(state) = &args.velocity_state {
        view.app_config.backlog.velocity_defaults.state = normalize_optional(state);
    }
    if let Some(auto_assign) = &args.velocity_auto_assign {
        view.app_config.backlog.velocity_defaults.auto_assign =
            parse_velocity_auto_assign(auto_assign)?;
    }
    apply_route_updates(&mut view.app_config, args)?;
    view.app_config.validate()?;

    let after = serde_json::to_value(&view.app_config)?;
    Ok(before != after)
}

fn validate_default_profile(app_config: &AppConfig, profile: Option<&str>) -> Result<()> {
    let Some(profile) = profile else {
        return Ok(());
    };
    if app_config.linear.profiles.contains_key(profile) {
        return Ok(());
    }
    Err(anyhow!(
        "Linear profile `{profile}` is not configured. Add it under `[linear.profiles.{profile}]` before selecting it as the default profile."
    ))
}

fn selected_global_agent(app_config: &AppConfig) -> String {
    app_config
        .agents
        .default_agent
        .clone()
        .unwrap_or_else(|| supported_agent_names()[0].to_string())
}

fn normalize_optional(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        None
    } else {
        Some(trimmed.to_string())
    }
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

fn render_route_override_summary(app_config: &AppConfig) -> String {
    format!(
        "{} family, {} command",
        app_config.agents.routing.families.len(),
        app_config.agents.routing.commands.len()
    )
}

fn apply_route_updates(app_config: &mut AppConfig, args: &ConfigArgs) -> Result<()> {
    if (args.route_agent.is_some() || args.route_model.is_some() || args.route_reasoning.is_some())
        && args.route.is_none()
    {
        return Err(anyhow!(
            "`--route-agent`, `--route-model`, and `--route-reasoning` require `--route <KEY>`"
        ));
    }
    if let Some(clear_route) = args.clear_route.as_deref() {
        let scope = route_scope_for_key(clear_route)?;
        app_config.clear_agent_route(scope, clear_route)?;
    }

    let Some(route) = args.route.as_deref() else {
        return Ok(());
    };
    if args.clear_route.is_some() {
        return Err(anyhow!(
            "`--route` cannot be combined with `--clear-route`; choose one action per invocation"
        ));
    }

    let scope = route_scope_for_key(route)?;
    let normalized = normalize_agent_route_key(scope, route)?;
    let mut updated = existing_route_config(app_config, scope, &normalized);
    if let Some(agent) = args.route_agent.as_deref() {
        updated.provider = normalize_optional(agent).map(|value| normalize_agent_name(&value));
    }
    if let Some(model) = args.route_model.as_deref() {
        updated.model = normalize_optional(model);
    }
    if let Some(reasoning) = args.route_reasoning.as_deref() {
        updated.reasoning = normalize_optional(reasoning);
    }
    let effective_provider = updated.provider.clone().or_else(|| {
        resolve_agent_route(
            app_config,
            &crate::config::PlanningMeta::default(),
            &normalized,
            crate::config::AgentConfigOverrides::default(),
        )
        .ok()
        .map(|resolved| resolved.provider)
    });
    if let Some(provider) = effective_provider.as_deref() {
        validate_agent_reasoning(
            provider,
            updated.model.as_deref(),
            updated.reasoning.as_deref(),
        )?;
    }

    let route_config = if updated.provider.is_none()
        && updated.model.is_none()
        && updated.reasoning.is_none()
    {
        return Err(anyhow!(
            "route `{normalized}` would be empty; use `--clear-route {normalized}` to remove the override"
        ));
    } else {
        updated
    };
    app_config.upsert_agent_route(scope, &normalized, route_config)?;
    Ok(())
}

fn existing_route_config(
    app_config: &AppConfig,
    scope: AgentRouteScope,
    key: &str,
) -> AgentRouteConfig {
    match scope {
        AgentRouteScope::Family => app_config
            .agents
            .routing
            .families
            .get(key)
            .cloned()
            .unwrap_or_default(),
        AgentRouteScope::Command => app_config
            .agents
            .routing
            .commands
            .get(key)
            .cloned()
            .unwrap_or_default(),
    }
}

fn route_scope_for_key(key: &str) -> Result<AgentRouteScope> {
    let normalized = normalize_agent_name(key);
    if supported_agent_route_families()
        .iter()
        .any(|candidate| *candidate == normalized)
    {
        Ok(AgentRouteScope::Family)
    } else if supported_agent_route_definitions()
        .iter()
        .any(|definition| definition.key == normalized)
    {
        Ok(AgentRouteScope::Command)
    } else {
        Err(anyhow!(
            "unknown route key `{}`; supported family keys: {}; supported command keys: {}",
            normalized,
            supported_agent_route_families().join(", "),
            supported_agent_route_definitions()
                .iter()
                .map(|definition| definition.key)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigStep {
    ApiKey,
    Team,
    ProjectId,
    ListenLabel,
    AssignmentScope,
    RefreshPolicy,
    PollInterval,
    PlanFollowUpLimit,
    PlanDefaultMode,
    PlanFastSingleTicket,
    PlanFastQuestions,
    VimMode,
    PlanLabel,
    TechnicalLabel,
    DefaultProfile,
    Agent,
    Model,
    DefaultReasoning,
    Save,
}

impl ConfigStep {
    fn all() -> [Self; 19] {
        [
            Self::ApiKey,
            Self::Team,
            Self::ProjectId,
            Self::ListenLabel,
            Self::AssignmentScope,
            Self::RefreshPolicy,
            Self::PollInterval,
            Self::PlanFollowUpLimit,
            Self::PlanDefaultMode,
            Self::PlanFastSingleTicket,
            Self::PlanFastQuestions,
            Self::VimMode,
            Self::PlanLabel,
            Self::TechnicalLabel,
            Self::DefaultProfile,
            Self::Agent,
            Self::Model,
            Self::DefaultReasoning,
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
            Self::ApiKey => "Linear API key",
            Self::Team => "Default team",
            Self::ProjectId => "Project ID",
            Self::ListenLabel => "Listen label",
            Self::AssignmentScope => "Assignee scope",
            Self::RefreshPolicy => "Refresh policy",
            Self::PollInterval => "Poll interval",
            Self::PlanFollowUpLimit => "Plan follow-ups",
            Self::PlanDefaultMode => "Plan mode",
            Self::PlanFastSingleTicket => "Fast plan shape",
            Self::PlanFastQuestions => "Fast plan questions",
            Self::VimMode => "Vim mode",
            Self::PlanLabel => "Plan label",
            Self::TechnicalLabel => "Tech label",
            Self::DefaultProfile => "Default profile",
            Self::Agent => "Default agent",
            Self::Model => "Default model",
            Self::DefaultReasoning => "Default reasoning",
            Self::Save => "Save",
        }
    }

    fn panel_label(self) -> &'static str {
        match self {
            Self::ApiKey => "Linear API key",
            Self::Team => "Default Linear team",
            Self::ProjectId => "Default Linear project ID",
            Self::ListenLabel => "Default listen label",
            Self::AssignmentScope => "Listen assignee scope",
            Self::RefreshPolicy => "Workspace refresh policy",
            Self::PollInterval => "Listen poll interval",
            Self::PlanFollowUpLimit => "Plan follow-up limit",
            Self::PlanDefaultMode => "Default plan mode",
            Self::PlanFastSingleTicket => "Fast single-ticket default",
            Self::PlanFastQuestions => "Fast follow-up batch size",
            Self::VimMode => "Install-scoped vim navigation",
            Self::PlanLabel => "Default plan label",
            Self::TechnicalLabel => "Default technical label",
            Self::DefaultProfile => "Default Linear profile",
            Self::Agent => "Default agent",
            Self::Model => "Default model",
            Self::DefaultReasoning => "Default reasoning effort",
            Self::Save => "Save configuration",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ConfigAction {
    Up,
    Down,
    Tab,
    BackTab,
    Enter,
    Esc,
}

#[derive(Debug, Clone)]
struct ConfigApp {
    keybindings: KeybindingPolicy,
    step: ConfigStep,
    api_key: InputFieldState,
    team: InputFieldState,
    project_id: InputFieldState,
    listen_label: InputFieldState,
    poll_interval: InputFieldState,
    plan_follow_up_limit: InputFieldState,
    plan_default_mode: SelectFieldState,
    plan_fast_single_ticket: SelectFieldState,
    plan_fast_questions: InputFieldState,
    vim_mode: SelectFieldState,
    plan_label: InputFieldState,
    technical_label: InputFieldState,
    assignment_scope: SelectFieldState,
    refresh_policy: SelectFieldState,
    default_profile: InputFieldState,
    default_reasoning: SelectFieldState,
    agent_field: SelectFieldState,
    model_field: SelectFieldState,
    summary_scroll: ScrollState,
    detected_agents: Vec<String>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct SubmittedConfig {
    api_key: Option<String>,
    team: Option<String>,
    project_id: Option<String>,
    listen_label: Option<String>,
    assignment_scope: ListenAssignmentScope,
    refresh_policy: ListenRefreshPolicy,
    poll_interval_seconds: Option<u64>,
    interactive_follow_up_questions: Option<usize>,
    plan_default_mode: Option<PlanDefaultMode>,
    fast_single_ticket: Option<bool>,
    fast_questions: Option<usize>,
    vim_mode: bool,
    plan_label: Option<String>,
    technical_label: Option<String>,
    default_profile: Option<String>,
    default_agent: String,
    default_model: Option<String>,
    default_reasoning: Option<String>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
enum ConfigDashboardExit {
    Cancelled,
    Submitted(SubmittedConfig),
    SubmittedRoute(AdvancedRouteSubmission),
}

impl ConfigApp {
    fn new(view: &ConfigViewData) -> Self {
        let agent_options = supported_agent_names()
            .iter()
            .map(|value| (*value).to_string())
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
        let selected_agent = selected_global_agent(&view.app_config);
        let agent_index = agent_options
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(&selected_agent))
            .unwrap_or(0);

        let assignment_scope_options = vec![
            "Any eligible issue".to_string(),
            "Viewer-assigned plus unassigned".to_string(),
        ];
        let refresh_options = vec![
            "Reuse workspace and refresh from origin/main".to_string(),
            "Recreate workspace from origin/main".to_string(),
        ];

        let mut app = Self {
            keybindings: KeybindingPolicy::new(view.app_config.vim_mode_enabled()),
            step: ConfigStep::ApiKey,
            api_key: InputFieldState::new(
                view.app_config.linear.api_key.clone().unwrap_or_default(),
            ),
            team: InputFieldState::new(view.app_config.linear.team.clone().unwrap_or_default()),
            project_id: InputFieldState::new(
                view.app_config
                    .defaults
                    .linear
                    .project_id
                    .clone()
                    .unwrap_or_default(),
            ),
            listen_label: InputFieldState::new(
                view.app_config
                    .defaults
                    .listen
                    .required_label
                    .clone()
                    .unwrap_or_default(),
            ),
            poll_interval: InputFieldState::new(
                view.app_config
                    .defaults
                    .listen
                    .poll_interval_seconds
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            ),
            plan_follow_up_limit: InputFieldState::new(
                view.app_config
                    .defaults
                    .plan
                    .interactive_follow_up_questions
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            ),
            plan_default_mode: SelectFieldState::new(
                plan_mode_options,
                match view.app_config.defaults.plan.default_mode {
                    Some(PlanDefaultMode::Normal) => 1,
                    Some(PlanDefaultMode::Fast) => 2,
                    None => 0,
                },
            ),
            plan_fast_single_ticket: SelectFieldState::new(
                fast_single_ticket_options,
                match view.app_config.defaults.plan.fast_single_ticket {
                    Some(true) => 1,
                    Some(false) => 2,
                    None => 0,
                },
            ),
            plan_fast_questions: InputFieldState::new(
                view.app_config
                    .defaults
                    .plan
                    .fast_questions
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            ),
            vim_mode: SelectFieldState::new(
                vec!["Disabled".to_string(), "Enabled".to_string()],
                usize::from(view.app_config.vim_mode_enabled()),
            ),
            plan_label: InputFieldState::new(
                view.app_config
                    .defaults
                    .issue_labels
                    .plan
                    .clone()
                    .unwrap_or_default(),
            ),
            technical_label: InputFieldState::new(
                view.app_config
                    .defaults
                    .issue_labels
                    .technical
                    .clone()
                    .unwrap_or_default(),
            ),
            assignment_scope: SelectFieldState::new(
                assignment_scope_options,
                match view
                    .app_config
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
                match view
                    .app_config
                    .defaults
                    .listen
                    .refresh_policy
                    .unwrap_or_default()
                {
                    ListenRefreshPolicy::ReuseAndRefresh => 0,
                    ListenRefreshPolicy::RecreateFromOriginMain => 1,
                },
            ),
            default_profile: InputFieldState::new(
                view.app_config
                    .linear
                    .default_profile
                    .clone()
                    .unwrap_or_default(),
            ),
            default_reasoning: SelectFieldState::new(vec!["Leave unset".to_string()], 0),
            agent_field: SelectFieldState::new(agent_options, agent_index),
            model_field: SelectFieldState::new(vec!["Leave unset".to_string()], 0),
            summary_scroll: ScrollState::default(),
            detected_agents: view.detected_agents.clone(),
            error: None,
        };
        app.sync_models(view.app_config.agents.default_model.as_deref());
        app.sync_reasoning(view.app_config.agents.default_reasoning.as_deref());
        app
    }

    fn current_agent(&self) -> &str {
        self.agent_field.selected_label().unwrap_or("codex")
    }

    fn sync_models(&mut self, preferred: Option<&str>) {
        let current = preferred
            .map(str::to_string)
            .or_else(|| self.model_field.selected_label().map(str::to_string))
            .filter(|value| value != "Leave unset");
        let mut options = vec!["Leave unset".to_string()];
        options.extend(
            supported_agent_models(self.current_agent())
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
            self.default_reasoning
                .selected_label()
                .map(str::to_string)
                .filter(|value| value != "Leave unset")
        });
        let mut options = vec!["Leave unset".to_string()];
        options.extend(
            supported_reasoning_options(
                self.current_agent(),
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
        self.default_reasoning = SelectFieldState::new(options, selected);
    }

    fn apply_action(&mut self, action: ConfigAction) -> Option<ConfigDashboardExit> {
        match action {
            ConfigAction::Tab => {
                self.step = self.step.next();
                None
            }
            ConfigAction::BackTab => {
                self.step = self.step.previous();
                None
            }
            ConfigAction::Enter => {
                if self.step == ConfigStep::Save {
                    match self.submit() {
                        Ok(submitted) => Some(ConfigDashboardExit::Submitted(submitted)),
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
            ConfigAction::Esc => Some(ConfigDashboardExit::Cancelled),
            ConfigAction::Up => {
                self.error = None;
                match self.step {
                    ConfigStep::Agent => {
                        self.agent_field.move_by(-1);
                        self.sync_models(None);
                    }
                    ConfigStep::Model => {
                        self.model_field.move_by(-1);
                        self.sync_reasoning(None);
                    }
                    ConfigStep::DefaultReasoning => self.default_reasoning.move_by(-1),
                    ConfigStep::AssignmentScope => self.assignment_scope.move_by(-1),
                    ConfigStep::RefreshPolicy => self.refresh_policy.move_by(-1),
                    ConfigStep::PlanDefaultMode => self.plan_default_mode.move_by(-1),
                    ConfigStep::PlanFastSingleTicket => self.plan_fast_single_ticket.move_by(-1),
                    ConfigStep::VimMode => {
                        self.vim_mode.move_by(-1);
                        self.keybindings = KeybindingPolicy::new(self.vim_mode.selected() == 1);
                    }
                    ConfigStep::Save => {
                        let viewport = summary_viewport(Rect::new(0, 0, 120, 32));
                        let _ = self.summary_scroll.apply_key_code_in_viewport(
                            KeyCode::Up,
                            viewport,
                            self.summary_content_rows(viewport.width),
                        );
                    }
                    _ => self.step = self.step.previous(),
                }
                None
            }
            ConfigAction::Down => {
                self.error = None;
                match self.step {
                    ConfigStep::Agent => {
                        self.agent_field.move_by(1);
                        self.sync_models(None);
                    }
                    ConfigStep::Model => {
                        self.model_field.move_by(1);
                        self.sync_reasoning(None);
                    }
                    ConfigStep::DefaultReasoning => self.default_reasoning.move_by(1),
                    ConfigStep::AssignmentScope => self.assignment_scope.move_by(1),
                    ConfigStep::RefreshPolicy => self.refresh_policy.move_by(1),
                    ConfigStep::PlanDefaultMode => self.plan_default_mode.move_by(1),
                    ConfigStep::PlanFastSingleTicket => self.plan_fast_single_ticket.move_by(1),
                    ConfigStep::VimMode => {
                        self.vim_mode.move_by(1);
                        self.keybindings = KeybindingPolicy::new(self.vim_mode.selected() == 1);
                    }
                    ConfigStep::Save => {
                        let viewport = summary_viewport(Rect::new(0, 0, 120, 32));
                        let _ = self.summary_scroll.apply_key_code_in_viewport(
                            KeyCode::Down,
                            viewport,
                            self.summary_content_rows(viewport.width),
                        );
                    }
                    _ => self.step = self.step.next(),
                }
                None
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent, summary_viewport: Rect) -> Option<ConfigDashboardExit> {
        if self.select_step_active()
            && let Some(delta) = self.keybindings.vertical_delta(key)
        {
            return self.apply_action(if delta < 0 {
                ConfigAction::Up
            } else {
                ConfigAction::Down
            });
        }

        match key.code {
            KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
                if self.step == ConfigStep::Save =>
            {
                let _ = self.summary_scroll.apply_key_code_in_viewport(
                    key.code,
                    summary_viewport,
                    self.summary_content_rows(summary_viewport.width),
                );
                None
            }
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Left | KeyCode::Right => {
                self.error = None;
                match self.step {
                    ConfigStep::ApiKey => {
                        let _ = self.api_key.handle_key(key);
                    }
                    ConfigStep::Team => {
                        let _ = self.team.handle_key(key);
                    }
                    ConfigStep::ProjectId => {
                        let _ = self.project_id.handle_key(key);
                    }
                    ConfigStep::ListenLabel => {
                        let _ = self.listen_label.handle_key(key);
                    }
                    ConfigStep::PollInterval => {
                        let _ = self.poll_interval.handle_key(key);
                    }
                    ConfigStep::PlanFollowUpLimit => {
                        let _ = self.plan_follow_up_limit.handle_key(key);
                    }
                    ConfigStep::PlanFastQuestions => {
                        let _ = self.plan_fast_questions.handle_key(key);
                    }
                    ConfigStep::VimMode => {}
                    ConfigStep::PlanLabel => {
                        let _ = self.plan_label.handle_key(key);
                    }
                    ConfigStep::TechnicalLabel => {
                        let _ = self.technical_label.handle_key(key);
                    }
                    ConfigStep::DefaultProfile => {
                        let _ = self.default_profile.handle_key(key);
                    }
                    ConfigStep::AssignmentScope
                    | ConfigStep::RefreshPolicy
                    | ConfigStep::PlanDefaultMode
                    | ConfigStep::PlanFastSingleTicket
                    | ConfigStep::DefaultReasoning
                    | ConfigStep::Agent
                    | ConfigStep::Model
                    | ConfigStep::Save => {}
                }
                None
            }
            KeyCode::Up => self.apply_action(ConfigAction::Up),
            KeyCode::Down => self.apply_action(ConfigAction::Down),
            KeyCode::Tab => self.apply_action(ConfigAction::Tab),
            KeyCode::BackTab => self.apply_action(ConfigAction::BackTab),
            KeyCode::Enter => self.apply_action(ConfigAction::Enter),
            KeyCode::Esc => self.apply_action(ConfigAction::Esc),
            _ => None,
        }
    }

    fn handle_summary_mouse(&mut self, mouse: MouseEvent, summary_viewport: Rect) -> bool {
        if self.step != ConfigStep::Save {
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
            ConfigStep::AssignmentScope
                | ConfigStep::RefreshPolicy
                | ConfigStep::PlanDefaultMode
                | ConfigStep::PlanFastSingleTicket
                | ConfigStep::VimMode
                | ConfigStep::Agent
                | ConfigStep::Model
                | ConfigStep::DefaultReasoning
        )
    }

    fn handle_paste(&mut self, text: &str) {
        self.error = None;
        match self.step {
            ConfigStep::ApiKey => {
                let _ = self.api_key.paste(text);
            }
            ConfigStep::Team => {
                let _ = self.team.paste(text);
            }
            ConfigStep::ProjectId => {
                let _ = self.project_id.paste(text);
            }
            ConfigStep::ListenLabel => {
                let _ = self.listen_label.paste(text);
            }
            ConfigStep::PollInterval => {
                let _ = self.poll_interval.paste(text);
            }
            ConfigStep::PlanFollowUpLimit => {
                let _ = self.plan_follow_up_limit.paste(text);
            }
            ConfigStep::PlanFastQuestions => {
                let _ = self.plan_fast_questions.paste(text);
            }
            ConfigStep::VimMode => {}
            ConfigStep::PlanLabel => {
                let _ = self.plan_label.paste(text);
            }
            ConfigStep::TechnicalLabel => {
                let _ = self.technical_label.paste(text);
            }
            ConfigStep::DefaultProfile => {
                let _ = self.default_profile.paste(text);
            }
            ConfigStep::AssignmentScope
            | ConfigStep::RefreshPolicy
            | ConfigStep::PlanDefaultMode
            | ConfigStep::PlanFastSingleTicket
            | ConfigStep::Agent
            | ConfigStep::Model
            | ConfigStep::DefaultReasoning
            | ConfigStep::Save => {}
        }
    }

    fn summary_text(&self, width: u16) -> Text<'static> {
        Text::from(summary_lines(
            width,
            &[
                ("Linear API key", summarize_secret(&self.api_key)),
                ("Default team", summarize_optional_value(&self.team)),
                ("Project ID", summarize_optional_value(&self.project_id)),
                ("Listen label", summarize_optional_value(&self.listen_label)),
                (
                    "Assignee scope",
                    summarize_optional_select(&self.assignment_scope, ""),
                ),
                (
                    "Refresh policy",
                    summarize_optional_select(&self.refresh_policy, ""),
                ),
                (
                    "Poll interval",
                    summarize_optional_value(&self.poll_interval),
                ),
                (
                    "Plan follow-ups",
                    summarize_optional_value(&self.plan_follow_up_limit),
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
                    summarize_optional_value(&self.plan_fast_questions),
                ),
                (
                    "Vim mode",
                    self.vim_mode
                        .selected_label()
                        .unwrap_or("Disabled")
                        .to_string(),
                ),
                ("Plan label", summarize_optional_value(&self.plan_label)),
                (
                    "Tech label",
                    summarize_optional_value(&self.technical_label),
                ),
                (
                    "Default profile",
                    summarize_optional_value(&self.default_profile),
                ),
                (
                    "Default agent",
                    self.agent_field
                        .selected_label()
                        .unwrap_or("unset")
                        .to_string(),
                ),
                (
                    "Default model",
                    self.model_field
                        .selected_label()
                        .unwrap_or("Leave unset")
                        .to_string(),
                ),
                (
                    "Default reasoning",
                    summarize_optional_select(&self.default_reasoning, "Leave unset"),
                ),
                (
                    "Detected agents",
                    if self.detected_agents.is_empty() {
                        "none".to_string()
                    } else {
                        self.detected_agents.join(", ")
                    },
                ),
            ],
        ))
    }

    fn summary_content_rows(&self, width: u16) -> usize {
        wrapped_rows(&plain_text(&self.summary_text(width.max(1))), width.max(1))
    }

    fn submit(&self) -> Result<SubmittedConfig> {
        let default_agent = normalize_agent_name(self.current_agent());
        let default_model = match self.model_field.selected() {
            0 => None,
            _ => self.model_field.selected_label().map(str::to_string),
        };
        validate_agent_name(&AppConfig::load()?, &default_agent)?;
        validate_agent_model(&default_agent, default_model.as_deref())?;
        let default_reasoning = match self.default_reasoning.selected() {
            0 => None,
            _ => self.default_reasoning.selected_label().map(str::to_string),
        };
        validate_agent_reasoning(
            &default_agent,
            default_model.as_deref(),
            default_reasoning.as_deref(),
        )?;
        let app_config = AppConfig::load()?;
        let default_profile = normalize_optional(self.default_profile.value());
        validate_default_profile(&app_config, default_profile.as_deref())?;

        let poll_interval_seconds = parse_optional_u64(
            self.poll_interval.value(),
            "listen poll interval",
            validate_listen_poll_interval_seconds,
        )?;
        let interactive_follow_up_questions = parse_optional_usize(
            self.plan_follow_up_limit.value(),
            "plan follow-up question limit",
            validate_interactive_plan_follow_up_question_limit,
        )?;
        let fast_questions = parse_optional_usize(
            self.plan_fast_questions.value(),
            "fast plan question limit",
            validate_fast_plan_question_limit,
        )?;

        Ok(SubmittedConfig {
            api_key: normalize_optional(self.api_key.value()),
            team: normalize_optional(self.team.value()),
            project_id: normalize_optional(self.project_id.value()),
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
            fast_questions,
            vim_mode: self.vim_mode.selected() == 1,
            plan_label: normalize_optional(self.plan_label.value()),
            technical_label: normalize_optional(self.technical_label.value()),
            default_profile,
            default_agent,
            default_model,
            default_reasoning,
        })
    }
}

impl SubmittedConfig {
    fn apply(&self, view: &mut ConfigViewData) -> Result<()> {
        validate_default_profile(&view.app_config, self.default_profile.as_deref())?;
        validate_agent_name(&view.app_config, &self.default_agent)?;
        validate_agent_model(&self.default_agent, self.default_model.as_deref())?;
        validate_agent_reasoning(
            &self.default_agent,
            self.default_model.as_deref(),
            self.default_reasoning.as_deref(),
        )?;
        view.app_config.linear.api_key = self.api_key.clone();
        view.app_config.linear.team = self.team.clone();
        view.app_config.defaults.linear.project_id = self.project_id.clone();
        view.app_config.defaults.listen.required_label = self.listen_label.clone();
        view.app_config.defaults.listen.assignment_scope = Some(self.assignment_scope);
        view.app_config.defaults.listen.refresh_policy = Some(self.refresh_policy);
        view.app_config.defaults.listen.poll_interval_seconds = self.poll_interval_seconds;
        view.app_config
            .defaults
            .plan
            .interactive_follow_up_questions = self.interactive_follow_up_questions;
        view.app_config.defaults.plan.default_mode = self.plan_default_mode;
        view.app_config.defaults.plan.fast_single_ticket = self.fast_single_ticket;
        view.app_config.defaults.plan.fast_questions = self.fast_questions;
        view.app_config.defaults.ui.vim_mode = self.vim_mode;
        view.app_config.defaults.issue_labels.plan = self.plan_label.clone();
        view.app_config.defaults.issue_labels.technical = self.technical_label.clone();
        view.app_config.linear.default_profile = self.default_profile.clone();
        view.app_config.agents.default_agent = Some(self.default_agent.clone());
        view.app_config.agents.default_model = self.default_model.clone();
        view.app_config.agents.default_reasoning = self.default_reasoning.clone();
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdvancedRoutingStep {
    Route,
    Agent,
    Model,
    Reasoning,
    Save,
}

impl AdvancedRoutingStep {
    fn all() -> [Self; 5] {
        [
            Self::Route,
            Self::Agent,
            Self::Model,
            Self::Reasoning,
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
            Self::Route => "Route",
            Self::Agent => "Agent override",
            Self::Model => "Model override",
            Self::Reasoning => "Reasoning override",
            Self::Save => "Save",
        }
    }
}

#[derive(Debug, Clone)]
struct RouteEntry {
    scope: AgentRouteScope,
    key: String,
    label: String,
}

#[derive(Debug, Clone)]
struct AdvancedRoutingApp {
    step: AdvancedRoutingStep,
    route_entries: Vec<RouteEntry>,
    route_field: SelectFieldState,
    agent_field: SelectFieldState,
    model_field: SelectFieldState,
    reasoning: SelectFieldState,
    agent_options: Vec<String>,
    app_config: AppConfig,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct AdvancedRouteSubmission {
    scope: AgentRouteScope,
    key: String,
    config: Option<AgentRouteConfig>,
}

impl AdvancedRouteSubmission {
    fn apply(&self, view: &mut ConfigViewData) -> Result<()> {
        match &self.config {
            Some(config) => {
                view.app_config
                    .upsert_agent_route(self.scope, &self.key, config.clone())?
            }
            None => {
                view.app_config.clear_agent_route(self.scope, &self.key)?;
            }
        }
        Ok(())
    }
}

impl AdvancedRoutingApp {
    fn new(view: &ConfigViewData) -> Result<Self> {
        let route_entries = route_entries();
        let route_options = route_entries
            .iter()
            .map(|entry| entry.label.clone())
            .collect::<Vec<_>>();
        let mut agent_options = vec!["Inherit".to_string()];
        agent_options.extend(route_agent_names(view));
        let mut app = Self {
            step: AdvancedRoutingStep::Route,
            route_entries,
            route_field: SelectFieldState::new(route_options, 0),
            agent_field: SelectFieldState::new(agent_options.clone(), 0),
            model_field: SelectFieldState::new(vec!["Inherit".to_string()], 0),
            reasoning: SelectFieldState::new(vec!["Inherit".to_string()], 0),
            agent_options,
            app_config: view.app_config.clone(),
            error: None,
        };
        app.sync_from_selected_route()?;
        Ok(app)
    }

    fn selected_entry(&self) -> &RouteEntry {
        &self.route_entries[self.route_field.selected()]
    }

    fn selected_agent_override(&self) -> Option<String> {
        match self.agent_field.selected_label() {
            Some("Inherit") | None => None,
            Some(value) => Some(normalize_agent_name(value)),
        }
    }

    fn effective_provider_for_selected_route(&self) -> Result<String> {
        Ok(resolve_agent_route(
            &self.app_config,
            &crate::config::PlanningMeta::default(),
            &self.selected_entry().key,
            crate::config::AgentConfigOverrides::default(),
        )?
        .provider)
    }

    fn sync_from_selected_route(&mut self) -> Result<()> {
        let entry = self.selected_entry().clone();
        let route_config = existing_route_config(&self.app_config, entry.scope, &entry.key);
        let agent_index = route_config
            .provider
            .as_deref()
            .and_then(|value| {
                self.agent_options
                    .iter()
                    .position(|candidate| candidate.eq_ignore_ascii_case(value))
            })
            .unwrap_or(0);
        self.agent_field = SelectFieldState::new(self.agent_options.clone(), agent_index);
        self.sync_models(route_config.model.as_deref())?;
        self.sync_reasoning(route_config.reasoning.as_deref())?;
        Ok(())
    }

    fn sync_models(&mut self, preferred: Option<&str>) -> Result<()> {
        let provider = self
            .selected_agent_override()
            .map(Ok)
            .unwrap_or_else(|| self.effective_provider_for_selected_route())
            .ok();
        let mut options = vec!["Inherit".to_string()];
        if let Some(provider) = provider.as_deref() {
            options.extend(
                supported_agent_models(provider)
                    .iter()
                    .map(|value| (*value).to_string()),
            );
        }
        let selected = preferred
            .and_then(|value| {
                options
                    .iter()
                    .position(|candidate| candidate.eq_ignore_ascii_case(value))
            })
            .unwrap_or(0);
        self.model_field = SelectFieldState::new(options, selected);
        self.sync_reasoning(None)?;
        Ok(())
    }

    fn sync_reasoning(&mut self, preferred: Option<&str>) -> Result<()> {
        let provider = self
            .selected_agent_override()
            .map(Ok)
            .unwrap_or_else(|| self.effective_provider_for_selected_route())
            .ok();
        let current = preferred.map(str::to_string).or_else(|| {
            self.reasoning
                .selected_label()
                .map(str::to_string)
                .filter(|value| value != "Inherit")
        });
        let mut options = vec!["Inherit".to_string()];
        if let Some(provider) = provider.as_deref() {
            options.extend(
                supported_reasoning_options(
                    provider,
                    match self.model_field.selected() {
                        0 => None,
                        _ => self.model_field.selected_label(),
                    },
                )
                .iter()
                .map(|value| (*value).to_string()),
            );
        }
        let selected = current
            .as_deref()
            .and_then(|value| {
                options
                    .iter()
                    .position(|candidate| candidate.eq_ignore_ascii_case(value))
            })
            .unwrap_or(0);
        self.reasoning = SelectFieldState::new(options, selected);
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<ConfigDashboardExit> {
        match key.code {
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Left | KeyCode::Right => None,
            KeyCode::Up => self.apply_action(ConfigAction::Up),
            KeyCode::Down => self.apply_action(ConfigAction::Down),
            KeyCode::Tab => self.apply_action(ConfigAction::Tab),
            KeyCode::BackTab => self.apply_action(ConfigAction::BackTab),
            KeyCode::Enter => self.apply_action(ConfigAction::Enter),
            KeyCode::Esc => self.apply_action(ConfigAction::Esc),
            _ => None,
        }
    }

    fn handle_paste(&mut self, _text: &str) {}

    fn apply_action(&mut self, action: ConfigAction) -> Option<ConfigDashboardExit> {
        match action {
            ConfigAction::Tab => {
                self.step = self.step.next();
                None
            }
            ConfigAction::BackTab => {
                self.step = self.step.previous();
                None
            }
            ConfigAction::Esc => Some(ConfigDashboardExit::Cancelled),
            ConfigAction::Enter => {
                if self.step == AdvancedRoutingStep::Save {
                    match self.submit() {
                        Ok(submitted) => Some(ConfigDashboardExit::SubmittedRoute(submitted)),
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
            ConfigAction::Up => {
                self.error = None;
                match self.step {
                    AdvancedRoutingStep::Route => {
                        self.route_field.move_by(-1);
                        if let Err(error) = self.sync_from_selected_route() {
                            self.error = Some(error.to_string());
                        }
                    }
                    AdvancedRoutingStep::Agent => {
                        self.agent_field.move_by(-1);
                        if let Err(error) = self.sync_models(None) {
                            self.error = Some(error.to_string());
                        }
                    }
                    AdvancedRoutingStep::Model => {
                        self.model_field.move_by(-1);
                        if let Err(error) = self.sync_reasoning(None) {
                            self.error = Some(error.to_string());
                        }
                    }
                    AdvancedRoutingStep::Reasoning => self.reasoning.move_by(-1),
                    AdvancedRoutingStep::Save => {
                        self.step = self.step.previous();
                    }
                }
                None
            }
            ConfigAction::Down => {
                self.error = None;
                match self.step {
                    AdvancedRoutingStep::Route => {
                        self.route_field.move_by(1);
                        if let Err(error) = self.sync_from_selected_route() {
                            self.error = Some(error.to_string());
                        }
                    }
                    AdvancedRoutingStep::Agent => {
                        self.agent_field.move_by(1);
                        if let Err(error) = self.sync_models(None) {
                            self.error = Some(error.to_string());
                        }
                    }
                    AdvancedRoutingStep::Model => {
                        self.model_field.move_by(1);
                        if let Err(error) = self.sync_reasoning(None) {
                            self.error = Some(error.to_string());
                        }
                    }
                    AdvancedRoutingStep::Reasoning => self.reasoning.move_by(1),
                    AdvancedRoutingStep::Save => {
                        self.step = self.step.next();
                    }
                }
                None
            }
        }
    }

    fn submit(&self) -> Result<AdvancedRouteSubmission> {
        let entry = self.selected_entry();
        let provider = self.selected_agent_override();
        let model = match self.model_field.selected_label() {
            Some("Inherit") | None => None,
            Some(value) => Some(value.to_string()),
        };
        let reasoning = match self.reasoning.selected_label() {
            Some("Inherit") | None => None,
            Some(value) => Some(value.to_string()),
        };
        if let Some(provider) = provider.as_deref() {
            validate_agent_name(&self.app_config, provider)?;
        }
        let effective_provider = provider
            .clone()
            .or_else(|| self.effective_provider_for_selected_route().ok());
        if let Some(model) = model.as_deref()
            && let Some(provider) = effective_provider.as_deref()
        {
            validate_agent_model(provider, Some(model))?;
        }
        if let Some(provider) = effective_provider.as_deref() {
            validate_agent_reasoning(provider, model.as_deref(), reasoning.as_deref())?;
        }
        let config = AgentRouteConfig {
            provider,
            model,
            reasoning,
        };
        let config =
            if config.provider.is_none() && config.model.is_none() && config.reasoning.is_none() {
                None
            } else {
                Some(config)
            };
        Ok(AdvancedRouteSubmission {
            scope: entry.scope,
            key: entry.key.clone(),
            config,
        })
    }
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
        .map_err(|_| anyhow!("{label} must be a whole number"))?;
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
        .map_err(|_| anyhow!("{label} must be a whole number"))?;
    validate(parsed)?;
    Ok(Some(parsed))
}

fn render_once(app: ConfigApp, args: &ConfigArgs) -> Result<String> {
    let backend = TestBackend::new(args.width, args.height);
    let mut terminal = Terminal::new(backend)?;
    let mut app = app;

    for action in args.events.iter().copied().map(ConfigAction::from) {
        if app.apply_action(action).is_some() {
            break;
        }
    }

    terminal.draw(|frame| render_config_dashboard(frame, &app))?;
    Ok(snapshot(terminal.backend()))
}

fn run_config_dashboard(app: ConfigApp) -> Result<ConfigDashboardExit> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = app;

    loop {
        terminal.draw(|frame| render_config_dashboard(frame, &app))?;

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

fn render_advanced_once(app: AdvancedRoutingApp, args: &ConfigArgs) -> Result<String> {
    let backend = TestBackend::new(args.width, args.height);
    let mut terminal = Terminal::new(backend)?;
    let mut app = app;

    for action in args.events.iter().copied().map(ConfigAction::from) {
        if app.apply_action(action).is_some() {
            break;
        }
    }

    terminal.draw(|frame| render_advanced_routing_dashboard(frame, &app))?;
    Ok(snapshot(terminal.backend()))
}

fn run_advanced_routing_dashboard(app: AdvancedRoutingApp) -> Result<ConfigDashboardExit> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = app;

    loop {
        terminal.draw(|frame| render_advanced_routing_dashboard(frame, &app))?;

        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(exit) = app.handle_key(key) {
                        return Ok(exit);
                    }
                }
                Event::Paste(text) => app.handle_paste(&text),
                _ => {}
            }
        }
    }
}

fn route_entries() -> Vec<RouteEntry> {
    let mut entries = supported_agent_route_families()
        .into_iter()
        .map(|family| RouteEntry {
            scope: AgentRouteScope::Family,
            key: family.to_string(),
            label: format!("Family: {family}"),
        })
        .collect::<Vec<_>>();
    entries.extend(
        supported_agent_route_definitions()
            .iter()
            .map(|definition| RouteEntry {
                scope: AgentRouteScope::Command,
                key: definition.key.to_string(),
                label: format!("Command: {} ({})", definition.key, definition.label),
            }),
    );
    entries
}

fn route_agent_names(view: &ConfigViewData) -> Vec<String> {
    let mut names = BTreeMap::new();
    for name in supported_agent_names() {
        names.insert((*name).to_string(), ());
    }
    for name in view.app_config.agents.commands.keys() {
        names.insert(normalize_agent_name(name), ());
    }
    for name in &view.detected_agents {
        names.insert(normalize_agent_name(name), ());
    }
    names.into_keys().collect()
}

fn render_advanced_routing_dashboard(frame: &mut Frame<'_>, app: &AdvancedRoutingApp) {
    let area = frame.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(0),
            Constraint::Length(4),
        ])
        .split(area);
    let header = Paragraph::new(Text::from(vec![
        Line::from("Advanced Agent Routing"),
        Line::from(
            "Set family-level and command-level agent defaults with effective inheritance previews.",
        ),
        Line::from("Primary config stays simple; this mode is the explicit per-command dashboard."),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("meta runtime config --advanced-routing"),
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(header, layout[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(28),
            Constraint::Min(32),
            Constraint::Length(46),
        ])
        .split(layout[1]);
    render_advanced_step_list(frame, app, body[0]);
    render_advanced_step_panel(frame, app, body[1]);
    render_advanced_summary_panel(frame, app, body[2]);
    render_advanced_footer(frame, app, layout[2]);
}

fn render_advanced_step_list(frame: &mut Frame<'_>, app: &AdvancedRoutingApp, area: Rect) {
    let lines = AdvancedRoutingStep::all()
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let selected = index == app.step.index();
            let marker = if selected { "> " } else { "  " };
            let style = if selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(
                format!("{marker}{}. {}", index + 1, step.label()),
                style,
            ))
        })
        .collect::<Vec<_>>();
    let paragraph = Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::ALL).title("Steps"))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_advanced_step_panel(frame: &mut Frame<'_>, app: &AdvancedRoutingApp, area: Rect) {
    let title = format!(
        "Step {} of {}: {}",
        app.step.index() + 1,
        AdvancedRoutingStep::all().len(),
        app.step.label()
    );
    match app.step {
        AdvancedRoutingStep::Route => render_select_panel(frame, area, &title, &app.route_field),
        AdvancedRoutingStep::Agent => render_select_panel(frame, area, &title, &app.agent_field),
        AdvancedRoutingStep::Model => render_select_panel(frame, area, &title, &app.model_field),
        AdvancedRoutingStep::Reasoning => render_select_panel(frame, area, &title, &app.reasoning),
        AdvancedRoutingStep::Save => render_save_panel(frame, area),
    }
}

fn render_advanced_summary_panel(frame: &mut Frame<'_>, app: &AdvancedRoutingApp, area: Rect) {
    let selected_key = &app.selected_entry().key;
    let mut lines = vec![
        Line::from(Span::styled(
            format!("Selected: {selected_key}"),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(String::new()),
    ];
    for entry in route_entries() {
        let line = summarize_route_line(&app.app_config, &entry);
        lines.push(Line::from(line));
    }
    let paragraph = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Effective routes"),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_advanced_footer(frame: &mut Frame<'_>, app: &AdvancedRoutingApp, area: Rect) {
    let controls = match app.step {
        AdvancedRoutingStep::Reasoning => {
            "Use Up/Down to choose the override. Enter advances. Esc cancels."
        }
        AdvancedRoutingStep::Save => {
            "Press Enter to save. Choosing Inherit reasoning clears the selected override."
        }
        _ => "Use Up/Down to choose. Enter or Tab advances. Shift+Tab goes back. Esc cancels.",
    };
    let status = app.error.as_deref().unwrap_or("Ready.");
    let footer = Paragraph::new(Text::from(vec![Line::from(controls), Line::from(status)]))
        .block(Block::default().borders(Borders::ALL).title("Controls"))
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, area);
}

fn summarize_route_line(app_config: &AppConfig, entry: &RouteEntry) -> String {
    match entry.scope {
        AgentRouteScope::Family => {
            let route_text = app_config
                .agents
                .routing
                .families
                .get(&entry.key)
                .map(summarize_raw_route_config)
                .unwrap_or_else(|| "inherit".to_string());
            let source = if app_config.agents.routing.families.contains_key(&entry.key) {
                format!("family:{}", entry.key)
            } else if app_config.agents.default_agent.is_some() {
                "global".to_string()
            } else {
                "unset".to_string()
            };
            format!("{} -> {} ({source})", entry.key, route_text)
        }
        AgentRouteScope::Command => {
            let resolved = resolve_agent_route(
                app_config,
                &crate::config::PlanningMeta::default(),
                &entry.key,
                crate::config::AgentConfigOverrides::default(),
            );
            match resolved {
                Ok(route) => format!(
                    "{} -> {} / {} ({})",
                    entry.key,
                    route.provider,
                    route.model.as_deref().unwrap_or("inherit"),
                    summarize_route_source(&route.provider_source)
                ),
                Err(error) => format!("{} -> error: {}", entry.key, error),
            }
        }
    }
}

fn summarize_raw_route_config(config: &AgentRouteConfig) -> String {
    let provider = config.provider.as_deref().unwrap_or("inherit");
    let model = config.model.as_deref().unwrap_or("inherit");
    let reasoning = config.reasoning.as_deref().unwrap_or("inherit");
    format!("{provider} / {model} / {reasoning}")
}

fn summarize_route_source(source: &AgentConfigSource) -> String {
    match source {
        AgentConfigSource::ExplicitOverride => "explicit".to_string(),
        AgentConfigSource::RepoDefault => "repo".to_string(),
        AgentConfigSource::CommandRoute(key) => format!("command:{key}"),
        AgentConfigSource::FamilyRoute(key) => format!("family:{key}"),
        AgentConfigSource::GlobalDefault => "global".to_string(),
    }
}

/// Minimum column width to show every step label without wrapping (single-column mode).
/// Accounts for `"> XX. Label"` prefix plus two border characters.
fn config_step_column_width() -> u16 {
    let steps = ConfigStep::all();
    let max_label = steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let digits = if i + 1 >= 10 { 2 } else { 1 };
            // "> " (2) + digits + ". " (2) + label
            2 + digits + 2 + s.label().len()
        })
        .max()
        .unwrap_or(20);
    // +2 for left/right border
    (max_label + 2) as u16
}

fn render_config_dashboard(frame: &mut Frame<'_>, app: &ConfigApp) {
    let area = frame.area();
    let header_height = if area.width >= 110 { 5 } else { 6 };
    let footer_height = if area.width >= 96 { 4 } else { 5 };
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(0),
            Constraint::Length(footer_height),
        ])
        .split(area);

    let header = Paragraph::new(Text::from(vec![
        Line::from("Meta Config"),
        Line::from(
            "Configure install-scoped Linear auth plus default agent settings shared across repositories.",
        ),
        Line::from(format!(
            "Detected supported agents on PATH: {}",
            if app.detected_agents.is_empty() {
                "none".to_string()
            } else {
                app.detected_agents.join(", ")
            }
        )),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Global configuration"),
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(header, layout[0]);

    let step_col = config_step_column_width();
    let body_area = layout[1];
    if body_area.width >= 118 {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(step_col),
                Constraint::Min(34),
                Constraint::Length(48),
            ])
            .split(body_area);
        render_step_list(frame, app, body[0], 1);
        render_step_panel(frame, app, body[1]);
        render_summary_panel(frame, app, body[2]);
    } else if body_area.width >= 90 {
        let sidebar_width = step_col.max(36);
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar_width), Constraint::Min(40)])
            .split(body_area);
        let sidebar = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(17), Constraint::Min(10)])
            .split(body[0]);
        render_step_list(frame, app, sidebar[0], 1);
        render_summary_panel(frame, app, sidebar[1]);
        render_step_panel(frame, app, body[1]);
    } else {
        let stacked = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(10),
                Constraint::Min(8),
                Constraint::Length(18),
            ])
            .split(body_area);
        render_step_list(frame, app, stacked[0], 2);
        render_step_panel(frame, app, stacked[1]);
        render_summary_panel(frame, app, stacked[2]);
    }
    render_footer(frame, app, layout[2]);
}

fn render_step_list(frame: &mut Frame<'_>, app: &ConfigApp, area: Rect, columns: usize) {
    let block = Block::default().borders(Borders::ALL).title("Steps");
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
    let steps = ConfigStep::all();
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
                Line::from(Span::styled(
                    format!("{marker}{}. {}", index + 1, step.label()),
                    style,
                ))
            })
            .collect::<Vec<_>>();
        let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, *chunk);
    }
}

fn render_step_panel(frame: &mut Frame<'_>, app: &ConfigApp, area: Rect) {
    let title = if area.width < 60 {
        app.step.panel_label().to_string()
    } else {
        format!(
            "Step {} of {}: {}",
            app.step.index() + 1,
            ConfigStep::all().len(),
            app.step.panel_label()
        )
    };

    match app.step {
        ConfigStep::ApiKey => render_input_panel(
            frame,
            area,
            &title,
            &app.api_key,
            "Paste the install-scoped Linear API key or leave blank to unset it.",
        ),
        ConfigStep::Team => render_input_panel(
            frame,
            area,
            &title,
            &app.team,
            "Optional default Linear team key used when a command does not set one explicitly.",
        ),
        ConfigStep::ProjectId => render_input_panel(
            frame,
            area,
            &title,
            &app.project_id,
            "Optional canonical Linear project ID (leave blank to unset).",
        ),
        ConfigStep::ListenLabel => render_input_panel(
            frame,
            area,
            &title,
            &app.listen_label,
            "Label that gates which Todo tickets `meta listen` picks up.",
        ),
        ConfigStep::AssignmentScope => {
            render_select_panel(frame, area, &title, &app.assignment_scope)
        }
        ConfigStep::RefreshPolicy => render_select_panel(frame, area, &title, &app.refresh_policy),
        ConfigStep::PollInterval => render_input_panel(
            frame,
            area,
            &title,
            &app.poll_interval,
            "Poll interval in seconds for `meta listen` (e.g. 7).",
        ),
        ConfigStep::PlanFollowUpLimit => render_input_panel(
            frame,
            area,
            &title,
            &app.plan_follow_up_limit,
            "Max follow-up questions for interactive `meta backlog plan` (e.g. 10).",
        ),
        ConfigStep::PlanDefaultMode => {
            render_select_panel(frame, area, &title, &app.plan_default_mode)
        }
        ConfigStep::PlanFastSingleTicket => {
            render_select_panel(frame, area, &title, &app.plan_fast_single_ticket)
        }
        ConfigStep::PlanFastQuestions => render_input_panel(
            frame,
            area,
            &title,
            &app.plan_fast_questions,
            "Max follow-up questions in the one-round fast planning Q&A (0-10).",
        ),
        ConfigStep::VimMode => render_select_panel(frame, area, &title, &app.vim_mode),
        ConfigStep::PlanLabel => render_input_panel(
            frame,
            area,
            &title,
            &app.plan_label,
            "Default label for plan issues (e.g. plan).",
        ),
        ConfigStep::TechnicalLabel => render_input_panel(
            frame,
            area,
            &title,
            &app.technical_label,
            "Default label for technical issues (e.g. technical).",
        ),
        ConfigStep::DefaultProfile => render_input_panel(
            frame,
            area,
            &title,
            &app.default_profile,
            "Optional default Linear profile name. The profile must already exist under [linear.profiles.<name>].",
        ),
        ConfigStep::Agent => render_select_panel(frame, area, &title, &app.agent_field),
        ConfigStep::Model => render_select_panel(frame, area, &title, &app.model_field),
        ConfigStep::DefaultReasoning => {
            render_select_panel(frame, area, &title, &app.default_reasoning)
        }
        ConfigStep::Save => render_save_panel(frame, area),
    }
}

fn render_summary_panel(frame: &mut Frame<'_>, app: &ConfigApp, area: Rect) {
    let active = app.step == ConfigStep::Save;
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

fn render_footer(frame: &mut Frame<'_>, app: &ConfigApp, area: Rect) {
    let controls = match app.step {
        ConfigStep::ApiKey
        | ConfigStep::Team
        | ConfigStep::ProjectId
        | ConfigStep::ListenLabel
        | ConfigStep::PollInterval
        | ConfigStep::PlanFollowUpLimit
        | ConfigStep::PlanFastQuestions
        | ConfigStep::PlanLabel
        | ConfigStep::TechnicalLabel
        | ConfigStep::DefaultProfile => {
            "Type or paste the value. Enter or Tab advances. Shift+Tab goes back. Esc cancels."
        }
        ConfigStep::AssignmentScope
        | ConfigStep::RefreshPolicy
        | ConfigStep::PlanDefaultMode
        | ConfigStep::PlanFastSingleTicket
        | ConfigStep::VimMode
        | ConfigStep::Agent
        | ConfigStep::Model
        | ConfigStep::DefaultReasoning => {
            "Use Up/Down to choose. Enter or Tab advances. Shift+Tab goes back. Esc cancels."
        }
        ConfigStep::Save => {
            "Review the summary. Up/Down and PgUp/PgDn/Home/End or the mouse wheel scroll. Enter saves. Shift+Tab goes back. Esc cancels."
        }
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
        Line::from("Review the summary and press Enter to save the install-scoped configuration."),
        Line::from("Repo defaults now live under `meta runtime setup`."),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Save"))
    .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn summarize_secret(field: &InputFieldState) -> String {
    mask_secret(normalize_optional(field.value()).as_deref())
}

fn summarize_optional_value(field: &InputFieldState) -> String {
    normalize_optional(field.value()).unwrap_or_else(|| "unset".to_string())
}

fn summarize_optional_select(field: &SelectFieldState, unset_label: &str) -> String {
    match field.selected_label() {
        Some(value) if value != unset_label => value.to_string(),
        _ => "unset".to_string(),
    }
}

fn summary_lines(width: u16, entries: &[(&str, String)]) -> Vec<Line<'static>> {
    let compact = width < 40;
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
    let footer_height = if area.width >= 96 { 4 } else { 5 };
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(0),
            Constraint::Length(footer_height),
        ])
        .split(area);
    let step_col = config_step_column_width();
    let body_area = layout[1];
    let summary_area = if body_area.width >= 118 {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(step_col),
                Constraint::Min(34),
                Constraint::Length(48),
            ])
            .split(body_area);
        body[2]
    } else if body_area.width >= 90 {
        let sidebar_width = step_col.max(36);
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar_width), Constraint::Min(40)])
            .split(body_area);
        let sidebar = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(17), Constraint::Min(10)])
            .split(body[0]);
        sidebar[1]
    } else {
        let stacked = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(10),
                Constraint::Min(8),
                Constraint::Length(18),
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

    use anyhow::Result;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;

    use super::{AdvancedRoutingApp, ConfigApp, ConfigStep, ConfigViewData, summary_viewport};
    use crate::config::{AgentSettings, AppConfig, InstallDefaults, InstallUiSettings};

    #[test]
    fn config_app_refreshes_reasoning_options_when_provider_changes() {
        let view = ConfigViewData {
            config_path: PathBuf::from("/tmp/metastack-config.toml"),
            app_config: AppConfig {
                agents: AgentSettings {
                    default_agent: Some("codex".to_string()),
                    default_model: Some("gpt-5.4".to_string()),
                    default_reasoning: Some("high".to_string()),
                    ..AgentSettings::default()
                },
                ..AppConfig::default()
            },
            detected_agents: Vec::new(),
        };

        let mut app = ConfigApp::new(&view);
        assert_eq!(
            app.default_reasoning.options(),
            &[
                "Leave unset".to_string(),
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
            ]
        );
        assert_eq!(app.default_reasoning.selected_label(), Some("high"));

        let claude_index = app
            .agent_field
            .options()
            .iter()
            .position(|option| option == "claude")
            .expect("claude provider should be listed");
        app.agent_field
            .move_by(claude_index as isize - app.agent_field.selected() as isize);
        app.sync_models(Some("sonnet"));
        app.sync_reasoning(Some("max"));

        assert_eq!(
            app.default_reasoning.options(),
            &[
                "Leave unset".to_string(),
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
                "max".to_string(),
            ]
        );
        assert_eq!(app.default_reasoning.selected_label(), Some("max"));
    }

    #[test]
    fn config_save_summary_scrolls_when_content_overflows() {
        let view = ConfigViewData {
            config_path: PathBuf::from("/tmp/metastack-config.toml"),
            app_config: AppConfig::default(),
            detected_agents: vec!["codex".to_string(), "claude".to_string()],
        };

        let mut app = ConfigApp::new(&view);
        app.step = ConfigStep::Save;
        let long = "profile-value-".repeat(12);
        let _ = app.default_profile.paste(&long);
        let _ = app.listen_label.paste(&long);

        let viewport = summary_viewport(Rect::new(0, 0, 72, 20));
        let _ = app.handle_key(KeyCode::End.into(), viewport);

        assert!(app.summary_scroll.offset() > 0);
    }

    #[test]
    fn config_save_summary_mouse_wheel_scrolls_when_content_overflows() {
        let view = ConfigViewData {
            config_path: PathBuf::from("/tmp/metastack-config.toml"),
            app_config: AppConfig::default(),
            detected_agents: vec!["codex".to_string(), "claude".to_string()],
        };

        let mut app = ConfigApp::new(&view);
        app.step = ConfigStep::Save;
        let long = "profile-value-".repeat(12);
        let _ = app.default_profile.paste(&long);
        let _ = app.listen_label.paste(&long);

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
    fn config_app_vim_keys_remain_literal_in_text_inputs() {
        let view = ConfigViewData {
            config_path: PathBuf::from("/tmp/metastack-config.toml"),
            app_config: AppConfig {
                defaults: InstallDefaults {
                    ui: InstallUiSettings { vim_mode: true },
                    ..InstallDefaults::default()
                },
                ..AppConfig::default()
            },
            detected_agents: Vec::new(),
        };

        let mut app = ConfigApp::new(&view);
        app.step = ConfigStep::Team;

        let _ = app.handle_key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            Rect::new(0, 0, 120, 32),
        );

        assert_eq!(app.team.value(), "j");
    }

    #[test]
    fn config_app_vim_keys_navigate_select_steps() {
        let view = ConfigViewData {
            config_path: PathBuf::from("/tmp/metastack-config.toml"),
            app_config: AppConfig {
                defaults: InstallDefaults {
                    ui: InstallUiSettings { vim_mode: true },
                    ..InstallDefaults::default()
                },
                ..AppConfig::default()
            },
            detected_agents: Vec::new(),
        };

        let mut app = ConfigApp::new(&view);
        app.step = ConfigStep::AssignmentScope;
        let initial = app.assignment_scope.selected();

        let _ = app.handle_key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            Rect::new(0, 0, 120, 32),
        );

        assert_eq!(app.assignment_scope.selected(), initial + 1);
    }

    #[test]
    fn advanced_routing_app_refreshes_reasoning_options_when_provider_changes() -> Result<()> {
        let view = ConfigViewData {
            config_path: PathBuf::from("/tmp/metastack-config.toml"),
            app_config: AppConfig {
                agents: AgentSettings {
                    default_agent: Some("codex".to_string()),
                    ..AgentSettings::default()
                },
                ..AppConfig::default()
            },
            detected_agents: Vec::new(),
        };

        let mut app = AdvancedRoutingApp::new(&view)?;
        let codex_index = app
            .agent_field
            .options()
            .iter()
            .position(|option| option == "codex")
            .expect("codex provider should be listed");
        app.agent_field
            .move_by(codex_index as isize - app.agent_field.selected() as isize);
        app.sync_models(Some("gpt-5.4"))?;
        app.sync_reasoning(Some("high"))?;

        assert_eq!(
            app.reasoning.options(),
            &[
                "Inherit".to_string(),
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
            ]
        );
        assert_eq!(app.reasoning.selected_label(), Some("high"));

        let claude_index = app
            .agent_field
            .options()
            .iter()
            .position(|option| option == "claude")
            .expect("claude provider should be listed");
        app.agent_field
            .move_by(claude_index as isize - app.agent_field.selected() as isize);
        app.sync_models(Some("sonnet"))?;
        app.sync_reasoning(Some("max"))?;

        assert_eq!(
            app.reasoning.options(),
            &[
                "Inherit".to_string(),
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
                "max".to_string(),
            ]
        );
        assert_eq!(app.reasoning.selected_label(), Some("max"));

        Ok(())
    }
}
