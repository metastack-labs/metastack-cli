use anyhow::Result;
use serde_json::json;

use crate::linear::TeamSummary;

use super::{ReqwestLinearClient, model::TeamsPayload};

const TEAMS_QUERY: &str = r#"
query Teams {
  teams(first: 50) {
    nodes {
      id
      key
      name
      states {
        nodes {
          id
          name
          type
        }
      }
    }
  }
}
"#;

impl ReqwestLinearClient {
    pub(super) async fn list_teams_resource(&self) -> Result<Vec<TeamSummary>> {
        let data: TeamsPayload = self.graphql().query(TEAMS_QUERY, json!({})).await?;

        Ok(data
            .teams
            .nodes
            .into_iter()
            .map(TeamSummary::from)
            .collect())
    }
}
