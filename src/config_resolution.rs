use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::agent_provider::{
    builtin_provider_adapter, builtin_provider_model_keys, builtin_provider_names,
    builtin_provider_reasoning_keys,
};
use crate::config::{
    AGENT_ROUTE_AGENTS_LISTEN, AGENT_ROUTE_AGENTS_REVIEW, AGENT_ROUTE_AGENTS_WORKFLOWS_RUN,
    AGENT_ROUTE_BACKLOG_IMPROVE, AGENT_ROUTE_BACKLOG_PLAN, AGENT_ROUTE_BACKLOG_SPEC,
    AGENT_ROUTE_BACKLOG_SPLIT, AGENT_ROUTE_CONTEXT_RELOAD, AGENT_ROUTE_CONTEXT_SCAN,
    AGENT_ROUTE_LINEAR_ISSUES_REFINE, AGENT_ROUTE_MERGE,
    AGENT_ROUTE_RUNTIME_CRON_PROMPT, AgentCommandConfig, AgentRouteConfig, AppConfig,
    DEFAULT_LINEAR_API_URL, LinearProfileSettings, LinearSettings, METASTACK_CONFIG_ENV,
    PlanningMeta,
};
use crate::fs::PlanningPaths;
use crate::linear::{LinearService, ReqwestLinearClient};

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
    pub(crate) fn global() -> Self {
        Self { route_key: None }
    }

    pub(crate) fn for_route(route_key: impl Into<String>) -> Self {
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

/// Loads repo-scoped planning metadata and returns a setup error when the repository is not bootstrapped.
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

/// Ensures repo-scoped issue labels exist when Linear auth and a default team are available.
pub async fn ensure_saved_issue_labels(
    root: &Path,
    app_config: &AppConfig,
    planning_meta: &PlanningMeta,
) -> Result<()> {
    let mut labels = std::collections::BTreeSet::from([
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

impl LinearConfig {
    /// Resolves the effective Linear client config using install, repo, environment, and CLI sources.
    ///
    /// Returns an error when the selected profile is invalid or no API key can be resolved.
    pub fn new_with_root(root: Option<&Path>, overrides: LinearConfigOverrides) -> Result<Self> {
        let app_config = AppConfig::load()?;
        let planning_meta = match root {
            Some(root) => PlanningMeta::load(root)?,
            None => PlanningMeta::default(),
        };

        Self::from_sources(&app_config, &planning_meta, root, overrides)
    }

    /// Resolves the effective Linear client config from preloaded install and repo config sources.
    ///
    /// Returns an error when the selected profile is invalid or no API key can be resolved.
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

    /// Returns the standard missing-auth error used by Linear-backed commands.
    pub fn missing_auth_error() -> anyhow::Error {
        anyhow!(
            "Linear auth is required for this command. Set LINEAR_API_KEY, run `meta runtime config`, or pass `--api-key <token>`."
        )
    }
}

/// Resolves the effective agent provider, model, and reasoning for a command run.
///
/// Returns an error when no provider can be resolved or a selected value is unsupported.
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

/// Returns the supported command-route definitions for agent-backed workflows.
pub fn supported_agent_route_definitions() -> &'static [AgentRouteDefinition] {
    const ROUTES: &[AgentRouteDefinition] = &[
        AgentRouteDefinition {
            key: AGENT_ROUTE_BACKLOG_SPEC,
            family: "backlog",
            label: "meta backlog spec",
        },
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
            key: AGENT_ROUTE_AGENTS_REVIEW,
            family: "agents",
            label: "meta agents review",
        },
        AgentRouteDefinition {
            key: AGENT_ROUTE_MERGE,
            family: "merge",
            label: "meta merge",
        },
    ];
    ROUTES
}

/// Returns the supported route-family keys referenced by command routes.
pub fn supported_agent_route_families() -> Vec<&'static str> {
    let mut families = supported_agent_route_definitions()
        .iter()
        .map(|definition| definition.family)
        .collect::<Vec<_>>();
    families.sort_unstable();
    families.dedup();
    families
}

/// Returns the command-route definition for a normalized or non-normalized route key.
pub fn supported_agent_route_definition(key: &str) -> Option<&'static AgentRouteDefinition> {
    let normalized = normalize_agent_name(key);
    supported_agent_route_definitions()
        .iter()
        .find(|definition| definition.key == normalized)
}

/// Normalizes and validates an agent route key for the requested scope.
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

/// Resolves the effective route-specific agent provider, model, and reasoning.
///
/// Returns an error when the route key is unknown, no provider can be resolved, or a selected
/// value is unsupported.
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

/// Resolves the install-scoped config file path from `METASTACK_CONFIG`, XDG, or the home directory.
pub fn resolve_config_path() -> Result<PathBuf> {
    config_path_from_env_or_home().ok_or_else(|| {
        anyhow!(
            "could not determine a config path; set {} or HOME/XDG_CONFIG_HOME",
            METASTACK_CONFIG_ENV
        )
    })
}

