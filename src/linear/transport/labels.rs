use anyhow::{Result, anyhow, bail};
use serde_json::json;

use crate::linear::{IssueLabelCreateRequest, LabelRef};

use super::{
    ReqwestLinearClient,
    model::{IssueLabelCreatePayload, IssueLabelsPayload},
    pagination::CursorPager,
};

const ISSUE_LABELS_PAGE_SIZE: usize = 100;

const ISSUE_LABELS_QUERY: &str = r#"
query IssueLabels($first: Int!, $after: String, $filter: IssueLabelFilter) {
  issueLabels(first: $first, after: $after, filter: $filter) {
    nodes {
      id
      name
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

impl ReqwestLinearClient {
    pub(super) async fn list_issue_labels_resource(
        &self,
        team: Option<&str>,
    ) -> Result<Vec<LabelRef>> {
        let mut labels = Vec::new();
        let mut pager = CursorPager::new(None, ISSUE_LABELS_PAGE_SIZE);
        let filter = team.map(|team| {
            json!({
                "team": {
                    "key": {
                        "eq": team
                    }
                }
            })
        });

        while let Some(page_size) = pager.next_page_size() {
            let data: IssueLabelsPayload = self
                .graphql()
                .query(
                    ISSUE_LABELS_QUERY,
                    json!({
                        "first": page_size,
                        "after": pager.after(),
                        "filter": filter,
                    }),
                )
                .await?;
            pager.advance(&data.issue_labels);
            labels.extend(data.issue_labels.nodes);
        }

        Ok(labels)
    }

    pub(super) async fn create_issue_label_resource(
        &self,
        request: IssueLabelCreateRequest,
    ) -> Result<LabelRef> {
        let query = r#"
mutation CreateIssueLabel($input: IssueLabelCreateInput!) {
  issueLabelCreate(input: $input) {
    success
    issueLabel {
      id
      name
    }
  }
}
"#;
        let data: IssueLabelCreatePayload = self
            .graphql()
            .query(
                query,
                json!({
                    "input": {
                        "teamId": request.team_id,
                        "name": request.name,
                    }
                }),
            )
            .await?;

        let payload = data.issue_label_create;
        if !payload.success {
            bail!("Linear did not confirm issue label creation");
        }

        payload
            .issue_label
            .ok_or_else(|| anyhow!("Linear issue label creation returned no issue label body"))
    }
}
