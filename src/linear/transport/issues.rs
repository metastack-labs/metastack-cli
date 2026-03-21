use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::linear::{
    IssueAssigneeFilter, IssueCreateRequest, IssueListFilters, IssueSummary, IssueUpdateRequest,
    UserRef,
};

use super::{
    ReqwestLinearClient,
    model::{
        Connection, IssueByIdPayload, IssueCommentsPayload, IssueCreatePayload, IssueUpdatePayload,
        IssuesPayload, UsersPayload,
    },
    pagination::CursorPager,
};

const ISSUES_PAGE_SIZE: usize = 100;
const ISSUE_COMMENTS_PAGE_SIZE: usize = 50;
const USERS_PAGE_SIZE: usize = 100;

const USERS_QUERY: &str = r#"
query Users($first: Int!, $after: String) {
  users(first: $first, after: $after) {
    nodes {
      id
      name
      email
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

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
      attachments(first: 100) {
        nodes {
          id
          title
          url
          sourceType
          metadata
        }
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
    createdAt
    user {
      name
    }
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
    createdAt
    user {
      name
    }
    resolvedAt
  }
  pageInfo {
    hasNextPage
    endCursor
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
  description
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
    pub(super) async fn list_users_resource(&self, limit: usize) -> Result<Vec<UserRef>> {
        let mut users = Vec::new();
        let mut pager = CursorPager::new(Some(limit.max(1)), USERS_PAGE_SIZE);

        while let Some(first) = pager.next_page_size() {
            let data: UsersPayload = self
                .graphql()
                .query(
                    USERS_QUERY,
                    json!({
                        "first": first,
                        "after": pager.after(),
                    }),
                )
                .await?;
            let mut page = data.users;
            users.append(&mut page.nodes);
            pager.advance(&page);
        }

        Ok(users)
    }

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
        let mut issue = data
            .issue
            .ok_or_else(|| anyhow!("Linear issue `{issue_id}` returned no issue body"))?;

        let mut all_comments = issue
            .comments
            .as_mut()
            .map(|comments| std::mem::take(&mut comments.nodes))
            .unwrap_or_default();
        let mut cursor = issue.comments.as_ref().and_then(|comments| {
            comments.page_info.as_ref().and_then(|page_info| {
                page_info
                    .has_next_page
                    .then(|| page_info.end_cursor.clone())
                    .flatten()
            })
        });

        while let Some(after) = cursor {
            let page = self.get_issue_comments_page(issue_id, Some(after)).await?;
            let connection = page
                .issue
                .and_then(|issue| issue.comments)
                .unwrap_or(Connection {
                    nodes: Vec::new(),
                    page_info: None,
                });
            all_comments.extend(connection.nodes);
            cursor = connection.page_info.and_then(|page_info| {
                page_info
                    .has_next_page
                    .then_some(page_info.end_cursor)
                    .flatten()
            });
        }

        if let Some(comments) = issue.comments.as_mut() {
            comments.nodes = all_comments;
        }

        Ok(IssueSummary::from(issue))
    }

    async fn get_issue_comments_page(
        &self,
        issue_id: &str,
        after: Option<String>,
    ) -> Result<IssueCommentsPayload> {
        let query = r#"
query IssueComments($id: String!, $first: Int!, $after: String) {
  issue(id: $id) {
    comments(first: $first, after: $after) {
      nodes {
        id
        body
        createdAt
        user {
          id
          name
          email
        }
        resolvedAt
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

        self.graphql()
            .query(
                query,
                json!({
                    "id": issue_id,
                    "first": ISSUE_COMMENTS_PAGE_SIZE,
                    "after": after,
                }),
            )
            .await
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
                        "assigneeId": request.assignee_id,
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
        if let Some(estimate) = request.estimate {
            input.insert("estimate".to_string(), Value::from(estimate));
        }
        if let Some(label_ids) = request.label_ids {
            input.insert("labelIds".to_string(), Value::from(label_ids));
        }
        if let Some(parent_id) = request.parent_id {
            input.insert("parentId".to_string(), Value::String(parent_id));
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
    match &filters.assignee {
        IssueAssigneeFilter::Any => {}
        IssueAssigneeFilter::Viewer { viewer_id } => {
            filter.insert(
                "assignee".to_string(),
                json!({
                    "id": {
                        "eq": viewer_id,
                    }
                }),
            );
        }
        IssueAssigneeFilter::ViewerOrUnassigned { viewer_id } => {
            filter.insert(
                "or".to_string(),
                json!([
                    {
                        "assignee": {
                            "id": {
                                "eq": viewer_id,
                            }
                        }
                    },
                    {
                        "assignee": {
                            "null": true,
                        }
                    }
                ]),
            );
        }
    }

    Value::Object(filter)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::render_issue_filter;
    use crate::linear::{IssueAssigneeFilter, IssueListFilters};

    #[test]
    fn render_issue_filter_includes_viewer_only_assignee_scope() {
        let filter = render_issue_filter(&IssueListFilters {
            assignee: IssueAssigneeFilter::Viewer {
                viewer_id: "viewer-1".to_string(),
            },
            ..IssueListFilters::default()
        });

        assert_eq!(
            filter,
            json!({
                "assignee": {
                    "id": {
                        "eq": "viewer-1"
                    }
                }
            })
        );
    }

    #[test]
    fn render_issue_filter_includes_viewer_or_unassigned_assignee_scope() {
        let filter = render_issue_filter(&IssueListFilters {
            assignee: IssueAssigneeFilter::ViewerOrUnassigned {
                viewer_id: "viewer-1".to_string(),
            },
            ..IssueListFilters::default()
        });

        assert_eq!(
            filter,
            json!({
                "or": [
                    {
                        "assignee": {
                            "id": {
                                "eq": "viewer-1"
                            }
                        }
                    },
                    {
                        "assignee": {
                            "null": true
                        }
                    }
                ]
            })
        );
    }

    #[test]
    fn render_issue_filter_omits_assignee_clause_for_all_assignees_scope() {
        let filter = render_issue_filter(&IssueListFilters::default());

        assert_eq!(filter, json!({}));
    }
}
