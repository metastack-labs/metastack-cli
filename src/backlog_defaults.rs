use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, PlanningMeta, VelocityAutoAssign, resolve_data_root};
use crate::linear::IssueSummary;
use crate::listen::store::resolve_source_project_root;

pub(crate) const DEFAULT_BACKLOG_STATE: &str = "Backlog";
const BACKLOG_SELECTIONS_VERSION: u8 = 1;

#[derive(Debug, Clone, Default)]
pub(crate) struct TicketOptionOverrides {
    pub(crate) state: Option<String>,
    pub(crate) priority: Option<u8>,
    pub(crate) labels: Vec<String>,
    pub(crate) assignee: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PlanTicketResolutionInput {
    pub(crate) zero_prompt: bool,
    pub(crate) explicit_team: Option<String>,
    pub(crate) explicit_project: Option<String>,
    pub(crate) overrides: TicketOptionOverrides,
    pub(crate) built_in_label: String,
    pub(crate) generated_priority: Option<u8>,
}

#[derive(Debug, Clone)]
pub(crate) struct TechnicalTicketResolutionInput {
    pub(crate) zero_prompt: bool,
    pub(crate) overrides: TicketOptionOverrides,
    pub(crate) built_in_label: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ResolvedBacklogTicketDefaults {
    pub(crate) team: Option<String>,
    pub(crate) project: Option<String>,
    pub(crate) project_id: Option<String>,
    pub(crate) state: Option<String>,
    pub(crate) priority: Option<u8>,
    pub(crate) labels: Vec<String>,
    pub(crate) assignee: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RememberedBacklogSelection {
    #[serde(default)]
    pub(crate) team: Option<String>,
    #[serde(default)]
    pub(crate) project_id: Option<String>,
    #[serde(default)]
    pub(crate) project_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RememberedBacklogSelectionsFile {
    version: u8,
    #[serde(default)]
    repositories: BTreeMap<String, RememberedBacklogSelection>,
}

/// Resolves the ticket defaults used by `meta backlog plan`.
pub(crate) fn resolve_plan_ticket_defaults(
    app_config: &AppConfig,
    planning_meta: &PlanningMeta,
    remembered: &RememberedBacklogSelection,
    input: &PlanTicketResolutionInput,
) -> ResolvedBacklogTicketDefaults {
    let explicit_project = normalized(input.explicit_project.clone());
    let remembered_project_id = input
        .zero_prompt
        .then(|| remembered.project_id.clone())
        .flatten();
    let velocity_project = input
        .zero_prompt
        .then(|| {
            normalized(planning_meta.backlog.velocity_defaults.project.clone())
                .or_else(|| normalized(app_config.backlog.velocity_defaults.project.clone()))
        })
        .flatten();
    let mut labels = vec![input.built_in_label.clone()];
    labels.extend(app_config.backlog.default_labels.clone());
    labels.extend(planning_meta.backlog.default_labels.clone());
    labels.extend(input.overrides.labels.clone());

    ResolvedBacklogTicketDefaults {
        team: normalized(input.explicit_team.clone())
            .or_else(|| input.zero_prompt.then(|| remembered.team.clone()).flatten())
            .or_else(|| normalized(planning_meta.linear.team.clone()))
            .or_else(|| normalized(app_config.linear.team.clone())),
        project: explicit_project.clone().or_else(|| {
            if remembered_project_id.is_some() {
                None
            } else {
                velocity_project.clone()
            }
        }),
        project_id: if explicit_project.is_some() {
            None
        } else {
            remembered_project_id.or_else(|| {
                if velocity_project.is_some() {
                    None
                } else {
                    planning_meta.effective_project_id(app_config)
                }
            })
        },
        state: normalized(input.overrides.state.clone())
            .or_else(|| {
                input
                    .zero_prompt
                    .then(|| {
                        normalized(planning_meta.backlog.velocity_defaults.state.clone()).or_else(
                            || normalized(app_config.backlog.velocity_defaults.state.clone()),
                        )
                    })
                    .flatten()
            })
            .or_else(|| normalized(planning_meta.backlog.default_state.clone()))
            .or_else(|| normalized(app_config.backlog.default_state.clone()))
            .or_else(|| Some(DEFAULT_BACKLOG_STATE.to_string())),
        priority: input
            .overrides
            .priority
            .or(input.generated_priority)
            .or(planning_meta.backlog.default_priority)
            .or(app_config.backlog.default_priority),
        labels: dedupe_labels(labels),
        assignee: normalized(input.overrides.assignee.clone())
            .or_else(|| {
                input
                    .zero_prompt
                    .then(|| velocity_auto_assignee(planning_meta, app_config))
                    .flatten()
            })
            .or_else(|| normalized(planning_meta.backlog.default_assignee.clone()))
            .or_else(|| normalized(app_config.backlog.default_assignee.clone())),
    }
}

/// Resolves the ticket defaults used by `meta backlog tech`.
pub(crate) fn resolve_technical_ticket_defaults(
    app_config: &AppConfig,
    planning_meta: &PlanningMeta,
    remembered: &RememberedBacklogSelection,
    input: &TechnicalTicketResolutionInput,
    parent: &IssueSummary,
) -> ResolvedBacklogTicketDefaults {
    let parent_project_id = parent.project.as_ref().map(|project| project.id.clone());
    let remembered_project_id = input
        .zero_prompt
        .then(|| remembered.project_id.clone())
        .flatten();
    let velocity_project = input
        .zero_prompt
        .then(|| {
            normalized(planning_meta.backlog.velocity_defaults.project.clone())
                .or_else(|| normalized(app_config.backlog.velocity_defaults.project.clone()))
        })
        .flatten();
    let mut labels = vec![input.built_in_label.clone()];
    labels.extend(app_config.backlog.default_labels.clone());
    labels.extend(planning_meta.backlog.default_labels.clone());
    labels.extend(input.overrides.labels.clone());

    ResolvedBacklogTicketDefaults {
        team: Some(parent.team.key.clone()),
        project: if parent_project_id.is_some() || remembered_project_id.is_some() {
            None
        } else {
            velocity_project.clone()
        },
        project_id: parent_project_id.or(remembered_project_id).or_else(|| {
            if velocity_project.is_some() {
                None
            } else {
                planning_meta.effective_project_id(app_config)
            }
        }),
        state: normalized(input.overrides.state.clone())
            .or_else(|| {
                input
                    .zero_prompt
                    .then(|| {
                        normalized(planning_meta.backlog.velocity_defaults.state.clone()).or_else(
                            || normalized(app_config.backlog.velocity_defaults.state.clone()),
                        )
                    })
                    .flatten()
            })
            .or_else(|| normalized(planning_meta.backlog.default_state.clone()))
            .or_else(|| normalized(app_config.backlog.default_state.clone()))
            .or_else(|| Some(DEFAULT_BACKLOG_STATE.to_string())),
        priority: input
            .overrides
            .priority
            .or(parent.priority)
            .or(planning_meta.backlog.default_priority)
            .or(app_config.backlog.default_priority),
        labels: dedupe_labels(labels),
        assignee: normalized(input.overrides.assignee.clone())
            .or_else(|| {
                input
                    .zero_prompt
                    .then(|| velocity_auto_assignee(planning_meta, app_config))
                    .flatten()
            })
            .or_else(|| normalized(planning_meta.backlog.default_assignee.clone()))
            .or_else(|| normalized(app_config.backlog.default_assignee.clone())),
    }
}

/// Loads the remembered project and team selection for the canonical repository root.
pub(crate) fn load_remembered_backlog_selection(root: &Path) -> Result<RememberedBacklogSelection> {
    let selections_path = remembered_backlog_selections_path()?;
    let repository_key = canonical_repository_key(root)?;
    let selections = match fs::read_to_string(&selections_path) {
        Ok(contents) => serde_json::from_str::<RememberedBacklogSelectionsFile>(&contents)
            .with_context(|| format!("failed to decode `{}`", selections_path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RememberedBacklogSelection::default());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read `{}`", selections_path.display()));
        }
    };

    Ok(selections
        .repositories
        .get(&repository_key)
        .cloned()
        .unwrap_or_default())
}

/// Persists the final project and team selection for the canonical repository root.
pub(crate) fn save_remembered_backlog_selection(root: &Path, issue: &IssueSummary) -> Result<()> {
    let selections_path = remembered_backlog_selections_path()?;
    let repository_key = canonical_repository_key(root)?;
    let mut selections = match fs::read_to_string(&selections_path) {
        Ok(contents) => serde_json::from_str::<RememberedBacklogSelectionsFile>(&contents)
            .with_context(|| format!("failed to decode `{}`", selections_path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            RememberedBacklogSelectionsFile {
                version: BACKLOG_SELECTIONS_VERSION,
                ..RememberedBacklogSelectionsFile::default()
            }
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read `{}`", selections_path.display()));
        }
    };
    selections.version = BACKLOG_SELECTIONS_VERSION;
    selections.repositories.insert(
        repository_key,
        RememberedBacklogSelection {
            team: Some(issue.team.key.clone()),
            project_id: issue.project.as_ref().map(|project| project.id.clone()),
            project_name: issue.project.as_ref().map(|project| project.name.clone()),
        },
    );

    if let Some(parent) = selections_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    let contents = serde_json::to_string_pretty(&selections)
        .context("failed to encode remembered backlog selections")?;
    fs::write(&selections_path, contents)
        .with_context(|| format!("failed to write `{}`", selections_path.display()))
}

fn canonical_repository_key(root: &Path) -> Result<String> {
    Ok(resolve_source_project_root(root)?.display().to_string())
}

fn remembered_backlog_selections_path() -> Result<PathBuf> {
    Ok(resolve_data_root()?.join("backlog").join("selections.json"))
}

fn dedupe_labels(labels: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::new();
    for label in labels {
        let Some(label) = normalized(Some(label)) else {
            continue;
        };
        if deduped
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&label))
        {
            continue;
        }
        deduped.push(label);
    }
    deduped
}

fn velocity_auto_assignee(
    app_configured: &PlanningMeta,
    global_config: &AppConfig,
) -> Option<String> {
    if app_configured.backlog.velocity_defaults.auto_assign == Some(VelocityAutoAssign::Viewer)
        || global_config.backlog.velocity_defaults.auto_assign == Some(VelocityAutoAssign::Viewer)
    {
        Some("viewer".to_string())
    } else {
        None
    }
}

fn normalized(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("none"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use tempfile::tempdir;

    use crate::config::{
        AppConfig, BacklogSettings, PlanningLinearSettings, PlanningMeta, VelocityDefaults,
    };
    use crate::linear::{IssueSummary, ProjectRef, TeamRef};

    use super::{
        DEFAULT_BACKLOG_STATE, PlanTicketResolutionInput, RememberedBacklogSelection,
        TechnicalTicketResolutionInput, TicketOptionOverrides, dedupe_labels,
        resolve_plan_ticket_defaults, resolve_technical_ticket_defaults,
    };

    #[test]
    fn plan_priority_prefers_generated_value_before_config_defaults() {
        let app_config = AppConfig {
            backlog: BacklogSettings {
                default_priority: Some(4),
                default_labels: vec!["global".to_string()],
                ..BacklogSettings::default()
            },
            ..AppConfig::default()
        };
        let planning_meta = PlanningMeta {
            backlog: BacklogSettings {
                default_priority: Some(3),
                default_labels: vec!["repo".to_string()],
                ..BacklogSettings::default()
            },
            ..PlanningMeta::default()
        };

        let resolved = resolve_plan_ticket_defaults(
            &app_config,
            &planning_meta,
            &RememberedBacklogSelection::default(),
            &PlanTicketResolutionInput {
                zero_prompt: false,
                explicit_team: None,
                explicit_project: None,
                overrides: TicketOptionOverrides::default(),
                built_in_label: "plan".to_string(),
                generated_priority: Some(2),
            },
        );

        assert_eq!(resolved.priority, Some(2));
        assert_eq!(resolved.state.as_deref(), Some(DEFAULT_BACKLOG_STATE));
        assert_eq!(resolved.labels, vec!["plan", "global", "repo"]);
    }

    #[test]
    fn technical_priority_prefers_parent_before_config_defaults() {
        let app_config = AppConfig {
            backlog: BacklogSettings {
                default_priority: Some(4),
                ..BacklogSettings::default()
            },
            ..AppConfig::default()
        };
        let planning_meta = PlanningMeta {
            backlog: BacklogSettings {
                default_priority: Some(3),
                ..BacklogSettings::default()
            },
            ..PlanningMeta::default()
        };
        let parent = IssueSummary {
            id: "parent-1".to_string(),
            identifier: "MET-1".to_string(),
            title: "Parent".to_string(),
            description: None,
            url: "https://linear.app/MET-1".to_string(),
            priority: Some(2),
            estimate: None,
            updated_at: "2026-03-19T00:00:00Z".to_string(),
            team: TeamRef {
                id: "team-1".to_string(),
                key: "MET".to_string(),
                name: "Metastack".to_string(),
            },
            project: Some(ProjectRef {
                id: "project-1".to_string(),
                name: "MetaStack CLI".to_string(),
            }),
            assignee: None,
            labels: Vec::new(),
            comments: Vec::new(),
            state: None,
            attachments: Vec::new(),
            parent: None,
            children: Vec::new(),
        };

        let resolved = resolve_technical_ticket_defaults(
            &app_config,
            &planning_meta,
            &RememberedBacklogSelection::default(),
            &TechnicalTicketResolutionInput {
                zero_prompt: false,
                overrides: TicketOptionOverrides::default(),
                built_in_label: "technical".to_string(),
            },
            &parent,
        );

        assert_eq!(resolved.priority, Some(2));
        assert_eq!(resolved.project_id.as_deref(), Some("project-1"));
        assert_eq!(resolved.state.as_deref(), Some(DEFAULT_BACKLOG_STATE));
    }

    #[test]
    fn zero_prompt_plan_prefers_remembered_selection_and_velocity_defaults() {
        let app_config = AppConfig {
            linear: crate::config::LinearSettings {
                team: Some("GLB".to_string()),
                ..crate::config::LinearSettings::default()
            },
            backlog: BacklogSettings {
                default_assignee: Some("global@example.com".to_string()),
                velocity_defaults: VelocityDefaults {
                    project: Some("Global Project".to_string()),
                    state: Some("Todo".to_string()),
                    auto_assign: None,
                },
                ..BacklogSettings::default()
            },
            ..AppConfig::default()
        };
        let planning_meta = PlanningMeta {
            linear: PlanningLinearSettings {
                team: Some("REP".to_string()),
                project_id: Some("project-repo".to_string()),
                ..PlanningLinearSettings::default()
            },
            backlog: BacklogSettings {
                default_assignee: Some("repo@example.com".to_string()),
                velocity_defaults: VelocityDefaults {
                    project: Some("Repo Project".to_string()),
                    state: Some("Started".to_string()),
                    auto_assign: Some(crate::config::VelocityAutoAssign::Viewer),
                },
                ..BacklogSettings::default()
            },
            ..PlanningMeta::default()
        };

        let resolved = resolve_plan_ticket_defaults(
            &app_config,
            &planning_meta,
            &RememberedBacklogSelection {
                team: Some("MEM".to_string()),
                project_id: Some("project-memory".to_string()),
                project_name: Some("Remembered".to_string()),
            },
            &PlanTicketResolutionInput {
                zero_prompt: true,
                explicit_team: None,
                explicit_project: None,
                overrides: TicketOptionOverrides::default(),
                built_in_label: "plan".to_string(),
                generated_priority: None,
            },
        );

        assert_eq!(resolved.team.as_deref(), Some("MEM"));
        assert_eq!(resolved.project_id.as_deref(), Some("project-memory"));
        assert_eq!(resolved.state.as_deref(), Some("Started"));
        assert_eq!(resolved.assignee.as_deref(), Some("viewer"));
    }

    #[test]
    fn label_deduplication_is_case_insensitive() {
        assert_eq!(
            dedupe_labels(vec![
                "plan".to_string(),
                "PLAN".to_string(),
                "ops".to_string(),
                " ops ".to_string(),
            ]),
            vec!["plan", "ops"]
        );
    }

    #[test]
    fn remembered_selection_serializes_per_repository_key() -> Result<()> {
        let temp = tempdir()?;
        let path = temp.path().join("selections.json");
        fs::write(
            &path,
            r#"{
  "version": 1,
  "repositories": {
    "/tmp/repo": {
      "team": "MET",
      "project_id": "project-1",
      "project_name": "MetaStack CLI"
    }
  }
}"#,
        )?;
        let contents = fs::read_to_string(path)?;
        let parsed: super::RememberedBacklogSelectionsFile = serde_json::from_str(&contents)?;
        assert_eq!(
            parsed.repositories["/tmp/repo"].team.as_deref(),
            Some("MET")
        );
        Ok(())
    }
}
