use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use crate::linear::{
    AttachmentCreateRequest, AttachmentSummary, IssueAssigneeFilter, IssueComment,
    IssueCreateRequest, IssueLabelCreateRequest, IssueListFilters, IssueSummary,
    IssueUpdateRequest, LabelRef, LinearClient, ProjectRef, ProjectSummary, TeamRef, TeamSummary,
    UserRef, WorkflowState,
};
use anyhow::Result;
use async_trait::async_trait;

#[derive(Clone, Default)]
pub(super) struct FakeLinearClient {
    pub(super) projects: Vec<ProjectSummary>,
    pub(super) users: Vec<UserRef>,
    pub(super) issues: Vec<IssueSummary>,
    pub(super) all_issues: Vec<IssueSummary>,
    pub(super) issue_labels: Vec<LabelRef>,
    pub(super) teams: Vec<TeamSummary>,
    pub(super) issue_detail: Option<IssueSummary>,
    pub(super) updated_issue: Option<IssueSummary>,
    pub(super) create_requests: Arc<Mutex<Vec<IssueCreateRequest>>>,
    pub(super) update_requests: Arc<Mutex<Vec<(String, IssueUpdateRequest)>>>,
    pub(super) created_issue_labels: Arc<Mutex<Vec<IssueLabelCreateRequest>>>,
    pub(super) created_comments: Arc<Mutex<Vec<(String, String)>>>,
    pub(super) updated_comments: Arc<Mutex<Vec<(String, String)>>>,
    pub(super) list_issues_calls: Arc<AtomicUsize>,
    pub(super) list_filtered_issues_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LinearClient for FakeLinearClient {
    async fn list_projects(&self, _limit: usize) -> Result<Vec<ProjectSummary>> {
        Ok(self.projects.clone())
    }

    async fn list_users(&self, _limit: usize) -> Result<Vec<UserRef>> {
        if self.users.is_empty() {
            Ok(vec![
                UserRef {
                    id: "viewer-1".to_string(),
                    name: "Viewer".to_string(),
                    email: Some("viewer@example.com".to_string()),
                },
                UserRef {
                    id: "user-2".to_string(),
                    name: "Someone Else".to_string(),
                    email: Some("else@example.com".to_string()),
                },
            ])
        } else {
            Ok(self.users.clone())
        }
    }

    async fn list_issues(&self, _limit: usize) -> Result<Vec<IssueSummary>> {
        self.list_issues_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.issues.clone())
    }

    async fn list_filtered_issues(&self, filters: &IssueListFilters) -> Result<Vec<IssueSummary>> {
        self.list_filtered_issues_calls
            .fetch_add(1, Ordering::SeqCst);
        let mut issues = if self.all_issues.is_empty() {
            self.issues.clone()
        } else {
            self.all_issues.clone()
        };
        if let Some(team) = filters.team.as_deref() {
            issues.retain(|issue| issue.team.key.eq_ignore_ascii_case(team));
        }
        if let Some(project) = filters.project.as_deref() {
            issues.retain(|issue| {
                issue
                    .project
                    .as_ref()
                    .map(|entry| entry.name.eq_ignore_ascii_case(project))
                    .unwrap_or(false)
            });
        }
        if let Some(project_id) = filters.project_id.as_deref() {
            issues.retain(|issue| {
                issue
                    .project
                    .as_ref()
                    .map(|entry| entry.id == project_id)
                    .unwrap_or(false)
            });
        }
        if let Some(state) = filters.state.as_deref() {
            issues.retain(|issue| {
                issue
                    .state
                    .as_ref()
                    .map(|entry| entry.name.eq_ignore_ascii_case(state))
                    .unwrap_or(false)
            });
        }
        match &filters.assignee {
            IssueAssigneeFilter::Any => {}
            IssueAssigneeFilter::Viewer { viewer_id } => {
                issues.retain(|issue| {
                    issue
                        .assignee
                        .as_ref()
                        .map(|assignee| assignee.id == *viewer_id)
                        .unwrap_or(false)
                });
            }
            IssueAssigneeFilter::ViewerOrUnassigned { viewer_id } => {
                issues.retain(|issue| {
                    issue
                        .assignee
                        .as_ref()
                        .map(|assignee| assignee.id == *viewer_id)
                        .unwrap_or(true)
                });
            }
        }
        if issues.len() > filters.limit.max(1) {
            issues.truncate(filters.limit.max(1));
        }
        Ok(issues)
    }

    async fn list_issue_labels(&self, _team: Option<&str>) -> Result<Vec<LabelRef>> {
        Ok(self.issue_labels.clone())
    }

    async fn get_issue(&self, _issue_id: &str) -> Result<IssueSummary> {
        Ok(self
            .issue_detail
            .clone()
            .expect("issue detail should be configured"))
    }

    async fn list_teams(&self) -> Result<Vec<TeamSummary>> {
        Ok(self.teams.clone())
    }

    async fn viewer(&self) -> Result<UserRef> {
        Ok(UserRef {
            id: "viewer-1".to_string(),
            name: "Viewer".to_string(),
            email: Some("viewer@example.com".to_string()),
        })
    }

