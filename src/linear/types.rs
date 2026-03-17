use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamRef {
    pub id: String,
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueComment {
    pub id: String,
    pub body: String,
    #[serde(default)]
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowState {
    pub id: String,
    pub name: String,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AttachmentSummary {
    pub id: String,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub source_type: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueLink {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub url: String,
    #[serde(default)]
    pub progress: Option<f64>,
    #[serde(default)]
    pub teams: Vec<TeamRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSummary {
    pub id: String,
    pub identifier: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub url: String,
    #[serde(default)]
    pub priority: Option<u8>,
    #[serde(default)]
    pub estimate: Option<f64>,
    pub updated_at: String,
    pub team: TeamRef,
    #[serde(default)]
    pub project: Option<ProjectRef>,
    #[serde(default)]
    pub assignee: Option<UserRef>,
    #[serde(default)]
    pub labels: Vec<LabelRef>,
    #[serde(default)]
    pub comments: Vec<IssueComment>,
    #[serde(default)]
    pub state: Option<WorkflowState>,
    #[serde(default)]
    pub attachments: Vec<AttachmentSummary>,
    #[serde(default)]
    pub parent: Option<IssueLink>,
    #[serde(default)]
    pub children: Vec<IssueLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamSummary {
    pub id: String,
    pub key: String,
    pub name: String,
    pub states: Vec<WorkflowState>,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectListFilters {
    pub team: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, Default)]
pub struct IssueListFilters {
    pub team: Option<String>,
    pub project: Option<String>,
    pub project_id: Option<String>,
    pub state: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct DashboardFilters {
    pub team: Option<String>,
    pub project: Option<String>,
    pub project_id: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct IssueCreateSpec {
    pub team: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub project: Option<String>,
    pub project_id: Option<String>,
    pub parent_id: Option<String>,
    pub state: Option<String>,
    pub priority: Option<u8>,
    #[allow(dead_code)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct IssueEditSpec {
    pub identifier: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub project: Option<String>,
    pub state: Option<String>,
    pub priority: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct IssueEditContext {
    pub issue: IssueSummary,
    pub team: TeamSummary,
}

#[derive(Debug, Clone)]
pub struct IssueCreateRequest {
    pub team_id: String,
    pub title: String,
    pub description: Option<String>,
    pub project_id: Option<String>,
    pub parent_id: Option<String>,
    pub state_id: Option<String>,
    pub priority: Option<u8>,
    #[allow(dead_code)]
    pub label_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct IssueLabelCreateRequest {
    pub team_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct IssueUpdateRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub project_id: Option<String>,
    pub state_id: Option<String>,
    pub priority: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct AttachmentCreateRequest {
    pub issue_id: String,
    pub title: String,
    pub url: String,
    pub metadata: Value,
}

#[derive(Debug, Clone)]
pub struct DashboardData {
    pub title: String,
    pub issues: Vec<IssueSummary>,
}

impl DashboardData {
    pub fn demo() -> Self {
        let team = TeamRef {
            id: "team-demo".to_string(),
            key: "MET".to_string(),
            name: "Metastack".to_string(),
        };
        let project = ProjectSummary {
            id: "project-demo".to_string(),
            name: "MetaStack CLI".to_string(),
            description: Some("Command-line workflows for engineering teams.".to_string()),
            url: "https://linear.app/metastack".to_string(),
            progress: Some(0.52),
            teams: vec![team.clone()],
        };

        Self {
            title: "Linear Issues (demo)".to_string(),
            issues: vec![
                IssueSummary {
                    id: "issue-11".to_string(),
                    identifier: "MET-11".to_string(),
                    title: "CLI Scaffolding & Modules".to_string(),
                    description: Some(
                        "Create planning, scan, and Linear command flows.".to_string(),
                    ),
                    url: "https://linear.app/metastack/MET-11".to_string(),
                    priority: Some(2),
                    estimate: Some(2.0),
                    updated_at: "2026-03-14T16:00:00Z".to_string(),
                    team: team.clone(),
                    project: Some(ProjectRef {
                        id: project.id.clone(),
                        name: project.name.clone(),
                    }),
                    assignee: None,
                    labels: Vec::new(),
                    comments: Vec::new(),
                    state: Some(WorkflowState {
                        id: "state-progress".to_string(),
                        name: "In Progress".to_string(),
                        kind: Some("started".to_string()),
                    }),
                    attachments: Vec::new(),
                    parent: None,
                    children: Vec::new(),
                },
                IssueSummary {
                    id: "issue-12".to_string(),
                    identifier: "MET-12".to_string(),
                    title: "Add Tests".to_string(),
                    description: Some("Cover the new CLI modules and runtime proofs.".to_string()),
                    url: "https://linear.app/metastack/MET-12".to_string(),
                    priority: Some(1),
                    estimate: Some(5.0),
                    updated_at: "2026-03-14T16:05:00Z".to_string(),
                    team: team.clone(),
                    project: Some(ProjectRef {
                        id: project.id.clone(),
                        name: project.name.clone(),
                    }),
                    assignee: None,
                    labels: Vec::new(),
                    comments: Vec::new(),
                    state: Some(WorkflowState {
                        id: "state-todo".to_string(),
                        name: "Todo".to_string(),
                        kind: Some("unstarted".to_string()),
                    }),
                    attachments: Vec::new(),
                    parent: None,
                    children: Vec::new(),
                },
            ],
        }
    }
}
