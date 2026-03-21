use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::agent_provider::{
    builtin_provider_adapter, builtin_provider_model_keys, builtin_provider_names,
    builtin_provider_reasoning_keys,
};
use crate::cli::{ListenRefreshPolicyArg, PromptTransportArg};
use crate::fs::PlanningPaths;
use crate::linear::{LinearService, ReqwestLinearClient};

pub const DEFAULT_LINEAR_API_URL: &str = "https://api.linear.app/graphql";
pub const METASTACK_CONFIG_ENV: &str = "METASTACK_CONFIG";
pub const DEFAULT_LISTEN_POLL_INTERVAL_SECONDS: u64 = 7;
pub const DEFAULT_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT: usize = 10;
pub const MIN_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT: usize = 1;
pub const MAX_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT: usize = 10;
pub const DEFAULT_FAST_PLAN_QUESTION_LIMIT: usize = 3;
pub const MIN_FAST_PLAN_QUESTION_LIMIT: usize = 0;
pub const MAX_FAST_PLAN_QUESTION_LIMIT: usize = 10;
pub const DEFAULT_SYNC_DISCUSSION_FILE_CHAR_LIMIT: usize = 20_000;
pub const DEFAULT_SYNC_DISCUSSION_PROMPT_CHAR_LIMIT: usize = 6_000;
pub const DEFAULT_MERGE_VALIDATION_REPAIR_ATTEMPTS: usize = 6;
pub const DEFAULT_MERGE_VALIDATION_TRANSIENT_RETRY_ATTEMPTS: usize = 3;
pub const DEFAULT_MERGE_PUBLICATION_RETRY_ATTEMPTS: usize = 5;
pub const AGENT_ROUTE_BACKLOG_PLAN: &str = "backlog.plan";
pub const AGENT_ROUTE_BACKLOG_IMPROVE: &str = "backlog.improve";
pub const AGENT_ROUTE_BACKLOG_SPLIT: &str = "backlog.split";
pub const AGENT_ROUTE_CONTEXT_SCAN: &str = "context.scan";
pub const AGENT_ROUTE_CONTEXT_RELOAD: &str = "context.reload";
pub const AGENT_ROUTE_LINEAR_ISSUES_REFINE: &str = "linear.issues.refine";
pub const AGENT_ROUTE_AGENTS_LISTEN: &str = "agents.listen";
pub const AGENT_ROUTE_AGENTS_WORKFLOWS_RUN: &str = "agents.workflows.run";
pub const AGENT_ROUTE_RUNTIME_CRON_PROMPT: &str = "runtime.cron.prompt";
pub const AGENT_ROUTE_MERGE: &str = "merge.run";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub linear: LinearSettings,
    #[serde(default)]
    pub backlog: BacklogSettings,
    #[serde(default)]
    pub agents: AgentSettings,
    #[serde(default)]
    pub merge: MergeSettings,
    #[serde(default)]
    pub defaults: InstallDefaults,
    #[serde(default)]
    pub onboarding: OnboardingSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanningMeta {
    #[serde(default)]
    pub linear: PlanningLinearSettings,
    #[serde(default)]
    pub backlog: BacklogSettings,
    #[serde(default)]
    pub sync: PlanningSyncSettings,
    #[serde(default)]
    pub agent: PlanningAgentSettings,
    #[serde(default)]
    pub listen: PlanningListenSettings,
    #[serde(default)]
    pub plan: PlanningPlanSettings,
    #[serde(default)]
    pub issue_labels: PlanningIssueLabels,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstallDefaults {
    #[serde(default)]
    pub linear: InstallLinearDefaults,
    #[serde(default)]
    pub listen: InstallListenSettings,
    #[serde(default)]
    pub plan: InstallPlanSettings,
    #[serde(default)]
    pub ui: InstallUiSettings,
    #[serde(default)]
    pub issue_labels: PlanningIssueLabels,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstallLinearDefaults {
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstallListenSettings {
    pub required_label: Option<String>,
    pub assignment_scope: Option<ListenAssignmentScope>,
    pub refresh_policy: Option<ListenRefreshPolicy>,
    pub poll_interval_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstallPlanSettings {
    pub interactive_follow_up_questions: Option<usize>,
    pub default_mode: Option<PlanDefaultMode>,
    pub fast_single_ticket: Option<bool>,
    pub fast_questions: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstallUiSettings {
    #[serde(default)]
    pub vim_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OnboardingSettings {
    #[serde(default)]
    pub completed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearSettings {
    pub api_key: Option<String>,
    #[serde(default = "default_linear_api_url")]
    pub api_url: String,
    pub team: Option<String>,
    pub default_profile: Option<String>,
    #[serde(default)]
    pub repo_auth: BTreeMap<String, RepoLinearAuthSettings>,
    #[serde(default)]
    pub profiles: BTreeMap<String, LinearProfileSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepoLinearAuthSettings {
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearProfileSettings {
    pub api_key: Option<String>,
    #[serde(default = "default_linear_api_url")]
    pub api_url: String,
    pub team: Option<String>,
    pub team_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanningLinearSettings {
    pub profile: Option<String>,
    pub team: Option<String>,
    pub project_id: Option<String>,
    #[serde(default)]
    pub ticket_context: PlanningTicketContextSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanningTicketContextSettings {
    pub discussion_prompt_chars: Option<usize>,
    pub discussion_persisted_chars: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanningSyncSettings {
    pub discussion_file_char_limit: Option<usize>,
    pub discussion_prompt_char_limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanningAgentSettings {
    #[serde(alias = "agent")]
    pub provider: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PlanningListenSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_labels: Option<Vec<String>>,
    pub assignment_scope: Option<ListenAssignmentScope>,
    pub refresh_policy: Option<ListenRefreshPolicy>,
    pub instructions_path: Option<String>,
    pub poll_interval_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanningPlanSettings {
    pub interactive_follow_up_questions: Option<usize>,
    pub default_mode: Option<PlanDefaultMode>,
    pub fast_single_ticket: Option<bool>,
    pub fast_questions: Option<usize>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanDefaultMode {
    #[default]
    Normal,
    Fast,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanningIssueLabels {
    pub plan: Option<String>,
    pub technical: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BacklogSettings {
    pub default_assignee: Option<String>,
    pub default_state: Option<String>,
    pub default_priority: Option<u8>,
    #[serde(default)]
    pub default_labels: Vec<String>,
    #[serde(default)]
    pub velocity_defaults: VelocityDefaults,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VelocityDefaults {
    pub project: Option<String>,
    pub state: Option<String>,
    pub auto_assign: Option<VelocityAutoAssign>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VelocityAutoAssign {
    Viewer,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentSettings {
    pub default_agent: Option<String>,
    pub default_model: Option<String>,
    pub default_reasoning: Option<String>,
    #[serde(default)]
    pub routing: AgentRoutingSettings,
    #[serde(default)]
    pub commands: BTreeMap<String, AgentCommandConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MergeSettings {
    pub validation_repair_attempts: Option<usize>,
    pub validation_transient_retry_attempts: Option<usize>,
    pub publication_retry_attempts: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentRoutingSettings {
    #[serde(default)]
    pub families: BTreeMap<String, AgentRouteConfig>,
    #[serde(default)]
    pub commands: BTreeMap<String, AgentRouteConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentRouteConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCommandConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub transport: PromptTransport,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PromptTransport {
    #[default]
    Arg,
    Stdin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListenAssignmentScope {
    #[default]
    Any,
    ViewerOnly,
    ViewerOrUnassigned,
}

impl ListenAssignmentScope {
    fn as_config_value(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::ViewerOnly => "viewer_only",
            Self::ViewerOrUnassigned => "viewer_or_unassigned",
        }
    }
}

impl Serialize for ListenAssignmentScope {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_config_value())
    }
}

impl<'de> Deserialize<'de> for ListenAssignmentScope {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "any" => Ok(Self::Any),
            "viewer_only" => Ok(Self::ViewerOnly),
            "viewer_or_unassigned" | "viewer" => Ok(Self::ViewerOrUnassigned),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["any", "viewer_only", "viewer_or_unassigned", "viewer"],
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ListenRefreshPolicy {
    #[default]
    ReuseAndRefresh,
    RecreateFromOriginMain,
}

#[derive(Debug, Clone)]
pub struct LinearConfig {
    pub api_key: String,
    pub api_url: String,
    pub default_team: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LinearConfigOverrides {
    pub api_key: Option<String>,
    pub api_url: Option<String>,
    pub default_team: Option<String>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentConfigOverrides {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone)]
struct AgentValueCandidate {
    value: String,
    source: AgentConfigSource,
    provider: Option<String>,
}

#[derive(Debug, Default)]
enum RequiredLabelsField {
    #[default]
    Missing,
    ExplicitNone,
    Labels(Vec<String>),
}

#[derive(Debug, Deserialize, Default)]
struct PlanningListenSettingsWire {
    #[serde(default)]
    required_labels: RequiredLabelsField,
    #[serde(default)]
    required_label: Option<String>,
    #[serde(default)]
    assignment_scope: Option<ListenAssignmentScope>,
    #[serde(default)]
    refresh_policy: Option<ListenRefreshPolicy>,
    instructions_path: Option<String>,
    poll_interval_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRouteScope {
    Family,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentRouteDefinition {
    pub key: &'static str,
    pub family: &'static str,
    pub label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentRoute {
    pub route_key: String,
    pub family_key: String,
    pub provider: String,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub provider_source: AgentConfigSource,
    pub model_source: Option<AgentConfigSource>,
    pub reasoning_source: Option<AgentConfigSource>,
}

/// Error returned when agent resolution cannot determine a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoAgentSelectedError {
    route_key: Option<String>,
}

impl NoAgentSelectedError {
    fn global() -> Self {
        Self { route_key: None }
    }

    fn for_route(route_key: impl Into<String>) -> Self {
        Self {
            route_key: Some(route_key.into()),
        }
    }

    /// Returns the route key whose resolution failed, or `None` for global resolution failures.
    pub fn route_key(&self) -> Option<&str> {
        self.route_key.as_deref()
    }
}

impl fmt::Display for NoAgentSelectedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.route_key() {
            Some(route_key) => write!(
                f,
                "no agent was selected for route `{route_key}`. Pass `--agent <NAME>` or configure a route or global default with `meta runtime config`."
            ),
            None => write!(
                f,
                "no agent was selected. Pass `--agent <NAME>` or run `meta runtime config` to configure a default agent."
            ),
        }
    }
}

impl std::error::Error for NoAgentSelectedError {}

/// Returns `true` when the provided error is a `NoAgentSelectedError`.
pub fn is_no_agent_selected_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<NoAgentSelectedError>().is_some()
}

/// Returns the route key attached to a `NoAgentSelectedError`, if one is present.
pub fn no_agent_selected_route_key(error: &anyhow::Error) -> Option<&str> {
    error
        .downcast_ref::<NoAgentSelectedError>()
        .and_then(NoAgentSelectedError::route_key)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentConfigSource {
    ExplicitOverride,
    CommandRoute(String),
    FamilyRoute(String),
    RepoDefault,
    GlobalDefault,
}

#[derive(Debug, Clone)]
pub struct ResolvedAgentConfig {
    pub provider: String,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub route_key: Option<String>,
    pub family_key: Option<String>,
    pub provider_source: AgentConfigSource,
    pub model_source: Option<AgentConfigSource>,
    pub reasoning_source: Option<AgentConfigSource>,
}

impl Default for LinearSettings {
    fn default() -> Self {
        Self {
            api_key: None,
            api_url: default_linear_api_url(),
            team: None,
            default_profile: None,
            repo_auth: BTreeMap::new(),
            profiles: BTreeMap::new(),
        }
    }
}

impl<'de> Deserialize<'de> for RequiredLabelsField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let labels = Option::<Vec<String>>::deserialize(deserializer)?;
        Ok(match labels.and_then(normalize_required_labels) {
            Some(labels) => Self::Labels(labels),
            None => Self::ExplicitNone,
        })
    }
}

impl<'de> Deserialize<'de> for PlanningListenSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PlanningListenSettingsWire::deserialize(deserializer)?;
        let required_labels = match wire.required_labels {
            RequiredLabelsField::Missing => {
                normalize_required_labels(wire.required_label.iter().map(String::as_str))
            }
            RequiredLabelsField::ExplicitNone => None,
            RequiredLabelsField::Labels(labels) => Some(labels),
        };

        Ok(Self {
            required_labels,
            assignment_scope: wire.assignment_scope,
            refresh_policy: wire.refresh_policy,
            instructions_path: wire.instructions_path,
            poll_interval_seconds: wire.poll_interval_seconds,
        })
    }
}

impl Default for LinearProfileSettings {
    fn default() -> Self {
        Self {
            api_key: None,
            api_url: default_linear_api_url(),
            team: None,
            team_name: None,
        }
    }
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let Some(path) = config_path_from_env_or_home() else {
            return Ok(Self::default());
        };

        match fs::read_to_string(&path) {
            Ok(contents) => {
                let parsed: Self = toml::from_str(&contents)
                    .with_context(|| format!("failed to parse `{}`", path.display()))?;
                parsed
                    .validate()
                    .with_context(|| format!("invalid `{}`", path.display()))?;
                Ok(parsed)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => {
                Err(error).with_context(|| format!("failed to read `{}`", path.display()))
            }
        }
    }

    pub fn save(&self) -> Result<PathBuf> {
        self.validate().context("config is invalid")?;
        let path = resolve_config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create `{}`", parent.display()))?;
        }

        let contents = toml::to_string_pretty(self).context("failed to encode config as TOML")?;
        fs::write(&path, contents)
            .with_context(|| format!("failed to write `{}`", path.display()))?;
        Ok(path)
    }

    pub fn resolve_agent_definition(&self, name: &str) -> Option<AgentCommandConfig> {
        self.agents
            .commands
            .get(&normalize_agent_name(name))
            .cloned()
            .or_else(|| builtin_agent_definition(name))
    }

    pub fn repo_linear_api_key(&self, root: &Path) -> Option<String> {
        self.linear
            .repo_auth
            .get(&repo_auth_key(root))
            .and_then(|entry| normalize_optional_ref(entry.api_key.as_deref()))
    }

    pub fn set_repo_linear_api_key(&mut self, root: &Path, api_key: Option<String>) {
        let key = repo_auth_key(root);
        match api_key {
            Some(api_key) => {
                self.linear.repo_auth.insert(
                    key,
                    RepoLinearAuthSettings {
                        api_key: Some(api_key),
                    },
                );
            }
            None => {
                self.linear.repo_auth.remove(&key);
            }
        }
    }

    /// Validates the install-scoped config payload before it is persisted or consumed.
    ///
    /// Returns an error when install defaults or global agent settings contain invalid values.
    pub fn validate(&self) -> Result<()> {
        self.backlog.validate("global backlog defaults")?;
        self.defaults.validate()?;
        self.validate_global_agent_defaults()?;
        self.validate_agent_routes()?;
        self.merge.validate()?;
        Ok(())
    }

    /// Reports whether first-run onboarding has already completed for this install.
    pub fn onboarding_complete(&self) -> bool {
        self.onboarding.completed
    }

    /// Reports whether install-scoped vim navigation aliases are enabled for supported TUI flows.
    pub fn vim_mode_enabled(&self) -> bool {
        self.defaults.ui.vim_mode
    }

    /// Marks first-run onboarding as completed in the install-scoped config.
    pub fn mark_onboarding_complete(&mut self) {
        self.onboarding.completed = true;
    }

    pub fn upsert_agent_route(
        &mut self,
        scope: AgentRouteScope,
        key: &str,
        config: AgentRouteConfig,
    ) -> Result<()> {
        let normalized = normalize_agent_route_key(scope, key)?;
        match scope {
            AgentRouteScope::Family => {
                self.agents.routing.families.insert(normalized, config);
            }
            AgentRouteScope::Command => {
                self.agents.routing.commands.insert(normalized, config);
            }
        }
        self.validate()
    }

    pub fn clear_agent_route(&mut self, scope: AgentRouteScope, key: &str) -> Result<bool> {
        let normalized = normalize_agent_route_key(scope, key)?;
        let removed = match scope {
            AgentRouteScope::Family => self.agents.routing.families.remove(&normalized).is_some(),
            AgentRouteScope::Command => self.agents.routing.commands.remove(&normalized).is_some(),
        };
        self.validate()?;
        Ok(removed)
    }

    fn validate_global_agent_defaults(&self) -> Result<()> {
        if let Some(provider) = normalize_optional_ref(self.agents.default_agent.as_deref()) {
            validate_agent_name(self, &provider)?;
            validate_agent_model(&provider, self.agents.default_model.as_deref())?;
            validate_agent_reasoning(
                &provider,
                self.agents.default_model.as_deref(),
                self.agents.default_reasoning.as_deref(),
            )?;
        } else if normalize_optional_ref(self.agents.default_model.as_deref()).is_some() {
            return Err(anyhow!(
                "global default model requires a global default agent under `[agents]`"
            ));
        } else if normalize_optional_ref(self.agents.default_reasoning.as_deref()).is_some() {
            return Err(anyhow!(
                "global default reasoning requires a global default agent under `[agents]`"
            ));
        }
        Ok(())
    }

    fn validate_agent_routes(&self) -> Result<()> {
        for key in self.agents.routing.families.keys() {
            normalize_agent_route_key(AgentRouteScope::Family, key)?;
        }
        for key in self.agents.routing.commands.keys() {
            normalize_agent_route_key(AgentRouteScope::Command, key)?;
        }

        for definition in supported_agent_route_definitions() {
            let family = self.agents.routing.families.get(definition.family);
            validate_agent_route_config(
                self,
                family,
                None,
                Some(definition.family),
                Some(definition.key),
            )?;
            validate_agent_route_config(
                self,
                self.agents.routing.commands.get(definition.key),
                family,
                Some(definition.family),
                Some(definition.key),
            )?;
        }
        Ok(())
    }
}

impl PlanningMeta {
    pub fn load(root: &Path) -> Result<Self> {
        let path = PlanningPaths::new(root).meta_path();

        match fs::read_to_string(&path) {
            Ok(contents) => {
                let parsed: Self = serde_json::from_str(&contents)
                    .with_context(|| format!("failed to parse `{}`", path.display()))?;
                parsed
                    .validate()
                    .with_context(|| format!("invalid `{}`", path.display()))?;
                Ok(parsed)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => {
                Err(error).with_context(|| format!("failed to read `{}`", path.display()))
            }
        }
    }

    pub fn save(&self, root: &Path) -> Result<PathBuf> {
        self.validate().context("planning metadata is invalid")?;
        let path = PlanningPaths::new(root).meta_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create `{}`", parent.display()))?;
        }

        let contents =
            serde_json::to_string_pretty(self).context("failed to encode planning metadata")?;
        fs::write(&path, contents)
            .with_context(|| format!("failed to write `{}`", path.display()))?;
        Ok(path)
    }

    /// Returns the repo-scoped interactive follow-up limit, falling back to the built-in default.
    pub fn interactive_follow_up_question_limit(&self) -> usize {
        self.plan
            .interactive_follow_up_questions
            .unwrap_or(DEFAULT_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT)
    }

    /// Returns the repo-scoped default plan mode, falling back to the built-in default.
    pub fn plan_default_mode(&self) -> PlanDefaultMode {
        self.plan.default_mode.unwrap_or_default()
    }

    /// Returns whether fast planning should prefer a single ticket by default.
    pub fn fast_single_ticket(&self) -> bool {
        self.plan.fast_single_ticket.unwrap_or(true)
    }

    /// Returns the repo-scoped fast follow-up question limit, falling back to the built-in default.
    pub fn fast_question_limit(&self) -> usize {
        self.plan
            .fast_questions
            .unwrap_or(DEFAULT_FAST_PLAN_QUESTION_LIMIT)
    }

    /// Resolves the effective Linear project selector using repo defaults before install defaults.
    pub fn effective_project_id(&self, app_config: &AppConfig) -> Option<String> {
        normalize_optional_ref(self.linear.project_id.as_deref())
            .or_else(|| normalize_optional_ref(app_config.defaults.linear.project_id.as_deref()))
    }

    /// Resolves the effective listen pickup label using repo defaults before install defaults.
    pub fn effective_listen_required_label(&self, app_config: &AppConfig) -> Option<String> {
        self.listen
            .required_label_names()
            .first()
            .cloned()
            .or_else(|| {
                normalize_optional_ref(app_config.defaults.listen.required_label.as_deref())
            })
    }

    /// Resolves the effective listen assignee scope using repo defaults before install defaults.
    pub fn effective_listen_assignment_scope(
        &self,
        app_config: &AppConfig,
    ) -> ListenAssignmentScope {
        self.listen
            .assignment_scope
            .or(app_config.defaults.listen.assignment_scope)
            .unwrap_or_default()
    }

    /// Resolves the effective listen refresh policy using repo defaults before install defaults.
    pub fn effective_listen_refresh_policy(&self, app_config: &AppConfig) -> ListenRefreshPolicy {
        self.listen
            .refresh_policy
            .or(app_config.defaults.listen.refresh_policy)
            .unwrap_or_default()
    }

    /// Resolves the effective listen poll interval using repo defaults before install defaults.
    pub fn effective_listen_poll_interval_seconds(&self, app_config: &AppConfig) -> u64 {
        self.listen
            .poll_interval_seconds
            .or(app_config.defaults.listen.poll_interval_seconds)
            .unwrap_or_else(|| self.listen.poll_interval_seconds())
    }

    /// Resolves the effective interactive follow-up limit using repo defaults before install defaults.
    pub fn effective_interactive_follow_up_question_limit(&self, app_config: &AppConfig) -> usize {
        self.plan
            .interactive_follow_up_questions
            .or(app_config.defaults.plan.interactive_follow_up_questions)
            .unwrap_or_else(|| self.interactive_follow_up_question_limit())
    }

    /// Resolves the effective default plan mode using repo defaults before install defaults.
    pub fn effective_plan_default_mode(&self, app_config: &AppConfig) -> PlanDefaultMode {
        self.plan
            .default_mode
            .or(app_config.defaults.plan.default_mode)
            .unwrap_or_else(|| self.plan_default_mode())
    }

    /// Resolves the effective fast single-ticket preference using repo defaults before install defaults.
    pub fn effective_fast_single_ticket(&self, app_config: &AppConfig) -> bool {
        self.plan
            .fast_single_ticket
            .or(app_config.defaults.plan.fast_single_ticket)
            .unwrap_or_else(|| self.fast_single_ticket())
    }

    /// Resolves the effective fast follow-up limit using repo defaults before install defaults.
    pub fn effective_fast_question_limit(&self, app_config: &AppConfig) -> usize {
        self.plan
            .fast_questions
            .or(app_config.defaults.plan.fast_questions)
            .unwrap_or_else(|| self.fast_question_limit())
    }

    /// Resolves the effective planning label using repo defaults before install defaults.
    pub fn effective_plan_label(&self, app_config: &AppConfig) -> String {
        self.issue_labels
            .plan
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                app_config
                    .defaults
                    .issue_labels
                    .plan
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            })
            .map(str::to_string)
            .unwrap_or_else(|| self.issue_labels.plan_label())
    }

    /// Resolves the effective technical label using repo defaults before install defaults.
    pub fn effective_technical_label(&self, app_config: &AppConfig) -> String {
        self.issue_labels
            .technical
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                app_config
                    .defaults
                    .issue_labels
                    .technical
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            })
            .map(str::to_string)
            .unwrap_or_else(|| self.issue_labels.technical_label())
    }

    /// Validates repo-scoped planning metadata before command execution or persistence.
    ///
    /// Returns an error when agent settings or promoted workflow defaults are invalid.
    pub fn validate(&self) -> Result<()> {
        self.backlog.validate("repo backlog defaults")?;
        if let Some(provider) = normalize_optional_ref(self.agent.provider.as_deref()) {
            validate_agent_model(&provider, self.agent.model.as_deref())?;
            validate_agent_reasoning(
                &provider,
                self.agent.model.as_deref(),
                self.agent.reasoning.as_deref(),
            )?;
        } else if normalize_optional_ref(self.agent.model.as_deref()).is_some() {
            return Err(anyhow!(
                "repo default model requires a repo default provider under `.metastack/meta.json`"
            ));
        } else if normalize_optional_ref(self.agent.reasoning.as_deref()).is_some() {
            return Err(anyhow!(
                "repo default reasoning requires a repo default provider under `.metastack/meta.json`"
            ));
        }
        if let Some(interval) = self.listen.poll_interval_seconds {
            validate_listen_poll_interval_seconds(interval)?;
        }
        if let Some(limit) = self.plan.interactive_follow_up_questions {
            validate_interactive_plan_follow_up_question_limit(limit)?;
        }
        if let Some(limit) = self.plan.fast_questions {
            validate_fast_plan_question_limit(limit)?;
        }
        self.sync.validate()?;
        Ok(())
    }
}

pub fn load_required_planning_meta(root: &Path, command_name: &str) -> Result<PlanningMeta> {
    let meta_path = PlanningPaths::new(root).meta_path();
    if !meta_path.is_file() {
        return Err(anyhow!(
            "`meta {command_name}` requires repo setup. Run `meta runtime setup --root {}` and rerun.",
            root.display()
        ));
    }
    PlanningMeta::load(root)
}

impl PlanningListenSettings {
    /// Returns configured listen labels, treating `None` and `[]` as no filter.
    pub(crate) fn required_label_names(&self) -> &[String] {
        self.required_labels
            .as_deref()
            .filter(|labels| !labels.is_empty())
            .unwrap_or(&[])
    }
    pub fn poll_interval_seconds(&self) -> u64 {
        self.poll_interval_seconds
            .unwrap_or(DEFAULT_LISTEN_POLL_INTERVAL_SECONDS)
    }

    /// Returns the repo-scoped assignee scope, falling back to the built-in default.
    pub fn assignment_scope(&self) -> ListenAssignmentScope {
        self.assignment_scope.unwrap_or_default()
    }

    /// Returns the repo-scoped refresh policy, falling back to the built-in default.
    pub fn refresh_policy(&self) -> ListenRefreshPolicy {
        self.refresh_policy.unwrap_or_default()
    }
}

impl InstallDefaults {
    /// Validates promoted install-scoped workflow defaults.
    ///
    /// Returns an error when persisted listen or planning defaults are outside supported ranges.
    pub fn validate(&self) -> Result<()> {
        if let Some(interval) = self.listen.poll_interval_seconds {
            validate_listen_poll_interval_seconds(interval)?;
        }
        if let Some(limit) = self.plan.interactive_follow_up_questions {
            validate_interactive_plan_follow_up_question_limit(limit)?;
        }
        if let Some(limit) = self.plan.fast_questions {
            validate_fast_plan_question_limit(limit)?;
        }
        Ok(())
    }
}

impl PlanningSyncSettings {
    pub fn discussion_file_char_limit(&self) -> usize {
        self.discussion_file_char_limit
            .unwrap_or(DEFAULT_SYNC_DISCUSSION_FILE_CHAR_LIMIT)
    }

    pub fn discussion_prompt_char_limit(&self) -> usize {
        self.discussion_prompt_char_limit
            .unwrap_or(DEFAULT_SYNC_DISCUSSION_PROMPT_CHAR_LIMIT)
    }

    fn validate(&self) -> Result<()> {
        if matches!(self.discussion_file_char_limit, Some(0)) {
            return Err(anyhow!(
                "repo sync discussion file char limit must be greater than zero"
            ));
        }
        if matches!(self.discussion_prompt_char_limit, Some(0)) {
            return Err(anyhow!(
                "repo sync discussion prompt char limit must be greater than zero"
            ));
        }
        Ok(())
    }
}

impl PlanningIssueLabels {
    pub fn plan_label(&self) -> String {
        self.plan
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("plan")
            .to_string()
    }

    pub fn technical_label(&self) -> String {
        self.technical
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("technical")
            .to_string()
    }
}

impl MergeSettings {
    pub fn validation_repair_attempts(&self) -> usize {
        self.validation_repair_attempts
            .unwrap_or(DEFAULT_MERGE_VALIDATION_REPAIR_ATTEMPTS)
    }

    pub fn validation_transient_retry_attempts(&self) -> usize {
        self.validation_transient_retry_attempts
            .unwrap_or(DEFAULT_MERGE_VALIDATION_TRANSIENT_RETRY_ATTEMPTS)
    }

    pub fn publication_retry_attempts(&self) -> usize {
        self.publication_retry_attempts
            .unwrap_or(DEFAULT_MERGE_PUBLICATION_RETRY_ATTEMPTS)
    }

    fn validate(&self) -> Result<()> {
        if let Some(limit) = self.validation_repair_attempts {
            validate_merge_validation_repair_attempts(limit)?;
        }
        if let Some(limit) = self.validation_transient_retry_attempts {
            validate_merge_validation_transient_retry_attempts(limit)?;
        }
        if let Some(limit) = self.publication_retry_attempts {
            validate_merge_publication_retry_attempts(limit)?;
        }
        Ok(())
    }
}

impl BacklogSettings {
    fn validate(&self, scope: &str) -> Result<()> {
        if self
            .default_assignee
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(anyhow!("{scope} assignee cannot be empty"));
        }
        if self
            .default_state
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(anyhow!("{scope} state cannot be empty"));
        }
        if let Some(priority) = self.default_priority {
            validate_backlog_default_priority(priority)
                .with_context(|| format!("invalid {scope} priority"))?;
        }
        validate_backlog_labels(&self.default_labels)
            .with_context(|| format!("invalid {scope} labels"))?;
        self.velocity_defaults
            .validate(scope)
            .with_context(|| format!("invalid {scope} velocity defaults"))?;
        Ok(())
    }
}

impl VelocityDefaults {
    fn validate(&self, _scope: &str) -> Result<()> {
        if self
            .project
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(anyhow!("velocity project cannot be empty"));
        }
        if self
            .state
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(anyhow!("velocity state cannot be empty"));
        }
        Ok(())
    }
}

impl LinearConfig {
    pub fn new_with_root(root: Option<&Path>, overrides: LinearConfigOverrides) -> Result<Self> {
        let app_config = AppConfig::load()?;
        let planning_meta = match root {
            Some(root) => PlanningMeta::load(root)?,
            None => PlanningMeta::default(),
        };

        Self::from_sources(&app_config, &planning_meta, root, overrides)
    }

    pub fn from_sources(
        app_config: &AppConfig,
        planning_meta: &PlanningMeta,
        root: Option<&Path>,
        overrides: LinearConfigOverrides,
    ) -> Result<Self> {
        let selected_profile = normalize_optional_owned(overrides.profile)
            .or_else(|| normalize_optional_ref(planning_meta.linear.profile.as_deref()))
            .or_else(|| normalize_optional_ref(app_config.linear.default_profile.as_deref()));

        let explicit_api_key = normalize_optional_owned(overrides.api_key);
        let explicit_api_url = normalize_optional_owned(overrides.api_url);
        let explicit_team = normalize_optional_owned(overrides.default_team);

        let profile = selected_profile
            .as_deref()
            .map(|name| resolve_named_profile(&app_config.linear, name))
            .transpose()?;
        let api_key = explicit_api_key
            .or_else(|| root.and_then(|root| app_config.repo_linear_api_key(root)))
            .or_else(|| profile.as_ref().and_then(ResolvedLinearProfile::api_key))
            .or_else(|| normalize_optional_ref(app_config.linear.api_key.as_deref()))
            .or_else(|| normalize_optional_owned(env::var("LINEAR_API_KEY").ok()))
            .ok_or_else(Self::missing_auth_error)?;
        let api_url = explicit_api_url
            .or_else(|| profile.as_ref().map(ResolvedLinearProfile::api_url))
            .or_else(|| normalize_optional_ref(Some(app_config.linear.api_url.as_str())))
            .or_else(|| normalize_optional_owned(env::var("LINEAR_API_URL").ok()))
            .unwrap_or_else(default_linear_api_url);
        let default_team = explicit_team
            .or_else(|| normalize_optional_ref(planning_meta.linear.team.as_deref()))
            .or_else(|| profile.as_ref().and_then(ResolvedLinearProfile::team))
            .or_else(|| normalize_optional_ref(app_config.linear.team.as_deref()))
            .or_else(|| normalize_optional_owned(env::var("LINEAR_TEAM").ok()));

        Ok(Self {
            api_key,
            api_url,
            default_team,
        })
    }

    pub fn missing_auth_error() -> anyhow::Error {
        anyhow!(
            "Linear auth is required for this command. Set LINEAR_API_KEY, run `meta runtime config`, or pass `--api-key <token>`."
        )
    }
}

pub async fn ensure_saved_issue_labels(
    root: &Path,
    app_config: &AppConfig,
    planning_meta: &PlanningMeta,
) -> Result<()> {
    let mut labels = BTreeSet::from([
        planning_meta.effective_plan_label(app_config),
        planning_meta.effective_technical_label(app_config),
    ]);
    if planning_meta.listen.required_label_names().is_empty() {
        if let Some(required_label) = planning_meta.effective_listen_required_label(app_config) {
            labels.insert(required_label);
        }
    } else {
        labels.extend(planning_meta.listen.required_label_names().iter().cloned());
    }
    let config = match LinearConfig::from_sources(
        app_config,
        planning_meta,
        Some(root),
        LinearConfigOverrides::default(),
    ) {
        Ok(config) => config,
        Err(error) if error.to_string() == LinearConfig::missing_auth_error().to_string() => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let Some(default_team) = config.default_team.clone() else {
        return Ok(());
    };

    let service = LinearService::new(ReqwestLinearClient::new(config)?, Some(default_team));
    service
        .ensure_issue_labels_exist(None, &labels.into_iter().collect::<Vec<_>>())
        .await
}

pub fn resolve_agent_config(
    app_config: &AppConfig,
    planning_meta: &PlanningMeta,
    route_key: Option<&str>,
    overrides: AgentConfigOverrides,
) -> Result<ResolvedAgentConfig> {
    let explicit_provider =
        normalize_optional_owned(overrides.provider).map(|value| normalize_agent_name(&value));
    if let Some(provider) = explicit_provider.as_deref() {
        validate_agent_name(app_config, provider)?;
    }

    let route = match route_key {
        Some(key) => match resolve_agent_route(
            app_config,
            planning_meta,
            key,
            AgentConfigOverrides::default(),
        ) {
            Ok(resolved) => Some(resolved),
            Err(error) if explicit_provider.is_some() && is_no_agent_selected_error(&error) => None,
            Err(error) => return Err(error),
        },
        None => None,
    };

    let provider = explicit_provider
        .clone()
        .or_else(|| route.as_ref().map(|resolved| resolved.provider.clone()))
        .or_else(|| normalize_optional_ref(planning_meta.agent.provider.as_deref()))
        .or_else(|| normalize_optional_ref(app_config.agents.default_agent.as_deref()))
        .map(|value| normalize_agent_name(&value))
        .ok_or_else(|| anyhow!(NoAgentSelectedError::global()))?;
    validate_agent_name(app_config, &provider)?;
    let (model, model_source) = resolve_supported_model(
        &provider,
        normalize_optional_owned(overrides.model).map(|value| AgentValueCandidate {
            value,
            source: AgentConfigSource::ExplicitOverride,
            provider: explicit_provider.clone(),
        }),
        route
            .as_ref()
            .and_then(|resolved| resolved.model.clone())
            .zip(
                route
                    .as_ref()
                    .and_then(|resolved| resolved.model_source.clone()),
            )
            .map(|(value, source)| AgentValueCandidate {
                value,
                source,
                provider: route.as_ref().map(|resolved| resolved.provider.clone()),
            }),
        None,
        normalize_optional_ref(planning_meta.agent.model.as_deref()).map(|value| {
            AgentValueCandidate {
                value,
                source: AgentConfigSource::RepoDefault,
                provider: normalize_optional_ref(planning_meta.agent.provider.as_deref())
                    .map(|provider| normalize_agent_name(&provider)),
            }
        }),
        normalize_optional_ref(app_config.agents.default_model.as_deref()).map(|value| {
            AgentValueCandidate {
                value,
                source: AgentConfigSource::GlobalDefault,
                provider: normalize_optional_ref(app_config.agents.default_agent.as_deref())
                    .map(|provider| normalize_agent_name(&provider)),
            }
        }),
    )?;
    let (reasoning, reasoning_source) = resolve_supported_reasoning(
        &provider,
        model.as_deref(),
        normalize_optional_owned(overrides.reasoning).map(|value| AgentValueCandidate {
            value,
            source: AgentConfigSource::ExplicitOverride,
            provider: explicit_provider.clone(),
        }),
        route
            .as_ref()
            .and_then(|resolved| resolved.reasoning.clone())
            .zip(
                route
                    .as_ref()
                    .and_then(|resolved| resolved.reasoning_source.clone()),
            )
            .map(|(value, source)| AgentValueCandidate {
                value,
                source,
                provider: route.as_ref().map(|resolved| resolved.provider.clone()),
            }),
        None,
        normalize_optional_ref(planning_meta.agent.reasoning.as_deref()).map(|value| {
            AgentValueCandidate {
                value,
                source: AgentConfigSource::RepoDefault,
                provider: normalize_optional_ref(planning_meta.agent.provider.as_deref())
                    .map(|provider| normalize_agent_name(&provider)),
            }
        }),
        normalize_optional_ref(app_config.agents.default_reasoning.as_deref()).map(|value| {
            AgentValueCandidate {
                value,
                source: AgentConfigSource::GlobalDefault,
                provider: normalize_optional_ref(app_config.agents.default_agent.as_deref())
                    .map(|provider| normalize_agent_name(&provider)),
            }
        }),
    )?;

    Ok(ResolvedAgentConfig {
        provider,
        model,
        reasoning,
        route_key: route.as_ref().map(|resolved| resolved.route_key.clone()),
        family_key: route.as_ref().map(|resolved| resolved.family_key.clone()),
        provider_source: explicit_provider
            .map(|_| AgentConfigSource::ExplicitOverride)
            .or_else(|| {
                route
                    .as_ref()
                    .map(|resolved| resolved.provider_source.clone())
            })
            .unwrap_or_else(|| {
                if normalize_optional_ref(planning_meta.agent.provider.as_deref()).is_some() {
                    AgentConfigSource::RepoDefault
                } else {
                    AgentConfigSource::GlobalDefault
                }
            }),
        model_source,
        reasoning_source,
    })
}

pub fn supported_agent_route_definitions() -> &'static [AgentRouteDefinition] {
    const ROUTES: &[AgentRouteDefinition] = &[
        AgentRouteDefinition {
            key: AGENT_ROUTE_BACKLOG_PLAN,
            family: "backlog",
            label: "meta backlog plan",
        },
        AgentRouteDefinition {
            key: AGENT_ROUTE_BACKLOG_IMPROVE,
            family: "backlog",
            label: "meta backlog improve",
        },
        AgentRouteDefinition {
            key: AGENT_ROUTE_BACKLOG_SPLIT,
            family: "backlog",
            label: "meta backlog split",
        },
        AgentRouteDefinition {
            key: AGENT_ROUTE_CONTEXT_SCAN,
            family: "context",
            label: "meta context scan",
        },
        AgentRouteDefinition {
            key: AGENT_ROUTE_CONTEXT_RELOAD,
            family: "context",
            label: "meta context reload",
        },
        AgentRouteDefinition {
            key: AGENT_ROUTE_LINEAR_ISSUES_REFINE,
            family: "linear",
            label: "meta linear issues refine",
        },
        AgentRouteDefinition {
            key: AGENT_ROUTE_AGENTS_LISTEN,
            family: "agents",
            label: "meta agents listen",
        },
        AgentRouteDefinition {
            key: AGENT_ROUTE_AGENTS_WORKFLOWS_RUN,
            family: "agents",
            label: "meta agents workflows run",
        },
        AgentRouteDefinition {
            key: AGENT_ROUTE_RUNTIME_CRON_PROMPT,
            family: "runtime.cron",
            label: "meta runtime cron prompt jobs",
        },
        AgentRouteDefinition {
            key: AGENT_ROUTE_MERGE,
            family: "merge",
            label: "meta merge",
        },
    ];
    ROUTES
}

pub fn supported_agent_route_families() -> Vec<&'static str> {
    let mut families = supported_agent_route_definitions()
        .iter()
        .map(|definition| definition.family)
        .collect::<Vec<_>>();
    families.sort_unstable();
    families.dedup();
    families
}

pub fn supported_agent_route_definition(key: &str) -> Option<&'static AgentRouteDefinition> {
    let normalized = normalize_agent_name(key);
    supported_agent_route_definitions()
        .iter()
        .find(|definition| definition.key == normalized)
}

pub fn normalize_agent_route_key(scope: AgentRouteScope, key: &str) -> Result<String> {
    let normalized = normalize_agent_name(key);
    let valid = match scope {
        AgentRouteScope::Family => supported_agent_route_families()
            .into_iter()
            .any(|candidate| candidate == normalized),
        AgentRouteScope::Command => supported_agent_route_definition(&normalized).is_some(),
    };
    if valid {
        Ok(normalized)
    } else {
        let expected = match scope {
            AgentRouteScope::Family => supported_agent_route_families().join(", "),
            AgentRouteScope::Command => supported_agent_route_definitions()
                .iter()
                .map(|definition| definition.key)
                .collect::<Vec<_>>()
                .join(", "),
        };
        Err(anyhow!(
            "unknown {} route key `{}`; supported keys: {}",
            match scope {
                AgentRouteScope::Family => "agent family",
                AgentRouteScope::Command => "agent command",
            },
            normalized,
            expected
        ))
    }
}

pub fn resolve_agent_route(
    app_config: &AppConfig,
    planning_meta: &PlanningMeta,
    route_key: &str,
    overrides: AgentConfigOverrides,
) -> Result<ResolvedAgentRoute> {
    let definition = supported_agent_route_definition(route_key).ok_or_else(|| {
        anyhow!(
            "unknown agent command route `{}`; supported keys: {}",
            normalize_agent_name(route_key),
            supported_agent_route_definitions()
                .iter()
                .map(|route| route.key)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    let explicit_provider =
        normalize_optional_owned(overrides.provider).map(|value| normalize_agent_name(&value));
    if let Some(provider) = explicit_provider.as_deref() {
        validate_agent_name(app_config, provider)?;
    }

    let route_command = app_config.agents.routing.commands.get(definition.key);
    let route_family = app_config.agents.routing.families.get(definition.family);
    let repo_provider = normalize_optional_ref(planning_meta.agent.provider.as_deref())
        .map(|value| normalize_agent_name(&value));
    let global_provider = normalize_optional_ref(app_config.agents.default_agent.as_deref())
        .map(|value| normalize_agent_name(&value));

    let (provider, provider_source) = if let Some(provider) = explicit_provider.clone() {
        (provider, AgentConfigSource::ExplicitOverride)
    } else if let Some(provider) = route_command
        .and_then(|config| normalize_optional_ref(config.provider.as_deref()))
        .map(|value| normalize_agent_name(&value))
    {
        (
            provider,
            AgentConfigSource::CommandRoute(definition.key.to_string()),
        )
    } else if let Some(provider) = route_family
        .and_then(|config| normalize_optional_ref(config.provider.as_deref()))
        .map(|value| normalize_agent_name(&value))
    {
        (
            provider,
            AgentConfigSource::FamilyRoute(definition.family.to_string()),
        )
    } else if let Some(provider) = repo_provider.clone() {
        (provider, AgentConfigSource::RepoDefault)
    } else if let Some(provider) = global_provider.clone() {
        (provider, AgentConfigSource::GlobalDefault)
    } else {
        return Err(anyhow!(NoAgentSelectedError::for_route(definition.key)));
    };
    validate_agent_name(app_config, &provider)?;

    let (model, model_source) = resolve_supported_model(
        &provider,
        normalize_optional_owned(overrides.model).map(|value| AgentValueCandidate {
            value,
            source: AgentConfigSource::ExplicitOverride,
            provider: explicit_provider.clone(),
        }),
        route_command
            .and_then(|config| normalize_optional_ref(config.model.as_deref()))
            .map(|value| AgentValueCandidate {
                value,
                source: AgentConfigSource::CommandRoute(definition.key.to_string()),
                provider: route_command
                    .and_then(|config| normalize_optional_ref(config.provider.as_deref()))
                    .map(|provider| normalize_agent_name(&provider)),
            }),
        route_family
            .and_then(|config| normalize_optional_ref(config.model.as_deref()))
            .map(|value| AgentValueCandidate {
                value,
                source: AgentConfigSource::FamilyRoute(definition.family.to_string()),
                provider: route_family
                    .and_then(|config| normalize_optional_ref(config.provider.as_deref()))
                    .map(|provider| normalize_agent_name(&provider)),
            }),
        normalize_optional_ref(planning_meta.agent.model.as_deref()).map(|value| {
            AgentValueCandidate {
                value,
                source: AgentConfigSource::RepoDefault,
                provider: repo_provider.clone(),
            }
        }),
        normalize_optional_ref(app_config.agents.default_model.as_deref()).map(|value| {
            AgentValueCandidate {
                value,
                source: AgentConfigSource::GlobalDefault,
                provider: global_provider.clone(),
            }
        }),
    )?;
    let (reasoning, reasoning_source) = resolve_supported_reasoning(
        &provider,
        model.as_deref(),
        normalize_optional_owned(overrides.reasoning).map(|value| AgentValueCandidate {
            value,
            source: AgentConfigSource::ExplicitOverride,
            provider: explicit_provider.clone(),
        }),
        route_command
            .and_then(|config| normalize_optional_ref(config.reasoning.as_deref()))
            .map(|value| AgentValueCandidate {
                value,
                source: AgentConfigSource::CommandRoute(definition.key.to_string()),
                provider: route_command
                    .and_then(|config| normalize_optional_ref(config.provider.as_deref()))
                    .map(|provider| normalize_agent_name(&provider)),
            }),
        route_family
            .and_then(|config| normalize_optional_ref(config.reasoning.as_deref()))
            .map(|value| AgentValueCandidate {
                value,
                source: AgentConfigSource::FamilyRoute(definition.family.to_string()),
                provider: route_family
                    .and_then(|config| normalize_optional_ref(config.provider.as_deref()))
                    .map(|provider| normalize_agent_name(&provider)),
            }),
        normalize_optional_ref(planning_meta.agent.reasoning.as_deref()).map(|value| {
            AgentValueCandidate {
                value,
                source: AgentConfigSource::RepoDefault,
                provider: repo_provider.clone(),
            }
        }),
        normalize_optional_ref(app_config.agents.default_reasoning.as_deref()).map(|value| {
            AgentValueCandidate {
                value,
                source: AgentConfigSource::GlobalDefault,
                provider: global_provider.clone(),
            }
        }),
    )?;

    Ok(ResolvedAgentRoute {
        route_key: definition.key.to_string(),
        family_key: definition.family.to_string(),
        provider,
        model,
        reasoning,
        provider_source,
        model_source,
        reasoning_source,
    })
}

impl From<PromptTransportArg> for PromptTransport {
    fn from(value: PromptTransportArg) -> Self {
        match value {
            PromptTransportArg::Arg => Self::Arg,
            PromptTransportArg::Stdin => Self::Stdin,
        }
    }
}

impl From<ListenRefreshPolicyArg> for ListenRefreshPolicy {
    fn from(value: ListenRefreshPolicyArg) -> Self {
        match value {
            ListenRefreshPolicyArg::ReuseAndRefresh => Self::ReuseAndRefresh,
            ListenRefreshPolicyArg::RecreateFromOriginMain => Self::RecreateFromOriginMain,
        }
    }
}

pub fn resolve_config_path() -> Result<PathBuf> {
    config_path_from_env_or_home().ok_or_else(|| {
        anyhow!(
            "could not determine a config path; set {} or HOME/XDG_CONFIG_HOME",
            METASTACK_CONFIG_ENV
        )
    })
}

pub fn resolve_data_root() -> Result<PathBuf> {
    let config_path = resolve_config_path()?;
    data_root_from_config_path(&config_path)
}

pub(crate) fn data_root_from_config_path(config_path: &Path) -> Result<PathBuf> {
    let config_parent = config_path.parent().ok_or_else(|| {
        anyhow!(
            "could not determine a MetaStack data root from config path `{}`",
            config_path.display()
        )
    })?;

    Ok(config_parent.join("data"))
}

pub fn normalize_agent_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

pub fn supported_agent_names() -> &'static [&'static str] {
    builtin_provider_names()
}

pub fn supported_agent_models(name: &str) -> Vec<&'static str> {
    builtin_provider_model_keys(name)
}

pub fn supported_reasoning_options(agent: &str, model: Option<&str>) -> Vec<&'static str> {
    builtin_provider_reasoning_keys(agent, model)
}

pub fn detect_supported_agents() -> Vec<String> {
    supported_agent_names()
        .iter()
        .copied()
        .filter(|name| command_exists(name))
        .map(str::to_string)
        .collect()
}

pub fn builtin_agent_definition(name: &str) -> Option<AgentCommandConfig> {
    builtin_provider_adapter(name).map(|provider| provider.command_definition())
}

fn command_exists(command: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&paths).any(|entry| {
        let candidate = entry.join(command);
        if candidate.is_file() {
            return true;
        }

        #[cfg(windows)]
        {
            let executable = entry.join(format!("{command}.exe"));
            executable.is_file()
        }

        #[cfg(not(windows))]
        {
            false
        }
    })
}

fn config_path_from_env_or_home() -> Option<PathBuf> {
    if let Some(path) = env::var_os(METASTACK_CONFIG_ENV) {
        return Some(PathBuf::from(path));
    }

    if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(path).join("metastack").join("config.toml"));
    }

    #[cfg(windows)]
    if let Some(path) = env::var_os("APPDATA") {
        return Some(PathBuf::from(path).join("metastack").join("config.toml"));
    }

    env::var_os("HOME").map(|path| {
        PathBuf::from(path)
            .join(".config")
            .join("metastack")
            .join("config.toml")
    })
}

fn default_linear_api_url() -> String {
    DEFAULT_LINEAR_API_URL.to_string()
}

/// Validates the configured fast-plan follow-up question limit.
///
/// Returns an error when the value is outside the supported range of 0 through 10.
pub fn validate_fast_plan_question_limit(limit: usize) -> Result<()> {
    if !(MIN_FAST_PLAN_QUESTION_LIMIT..=MAX_FAST_PLAN_QUESTION_LIMIT).contains(&limit) {
        return Err(anyhow!(
            "fast plan follow-up question limit must be between {} and {}",
            MIN_FAST_PLAN_QUESTION_LIMIT,
            MAX_FAST_PLAN_QUESTION_LIMIT
        ));
    }
    Ok(())
}

fn normalize_required_labels<I, S>(values: I) -> Option<Vec<String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();

    for value in values {
        let trimmed = value.as_ref().trim();
        if trimmed.is_empty() {
            continue;
        }

        if seen.insert(trimmed.to_ascii_lowercase()) {
            normalized.push(trimmed.to_string());
        }
    }

    (!normalized.is_empty()).then_some(normalized)
}

/// Parses comma-separated listen labels and removes empty or duplicate values.
pub(crate) fn parse_listen_required_labels_csv(value: &str) -> Option<Vec<String>> {
    normalize_required_labels(value.split(','))
}

fn resolve_supported_model(
    provider: &str,
    explicit: Option<AgentValueCandidate>,
    command_route: Option<AgentValueCandidate>,
    family_route: Option<AgentValueCandidate>,
    repo_default: Option<AgentValueCandidate>,
    global_default: Option<AgentValueCandidate>,
) -> Result<(Option<String>, Option<AgentConfigSource>)> {
    resolve_supported_agent_value(
        provider,
        explicit,
        [command_route, family_route, repo_default, global_default],
        |value| validate_agent_model(provider, Some(value)),
    )
}

fn resolve_supported_reasoning(
    provider: &str,
    model: Option<&str>,
    explicit: Option<AgentValueCandidate>,
    command_route: Option<AgentValueCandidate>,
    family_route: Option<AgentValueCandidate>,
    repo_default: Option<AgentValueCandidate>,
    global_default: Option<AgentValueCandidate>,
) -> Result<(Option<String>, Option<AgentConfigSource>)> {
    resolve_supported_agent_value(
        provider,
        explicit,
        [command_route, family_route, repo_default, global_default],
        |value| validate_agent_reasoning(provider, model, Some(value)),
    )
}

fn resolve_supported_agent_value<const N: usize>(
    provider: &str,
    explicit: Option<AgentValueCandidate>,
    fallbacks: [Option<AgentValueCandidate>; N],
    validate: impl Fn(&str) -> Result<()>,
) -> Result<(Option<String>, Option<AgentConfigSource>)> {
    if let Some(candidate) = explicit {
        if candidate
            .provider
            .as_deref()
            .is_some_and(|value| value != provider)
        {
            return Ok((None, None));
        }
        validate(&candidate.value)?;
        return Ok((Some(candidate.value), Some(candidate.source)));
    }

    for candidate in fallbacks.into_iter().flatten() {
        if candidate
            .provider
            .as_deref()
            .is_some_and(|value| value != provider)
        {
            continue;
        }
        if validate(&candidate.value).is_ok() {
            return Ok((Some(candidate.value), Some(candidate.source)));
        }
    }

    Ok((None, None))
}

pub fn validate_supported_agent(agent: &str) -> Result<()> {
    let normalized = normalize_agent_name(agent);
    if supported_agent_names()
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&normalized))
    {
        Ok(())
    } else {
        Err(anyhow!(
            "agent `{normalized}` is not supported; choose one of: {}",
            supported_agent_names().join(", ")
        ))
    }
}

pub fn validate_agent_name(app_config: &AppConfig, agent: &str) -> Result<()> {
    let normalized = normalize_agent_name(agent);
    if app_config.resolve_agent_definition(&normalized).is_some() {
        Ok(())
    } else {
        validate_supported_agent(&normalized)
    }
}

pub fn validate_agent_model(agent: &str, model: Option<&str>) -> Result<()> {
    let supported_models = supported_agent_models(agent);
    if supported_models.is_empty() {
        return Ok(());
    }

    if let Some(model) = normalize_optional_ref(model)
        && supported_models
            .iter()
            .all(|candidate| !candidate.eq_ignore_ascii_case(&model))
    {
        return Err(anyhow!(
            "model `{model}` is not supported for agent `{}`; supported models: {}",
            normalize_agent_name(agent),
            supported_models.join(", ")
        ));
    }

    Ok(())
}

pub fn validate_agent_reasoning(
    agent: &str,
    model: Option<&str>,
    reasoning: Option<&str>,
) -> Result<()> {
    let Some(reasoning) = normalize_optional_ref(reasoning) else {
        return Ok(());
    };
    let supported_reasoning = supported_reasoning_options(agent, model);
    if supported_reasoning.is_empty() {
        if supported_agent_models(agent).is_empty() {
            return Ok(());
        }

        return Err(anyhow!(
            "reasoning `{reasoning}` requires an explicit supported model for agent `{}`; choose one of: {}",
            normalize_agent_name(agent),
            supported_agent_models(agent).join(", ")
        ));
    }

    if supported_reasoning
        .iter()
        .all(|candidate| !candidate.eq_ignore_ascii_case(&reasoning))
    {
        return Err(anyhow!(
            "reasoning `{reasoning}` is not supported for agent `{}` and model `{}`; supported reasoning: {}",
            normalize_agent_name(agent),
            model.unwrap_or(""),
            supported_reasoning.join(", ")
        ));
    }

    Ok(())
}

pub fn validate_interactive_plan_follow_up_question_limit(limit: usize) -> Result<()> {
    if (MIN_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT
        ..=MAX_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT)
        .contains(&limit)
    {
        Ok(())
    } else {
        Err(anyhow!(
            "interactive plan follow-up question limit must be between {} and {}; got {limit}",
            MIN_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT,
            MAX_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT
        ))
    }
}

pub fn validate_listen_poll_interval_seconds(interval: u64) -> Result<()> {
    if interval >= 1 {
        Ok(())
    } else {
        Err(anyhow!(
            "listen poll interval must be at least 1 second; got {interval}"
        ))
    }
}

fn validate_merge_validation_repair_attempts(limit: usize) -> Result<()> {
    if limit >= 1 {
        Ok(())
    } else {
        Err(anyhow!(
            "merge validation repair attempt limit must be at least 1; got {limit}"
        ))
    }
}

pub fn validate_backlog_default_priority(priority: u8) -> Result<()> {
    if (1..=4).contains(&priority) {
        Ok(())
    } else {
        Err(anyhow!(
            "backlog default priority must be between 1 and 4; got {priority}"
        ))
    }
}

fn validate_merge_validation_transient_retry_attempts(limit: usize) -> Result<()> {
    if limit <= 10 {
        Ok(())
    } else {
        Err(anyhow!(
            "merge transient validation retry attempt limit must be between 0 and 10; got {limit}"
        ))
    }
}

fn validate_merge_publication_retry_attempts(limit: usize) -> Result<()> {
    if limit >= 1 {
        Ok(())
    } else {
        Err(anyhow!(
            "merge publication retry attempt limit must be at least 1; got {limit}"
        ))
    }
}

pub fn validate_backlog_labels(labels: &[String]) -> Result<()> {
    for label in labels {
        if label.trim().is_empty() {
            return Err(anyhow!(
                "backlog default labels cannot include empty values"
            ));
        }
    }
    Ok(())
}

fn resolve_named_profile<'a>(
    settings: &'a LinearSettings,
    name: &str,
) -> Result<ResolvedLinearProfile<'a>> {
    let profile = settings
        .profiles
        .get(name)
        .ok_or_else(|| anyhow!("Linear profile `{name}` is not configured. Add it under `[linear.profiles.{name}]` or switch the selected profile."))?;
    let mut missing = Vec::new();
    if normalize_optional_ref(profile.api_key.as_deref()).is_none() {
        missing.push("api_key");
    }
    if normalize_optional_ref(Some(profile.api_url.as_str())).is_none() {
        missing.push("api_url");
    }
    if !missing.is_empty() {
        return Err(anyhow!(
            "Linear profile `{name}` is incomplete; missing required field{}: {}",
            if missing.len() == 1 { "" } else { "s" },
            missing.join(", ")
        ));
    }

    Ok(ResolvedLinearProfile { profile })
}

