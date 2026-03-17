use anyhow::{Result, anyhow, bail};
use serde_json::json;

use crate::linear::IssueComment;

use super::{ReqwestLinearClient, model::CommentMutationPayload};

impl ReqwestLinearClient {
    pub(super) async fn create_comment_resource(
        &self,
        issue_id: &str,
        body: String,
    ) -> Result<IssueComment> {
        let query = r#"
mutation CreateComment($input: CommentCreateInput!) {
  commentCreate(input: $input) {
    success
    comment {
      id
      body
      resolvedAt
    }
  }
}
"#;
        let data: CommentMutationPayload = self
            .graphql()
            .query(
                query,
                json!({
                    "input": {
                        "issueId": issue_id,
                        "body": body,
                    }
                }),
            )
            .await?;

        self.parse_comment_payload(data, "creation")
    }

    pub(super) async fn update_comment_resource(
        &self,
        comment_id: &str,
        body: String,
    ) -> Result<IssueComment> {
        let query = r#"
mutation UpdateComment($id: String!, $input: CommentUpdateInput!) {
  commentUpdate(id: $id, input: $input) {
    success
    comment {
      id
      body
      resolvedAt
    }
  }
}
"#;
        let data: CommentMutationPayload = self
            .graphql()
            .query(
                query,
                json!({
                    "id": comment_id,
                    "input": {
                        "body": body,
                    }
                }),
            )
            .await?;

        self.parse_comment_payload(data, "update")
    }

    fn parse_comment_payload(
        &self,
        payload: CommentMutationPayload,
        action: &str,
    ) -> Result<IssueComment> {
        let payload = payload.comment_mutation;
        if !payload.success {
            bail!("Linear did not confirm comment {action}");
        }

        payload
            .comment
            .map(IssueComment::from)
            .ok_or_else(|| anyhow!("Linear comment {action} returned no comment body"))
    }
}
