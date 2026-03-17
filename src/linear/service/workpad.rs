use anyhow::Result;

use crate::linear::{IssueComment, IssueSummary, LinearClient};

use super::LinearService;

impl<C> LinearService<C>
where
    C: LinearClient,
{
    pub async fn upsert_workpad_comment(
        &self,
        issue: &IssueSummary,
        body: String,
    ) -> Result<IssueComment> {
        if let Some(comment) = issue
            .comments
            .iter()
            .find(|comment| is_active_workpad_comment(comment))
        {
            return self.client.update_comment(&comment.id, body).await;
        }

        self.client.create_comment(&issue.id, body).await
    }
}

fn is_active_workpad_comment(comment: &IssueComment) -> bool {
    comment.resolved_at.is_none() && comment.body.contains("## Codex Workpad")
}