/// Resolves the install-scoped data directory that is colocated with the config file.
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

/// Normalizes provider and route keys using the CLI's canonical lowercase form.
pub fn normalize_agent_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

/// Returns the built-in provider names recognized by the CLI.
pub fn supported_agent_names() -> &'static [&'static str] {
    builtin_provider_names()
}

/// Returns the supported built-in model keys for a provider.
pub fn supported_agent_models(name: &str) -> Vec<&'static str> {
    builtin_provider_model_keys(name)
}

/// Returns the supported built-in reasoning options for a provider and optional model.
pub fn supported_reasoning_options(agent: &str, model: Option<&str>) -> Vec<&'static str> {
    builtin_provider_reasoning_keys(agent, model)
}

/// Detects built-in providers whose command adapters are present on `PATH`.
pub fn detect_supported_agents() -> Vec<String> {
    supported_agent_names()
        .iter()
        .copied()
        .filter(|name| command_exists(name))
        .map(str::to_string)
        .collect()
}

/// Returns the built-in command definition for a provider when one exists.
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

pub(crate) fn config_path_from_env_or_home() -> Option<PathBuf> {
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

pub(crate) fn default_linear_api_url() -> String {
    DEFAULT_LINEAR_API_URL.to_string()
}

/// Validates the configured fast-plan follow-up question limit.
///
/// Returns an error when the value is outside the supported range of 0 through 10.
pub fn validate_fast_plan_question_limit(limit: usize) -> Result<()> {
    if !(crate::config::MIN_FAST_PLAN_QUESTION_LIMIT..=crate::config::MAX_FAST_PLAN_QUESTION_LIMIT)
        .contains(&limit)
    {
        return Err(anyhow!(
            "fast plan follow-up question limit must be between {} and {}",
            crate::config::MIN_FAST_PLAN_QUESTION_LIMIT,
            crate::config::MAX_FAST_PLAN_QUESTION_LIMIT
        ));
    }
    Ok(())
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

/// Validates that the given provider name is supported by the CLI.
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

/// Validates that a provider name resolves to either a configured custom agent or a built-in provider.
pub fn validate_agent_name(app_config: &AppConfig, agent: &str) -> Result<()> {
    let normalized = normalize_agent_name(agent);
    if app_config.resolve_agent_definition(&normalized).is_some() {
        Ok(())
    } else {
        validate_supported_agent(&normalized)
    }
}

/// Validates that a model is supported for the selected provider.
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

/// Validates that a reasoning value is supported for the selected provider and model.
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

/// Validates the interactive follow-up question limit used by `meta backlog plan`.
pub fn validate_interactive_plan_follow_up_question_limit(limit: usize) -> Result<()> {
    if (crate::config::MIN_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT
        ..=crate::config::MAX_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT)
        .contains(&limit)
    {
        Ok(())
    } else {
        Err(anyhow!(
            "interactive plan follow-up question limit must be between {} and {}; got {limit}",
            crate::config::MIN_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT,
            crate::config::MAX_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT
        ))
    }
}

/// Validates the listen polling interval in seconds.
pub fn validate_listen_poll_interval_seconds(interval: u64) -> Result<()> {
    if interval >= 1 {
        Ok(())
    } else {
        Err(anyhow!(
            "listen poll interval must be at least 1 second; got {interval}"
        ))
    }
}

pub(crate) fn validate_merge_validation_repair_attempts(limit: usize) -> Result<()> {
    if limit >= 1 {
        Ok(())
    } else {
        Err(anyhow!(
            "merge validation repair attempt limit must be at least 1; got {limit}"
        ))
    }
}

/// Validates the persisted default backlog priority.
pub fn validate_backlog_default_priority(priority: u8) -> Result<()> {
    if (1..=4).contains(&priority) {
        Ok(())
    } else {
        Err(anyhow!(
            "backlog default priority must be between 1 and 4; got {priority}"
        ))
    }
}

pub(crate) fn validate_merge_validation_transient_retry_attempts(limit: usize) -> Result<()> {
    if limit <= 10 {
        Ok(())
    } else {
        Err(anyhow!(
            "merge transient validation retry attempt limit must be between 0 and 10; got {limit}"
        ))
    }
}

pub(crate) fn validate_merge_publication_retry_attempts(limit: usize) -> Result<()> {
    if limit >= 1 {
        Ok(())
    } else {
        Err(anyhow!(
            "merge publication retry attempt limit must be at least 1; got {limit}"
        ))
    }
}

/// Validates the persisted backlog label list.
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

pub(crate) fn normalize_optional_ref(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(crate) fn validate_agent_route_config(
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
