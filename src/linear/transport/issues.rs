use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::linear::{IssueCreateRequest, IssueListFilters, IssueSummary, IssueUpdateRequest};

use super::{
    ReqwestLinearClient,
    model::{IssueByIdPayload, IssueCreatePayload, IssueUpdatePayload, IssuesPayload},
    pagination::CursorPager,
};

const ISSUES_PAGE_SIZE: usize = 100;

const ISSUES_QUERY: &str = r#"
query Issues($first: Int!, $after: String, $filter: IssueFilter) {
  issues(first: $first, after: $after, filter: $filter) {
    nodes {
      id
      identifier
      title
      description
      url
      priority
      estimate
      updatedAt
      team {
        id
        key
        name
      }
      project {
        id
        name
      }
      assignee {
        id
        name
        email
      }
      labels {
        nodes {
          id
          name
        }
      }
      state {
        id
        name
        type
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

const ISSUE_FIELDS: &str = r#"
id
identifier
title
description
url
priority
estimate
updatedAt
team {
  id
  key
  name
}
project {
  id
  name
}
assignee {
  id
  name
  email
}
labels {
  nodes {
    id
    name
  }
}
comments(first: 50) {
  nodes {
    id
    body
    resolvedAt
  }
}
state {
  id
  name
  type
}
"#;

const ISSUE_DETAIL_FIELDS: &str = r#"
id
identifier
title
description
url
priority
estimate
updatedAt
team {
  id
  key
  name
}
project {
  id
  name
}
assignee {
  id
  name
  email
}
labels {
  nodes {
    id
    name
  }
}
comments(first: 50) {
  nodes {
    id
    body
    resolvedAt
  }
}
state {
  id
  name
  type
}
attachments(first: 100) {
  nodes {
    id
    title
    url
    sourceType
    metadata
  }
}
parent {
  id
  identifier
  title
  url
}
children(first: 100) {
  nodes {
    id
    identifier
    title
    url
  }
}
"#;

impl ReqwestLinearClient {
    pub(super) async fn list_issues_resource(&self, limit: usize) -> Result<Vec<IssueSummary>> {
        self.collect_issues(Some(limit.max(1)), None).await
    }

    pub(super) async fn list_filtered_issues_resource(
        &self,
        filters: &IssueListFilters,
    ) -> Result<Vec<IssueSummary>> {
        self.collect_issues(
            Some(filters.limit.max(1)),
            Some(render_issue_filter(filters)),
        )
        .await
    }

    pub(super) async fn get_issue_resource(&self, issue_id: &str) -> Result<IssueSummary> {
        let query = format!(
            r#"
query Issue($id: String!) {{
  issue(id: $id) {{
    {ISSUE_DETAIL_FIELDS}
  }}
}}
"#
        );
        let data: IssueByIdPayload = self
            .graphql()
            .query(&query, json!({ "id": issue_id }))
            .await?;
        data.issue
            .map(IssueSummary::from)
            .ok_or_else(|| anyhow!("Linear issue `{issue_id}` returned no issue body"))
    }

    pub(super) async fn create_issue_resource(
        &self,
        request: IssueCreateRequest,
    ) -> Result<IssueSummary> {
        let query = format!(
            r#"
mutation CreateIssue($input: IssueCreateInput!) {{
  issueCreate(input: $input) {{
    success
    issue {{
      {ISSUE_FIELDS}
    }}
  }}
}}
"#
        );
        let data: IssueCreatePayload = self
            .graphql()
            .query(
                &query,
                json!({
                    "input": {
                        "teamId": request.team_id,
                        "title": request.title,
                        "description": request.description,
                        "projectId": request.project_id,
                        "parentId": request.parent_id,
                        "stateId": request.state_id,
                        "priority": request.priority,
                        "labelIds": request.label_ids,
                    }
                }),
            )
            .await?;

        let payload = data.issue_create;
        if !payload.success {
            bail!("Linear did not confirm issue creation");
        }

        payload
            .issue
            .map(IssueSummary::from)
            .ok_or_else(|| anyhow!("Linear issue creation returned no issue body"))
    }

    pub(super) async fn update_issue_resource(
        &self,
        issue_id: &str,
        request: IssueUpdateRequest,
    ) -> Result<IssueSummary> {
        let query = format!(
            r#"
mutation UpdateIssue($id: String!, $input: IssueUpdateInput!) {{
  issueUpdate(id: $id, input: $input) {{
    success
    issue {{
      {ISSUE_FIELDS}
    }}
  }}
}}
"#
        );
        let mut input = serde_json::Map::new();
        if let Some(title) = request.title {
            input.insert("title".to_string(), Value::String(title));
        }
        if let Some(description) = request.description {
            input.insert("description".to_string(), Value::String(description));
        }
        if let Some(project_id) = request.project_id {
            input.insert("projectId".to_string(), Value::String(project_id));
        }
        if let Some(state_id) = request.state_id {
            input.insert("stateId".to_string(), Value::String(state_id));
        }
        if let Some(priority) = request.priority {
            input.insert("priority".to_string(), Value::from(priority));
        }
        let data: IssueUpdatePayload = self
            .graphql()
            .query(
                &query,
                json!({
                    "id": issue_id,
                    "input": Value::Object(input),
                }),
            )
            .await?;

        let payload = data.issue_update;
        if !payload.success {
            bail!("Linear did not confirm issue update");
        }

        payload
            .issue
            .map(IssueSummary::from)
            .ok_or_else(|| anyhow!("Linear issue update returned no issue body"))
    }

    async fn collect_issues(
        &self,
        limit: Option<usize>,
        filter: Option<Value>,
    ) -> Result<Vec<IssueSummary>> {
        let mut issues = Vec::new();
        let mut pager = CursorPager::new(limit, ISSUES_PAGE_SIZE);

        while let Some(page_size) = pager.next_page_size() {
            let data: IssuesPayload = self
                .graphql()
                .query(
                    ISSUES_QUERY,
                    json!({
                        "first": page_size,
                        "after": pager.after(),
                        "filter": filter,
                    }),
                )
                .await?;
            pager.advance(&data.issues);
            issues.extend(data.issues.nodes.into_iter().map(IssueSummary::from));
        }

        Ok(issues)
    }
}

fn render_issue_filter(filters: &IssueListFilters) -> Value {
    let mut filter = serde_json::Map::new();

    if let Some(team) = filters.team.as_deref() {
        filter.insert(
            "team".to_string(),
            json!({
                "key": {
                    "eq": team,
                }
            }),
        );
    }
    if let Some(project) = filters.project.as_deref() {
        filter.insert(
            "project".to_string(),
            json!({
                "name": {
                    "eq": project,
                }
            }),
        );
    }
    if let Some(project_id) = filters.project_id.as_deref() {
        filter.insert(
            "project".to_string(),
            json!({
                "id": {
                    "eq": project_id,
                }
            }),
        );
    }
    if let Some(state) = filters.state.as_deref() {
        filter.insert(
            "state".to_string(),
            json!({
                "name": {
                    "eq": state,
                }
            }),
        );
    }

    Value::Object(filter)
}
