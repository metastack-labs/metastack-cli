use anyhow::Result;

use crate::linear::{AttachmentCreateRequest, AttachmentSummary, LinearClient};

use super::LinearService;

impl<C> LinearService<C>
where
    C: LinearClient,
{
    pub async fn upload_file(
        &self,
        filename: &str,
        content_type: &str,
        contents: Vec<u8>,
    ) -> Result<String> {
        self.client
            .upload_file(filename, content_type, contents)
            .await
    }

    pub async fn create_attachment(
        &self,
        request: AttachmentCreateRequest,
    ) -> Result<AttachmentSummary> {
        self.client.create_attachment(request).await
    }

    pub async fn delete_attachment(&self, attachment_id: &str) -> Result<()> {
        self.client.delete_attachment(attachment_id).await
    }

    pub async fn download_file(&self, url: &str) -> Result<Vec<u8>> {
        self.client.download_file(url).await
    }
}
