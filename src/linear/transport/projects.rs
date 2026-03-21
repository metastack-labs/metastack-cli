use anyhow::Result;
use serde_json::json;

use crate::linear::ProjectSummary;

use super::{ReqwestLinearClient, model::ProjectsPayload};

const PROJECTS_QUERY: &str = r#"
query Projects($first: Int!) {
  projects(first: $first) {
    nodes {
      id
      name
      description
      url
      progress
      teams(first: 10) {
        nodes {
          id
          key
          name
        }
      }
    }
  }
}
"#;

impl ReqwestLinearClient {
    pub(super) async fn list_projects_resource(&self, limit: usize) -> Result<Vec<ProjectSummary>> {
        let data: ProjectsPayload = self
            .graphql()
            .query(PROJECTS_QUERY, json!({ "first": limit.max(1) }))
            .await?;

        Ok(data
            .projects
            .nodes
            .into_iter()
            .map(ProjectSummary::from)
            .collect())
    }
}
