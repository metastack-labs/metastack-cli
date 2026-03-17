use anyhow::{Result, anyhow, bail};
use serde_json::json;

use crate::linear::{AttachmentCreateRequest, AttachmentSummary};

use super::{
    ReqwestLinearClient,
    model::{AttachmentMutationPayload, SuccessOnlyPayload},
};

impl ReqwestLinearClient {
    pub(super) async fn create_attachment_resource(
        &self,
        request: AttachmentCreateRequest,
    ) -> Result<AttachmentSummary> {
        let query = r#"
mutation CreateAttachment($input: AttachmentCreateInput!) {
  attachmentCreate(input: $input) {
    success
    attachment {
      id
      title
      url
      sourceType
      metadata
    }
  }
}
"#;
        let data: AttachmentMutationPayload = self
            .graphql()
            .query(
                query,
                json!({
                    "input": {
                        "issueId": request.issue_id,
                        "title": request.title,
                        "url": request.url,
                        "metadata": request.metadata,
                    }
                }),
            )
            .await?;
        let payload = data.attachment_mutation;
        if !payload.success {
            bail!("Linear did not confirm attachment creation");
        }

        payload
            .attachment
            .map(AttachmentSummary::from)
            .ok_or_else(|| anyhow!("Linear attachment creation returned no attachment body"))
    }

    pub(super) async fn delete_attachment_resource(&self, attachment_id: &str) -> Result<()> {
        let query = r#"
mutation DeleteAttachment($id: String!) {
  attachmentDelete(id: $id) {
    success
  }
}
"#;
        let data: SuccessOnlyPayload = self
            .graphql()
            .query(query, json!({ "id": attachment_id }))
            .await?;
        if !data.success_payload.success {
            bail!("Linear did not confirm attachment deletion");
        }

        Ok(())
    }
}
