use anyhow::{Result, anyhow, bail};

use crate::linear::{
    IssueCreateRequest, IssueCreateSpec, IssueEditContext, IssueEditSpec, IssueListFilters,
    IssueSummary, IssueUpdateRequest, LinearClient,
};

use super::{
    LinearService,
    resolution::{identifier_team, resolve_state_id},
};

#[derive(Debug, Clone)]
struct IssueListSelection {
    team: Option<String>,
    project: Option<String>,
    project_id: Option<String>,
    state: Option<String>,
    limit: usize,
}

impl IssueListSelection {
    fn new(filters: IssueListFilters, default_team: Option<String>) -> Self {
        Self {
            team: filters.team.or(default_team),
            project: filters.project,
            project_id: filters.project_id,
            state: filters.state,
            limit: filters.limit.max(1),
        }
    }

    fn needs_full_scan(&self) -> bool {
        self.team.is_some()
            || self.project.is_some()
            || self.project_id.is_some()
            || self.state.is_some()
    }
}

impl<C> LinearService<C>
where
    C: LinearClient,
{
    pub async fn list_issues(&self, filters: IssueListFilters) -> Result<Vec<IssueSummary>> {
        let selection = IssueListSelection::new(filters, self.default_team.clone());
        let mut issues = if selection.needs_full_scan() {
            self.client
                .list_filtered_issues(&IssueListFilters {
                    team: selection.team.clone(),
                    project: selection.project.clone(),
                    project_id: selection.project_id.clone(),
                    state: selection.state.clone(),
                    limit: selection.limit,
                })
                .await?
        } else {
            self.client.list_issues(selection.limit).await?
        };

        if let Some(team) = selection.team.as_deref() {
            issues.retain(|issue| issue.team.key.eq_ignore_ascii_case(team));
        }
        if let Some(project) = selection.project.as_deref() {
            issues.retain(|issue| {
                issue
                    .project
                    .as_ref()
                    .map(|entry| entry.name.eq_ignore_ascii_case(project))
                    .unwrap_or(false)
            });
        }
        if let Some(project_id) = selection.project_id.as_deref() {
            issues.retain(|issue| {
                issue
                    .project
                    .as_ref()
                    .map(|entry| {
                        entry.id == project_id || entry.name.eq_ignore_ascii_case(project_id)
                    })
                    .unwrap_or(false)
            });
        }
        if let Some(state) = selection.state.as_deref() {
            issues.retain(|issue| {
                issue
                    .state
                    .as_ref()
                    .map(|entry| entry.name.eq_ignore_ascii_case(state))
                    .unwrap_or(false)
            });
        }

        issues.sort_by(|left, right| left.identifier.cmp(&right.identifier));
        if issues.len() > selection.limit {
            issues.truncate(selection.limit);
        }
        Ok(issues)
    }

    pub async fn find_issue_by_identifier(
        &self,
        identifier: &str,
        filters: IssueListFilters,
    ) -> Result<Option<IssueSummary>> {
        let mut filters = filters;
        filters.limit = filters.limit.max(250);
        let issues = self.list_issues(filters).await?;
        Ok(issues
            .into_iter()
            .find(|issue| issue.identifier.eq_ignore_ascii_case(identifier)))
    }

    pub async fn load_issue_edit_context(&self, identifier: &str) -> Result<IssueEditContext> {
        let issue = self
            .find_issue_by_identifier(
                identifier,
                IssueListFilters {
                    team: identifier_team(identifier).map(str::to_string),
                    ..IssueListFilters::default()
                },
            )
            .await?
            .ok_or_else(|| anyhow!("issue `{identifier}` was not found in Linear"))?;
        let teams = self.client.list_teams().await?;
        let team = teams
            .into_iter()
            .find(|team| team.key.eq_ignore_ascii_case(&issue.team.key))
            .ok_or_else(|| anyhow!("team `{}` was not found in Linear", issue.team.key))?;

        Ok(IssueEditContext { issue, team })
    }

    pub async fn load_issue(&self, identifier: &str) -> Result<IssueSummary> {
        let issue = self
            .find_issue_by_identifier(
                identifier,
                IssueListFilters {
                    team: identifier_team(identifier).map(str::to_string),
                    ..IssueListFilters::default()
                },
            )
            .await?
            .ok_or_else(|| anyhow!("issue `{identifier}` was not found in Linear"))?;

        self.client.get_issue(&issue.id).await
    }

    pub async fn create_issue(&self, spec: IssueCreateSpec) -> Result<IssueSummary> {
        let teams = self.client.list_teams().await?;
        let team = self.resolve_team(spec.team.as_deref(), &teams)?;
        let state_id = resolve_state_id(spec.state.as_deref(), team)?;
        let project_id = self
            .resolve_project_id(
                spec.project.as_deref(),
                spec.project_id.as_deref(),
                Some(&team.key),
            )
            .await?;
        let label_ids = self.resolve_label_ids(&spec.labels, &team.key).await?;

        self.client
            .create_issue(IssueCreateRequest {
                team_id: team.id.clone(),
                title: spec.title,
                description: spec.description,
                project_id,
                parent_id: spec.parent_id,
                state_id,
                priority: spec.priority,
                label_ids,
            })
            .await
    }

    pub async fn edit_issue(&self, spec: IssueEditSpec) -> Result<IssueSummary> {
        let IssueEditContext { issue, team }: IssueEditContext =
            self.load_issue_edit_context(&spec.identifier).await?;
        let state_id = resolve_state_id(spec.state.as_deref(), &team)?;
        let project_id = self
            .resolve_project_id(spec.project.as_deref(), None, Some(&issue.team.key))
            .await?;

        if spec.title.is_none()
            && spec.description.is_none()
            && spec.project.is_none()
            && spec.state.is_none()
            && spec.priority.is_none()
        {
            bail!("no issue fields were provided to edit");
        }

        self.client
            .update_issue(
                &issue.id,
                IssueUpdateRequest {
                    title: spec.title,
                    description: spec.description,
                    project_id,
                    state_id,
                    priority: spec.priority,
                },
            )
            .await
    }
}