#[derive(Debug, Clone, Copy)]
struct ResolvedLinearProfile<'a> {
    profile: &'a LinearProfileSettings,
}

impl ResolvedLinearProfile<'_> {
    fn api_key(&self) -> Option<String> {
        normalize_optional_ref(self.profile.api_key.as_deref())
    }

    fn api_url(&self) -> String {
        normalize_optional_ref(Some(self.profile.api_url.as_str()))
            .unwrap_or_else(default_linear_api_url)
    }

    fn team(&self) -> Option<String> {
        normalize_optional_ref(self.profile.team.as_deref())
    }
}

fn normalize_optional_owned(value: Option<String>) -> Option<String> {
    value.and_then(|value| normalize_optional_ref(Some(value.as_str())))
}

fn repo_auth_key(root: &Path) -> String {
    root.display().to_string()
}

fn normalize_optional_ref(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn validate_agent_route_config(
    app_config: &AppConfig,
    route: Option<&AgentRouteConfig>,
    fallback: Option<&AgentRouteConfig>,
    family_key: Option<&str>,
    command_key: Option<&str>,
) -> Result<()> {
    let Some(route) = route else {
        return Ok(());
    };
    let provider = normalize_optional_ref(route.provider.as_deref())
        .or_else(|| fallback.and_then(|config| normalize_optional_ref(config.provider.as_deref())))
        .or_else(|| normalize_optional_ref(app_config.agents.default_agent.as_deref()));
    let context = if let Some(command_key) = command_key {
        format!("agent route `{command_key}`")
    } else if let Some(family_key) = family_key {
        format!("agent family route `{family_key}`")
    } else {
        "agent route".to_string()
    };

    if let Some(provider_value) = normalize_optional_ref(route.provider.as_deref()) {
        validate_agent_name(app_config, &provider_value)
            .with_context(|| format!("{context} has an invalid provider"))?;
    }

    if normalize_optional_ref(route.model.as_deref()).is_some() {
        let provider = provider.clone().ok_or_else(|| {
            anyhow!(
                "{context} sets a model but no provider can be resolved from the route, family, or global defaults"
            )
        })?;
        validate_agent_model(&provider, route.model.as_deref())
            .with_context(|| format!("{context} has an invalid model"))?;
    }
    if normalize_optional_ref(route.reasoning.as_deref()).is_some() {
        let provider = provider.clone().ok_or_else(|| {
            anyhow!(
                "{context} sets reasoning but no provider can be resolved from the route, family, or global defaults"
            )
        })?;
        let model = normalize_optional_ref(route.model.as_deref())
            .or_else(|| fallback.and_then(|config| normalize_optional_ref(config.model.as_deref())))
            .or_else(|| normalize_optional_ref(app_config.agents.default_model.as_deref()));
        validate_agent_reasoning(&provider, model.as_deref(), route.reasoning.as_deref())
            .with_context(|| format!("{context} has invalid reasoning"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Mutex, OnceLock};

    use serde_json::json;
    use tempfile::tempdir;

    use super::{
        AGENT_ROUTE_BACKLOG_PLAN, AGENT_ROUTE_BACKLOG_SPLIT, AgentConfigOverrides,
        AgentConfigSource, AgentRouteConfig, AgentRouteScope, AgentRoutingSettings, AgentSettings,
        AppConfig, BacklogSettings, DEFAULT_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT,
        DEFAULT_LISTEN_POLL_INTERVAL_SECONDS, DEFAULT_MERGE_PUBLICATION_RETRY_ATTEMPTS,
        DEFAULT_MERGE_VALIDATION_REPAIR_ATTEMPTS,
        DEFAULT_MERGE_VALIDATION_TRANSIENT_RETRY_ATTEMPTS, InstallDefaults, InstallLinearDefaults,
        InstallListenSettings, InstallPlanSettings, InstallUiSettings, ListenAssignmentScope,
        METASTACK_CONFIG_ENV, MergeSettings, NoAgentSelectedError, PlanningAgentSettings,
        PlanningIssueLabels, PlanningListenSettings, PlanningMeta, PlanningPlanSettings,
        VelocityAutoAssign, VelocityDefaults, is_no_agent_selected_error,
        no_agent_selected_route_key, normalize_agent_route_key, parse_listen_required_labels_csv,
        resolve_agent_config, resolve_agent_route, validate_agent_reasoning,
        validate_interactive_plan_follow_up_question_limit, validate_listen_poll_interval_seconds,
    };

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn listen_assignment_scope_serializes_to_explicit_values() {
        assert_eq!(
            serde_json::to_value(ListenAssignmentScope::ViewerOnly).unwrap(),
            json!("viewer_only")
        );
        assert_eq!(
            serde_json::to_value(ListenAssignmentScope::ViewerOrUnassigned).unwrap(),
            json!("viewer_or_unassigned")
        );
        assert_eq!(
            serde_json::to_value(ListenAssignmentScope::Any).unwrap(),
            json!("any")
        );
    }

    #[test]
    fn listen_assignment_scope_accepts_legacy_viewer_value() {
        assert_eq!(
            serde_json::from_value::<ListenAssignmentScope>(json!("viewer")).unwrap(),
            ListenAssignmentScope::ViewerOrUnassigned
        );
    }

    #[test]
    fn planning_meta_loads_legacy_viewer_assignment_scope_deterministically() {
        let meta: PlanningMeta = serde_json::from_value(json!({
            "listen": {
                "assignment_scope": "viewer"
            }
        }))
        .unwrap();

        assert_eq!(
            meta.listen.assignment_scope,
            Some(ListenAssignmentScope::ViewerOrUnassigned)
        );
    }

    #[test]
    fn interactive_plan_follow_up_limit_defaults_to_ten() {
        assert_eq!(
            PlanningMeta::default().interactive_follow_up_question_limit(),
            DEFAULT_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT
        );
    }

    #[test]
    fn listen_poll_interval_defaults_to_seven_seconds() {
        assert_eq!(
            PlanningListenSettings::default().poll_interval_seconds(),
            DEFAULT_LISTEN_POLL_INTERVAL_SECONDS
        );
    }

    #[test]
    fn merge_validation_repair_attempts_default_to_six() {
        assert_eq!(
            MergeSettings::default().validation_repair_attempts(),
            DEFAULT_MERGE_VALIDATION_REPAIR_ATTEMPTS
        );
    }

    #[test]
    fn merge_transient_validation_retries_default_to_three() {
        assert_eq!(
            MergeSettings::default().validation_transient_retry_attempts(),
            DEFAULT_MERGE_VALIDATION_TRANSIENT_RETRY_ATTEMPTS
        );
    }

    #[test]
    fn merge_publication_retries_default_to_five() {
        assert_eq!(
            MergeSettings::default().publication_retry_attempts(),
            DEFAULT_MERGE_PUBLICATION_RETRY_ATTEMPTS
        );
    }

    #[test]
    fn planning_meta_validation_rejects_out_of_range_follow_up_limits() {
        let mut meta = PlanningMeta {
            plan: PlanningPlanSettings {
                interactive_follow_up_questions: Some(0),
                ..PlanningPlanSettings::default()
            },
            ..PlanningMeta::default()
        };

        assert_eq!(
            meta.validate().unwrap_err().to_string(),
            "interactive plan follow-up question limit must be between 1 and 10; got 0"
        );

        meta.plan.interactive_follow_up_questions = Some(11);
        assert_eq!(
            meta.validate().unwrap_err().to_string(),
            "interactive plan follow-up question limit must be between 1 and 10; got 11"
        );
    }

    #[test]
    fn explicit_follow_up_limit_is_returned_when_configured() {
        let meta = PlanningMeta {
            plan: PlanningPlanSettings {
                interactive_follow_up_questions: Some(4),
                ..PlanningPlanSettings::default()
            },
            ..PlanningMeta::default()
        };

        assert_eq!(meta.interactive_follow_up_question_limit(), 4);
    }

    #[test]
    fn explicit_listen_poll_interval_is_returned_when_configured() {
        let settings = PlanningListenSettings {
            poll_interval_seconds: Some(42),
            ..PlanningListenSettings::default()
        };

        assert_eq!(settings.poll_interval_seconds(), 42);
    }

    #[test]
    fn planning_meta_deserializes_required_labels_list() {
        let meta: PlanningMeta = serde_json::from_str(
            r#"{
                "listen": {
                    "required_labels": ["Plan", "Urgent", "plan"]
                }
            }"#,
        )
        .expect("required_labels should deserialize");

        assert_eq!(
            meta.listen.required_labels,
            Some(vec!["Plan".to_string(), "Urgent".to_string()])
        );
    }

    #[test]
    fn install_defaults_fill_missing_repo_defaults() {
        let app_config = AppConfig {
            defaults: InstallDefaults {
                linear: InstallLinearDefaults {
                    project_id: Some("project-install".to_string()),
                },
                listen: InstallListenSettings {
                    required_label: Some("agent".to_string()),
                    assignment_scope: Some(super::ListenAssignmentScope::ViewerOrUnassigned),
                    refresh_policy: Some(super::ListenRefreshPolicy::RecreateFromOriginMain),
                    poll_interval_seconds: Some(42),
                },
                plan: InstallPlanSettings {
                    interactive_follow_up_questions: Some(4),
                    ..InstallPlanSettings::default()
                },
                ui: InstallUiSettings { vim_mode: true },
                issue_labels: PlanningIssueLabels {
                    plan: Some("planning".to_string()),
                    technical: Some("engineering".to_string()),
                },
            },
            ..AppConfig::default()
        };

        let planning_meta = PlanningMeta::default();
        assert_eq!(
            planning_meta.effective_project_id(&app_config).as_deref(),
            Some("project-install")
        );
        assert_eq!(
            planning_meta
                .effective_listen_required_label(&app_config)
                .as_deref(),
            Some("agent")
        );
        assert_eq!(
            planning_meta.effective_listen_assignment_scope(&app_config),
            super::ListenAssignmentScope::ViewerOrUnassigned
        );
        assert_eq!(
            planning_meta.effective_listen_refresh_policy(&app_config),
            super::ListenRefreshPolicy::RecreateFromOriginMain
        );
        assert_eq!(
            planning_meta.effective_listen_poll_interval_seconds(&app_config),
            42
        );
        assert_eq!(
            planning_meta.effective_interactive_follow_up_question_limit(&app_config),
            4
        );
        assert!(app_config.vim_mode_enabled());
        assert_eq!(planning_meta.effective_plan_label(&app_config), "planning");
        assert_eq!(
            planning_meta.effective_technical_label(&app_config),
            "engineering"
        );
    }

    #[test]
    fn app_config_deserializes_without_install_defaults() {
        let app_config: AppConfig = toml::from_str(
            r#"
            [onboarding]
            completed = true
            "#,
        )
        .expect("minimal legacy config should deserialize");

        assert!(app_config.onboarding.completed);
        assert_eq!(app_config.defaults.linear.project_id, None);
        assert_eq!(app_config.defaults.listen.required_label, None);
        assert_eq!(app_config.defaults.listen.assignment_scope, None);
        assert_eq!(app_config.defaults.listen.refresh_policy, None);
        assert_eq!(app_config.defaults.listen.poll_interval_seconds, None);
        assert_eq!(
            app_config.defaults.plan.interactive_follow_up_questions,
            None
        );
        assert!(!app_config.defaults.ui.vim_mode);
        assert_eq!(app_config.defaults.issue_labels.plan, None);
        assert_eq!(app_config.defaults.issue_labels.technical, None);
    }

    #[test]
    fn planning_meta_treats_null_required_labels_as_unset() {
        let meta: PlanningMeta = serde_json::from_str(
            r#"{
                "listen": {
                    "required_labels": null
                }
            }"#,
        )
        .expect("null required_labels should deserialize");

        assert_eq!(meta.listen.required_labels, None);
        assert!(meta.listen.required_label_names().is_empty());
    }

    #[test]
    fn planning_meta_treats_empty_required_labels_as_unset() {
        let meta: PlanningMeta = serde_json::from_str(
            r#"{
                "listen": {
                    "required_labels": []
                }
            }"#,
        )
        .expect("empty required_labels should deserialize");

        assert_eq!(meta.listen.required_labels, None);
        assert!(meta.listen.required_label_names().is_empty());
    }

    #[test]
    fn planning_meta_deserializes_legacy_required_label_into_list() {
        let meta: PlanningMeta = serde_json::from_str(
            r#"{
                "listen": {
                    "required_label": "plan"
                }
            }"#,
        )
        .expect("legacy required_label should deserialize");

        assert_eq!(meta.listen.required_labels, Some(vec!["plan".to_string()]));
    }

    #[test]
    fn explicit_required_labels_override_legacy_required_label() {
        let meta: PlanningMeta = serde_json::from_str(
            r#"{
                "listen": {
                    "required_labels": [],
                    "required_label": "plan"
                }
            }"#,
        )
        .expect("required_labels should take precedence");

        assert_eq!(meta.listen.required_labels, None);
    }

    #[test]
    fn parse_listen_required_labels_csv_trims_and_deduplicates() {
        assert_eq!(
            parse_listen_required_labels_csv(" plan, urgent ,Plan,,"),
            Some(vec!["plan".to_string(), "urgent".to_string()])
        );
    }

    #[test]
    fn repo_defaults_override_install_defaults() {
        let app_config = AppConfig {
            defaults: InstallDefaults {
                linear: InstallLinearDefaults {
                    project_id: Some("project-install".to_string()),
                },
                listen: InstallListenSettings {
                    required_label: Some("agent".to_string()),
                    assignment_scope: Some(super::ListenAssignmentScope::ViewerOrUnassigned),
                    refresh_policy: Some(super::ListenRefreshPolicy::RecreateFromOriginMain),
                    poll_interval_seconds: Some(42),
                },
                plan: InstallPlanSettings {
                    interactive_follow_up_questions: Some(4),
                    ..InstallPlanSettings::default()
                },
                ui: InstallUiSettings { vim_mode: true },
                issue_labels: PlanningIssueLabels {
                    plan: Some("planning".to_string()),
                    technical: Some("engineering".to_string()),
                },
            },
            ..AppConfig::default()
        };
        let planning_meta = PlanningMeta {
            linear: super::PlanningLinearSettings {
                project_id: Some("project-repo".to_string()),
                ..super::PlanningLinearSettings::default()
            },
            listen: PlanningListenSettings {
                required_labels: Some(vec!["repo-agent".to_string()]),
                assignment_scope: Some(super::ListenAssignmentScope::Any),
                refresh_policy: Some(super::ListenRefreshPolicy::ReuseAndRefresh),
                poll_interval_seconds: Some(7),
                ..PlanningListenSettings::default()
            },
            plan: PlanningPlanSettings {
                interactive_follow_up_questions: Some(9),
                ..PlanningPlanSettings::default()
            },
            issue_labels: PlanningIssueLabels {
                plan: Some("repo-plan".to_string()),
                technical: Some("repo-tech".to_string()),
            },
            ..PlanningMeta::default()
        };

        assert_eq!(
            planning_meta.effective_project_id(&app_config).as_deref(),
            Some("project-repo")
        );
        assert_eq!(
            planning_meta
                .effective_listen_required_label(&app_config)
                .as_deref(),
            Some("repo-agent")
        );
        assert_eq!(
            planning_meta.effective_listen_assignment_scope(&app_config),
            super::ListenAssignmentScope::Any
        );
        assert_eq!(
            planning_meta.effective_listen_refresh_policy(&app_config),
            super::ListenRefreshPolicy::ReuseAndRefresh
        );
        assert_eq!(
            planning_meta.effective_listen_poll_interval_seconds(&app_config),
            7
        );
        assert_eq!(
            planning_meta.effective_interactive_follow_up_question_limit(&app_config),
            9
        );
        assert!(app_config.vim_mode_enabled());
        assert_eq!(planning_meta.effective_plan_label(&app_config), "repo-plan");
        assert_eq!(
            planning_meta.effective_technical_label(&app_config),
            "repo-tech"
        );
    }

    #[test]
    fn interactive_follow_up_limit_validation_accepts_values_in_range() {
        assert!(validate_interactive_plan_follow_up_question_limit(1).is_ok());
        assert!(validate_interactive_plan_follow_up_question_limit(10).is_ok());
    }

    #[test]
    fn listen_poll_interval_validation_rejects_zero() {
        assert_eq!(
            validate_listen_poll_interval_seconds(0)
                .unwrap_err()
                .to_string(),
            "listen poll interval must be at least 1 second; got 0"
        );
    }

    #[test]
    fn listen_poll_interval_validation_accepts_positive_values() {
        assert!(validate_listen_poll_interval_seconds(1).is_ok());
        assert!(validate_listen_poll_interval_seconds(60).is_ok());
    }

    #[test]
    fn app_config_validation_rejects_zero_merge_validation_repair_attempts() {
        let config = AppConfig {
            merge: MergeSettings {
                validation_repair_attempts: Some(0),
                ..MergeSettings::default()
            },
            ..AppConfig::default()
        };

        assert_eq!(
            config.validate().unwrap_err().to_string(),
            "merge validation repair attempt limit must be at least 1; got 0"
        );
    }

    #[test]
    fn backlog_settings_validation_rejects_out_of_range_priorities() {
        let config = AppConfig {
            backlog: BacklogSettings {
                default_priority: Some(5),
                ..BacklogSettings::default()
            },
            ..AppConfig::default()
        };

        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("invalid global backlog defaults priority")
        );
    }

    #[test]
    fn app_config_validation_rejects_excessive_merge_transient_retry_attempts() {
        let config = AppConfig {
            merge: MergeSettings {
                validation_transient_retry_attempts: Some(11),
                ..MergeSettings::default()
            },
            ..AppConfig::default()
        };

        assert_eq!(
            config.validate().unwrap_err().to_string(),
            "merge transient validation retry attempt limit must be between 0 and 10; got 11"
        );
    }

    #[test]
    fn backlog_settings_validation_rejects_empty_labels() {
        let meta = PlanningMeta {
            backlog: BacklogSettings {
                default_labels: vec!["team-a".to_string(), " ".to_string()],
                ..BacklogSettings::default()
            },
            ..PlanningMeta::default()
        };

        assert!(
            meta.validate()
                .unwrap_err()
                .to_string()
                .contains("invalid repo backlog defaults labels")
        );
    }

    #[test]
    fn app_config_validation_rejects_zero_merge_publication_retry_attempts() {
        let config = AppConfig {
            merge: MergeSettings {
                publication_retry_attempts: Some(0),
                ..MergeSettings::default()
            },
            ..AppConfig::default()
        };

        assert_eq!(
            config.validate().unwrap_err().to_string(),
            "merge publication retry attempt limit must be at least 1; got 0"
        );
    }

    #[test]
    fn backlog_settings_validation_accepts_supported_velocity_auto_assign() {
        let meta = PlanningMeta {
            backlog: BacklogSettings {
                velocity_defaults: VelocityDefaults {
                    project: Some("MetaStack CLI".to_string()),
                    state: Some("Backlog".to_string()),
                    auto_assign: Some(VelocityAutoAssign::Viewer),
                },
                ..BacklogSettings::default()
            },
            ..PlanningMeta::default()
        };

        assert!(meta.validate().is_ok());
    }

    #[test]
    fn resolve_agent_route_prefers_command_then_family_then_global_defaults() {
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: Some("codex".to_string()),
                default_model: Some("gpt-5.4".to_string()),
                default_reasoning: Some("medium".to_string()),
                routing: AgentRoutingSettings {
                    families: BTreeMap::from([(
                        "backlog".to_string(),
                        AgentRouteConfig {
                            provider: Some("claude".to_string()),
                            model: Some("opus".to_string()),
                            reasoning: Some("high".to_string()),
                        },
                    )]),
                    commands: BTreeMap::from([(
                        AGENT_ROUTE_BACKLOG_PLAN.to_string(),
                        AgentRouteConfig {
                            provider: Some("codex".to_string()),
                            model: Some("gpt-5.3-codex".to_string()),
                            reasoning: Some("low".to_string()),
                        },
                    )]),
                },
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };

        let plan = resolve_agent_route(
            &config,
            &PlanningMeta::default(),
            AGENT_ROUTE_BACKLOG_PLAN,
            Default::default(),
        )
        .expect("plan route should resolve");
        assert_eq!(plan.provider, "codex");
        assert_eq!(plan.model.as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(plan.reasoning.as_deref(), Some("low"));
        assert!(matches!(
            plan.provider_source,
            super::AgentConfigSource::CommandRoute(_)
        ));

        let split = resolve_agent_route(
            &config,
            &PlanningMeta::default(),
            AGENT_ROUTE_BACKLOG_SPLIT,
            Default::default(),
        )
        .expect("split route should resolve");
        assert_eq!(split.provider, "claude");
        assert_eq!(split.model.as_deref(), Some("opus"));
        assert_eq!(split.reasoning.as_deref(), Some("high"));
        assert!(matches!(
            split.provider_source,
            super::AgentConfigSource::FamilyRoute(_)
        ));
    }

    #[test]
    fn app_config_validation_rejects_invalid_route_model_combinations() {
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: Some("codex".to_string()),
                default_model: Some("gpt-5.4".to_string()),
                default_reasoning: None,
                routing: AgentRoutingSettings {
                    families: BTreeMap::new(),
                    commands: BTreeMap::from([(
                        AGENT_ROUTE_BACKLOG_PLAN.to_string(),
                        AgentRouteConfig {
                            provider: None,
                            model: Some("opus".to_string()),
                            reasoning: None,
                        },
                    )]),
                },
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };

        let error = config.validate().expect_err("route validation should fail");
        assert!(
            error
                .to_string()
                .contains("agent route `backlog.plan` has an invalid model")
        );
    }

    #[test]
    fn route_key_normalization_rejects_unknown_keys() {
        let error = normalize_agent_route_key(AgentRouteScope::Command, "backlog.unknown")
            .expect_err("unknown route should fail");
        assert!(
            error
                .to_string()
                .contains("unknown agent command route key `backlog.unknown`")
        );
    }

    #[test]
    fn app_config_save_and_load_round_trip_vim_mode() {
        let _guard = env_lock().lock().expect("env lock should be available");
        let temp = tempdir().expect("temp dir should build");
        let config_path = temp.path().join("config.toml");
        unsafe {
            std::env::set_var(METASTACK_CONFIG_ENV, &config_path);
        }

        let config = AppConfig {
            defaults: InstallDefaults {
                ui: InstallUiSettings { vim_mode: true },
                ..InstallDefaults::default()
            },
            ..AppConfig::default()
        };

        config.save().expect("config should save");
        let loaded = AppConfig::load().expect("config should load");

        unsafe {
            std::env::remove_var(METASTACK_CONFIG_ENV);
        }

        assert!(loaded.vim_mode_enabled());
        assert!(config_path.is_file());
    }

    #[test]
    fn app_config_toml_round_trips_vim_mode() {
        let config = AppConfig {
            defaults: InstallDefaults {
                ui: InstallUiSettings { vim_mode: true },
                ..InstallDefaults::default()
            },
            ..AppConfig::default()
        };

        let encoded = toml::to_string_pretty(&config).expect("config should encode");
        let decoded: AppConfig = toml::from_str(&encoded).expect("config should decode");

        assert!(decoded.vim_mode_enabled());
    }

    #[test]
    fn app_config_toml_round_trips_advanced_routing() {
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: Some("codex".to_string()),
                default_model: Some("gpt-5.4".to_string()),
                default_reasoning: None,
                routing: AgentRoutingSettings {
                    families: BTreeMap::from([(
                        "backlog".to_string(),
                        AgentRouteConfig {
                            provider: Some("claude".to_string()),
                            model: Some("opus".to_string()),
                            reasoning: Some("high".to_string()),
                        },
                    )]),
                    commands: BTreeMap::from([(
                        AGENT_ROUTE_BACKLOG_PLAN.to_string(),
                        AgentRouteConfig {
                            provider: Some("codex".to_string()),
                            model: Some("gpt-5.3-codex".to_string()),
                            reasoning: Some("low".to_string()),
                        },
                    )]),
                },
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };

        let encoded = toml::to_string_pretty(&config).expect("config should encode");
        let decoded: AppConfig = toml::from_str(&encoded).expect("config should decode");
        decoded.validate().expect("decoded config should validate");
        assert_eq!(
            decoded
                .agents
                .routing
                .commands
                .get(AGENT_ROUTE_BACKLOG_PLAN)
                .and_then(|route| route.model.as_deref()),
            Some("gpt-5.3-codex")
        );
        assert_eq!(
            decoded
                .agents
                .routing
                .families
                .get("backlog")
                .and_then(|route| route.provider.as_deref()),
            Some("claude")
        );
    }

    #[test]
    fn resolve_agent_route_ignores_repo_defaults_when_route_defaults_exist() {
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: Some("codex".to_string()),
                default_model: Some("gpt-5.4".to_string()),
                default_reasoning: Some("medium".to_string()),
                routing: AgentRoutingSettings {
                    families: BTreeMap::from([(
                        "backlog".to_string(),
                        AgentRouteConfig {
                            provider: Some("claude".to_string()),
                            model: Some("opus".to_string()),
                            reasoning: Some("high".to_string()),
                        },
                    )]),
                    commands: BTreeMap::from([(
                        AGENT_ROUTE_BACKLOG_PLAN.to_string(),
                        AgentRouteConfig {
                            provider: Some("codex".to_string()),
                            model: Some("gpt-5.3-codex".to_string()),
                            reasoning: Some("low".to_string()),
                        },
                    )]),
                },
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };
        let planning_meta = PlanningMeta {
            agent: PlanningAgentSettings {
                provider: Some("claude".to_string()),
                model: Some("sonnet".to_string()),
                reasoning: Some("medium".to_string()),
            },
            ..PlanningMeta::default()
        };

        let plan = resolve_agent_route(
            &config,
            &planning_meta,
            AGENT_ROUTE_BACKLOG_PLAN,
            Default::default(),
        )
        .expect("plan route should resolve");
        assert_eq!(plan.provider, "codex");
        assert_eq!(plan.model.as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(plan.reasoning.as_deref(), Some("low"));
        assert!(matches!(
            plan.provider_source,
            super::AgentConfigSource::CommandRoute(_)
        ));

        let split = resolve_agent_route(
            &config,
            &planning_meta,
            AGENT_ROUTE_BACKLOG_SPLIT,
            Default::default(),
        )
        .expect("split route should resolve");
        assert_eq!(split.provider, "claude");
        assert_eq!(split.model.as_deref(), Some("opus"));
        assert_eq!(split.reasoning.as_deref(), Some("high"));
        assert!(matches!(
            split.provider_source,
            super::AgentConfigSource::FamilyRoute(_)
        ));
    }

    #[test]
    fn resolve_agent_config_uses_routes_before_repo_defaults() {
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: Some("codex".to_string()),
                default_model: Some("gpt-5.4".to_string()),
                default_reasoning: Some("medium".to_string()),
                routing: AgentRoutingSettings {
                    families: BTreeMap::from([(
                        "backlog".to_string(),
                        AgentRouteConfig {
                            provider: Some("claude".to_string()),
                            model: Some("opus".to_string()),
                            reasoning: Some("high".to_string()),
                        },
                    )]),
                    commands: BTreeMap::new(),
                },
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };
        let planning_meta = PlanningMeta {
            agent: PlanningAgentSettings {
                provider: Some("codex".to_string()),
                model: Some("gpt-5.3-codex".to_string()),
                reasoning: Some("low".to_string()),
            },
            ..PlanningMeta::default()
        };

        let routed = resolve_agent_config(
            &config,
            &planning_meta,
            Some(AGENT_ROUTE_BACKLOG_SPLIT),
            Default::default(),
        )
        .expect("routed config should resolve");
        assert_eq!(routed.provider, "claude");
        assert_eq!(routed.model.as_deref(), Some("opus"));
        assert_eq!(routed.reasoning.as_deref(), Some("high"));

        let no_route_config = AppConfig {
            agents: AgentSettings {
                default_agent: Some("claude".to_string()),
                default_model: Some("opus".to_string()),
                default_reasoning: Some("high".to_string()),
                routing: AgentRoutingSettings::default(),
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };
        let repo_fallback = resolve_agent_config(
            &no_route_config,
            &planning_meta,
            Some(AGENT_ROUTE_BACKLOG_PLAN),
            Default::default(),
        )
        .expect("route without override should fall back to repo defaults");
        assert_eq!(repo_fallback.provider, "codex");
        assert_eq!(repo_fallback.model.as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(repo_fallback.reasoning.as_deref(), Some("low"));

        let unrouted =
            resolve_agent_config(&no_route_config, &planning_meta, None, Default::default())
                .expect("unrouted config should still resolve");
        assert_eq!(unrouted.provider, "codex");
        assert_eq!(unrouted.model.as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(unrouted.reasoning.as_deref(), Some("low"));
    }

    #[test]
    fn resolve_agent_route_returns_typed_missing_agent_error() {
        let error = resolve_agent_route(
            &AppConfig::default(),
            &PlanningMeta::default(),
            AGENT_ROUTE_BACKLOG_PLAN,
            Default::default(),
        )
        .expect_err("route without any provider should fail");

        assert!(is_no_agent_selected_error(&error));
        assert_eq!(
            no_agent_selected_route_key(&error),
            Some(AGENT_ROUTE_BACKLOG_PLAN)
        );
        assert_eq!(
            error.to_string(),
            NoAgentSelectedError::for_route(AGENT_ROUTE_BACKLOG_PLAN).to_string()
        );
    }

    #[test]
    fn resolve_agent_config_returns_typed_missing_agent_error_without_route() {
        let error = resolve_agent_config(
            &AppConfig::default(),
            &PlanningMeta::default(),
            None,
            Default::default(),
        )
        .expect_err("global config without any provider should fail");

        assert!(is_no_agent_selected_error(&error));
        assert_eq!(no_agent_selected_route_key(&error), None);
        assert_eq!(
            error.to_string(),
            NoAgentSelectedError::global().to_string()
        );
    }

    #[test]
    fn resolve_agent_config_skips_incompatible_route_values_when_provider_is_overridden() {
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: Some("codex".to_string()),
                default_model: Some("gpt-5.4".to_string()),
                default_reasoning: Some("low".to_string()),
                routing: AgentRoutingSettings {
                    families: BTreeMap::new(),
                    commands: BTreeMap::from([(
                        AGENT_ROUTE_BACKLOG_PLAN.to_string(),
                        AgentRouteConfig {
                            provider: Some("codex".to_string()),
                            model: Some("gpt-5.4".to_string()),
                            reasoning: Some("high".to_string()),
                        },
                    )]),
                },
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };
        let planning_meta = PlanningMeta {
            agent: PlanningAgentSettings {
                provider: Some("claude".to_string()),
                model: Some("haiku".to_string()),
                reasoning: Some("low".to_string()),
            },
            ..PlanningMeta::default()
        };

        let resolved = resolve_agent_config(
            &config,
            &planning_meta,
            Some(AGENT_ROUTE_BACKLOG_PLAN),
            AgentConfigOverrides {
                provider: Some("claude".to_string()),
                model: None,
                reasoning: None,
            },
        )
        .expect("explicit provider override should skip incompatible route values");

        assert_eq!(resolved.provider, "claude");
        assert_eq!(resolved.model.as_deref(), Some("haiku"));
        assert_eq!(resolved.reasoning.as_deref(), Some("low"));
        assert_eq!(
            resolved.provider_source,
            AgentConfigSource::ExplicitOverride
        );
        assert_eq!(resolved.model_source, Some(AgentConfigSource::RepoDefault));
        assert_eq!(
            resolved.reasoning_source,
            Some(AgentConfigSource::RepoDefault)
        );
    }

    #[test]
    fn resolve_agent_route_skips_incompatible_route_values_when_provider_is_overridden() {
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: Some("codex".to_string()),
                default_model: Some("gpt-5.4".to_string()),
                default_reasoning: Some("low".to_string()),
                routing: AgentRoutingSettings {
                    families: BTreeMap::from([(
                        "backlog".to_string(),
                        AgentRouteConfig {
                            provider: Some("codex".to_string()),
                            model: Some("gpt-5.4".to_string()),
                            reasoning: Some("high".to_string()),
                        },
                    )]),
                    commands: BTreeMap::new(),
                },
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };
        let planning_meta = PlanningMeta {
            agent: PlanningAgentSettings {
                provider: Some("claude".to_string()),
                model: Some("haiku".to_string()),
                reasoning: Some("low".to_string()),
            },
            ..PlanningMeta::default()
        };

        let resolved = resolve_agent_route(
            &config,
            &planning_meta,
            AGENT_ROUTE_BACKLOG_PLAN,
            AgentConfigOverrides {
                provider: Some("claude".to_string()),
                model: None,
                reasoning: None,
            },
        )
        .expect("explicit provider override should skip incompatible family route values");

        assert_eq!(resolved.provider, "claude");
        assert_eq!(resolved.model.as_deref(), Some("haiku"));
        assert_eq!(resolved.reasoning.as_deref(), Some("low"));
        assert_eq!(
            resolved.provider_source,
            AgentConfigSource::ExplicitOverride
        );
        assert_eq!(resolved.model_source, Some(AgentConfigSource::RepoDefault));
        assert_eq!(
            resolved.reasoning_source,
            Some(AgentConfigSource::RepoDefault)
        );
    }

    #[test]
    fn validate_agent_reasoning_rejects_invalid_builtin_reasoning() {
        let error = validate_agent_reasoning("claude", Some("haiku"), Some("xhigh"))
            .expect_err("claude should reject unsupported reasoning");
        assert!(
            error
                .to_string()
                .contains("supported reasoning: low, medium, high, max")
        );
    }

    #[test]
    fn app_config_validation_rejects_invalid_global_reasoning_combinations() {
        let config = AppConfig {
            agents: AgentSettings {
                default_agent: Some("claude".to_string()),
                default_model: Some("haiku".to_string()),
                default_reasoning: Some("xhigh".to_string()),
                routing: AgentRoutingSettings::default(),
                commands: BTreeMap::new(),
            },
            ..AppConfig::default()
        };

        let error = config
            .validate()
            .expect_err("global reasoning should be validated");
        assert!(
            error
                .to_string()
                .contains("supported reasoning: low, medium, high, max")
        );
    }
}
