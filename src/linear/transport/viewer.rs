use anyhow::Result;
use serde_json::json;

use crate::linear::UserRef;

use super::{ReqwestLinearClient, model::ViewerPayload};

const VIEWER_QUERY: &str = r#"
query Viewer {
  viewer {
    id
    name
    email
  }
}
"#;

impl ReqwestLinearClient {
    pub(super) async fn viewer_resource(&self) -> Result<UserRef> {
        let data: ViewerPayload = self.graphql().query(VIEWER_QUERY, json!({})).await?;
        Ok(data.viewer)
    }
}
