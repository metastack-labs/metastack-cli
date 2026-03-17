use anyhow::Result;

use crate::linear::{
    DashboardData, DashboardFilters, IssueListFilters, LinearClient, ProjectListFilters,
    ProjectSummary, UserRef,
};

use super::{LinearService, resolution::project_has_team};

impl<C> LinearService<C>
where
    C: LinearClient,
{
    pub async fn viewer(&self) -> Result<UserRef> {
        self.client.viewer().await
    }

    pub async fn list_projects(&self, filters: ProjectListFilters) -> Result<Vec<ProjectSummary>> {
        let mut projects = self.client.list_projects(filters.limit.max(1)).await?;
        if let Some(team) = filters.team.or_else(|| self.default_team.clone()) {
            projects.retain(|project| project_has_team(project, &team));
        }

        projects.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(projects)
    }

    pub async fn load_dashboard(&self, filters: DashboardFilters) -> Result<DashboardData> {
        let team = filters.team.or_else(|| self.default_team.clone());
        let issues = self
            .list_issues(IssueListFilters {
                team: team.clone(),
                project: filters.project.clone(),
                project_id: filters.project_id.clone(),
                state: None,
                limit: filters.limit.max(1),
            })
            .await?;

        let project_label = filters.project.clone().or(filters.project_id.clone());
        let title = match (&team, &project_label) {
            (Some(team), Some(project)) => format!("Linear Issues ({team} / {project})"),
            (Some(team), None) => format!("Linear Issues ({team})"),
            (None, Some(project)) => format!("Linear Issues ({project})"),
            (None, None) => "Linear Issues".to_string(),
        };

        Ok(DashboardData { title, issues })
    }
}
