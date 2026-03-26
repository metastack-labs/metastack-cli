use anyhow::Result;

use crate::linear::{IssueComment, IssueSummary, LinearClient};

use super::LinearService;

impl<C> LinearService<C>
where
    C: LinearClient,
{
    /// Create or update a single active issue comment identified by a stable body marker.
    ///
    /// Returns an error when the underlying Linear comment mutation fails.
    pub async fn upsert_comment_with_marker(
        &self,
        issue: &IssueSummary,
        marker: &str,
        body: String,
    ) -> Result<IssueComment> {
        if let Some(comment) = issue
            .comments
            .iter()
            .find(|comment| is_active_marker_comment(comment, marker))
        {
            return self.client.update_comment(&comment.id, body).await;
        }

        self.client.create_comment(&issue.id, body).await
    }

    /// Create or update the active `## Codex Workpad` comment for an issue.
    ///
    /// Returns an error when the underlying Linear comment mutation fails.
    pub async fn upsert_workpad_comment(
        &self,
        issue: &IssueSummary,
        body: String,
    ) -> Result<IssueComment> {
        self.upsert_comment_with_marker(issue, "## Codex Workpad", body)
            .await
    }

    /// Update the tracked `## Codex Workpad` comment by id.
    ///
    /// Returns an error when the underlying Linear comment mutation fails.
    pub async fn update_workpad_comment_by_id(
        &self,
        comment_id: &str,
        body: String,
    ) -> Result<IssueComment> {
        self.client.update_comment(comment_id, body).await
    }
}

fn is_active_marker_comment(comment: &IssueComment, marker: &str) -> bool {
    comment.resolved_at.is_none() && comment.body.contains(marker)
}
