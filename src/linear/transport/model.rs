use serde::Deserialize;
use serde_json::Value;

use crate::linear::{
    AttachmentSummary, IssueComment, IssueLink, IssueSummary, LabelRef, ProjectRef, ProjectSummary,
    TeamRef, TeamSummary, UserRef, WorkflowState,
};

#[derive(Debug, Deserialize)]
pub(super) struct GraphqlEnvelope<T> {
    pub(super) data: Option<T>,
    #[serde(default)]
    pub(super) errors: Option<Vec<GraphqlError>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GraphqlError {
    pub(super) message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Connection<T> {
    pub(super) nodes: Vec<T>,
    #[serde(default)]
    pub(super) page_info: Option<PageInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PageInfo {
    pub(super) has_next_page: bool,
    pub(super) end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ProjectsPayload {
    pub(super) projects: Connection<ProjectNode>,
}

#[derive(Debug, Deserialize)]
pub(super) struct UsersPayload {
    pub(super) users: Connection<UserRef>,
}

#[derive(Debug, Deserialize)]
pub(super) struct IssuesPayload {
    pub(super) issues: Connection<IssueNode>,
}

#[derive(Debug, Deserialize)]
pub(super) struct IssueLabelsPayload {
    #[serde(rename = "issueLabels")]
    pub(super) issue_labels: Connection<LabelRef>,
}

#[derive(Debug, Deserialize)]
pub(super) struct TeamsPayload {
    pub(super) teams: Connection<TeamNode>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ViewerPayload {
    pub(super) viewer: UserRef,
}

#[derive(Debug, Deserialize)]
pub(super) struct IssueByIdPayload {
    pub(super) issue: Option<IssueNode>,
}

#[derive(Debug, Deserialize)]
pub(super) struct IssueCommentsPayload {
    pub(super) issue: Option<IssueCommentsNode>,
}

#[derive(Debug, Deserialize)]
pub(super) struct IssueCreatePayload {
    #[serde(rename = "issueCreate")]
    pub(super) issue_create: IssueMutationNode,
}

#[derive(Debug, Deserialize)]
pub(super) struct IssueUpdatePayload {
    #[serde(rename = "issueUpdate")]
    pub(super) issue_update: IssueMutationNode,
}

#[derive(Debug, Deserialize)]
pub(super) struct IssueLabelCreatePayload {
    #[serde(rename = "issueLabelCreate")]
    pub(super) issue_label_create: IssueLabelMutationNode,
}

#[derive(Debug, Deserialize)]
pub(super) struct CommentMutationPayload {
    #[serde(rename = "commentCreate", alias = "commentUpdate")]
    pub(super) comment_mutation: CommentMutationNode,
}

#[derive(Debug, Deserialize)]
pub(super) struct AttachmentMutationPayload {
    #[serde(rename = "attachmentCreate")]
    pub(super) attachment_mutation: AttachmentMutationNode,
}

#[derive(Debug, Deserialize)]
pub(super) struct SuccessOnlyPayload {
    #[serde(rename = "attachmentDelete")]
    pub(super) success_payload: SuccessNode,
}

#[derive(Debug, Deserialize)]
pub(super) struct UploadPayload {
    #[serde(rename = "fileUpload")]
    pub(super) upload: UploadMutationNode,
}

#[derive(Debug, Deserialize)]
pub(super) struct IssueMutationNode {
    pub(super) success: bool,
    pub(super) issue: Option<IssueNode>,
}

#[derive(Debug, Deserialize)]
pub(super) struct IssueCommentsNode {
    #[serde(default)]
    pub(super) comments: Option<Connection<CommentNode>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct IssueLabelMutationNode {
    pub(super) success: bool,
    #[serde(rename = "issueLabel")]
    pub(super) issue_label: Option<LabelRef>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CommentMutationNode {
    pub(super) success: bool,
    pub(super) comment: Option<CommentNode>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AttachmentMutationNode {
    pub(super) success: bool,
    pub(super) attachment: Option<AttachmentNode>,
}

#[derive(Debug, Deserialize)]
pub(super) struct SuccessNode {
    pub(super) success: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UploadMutationNode {
    pub(super) success: bool,
    pub(super) upload_file: Option<UploadFileNode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UploadFileNode {
    pub(super) upload_url: String,
    pub(super) asset_url: String,
    #[serde(default)]
    pub(super) headers: Vec<UploadHeaderNode>,
}

#[derive(Debug, Deserialize)]
pub(super) struct UploadHeaderNode {
    pub(super) key: String,
    pub(super) value: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ProjectNode {
    pub(super) id: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) description: Option<String>,
    pub(super) url: String,
    #[serde(default)]
    pub(super) progress: Option<f64>,
    pub(super) teams: Connection<TeamRef>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IssueNode {
    pub(super) id: String,
    pub(super) identifier: String,
    pub(super) title: String,
    #[serde(default)]
    pub(super) description: Option<String>,
    pub(super) url: String,
    #[serde(default)]
    pub(super) priority: Option<u8>,
    #[serde(default)]
    pub(super) estimate: Option<f64>,
    pub(super) updated_at: String,
    pub(super) team: TeamRef,
    #[serde(default)]
    pub(super) project: Option<ProjectRef>,
    #[serde(default)]
    pub(super) assignee: Option<UserRef>,
    #[serde(default)]
    pub(super) labels: Option<Connection<LabelRef>>,
    #[serde(default)]
    pub(super) comments: Option<Connection<CommentNode>>,
    #[serde(default)]
    pub(super) state: Option<WorkflowState>,
    #[serde(default)]
    pub(super) attachments: Option<Connection<AttachmentNode>>,
    #[serde(default)]
    pub(super) parent: Option<IssueLinkNode>,
    #[serde(default)]
    pub(super) children: Option<Connection<IssueLinkNode>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AttachmentNode {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) url: String,
    #[serde(default)]
    pub(super) source_type: Option<String>,
    #[serde(default)]
    pub(super) metadata: Value,
}

#[derive(Debug, Deserialize)]
pub(super) struct IssueLinkNode {
    pub(super) id: String,
    pub(super) identifier: String,
    pub(super) title: String,
    pub(super) url: String,
    #[serde(default)]
    pub(super) description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CommentNode {
    pub(super) id: String,
    pub(super) body: String,
    #[serde(default)]
    pub(super) created_at: Option<String>,
    #[serde(default)]
    pub(super) user: Option<CommentUserNode>,
    #[serde(default)]
    pub(super) resolved_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CommentUserNode {
    pub(super) name: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct TeamNode {
    pub(super) id: String,
    pub(super) key: String,
    pub(super) name: String,
    pub(super) states: Connection<WorkflowState>,
}

impl From<ProjectNode> for ProjectSummary {
    fn from(value: ProjectNode) -> Self {
        Self {
            id: value.id,
            name: value.name,
            description: value.description,
            url: value.url,
            progress: value.progress,
            teams: value.teams.nodes,
        }
    }
}

impl From<IssueNode> for IssueSummary {
    fn from(value: IssueNode) -> Self {
        Self {
            id: value.id,
            identifier: value.identifier,
            title: value.title,
            description: value.description,
            url: value.url,
            priority: value.priority,
            estimate: value.estimate,
            updated_at: value.updated_at,
            team: value.team,
            project: value.project,
            assignee: value.assignee,
            labels: value.labels.map(|labels| labels.nodes).unwrap_or_default(),
            comments: value
                .comments
                .map(|comments| comments.nodes.into_iter().map(IssueComment::from).collect())
                .unwrap_or_default(),
            state: value.state,
            attachments: value
                .attachments
                .map(|attachments| {
                    attachments
                        .nodes
                        .into_iter()
                        .map(AttachmentSummary::from)
                        .collect()
                })
                .unwrap_or_default(),
            parent: value.parent.map(IssueLink::from),
            children: value
                .children
                .map(|children| children.nodes.into_iter().map(IssueLink::from).collect())
                .unwrap_or_default(),
        }
    }
}

impl From<CommentNode> for IssueComment {
    fn from(value: CommentNode) -> Self {
        Self {
            id: value.id,
            body: value.body,
            created_at: value.created_at,
            user_name: value.user.map(|user| user.name),
            resolved_at: value.resolved_at,
        }
    }
}

impl From<AttachmentNode> for AttachmentSummary {
    fn from(value: AttachmentNode) -> Self {
        Self {
            id: value.id,
            title: value.title,
            url: value.url,
            source_type: value.source_type,
            metadata: value.metadata,
        }
    }
}

impl From<IssueLinkNode> for IssueLink {
    fn from(value: IssueLinkNode) -> Self {
        Self {
            id: value.id,
            identifier: value.identifier,
            title: value.title,
            url: value.url,
            description: value.description,
        }
    }
}

impl From<TeamNode> for TeamSummary {
    fn from(value: TeamNode) -> Self {
        Self {
            id: value.id,
            key: value.key,
            name: value.name,
            states: value.states.nodes,
        }
    }
}
