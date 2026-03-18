use std::collections::BTreeMap;
use std::io;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, anyhow};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
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

use crate::cli::ConfigArgs;
use crate::config::{
    AgentConfigSource, AgentRouteConfig, AgentRouteScope, AppConfig, detect_supported_agents,
    normalize_agent_name, normalize_agent_route_key, resolve_agent_route, supported_agent_models,
    supported_agent_names, supported_agent_route_definitions, supported_agent_route_families,
    supported_reasoning_options, validate_agent_model, validate_agent_name,
    validate_agent_reasoning,
};
use crate::tui::fields::{InputFieldState, SelectFieldState};

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

    if args.json {
        return Ok(ConfigCommandOutput::Json(render_json(&view)?));
    }

    if has_direct_updates(args) {
        let changed = apply_direct_updates(&mut view, args)?;
        save_view(&view)?;
        return Ok(ConfigCommandOutput::Text(
            ConfigReport {
                config_path: view.config_path.clone(),
                changed,
            }
            .render(&view),
        ));
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
        "Linear API key: {}",
        mask_secret(view.app_config.linear.api_key.as_deref())
    ));
    lines.push(format!(
        "Default Linear team: {}",
        display_optional(view.app_config.linear.team.as_deref())
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
        || args.default_profile.is_some()
        || args.default_agent.is_some()
        || args.default_model.is_some()
        || args.default_reasoning.is_some()
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
    DefaultProfile,
    Agent,
    Model,
    DefaultReasoning,
    Save,
}

impl ConfigStep {
    fn all() -> [Self; 7] {
        [
            Self::ApiKey,
            Self::Team,
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
    step: ConfigStep,
    api_key: InputFieldState,
    team: InputFieldState,
    default_profile: InputFieldState,
    default_reasoning: SelectFieldState,
    agent_field: SelectFieldState,
    model_field: SelectFieldState,
    detected_agents: Vec<String>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct SubmittedConfig {
    api_key: Option<String>,
    team: Option<String>,
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
        let selected_agent = selected_global_agent(&view.app_config);
        let agent_index = agent_options
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(&selected_agent))
            .unwrap_or(0);
        let mut app = Self {
            step: ConfigStep::ApiKey,
            api_key: InputFieldState::new(
                view.app_config.linear.api_key.clone().unwrap_or_default(),
            ),
            team: InputFieldState::new(view.app_config.linear.team.clone().unwrap_or_default()),
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
                if self.step == ConfigStep::Agent {
                    self.agent_field.move_by(-1);
                    self.sync_models(None);
                } else if self.step == ConfigStep::Model {
                    self.model_field.move_by(-1);
                    self.sync_reasoning(None);
                } else if self.step == ConfigStep::DefaultReasoning {
                    self.default_reasoning.move_by(-1);
                } else {
                    self.step = self.step.previous();
                }
                None
            }
            ConfigAction::Down => {
                self.error = None;
                if self.step == ConfigStep::Agent {
                    self.agent_field.move_by(1);
                    self.sync_models(None);
                } else if self.step == ConfigStep::Model {
                    self.model_field.move_by(1);
                    self.sync_reasoning(None);
                } else if self.step == ConfigStep::DefaultReasoning {
                    self.default_reasoning.move_by(1);
                } else {
                    self.step = self.step.next();
                }
                None
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<ConfigDashboardExit> {
        match key.code {
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Left | KeyCode::Right => {
                self.error = None;
                match self.step {
                    ConfigStep::ApiKey => {
                        let _ = self.api_key.handle_key(key);
                    }
                    ConfigStep::Team => {
                        let _ = self.team.handle_key(key);
                    }
                    ConfigStep::DefaultProfile => {
                        let _ = self.default_profile.handle_key(key);
                    }
                    ConfigStep::DefaultReasoning
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

    fn handle_paste(&mut self, text: &str) {
        self.error = None;
        match self.step {
            ConfigStep::ApiKey => {
                let _ = self.api_key.paste(text);
            }
            ConfigStep::Team => {
                let _ = self.team.paste(text);
            }
            ConfigStep::DefaultProfile => {
                let _ = self.default_profile.paste(text);
            }
            ConfigStep::Agent | ConfigStep::Model | ConfigStep::Save => {}
            ConfigStep::DefaultReasoning => {}
        }
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

        Ok(SubmittedConfig {
            api_key: normalize_optional(self.api_key.value()),
            team: normalize_optional(self.team.value()),
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
    execute!(stdout, EnterAlternateScreen)?;
    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = app;

    loop {
        terminal.draw(|frame| render_config_dashboard(frame, &app))?;

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

    let body_area = layout[1];
    if body_area.width >= 118 {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(26),
                Constraint::Min(34),
                Constraint::Length(40),
            ])
            .split(body_area);
        render_step_list(frame, app, body[0], 1);
        render_step_panel(frame, app, body[1]);
        render_summary_panel(frame, app, body[2]);
    } else if body_area.width >= 90 {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(36), Constraint::Min(40)])
            .split(body_area);
        let sidebar = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(9), Constraint::Min(10)])
            .split(body[0]);
        render_step_list(frame, app, sidebar[0], 1);
        render_summary_panel(frame, app, sidebar[1]);
        render_step_panel(frame, app, body[1]);
    } else {
        let stacked = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(6),
                Constraint::Min(8),
                Constraint::Length(10),
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
    let summary = summary_lines(
        area.width,
        &[
            ("Linear API key", summarize_secret(&app.api_key)),
            ("Default team", summarize_optional_value(&app.team)),
            (
                "Default profile",
                summarize_optional_value(&app.default_profile),
            ),
            (
                "Default agent",
                app.agent_field
                    .selected_label()
                    .unwrap_or("unset")
                    .to_string(),
            ),
            (
                "Default model",
                app.model_field
                    .selected_label()
                    .unwrap_or("Leave unset")
                    .to_string(),
            ),
            (
                "Default reasoning",
                summarize_optional_select(&app.default_reasoning, "Leave unset"),
            ),
            (
                "Detected agents",
                if app.detected_agents.is_empty() {
                    "none".to_string()
                } else {
                    app.detected_agents.join(", ")
                },
            ),
        ],
    );
    let paragraph = Paragraph::new(Text::from(summary))
        .block(Block::default().borders(Borders::ALL).title("Summary"))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame<'_>, app: &ConfigApp, area: Rect) {
    let controls = match app.step {
        ConfigStep::ApiKey | ConfigStep::Team | ConfigStep::DefaultProfile => {
            "Type or paste the value. Enter or Tab advances. Shift+Tab goes back. Esc cancels."
        }
        ConfigStep::Agent | ConfigStep::Model | ConfigStep::DefaultReasoning => {
            "Use Up/Down to choose. Enter or Tab advances. Shift+Tab goes back. Esc cancels."
        }
        ConfigStep::Save => "Press Enter to save. Shift+Tab goes back. Esc cancels.",
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
    let rendered = field.render(placeholder, true);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("{title} [editing]"))
        .border_style(Style::default().add_modifier(Modifier::BOLD));
    let inner = block.inner(area);
    let paragraph = Paragraph::new(rendered.text.clone())
        .block(block)
        .wrap(Wrap { trim: false });
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
        let _ = execute!(stdout, LeaveAlternateScreen);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use anyhow::Result;

    use super::{AdvancedRoutingApp, ConfigApp, ConfigViewData};
    use crate::config::{AgentSettings, AppConfig};

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