    async fn create_issue(&self, request: IssueCreateRequest) -> Result<IssueSummary> {
        self.create_requests
            .lock()
            .expect("mutex poisoned")
            .push(request);
        Ok(issue("MET-42", "Todo", Some("project-1"), "MetaStack CLI"))
    }

    async fn create_issue_label(&self, request: IssueLabelCreateRequest) -> Result<LabelRef> {
        self.created_issue_labels
            .lock()
            .expect("mutex poisoned")
            .push(request.clone());
        Ok(LabelRef {
            id: format!("label-{}", request.name),
            name: request.name,
        })
    }

    async fn update_issue(
        &self,
        issue_id: &str,
        request: IssueUpdateRequest,
    ) -> Result<IssueSummary> {
        self.update_requests
            .lock()
            .expect("mutex poisoned")
            .push((issue_id.to_string(), request));
        Ok(self
            .updated_issue
            .clone()
            .unwrap_or_else(|| issue("MET-11", "In Progress", Some("project-1"), "MetaStack CLI")))
    }

    async fn create_comment(&self, issue_id: &str, body: String) -> Result<IssueComment> {
        self.created_comments
            .lock()
            .expect("mutex poisoned")
            .push((issue_id.to_string(), body.clone()));
        Ok(IssueComment {
            id: "comment-new".to_string(),
            body,
            created_at: None,
            user_name: None,
            resolved_at: None,
        })
    }

    async fn update_comment(&self, comment_id: &str, body: String) -> Result<IssueComment> {
        self.updated_comments
            .lock()
            .expect("mutex poisoned")
            .push((comment_id.to_string(), body.clone()));
        Ok(IssueComment {
            id: comment_id.to_string(),
            body,
            created_at: None,
            user_name: None,
            resolved_at: None,
        })
    }

    async fn upload_file(
        &self,
        _filename: &str,
        _content_type: &str,
        _contents: Vec<u8>,
    ) -> Result<String> {
        unreachable!("upload_file is not used in these tests")
    }

    async fn create_attachment(
        &self,
        _request: AttachmentCreateRequest,
    ) -> Result<AttachmentSummary> {
        unreachable!("create_attachment is not used in these tests")
    }

    async fn delete_attachment(&self, _attachment_id: &str) -> Result<()> {
        unreachable!("delete_attachment is not used in these tests")
    }

    async fn download_file(&self, _url: &str) -> Result<Vec<u8>> {
        unreachable!("download_file is not used in these tests")
    }
}

pub(super) fn issue(
    identifier: &str,
    state_name: &str,
    project_id: Option<&str>,
    project_name: &str,
) -> IssueSummary {
    issue_for_team(identifier, "MET", state_name, project_id, project_name)
}

pub(super) fn issue_for_team(
    identifier: &str,
    team_key: &str,
    state_name: &str,
    project_id: Option<&str>,
    project_name: &str,
) -> IssueSummary {
    let team = TeamRef {
        id: format!("team-{team_key}"),
        key: team_key.to_string(),
        name: format!("Team {team_key}"),
    };

    IssueSummary {
        id: format!("issue-{}", identifier.to_ascii_lowercase()),
        identifier: identifier.to_string(),
        title: format!("Issue {identifier}"),
        description: Some(format!("Description for {identifier}")),
        url: format!("https://linear.app/issues/{identifier}"),
        priority: Some(2),
        estimate: Some(3.0),
        updated_at: "2026-03-14T16:00:00Z".to_string(),
        team,
        project: project_id.map(|project_id| ProjectRef {
            id: project_id.to_string(),
            name: project_name.to_string(),
        }),
        assignee: None,
        labels: Vec::new(),
        comments: Vec::new(),
        state: Some(WorkflowState {
            id: format!(
                "state-{}",
                state_name.to_ascii_lowercase().replace(' ', "-")
            ),
            name: state_name.to_string(),
            kind: Some("started".to_string()),
        }),
        attachments: Vec::new(),
        parent: None,
        children: Vec::new(),
    }
}

pub(super) fn project(id: &str, name: &str, team_keys: &[&str]) -> ProjectSummary {
    ProjectSummary {
        id: id.to_string(),
        name: name.to_string(),
        description: Some(format!("Project {name}")),
        url: format!("https://linear.app/projects/{id}"),
        progress: Some(0.42),
        teams: team_keys
            .iter()
            .map(|key| TeamRef {
                id: format!("team-{key}"),
                key: (*key).to_string(),
                name: format!("Team {key}"),
            })
            .collect(),
    }
}

pub(super) fn team(key: &str, states: &[(&str, &str)]) -> TeamSummary {
    TeamSummary {
        id: format!("team-{key}"),
        key: key.to_string(),
        name: format!("Team {key}"),
        states: states
            .iter()
            .map(|(id, name)| WorkflowState {
                id: (*id).to_string(),
                name: (*name).to_string(),
                kind: Some("started".to_string()),
            })
            .collect(),
    }
}

pub(super) fn comment(id: &str, body: &str, resolved_at: Option<&str>) -> IssueComment {
    IssueComment {
        id: id.to_string(),
        body: body.to_string(),
        created_at: None,
        user_name: None,
        resolved_at: resolved_at.map(str::to_string),
    }
}

pub(super) fn label(id: &str, name: &str) -> LabelRef {
    LabelRef {
        id: id.to_string(),
        name: name.to_string(),
    }
}
