use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;

use super::{ReqwestLinearClient, model::UploadPayload};

impl ReqwestLinearClient {
    pub(super) async fn upload_file_resource(
        &self,
        filename: &str,
        content_type: &str,
        contents: Vec<u8>,
    ) -> Result<String> {
        let query = r#"
mutation UploadFile($contentType: String!, $filename: String!, $size: Int!) {
  fileUpload(contentType: $contentType, filename: $filename, size: $size) {
    success
    uploadFile {
      uploadUrl
      assetUrl
      headers {
        key
        value
      }
    }
  }
}
"#;
        let size = i32::try_from(contents.len()).context("file is too large for Linear upload")?;
        let data: UploadPayload = self
            .graphql()
            .query(
                query,
                json!({
                    "contentType": content_type,
                    "filename": filename,
                    "size": size,
                }),
            )
            .await?;
        let payload = data.upload;
        if !payload.success {
            bail!("Linear did not confirm file upload setup");
        }
        let upload_file = payload
            .upload_file
            .ok_or_else(|| anyhow!("Linear upload response returned no upload target"))?;

        self.graphql()
            .upload(upload_file, content_type, contents)
            .await
    }

    pub(super) async fn download_file_resource(&self, url: &str) -> Result<Vec<u8>> {
        self.graphql().download(url).await
    }
}
