use anyhow::{Context, Result, anyhow, bail};
use reqwest::{
    Client, Url,
    header::{AUTHORIZATION, CONTENT_TYPE},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::config::LinearConfig;

use super::model::{GraphqlEnvelope, UploadFileNode};

pub(super) struct GraphqlTransport<'a> {
    config: &'a LinearConfig,
    http: &'a Client,
}

impl<'a> GraphqlTransport<'a> {
    pub(super) fn new(config: &'a LinearConfig, http: &'a Client) -> Self {
        Self { config, http }
    }

    pub(super) async fn query<T>(&self, query: &str, variables: Value) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let response = self
            .http
            .post(&self.config.api_url)
            .header(AUTHORIZATION, &self.config.api_key)
            .json(&json!({
                "query": query,
                "variables": variables,
            }))
            .send()
            .await
            .context("failed to reach the Linear GraphQL endpoint")?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read the Linear response body")?;

        if !status.is_success() {
            bail!("Linear request failed with status {status}: {body}");
        }

        let payload: GraphqlEnvelope<T> =
            serde_json::from_str(&body).context("failed to decode the Linear response payload")?;

        if let Some(errors) = payload.errors {
            let message = errors
                .into_iter()
                .map(|error| error.message)
                .collect::<Vec<_>>()
                .join("; ");
            bail!("Linear request failed: {message}");
        }

        payload
            .data
            .ok_or_else(|| anyhow!("Linear returned no data"))
    }

    pub(super) async fn upload(
        &self,
        upload_file: UploadFileNode,
        content_type: &str,
        contents: Vec<u8>,
    ) -> Result<String> {
        let mut request = self
            .http
            .put(&upload_file.upload_url)
            .header(CONTENT_TYPE, content_type);
        for header in upload_file.headers {
            request = request.header(&header.key, &header.value);
        }

        let response = request
            .body(contents)
            .send()
            .await
            .context("failed to upload file contents to Linear storage")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .context("failed to read the Linear upload response body")?;
            bail!("Linear file upload failed with status {status}: {body}");
        }

        Ok(upload_file.asset_url)
    }

    pub(super) async fn download(&self, url: &str) -> Result<Vec<u8>> {
        let request = self.build_download_request(url)?;
        let response = request
            .send()
            .await
            .with_context(|| format!("failed to download `{url}`"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .context("failed to read the download response body")?;
            bail!("download failed with status {status}: {body}");
        }

        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .context("failed to read the downloaded file bytes")
    }

    pub(super) fn build_download_request(&self, url: &str) -> Result<reqwest::RequestBuilder> {
        let parsed =
            Url::parse(url).with_context(|| format!("failed to parse download URL `{url}`"))?;
        let mut request = self.http.get(parsed);
        if should_authorize_linear_upload_download(url)? {
            request = request.header(AUTHORIZATION, &self.config.api_key);
        }
        Ok(request)
    }
}

pub(super) fn should_authorize_linear_upload_download(url: &str) -> Result<bool> {
    let parsed =
        Url::parse(url).with_context(|| format!("failed to parse download URL `{url}`"))?;
    Ok(matches!(parsed.host_str(), Some("uploads.linear.app")))
}

#[cfg(test)]
mod tests {
    use reqwest::Client;

    use crate::config::LinearConfig;

    use super::{GraphqlTransport, should_authorize_linear_upload_download};

    #[test]
    fn upload_download_auth_is_limited_to_linear_uploads_host() {
        assert!(
            should_authorize_linear_upload_download("https://uploads.linear.app/file/test.png")
                .expect("uploads.linear.app URL should parse")
        );
        assert!(
            !should_authorize_linear_upload_download("https://cdn.example.com/file/test.png")
                .expect("cdn URL should parse")
        );
    }

    #[test]
    fn upload_downloads_send_the_raw_api_key_in_authorization() {
        let config = LinearConfig {
            api_key: "linear-token".to_string(),
            api_url: "https://api.linear.app/graphql".to_string(),
            default_team: None,
        };
        let http = Client::new();
        let transport = GraphqlTransport::new(&config, &http);

        let request = transport
            .build_download_request("https://uploads.linear.app/file/test.png")
            .expect("request builder should build")
            .build()
            .expect("request should build");

        assert_eq!(
            request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("linear-token")
        );
    }

    #[test]
    fn non_linear_downloads_do_not_send_special_authorization() {
        let config = LinearConfig {
            api_key: "linear-token".to_string(),
            api_url: "https://api.linear.app/graphql".to_string(),
            default_team: None,
        };
        let http = Client::new();
        let transport = GraphqlTransport::new(&config, &http);

        let request = transport
            .build_download_request("https://cdn.example.com/file/test.png")
            .expect("request builder should build")
            .build()
            .expect("request should build");

        assert!(
            request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .is_none(),
            "non-Linear downloads should not include special auth",
        );
    }
}
