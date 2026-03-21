use anyhow::{Result, anyhow, bail};

use crate::linear::{
    IssueLabelCreateRequest, LabelRef, LinearClient, ProjectSummary, TeamSummary, UserRef,
};

use super::LinearService;

impl<C> LinearService<C>
where
    C: LinearClient,
{
    pub async fn ensure_issue_labels_exist(
        &self,
        team: Option<String>,
        labels: &[String],
    ) -> Result<()> {
        self.reconcile_issue_labels(team, labels).await?;
        Ok(())
    }

    pub async fn load_issue_create_team(&self, team: Option<String>) -> Result<TeamSummary> {
        let teams = self.client.list_teams().await?;
        self.resolve_team(team.as_deref(), &teams).cloned()
    }

    pub async fn resolve_project_selector_strict(
        &self,
        project_selector: &str,
        team: Option<&str>,
    ) -> Result<String> {
        let project_selector = project_selector.trim();
        let matches = self
            .client
            .list_projects(100)
            .await?
            .into_iter()
            .filter(|project| {
                (project.id == project_selector
                    || project.name.eq_ignore_ascii_case(project_selector))
                    && team
                        .map(|team| project_has_team(project, team))
                        .unwrap_or(true)
            })
            .collect::<Vec<_>>();

        match matches.as_slice() {
            [] => Err(anyhow!(render_missing_project_error(
                project_selector,
                team
            ))),
            [project] => Ok(project.id.clone()),
            candidates => Err(anyhow!(render_ambiguous_project_error(
                project_selector,
                team,
                candidates,
            ))),
        }
    }

    pub async fn resolve_assignee_id(&self, assignee: Option<&str>) -> Result<Option<String>> {
        let Some(assignee) = normalize_user_selector(assignee) else {
            return Ok(None);
        };
        if assignee.eq_ignore_ascii_case("viewer") {
            return Ok(Some(self.client.viewer().await?.id));
        }

        let users = self.client.list_users(250).await?;
        let matches = users
            .into_iter()
            .filter(|user| user_matches_selector(user, &assignee))
            .collect::<Vec<_>>();

        match matches.as_slice() {
            [] => Err(anyhow!(render_missing_assignee_error(&assignee))),
            [user] => Ok(Some(user.id.clone())),
            users => Err(anyhow!(render_ambiguous_assignee_error(&assignee, users))),
        }
    }

    pub(super) fn resolve_team<'a>(
        &'a self,
        explicit_team: Option<&str>,
        teams: &'a [TeamSummary],
    ) -> Result<&'a TeamSummary> {
        if let Some(team) = explicit_team.or(self.default_team.as_deref()) {
            return teams
                .iter()
                .find(|candidate| candidate.key.eq_ignore_ascii_case(team))
                .ok_or_else(|| anyhow!("team `{team}` was not found in Linear"));
        }

        if teams.len() == 1 {
            return Ok(&teams[0]);
        }

        bail!("this command needs a team. Pass --team <KEY> or set LINEAR_TEAM.")
    }

    pub(super) async fn resolve_project_id(
        &self,
        project_name: Option<&str>,
        project_id: Option<&str>,
        team: Option<&str>,
    ) -> Result<Option<String>> {
        let project_selector = project_id.or(project_name);
        let Some(project_selector) = project_selector else {
            return Ok(None);
        };

        let projects = self.client.list_projects(100).await?;
        let project = projects.into_iter().find(|project| {
            (project.id == project_selector || project.name.eq_ignore_ascii_case(project_selector))
                && team
                    .map(|team| project_has_team(project, team))
                    .unwrap_or(true)
        });

        if let Some(project) = project {
            return Ok(Some(project.id));
        }

        if let Some(project_id) = project_id {
            return Ok(Some(project_id.to_string()));
        }

        Err(anyhow!(
            "project `{project_selector}` was not found in Linear"
        ))
    }

    pub(super) async fn ensure_and_resolve_label_ids(
        &self,
        team: Option<String>,
        labels: &[String],
    ) -> Result<Vec<String>> {
        let requested = normalize_requested_labels(labels);
        if requested.is_empty() {
            return Ok(Vec::new());
        }

        let available_labels = self.reconcile_issue_labels(team, &requested).await?;
        let mut resolved = Vec::with_capacity(requested.len());
        let mut missing = Vec::new();

        for label in requested {
            if let Some(entry) = available_labels
                .iter()
                .find(|entry| entry.name.eq_ignore_ascii_case(&label))
            {
                resolved.push(entry.id.clone());
            } else {
                missing.push(label);
            }
        }

        if !missing.is_empty() {
            bail!(
                "issue label(s) {} were not found after reconciliation",
                missing
                    .into_iter()
                    .map(|label| format!("`{label}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        Ok(resolved)
    }

    async fn reconcile_issue_labels(
        &self,
        team: Option<String>,
        labels: &[String],
    ) -> Result<Vec<LabelRef>> {
        let requested = normalize_requested_labels(labels);
        if requested.is_empty() {
            return Ok(Vec::new());
        }

        let selected_team = team.or_else(|| self.default_team.clone());
        let Some(team_selector) = selected_team else {
            return Ok(Vec::new());
        };

        let teams = self.client.list_teams().await?;
        let team = self.resolve_team(Some(&team_selector), &teams)?.clone();
        let mut available_labels = self.client.list_issue_labels(Some(&team.key)).await?;

        for label in requested {
            if available_labels
                .iter()
                .any(|existing| existing.name.eq_ignore_ascii_case(&label))
            {
                continue;
            }

            match self
                .client
                .create_issue_label(IssueLabelCreateRequest {
                    team_id: team.id.clone(),
                    name: label.clone(),
                })
                .await
            {
                Ok(created) => {
                    available_labels.push(created);
                }
                Err(error) if is_duplicate_label_error(&error) => {
                    let refreshed_labels = self.client.list_issue_labels(Some(&team.key)).await?;
                    if let Some(existing) = refreshed_labels
                        .into_iter()
                        .find(|existing| existing.name.eq_ignore_ascii_case(&label))
                    {
                        available_labels.push(existing);
                    } else {
                        available_labels.push(LabelRef {
                            id: format!("pending-label-{label}"),
                            name: label.clone(),
                        });
                    }
                }
                Err(error) => return Err(error),
            }
        }

        Ok(available_labels)
    }
}

pub(super) fn resolve_state_id(
    state_name: Option<&str>,
    team: &TeamSummary,
) -> Result<Option<String>> {
    let Some(state_name) = state_name else {
        return Ok(None);
    };

    let state = team
        .states
        .iter()
        .find(|state| state.name.eq_ignore_ascii_case(state_name))
        .ok_or_else(|| anyhow!("state `{state_name}` was not found on team `{}`", team.key))?;

    Ok(Some(state.id.clone()))
}

pub(super) fn project_has_team(project: &ProjectSummary, team_key: &str) -> bool {
    project
        .teams
        .iter()
        .any(|team| team.key.eq_ignore_ascii_case(team_key))
}

pub(super) fn identifier_team(identifier: &str) -> Option<&str> {
    identifier.split_once('-').map(|(team, _)| team)
}

pub(super) fn normalize_requested_labels(labels: &[String]) -> Vec<String> {
    let mut requested = Vec::new();
    for label in labels {
        let normalized = label.trim();
        if normalized.is_empty()
            || requested
                .iter()
                .any(|entry: &String| entry.eq_ignore_ascii_case(normalized))
        {
            continue;
        }
        requested.push(normalized.to_string());
    }

    requested
}

fn normalize_user_selector(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn user_matches_selector(user: &UserRef, selector: &str) -> bool {
    user.id == selector
        || user.name.eq_ignore_ascii_case(selector)
        || user
            .email
            .as_deref()
            .map(|email| email.eq_ignore_ascii_case(selector))
            .unwrap_or(false)
}

fn is_duplicate_label_error(error: &anyhow::Error) -> bool {
    error
        .to_string()
        .to_ascii_lowercase()
        .contains("duplicate label name")
}

pub(super) fn render_missing_assignee_error(assignee: &str) -> String {
    format!(
        "assignee `{assignee}` was not found in Linear; use `viewer`, a user ID, exact name, or exact email"
    )
}

pub(super) fn render_ambiguous_assignee_error(assignee: &str, users: &[UserRef]) -> String {
    let matches = users
        .iter()
        .map(|user| match user.email.as_deref() {
            Some(email) => format!("`{}` ({}, {})", user.name, user.id, email),
            None => format!("`{}` ({})", user.name, user.id),
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("assignee `{assignee}` matched multiple Linear users: {matches}")
}

pub(super) fn render_missing_project_error(project_selector: &str, team: Option<&str>) -> String {
    match team {
        Some(team) => {
            format!("project `{project_selector}` was not found in Linear for team `{team}`")
        }
        None => format!("project `{project_selector}` was not found in Linear"),
    }
}

pub(super) fn render_ambiguous_project_error(
    project_selector: &str,
    team: Option<&str>,
    candidates: &[ProjectSummary],
) -> String {
    let scope = team
        .map(|team| format!(" for team `{team}`"))
        .unwrap_or_default();
    let candidates = candidates
        .iter()
        .map(|project| {
            let teams = project
                .teams
                .iter()
                .map(|team| team.key.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("`{}` ({}, teams: {})", project.name, project.id, teams)
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("project `{project_selector}` matched multiple Linear projects{scope}: {candidates}")
}
