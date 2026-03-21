use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;

use crate::config::LinearConfig;
use crate::linear::{
    AttachmentCreateRequest, AttachmentSummary, IssueComment, IssueCreateRequest,
    IssueDependencySnapshot, IssueLabelCreateRequest, IssueListFilters, IssueRelationCreateRequest,
    IssueRelationSummary, IssueRelationUpdateRequest, IssueSummary, IssueUpdateRequest, LabelRef,
    ProjectSummary, TeamSummary, UserRef,
};

mod attachments;
mod comments;
mod graphql;
mod issues;
mod labels;
mod model;
mod pagination;
mod projects;
mod teams;
#[cfg(test)]
mod tests;
mod uploads;
mod viewer;

#[async_trait]
pub trait LinearClient: Send + Sync {
    async fn list_projects(&self, limit: usize) -> Result<Vec<ProjectSummary>>;
    async fn list_users(&self, limit: usize) -> Result<Vec<UserRef>>;
    async fn list_issues(&self, limit: usize) -> Result<Vec<IssueSummary>>;
    async fn list_filtered_issues(&self, filters: &IssueListFilters) -> Result<Vec<IssueSummary>>;
    async fn list_issue_labels(&self, team: Option<&str>) -> Result<Vec<LabelRef>>;
    async fn get_issue(&self, issue_id: &str) -> Result<IssueSummary>;
    async fn get_issue_dependency_snapshot(
        &self,
        issue_id: &str,
    ) -> Result<IssueDependencySnapshot> {
        self.get_issue(issue_id)
            .await
            .map(|issue| IssueDependencySnapshot {
                issue,
                relations: Vec::new(),
                inverse_relations: Vec::new(),
            })
    }
    async fn list_teams(&self) -> Result<Vec<TeamSummary>>;
    async fn viewer(&self) -> Result<UserRef>;
    async fn create_issue(&self, request: IssueCreateRequest) -> Result<IssueSummary>;
    async fn create_issue_label(&self, request: IssueLabelCreateRequest) -> Result<LabelRef>;
    async fn update_issue(
        &self,
        issue_id: &str,
        request: IssueUpdateRequest,
    ) -> Result<IssueSummary>;
    async fn create_issue_relation(
        &self,
        _request: IssueRelationCreateRequest,
    ) -> Result<IssueRelationSummary> {
        anyhow::bail!("issue relation mutations are not supported by this Linear client")
    }
    async fn update_issue_relation(
        &self,
        _relation_id: &str,
        _request: IssueRelationUpdateRequest,
    ) -> Result<IssueRelationSummary> {
        anyhow::bail!("issue relation mutations are not supported by this Linear client")
    }
    async fn create_comment(&self, issue_id: &str, body: String) -> Result<IssueComment>;
    async fn update_comment(&self, comment_id: &str, body: String) -> Result<IssueComment>;
    async fn upload_file(
        &self,
        filename: &str,
        content_type: &str,
        contents: Vec<u8>,
    ) -> Result<String>;
    async fn create_attachment(
        &self,
        request: AttachmentCreateRequest,
    ) -> Result<AttachmentSummary>;
    async fn delete_attachment(&self, attachment_id: &str) -> Result<()>;
    async fn download_file(&self, url: &str) -> Result<Vec<u8>>;
}

#[derive(Debug, Clone)]
pub struct ReqwestLinearClient {
    config: LinearConfig,
    http: Client,
}

impl ReqwestLinearClient {
    pub fn new(config: LinearConfig) -> Result<Self> {
        Ok(Self {
            config,
            http: Client::builder()
                .build()
                .context("failed to initialize the HTTP client")?,
        })
    }

    fn graphql(&self) -> graphql::GraphqlTransport<'_> {
        graphql::GraphqlTransport::new(&self.config, &self.http)
    }
}

#[async_trait]
impl LinearClient for ReqwestLinearClient {
    async fn list_projects(&self, limit: usize) -> Result<Vec<ProjectSummary>> {
        self.list_projects_resource(limit).await
    }

    async fn list_users(&self, limit: usize) -> Result<Vec<UserRef>> {
        self.list_users_resource(limit).await
    }

    async fn list_issues(&self, limit: usize) -> Result<Vec<IssueSummary>> {
        self.list_issues_resource(limit).await
    }

    async fn list_filtered_issues(&self, filters: &IssueListFilters) -> Result<Vec<IssueSummary>> {
        self.list_filtered_issues_resource(filters).await
    }

    async fn list_issue_labels(&self, team: Option<&str>) -> Result<Vec<LabelRef>> {
        self.list_issue_labels_resource(team).await
    }

    async fn get_issue(&self, issue_id: &str) -> Result<IssueSummary> {
        self.get_issue_resource(issue_id).await
    }

    async fn get_issue_dependency_snapshot(
        &self,
        issue_id: &str,
    ) -> Result<IssueDependencySnapshot> {
        self.get_issue_dependency_snapshot_resource(issue_id).await
    }

    async fn list_teams(&self) -> Result<Vec<TeamSummary>> {
        self.list_teams_resource().await
    }

    async fn viewer(&self) -> Result<UserRef> {
        self.viewer_resource().await
    }

    async fn create_issue(&self, request: IssueCreateRequest) -> Result<IssueSummary> {
        self.create_issue_resource(request).await
    }

    async fn create_issue_label(&self, request: IssueLabelCreateRequest) -> Result<LabelRef> {
        self.create_issue_label_resource(request).await
    }

    async fn update_issue(
        &self,
        issue_id: &str,
        request: IssueUpdateRequest,
    ) -> Result<IssueSummary> {
        self.update_issue_resource(issue_id, request).await
    }

    async fn create_issue_relation(
        &self,
        request: IssueRelationCreateRequest,
    ) -> Result<IssueRelationSummary> {
        self.create_issue_relation_resource(request).await
    }

    async fn update_issue_relation(
        &self,
        relation_id: &str,
        request: IssueRelationUpdateRequest,
    ) -> Result<IssueRelationSummary> {
        self.update_issue_relation_resource(relation_id, request)
            .await
    }

    async fn create_comment(&self, issue_id: &str, body: String) -> Result<IssueComment> {
        self.create_comment_resource(issue_id, body).await
    }

    async fn update_comment(&self, comment_id: &str, body: String) -> Result<IssueComment> {
        self.update_comment_resource(comment_id, body).await
    }

    async fn upload_file(
        &self,
        filename: &str,
        content_type: &str,
        contents: Vec<u8>,
    ) -> Result<String> {
        self.upload_file_resource(filename, content_type, contents)
            .await
    }

    async fn create_attachment(
        &self,
        request: AttachmentCreateRequest,
    ) -> Result<AttachmentSummary> {
        self.create_attachment_resource(request).await
    }

    async fn delete_attachment(&self, attachment_id: &str) -> Result<()> {
        self.delete_attachment_resource(attachment_id).await
    }

    async fn download_file(&self, url: &str) -> Result<Vec<u8>> {
        self.download_file_resource(url).await
    }
}
