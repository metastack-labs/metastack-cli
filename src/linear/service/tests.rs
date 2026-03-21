use std::sync::atomic::Ordering;

use anyhow::{Result, anyhow};
use async_trait::async_trait;

use super::{
    LinearService,
    test_support::{FakeLinearClient, comment, issue, issue_for_team, label, project, team},
};
use crate::linear::{
    AttachmentCreateRequest, AttachmentSummary, IssueAssigneeFilter, IssueComment,
    IssueCreateRequest, IssueLabelCreateRequest, IssueListFilters, IssueSummary,
    IssueUpdateRequest, LabelRef, LinearClient, ProjectSummary, TeamSummary, UserRef,
};
use crate::linear::{IssueCreateSpec, IssueEditSpec};

#[tokio::test]
async fn list_issues_uses_filtered_query_and_applies_filters() {
    let client = FakeLinearClient {
        issues: vec![issue("MET-01", "Todo", Some("project-1"), "MetaStack CLI")],
        all_issues: vec![
            issue("MET-12", "In Progress", Some("project-1"), "MetaStack CLI"),
            issue_for_team(
                "OPS-02",
                "OPS",
                "In Progress",
                Some("project-2"),
                "Operations",
            ),
            issue("MET-11", "Todo", Some("project-1"), "MetaStack CLI"),
        ],
        ..FakeLinearClient::default()
    };
    let service = LinearService::new(client.clone(), Some("MET".to_string()));

    let issues = service
        .list_issues(IssueListFilters {
            state: Some("In Progress".to_string()),
            limit: 5,
            ..IssueListFilters::default()
        })
        .await
        .expect("list issues should succeed");

    assert_eq!(
        issues
            .iter()
            .map(|issue| issue.identifier.as_str())
            .collect::<Vec<_>>(),
        vec!["MET-12"]
    );
    assert_eq!(client.list_filtered_issues_calls.load(Ordering::SeqCst), 1);
    assert_eq!(client.list_issues_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn list_issues_keeps_only_viewer_assigned_items_in_viewer_only_scope() {
    let mut viewer_issue = issue("MET-11", "Todo", Some("project-1"), "MetaStack CLI");
    viewer_issue.assignee = Some(UserRef {
        id: "viewer-1".to_string(),
        name: "Viewer".to_string(),
        email: Some("viewer@example.com".to_string()),
    });
    let mut foreign_issue = issue("MET-12", "Todo", Some("project-1"), "MetaStack CLI");
    foreign_issue.assignee = Some(UserRef {
        id: "viewer-2".to_string(),
        name: "Someone Else".to_string(),
        email: Some("else@example.com".to_string()),
    });
    let client = FakeLinearClient {
        all_issues: vec![
            viewer_issue,
            issue("MET-13", "Todo", Some("project-1"), "MetaStack CLI"),
            foreign_issue,
        ],
        ..FakeLinearClient::default()
    };
    let service = LinearService::new(client.clone(), Some("MET".to_string()));

    let issues = service
        .list_issues(IssueListFilters {
            team: Some("MET".to_string()),
            project_id: Some("project-1".to_string()),
            state: Some("Todo".to_string()),
            assignee: IssueAssigneeFilter::Viewer {
                viewer_id: "viewer-1".to_string(),
            },
            limit: 10,
            ..IssueListFilters::default()
        })
        .await
        .expect("viewer-scoped issues should load");

    assert_eq!(
        issues
            .iter()
            .map(|issue| issue.identifier.as_str())
            .collect::<Vec<_>>(),
        vec!["MET-11"]
    );
    assert_eq!(client.list_filtered_issues_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn list_issues_keeps_viewer_assigned_and_unassigned_items_in_viewer_or_unassigned_scope() {
    let mut viewer_issue = issue("MET-11", "Todo", Some("project-1"), "MetaStack CLI");
    viewer_issue.assignee = Some(UserRef {
        id: "viewer-1".to_string(),
        name: "Viewer".to_string(),
        email: Some("viewer@example.com".to_string()),
    });
    let mut foreign_issue = issue("MET-12", "Todo", Some("project-1"), "MetaStack CLI");
    foreign_issue.assignee = Some(UserRef {
        id: "viewer-2".to_string(),
        name: "Someone Else".to_string(),
        email: Some("else@example.com".to_string()),
    });
    let client = FakeLinearClient {
        all_issues: vec![
            viewer_issue,
            issue("MET-13", "Todo", Some("project-1"), "MetaStack CLI"),
            foreign_issue,
        ],
        ..FakeLinearClient::default()
    };
    let service = LinearService::new(client.clone(), Some("MET".to_string()));

    let issues = service
        .list_issues(IssueListFilters {
            team: Some("MET".to_string()),
            project_id: Some("project-1".to_string()),
            state: Some("Todo".to_string()),
            assignee: IssueAssigneeFilter::ViewerOrUnassigned {
                viewer_id: "viewer-1".to_string(),
            },
            limit: 10,
            ..IssueListFilters::default()
        })
        .await
        .expect("viewer-scoped issues should load");

    assert_eq!(
        issues
            .iter()
            .map(|issue| issue.identifier.as_str())
            .collect::<Vec<_>>(),
        vec!["MET-11", "MET-13"]
    );
    assert_eq!(client.list_filtered_issues_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn create_issue_resolves_team_state_and_project_for_team() {
    let client = FakeLinearClient {
        teams: vec![team(
            "MET",
            &[("state-1", "Todo"), ("state-2", "In Progress")],
        )],
        projects: vec![
            project("project-1", "MetaStack CLI", &["MET"]),
            project("project-2", "Other", &["OPS"]),
        ],
        ..FakeLinearClient::default()
    };
    let service = LinearService::new(client.clone(), None);

    service
        .create_issue(IssueCreateSpec {
            team: Some("MET".to_string()),
            title: "Refactor boundaries".to_string(),
            description: Some("Split handlers and services".to_string()),
            project: Some("MetaStack CLI".to_string()),
            project_id: None,
            parent_id: None,
            state: Some("In Progress".to_string()),
            priority: Some(2),
            assignee_id: None,
            labels: Vec::new(),
        })
        .await
        .expect("create issue should succeed");

    let requests = client.create_requests.lock().expect("mutex poisoned");
    let request = requests
        .first()
        .expect("a create request should be recorded");
    assert_eq!(request.team_id, "team-MET");
    assert_eq!(request.project_id.as_deref(), Some("project-1"));
    assert_eq!(request.state_id.as_deref(), Some("state-2"));
    assert_eq!(request.priority, Some(2));
}

#[tokio::test]
async fn create_issue_keeps_explicit_project_id_when_lookup_misses() {
    let client = FakeLinearClient {
        teams: vec![team("MET", &[("state-1", "Todo")])],
        projects: vec![project("project-1", "MetaStack CLI", &["MET"])],
        ..FakeLinearClient::default()
    };
    let service = LinearService::new(client.clone(), None);

    service
        .create_issue(IssueCreateSpec {
            team: Some("MET".to_string()),
            title: "Use seeded project".to_string(),
            description: None,
            project: None,
            project_id: Some("project-seeded".to_string()),
            parent_id: None,
            state: Some("Todo".to_string()),
            priority: None,
            assignee_id: None,
            labels: Vec::new(),
        })
        .await
        .expect("create issue should succeed");

    let requests = client.create_requests.lock().expect("mutex poisoned");
    let request = requests
        .first()
        .expect("a create request should be recorded");
    assert_eq!(request.project_id.as_deref(), Some("project-seeded"));
}

#[tokio::test]
async fn create_issue_passes_resolved_assignee_id() {
    let client = FakeLinearClient {
        teams: vec![team("MET", &[("state-1", "Backlog")])],
        ..FakeLinearClient::default()
    };
    let service = LinearService::new(client.clone(), None);

    service
        .create_issue(IssueCreateSpec {
            team: Some("MET".to_string()),
            title: "Assigned issue".to_string(),
            description: None,
            project: None,
            project_id: None,
            parent_id: None,
            state: Some("Backlog".to_string()),
            priority: Some(2),
            assignee_id: Some("viewer-1".to_string()),
            labels: Vec::new(),
        })
        .await
        .expect("create issue should succeed");

    let requests = client.create_requests.lock().expect("mutex poisoned");
    assert_eq!(requests[0].assignee_id.as_deref(), Some("viewer-1"));
}

#[tokio::test]
async fn resolve_assignee_id_supports_viewer_user_ids_names_and_emails() {
    let client = FakeLinearClient::default();
    let service = LinearService::new(client, Some("MET".to_string()));

    assert_eq!(
        service
            .resolve_assignee_id(Some("viewer"))
            .await
            .expect("viewer should resolve"),
        Some("viewer-1".to_string())
    );
    assert_eq!(
        service
            .resolve_assignee_id(Some("user-2"))
            .await
            .expect("user id should resolve"),
        Some("user-2".to_string())
    );
    assert_eq!(
        service
            .resolve_assignee_id(Some("Someone Else"))
            .await
            .expect("user name should resolve"),
        Some("user-2".to_string())
    );
    assert_eq!(
        service
            .resolve_assignee_id(Some("else@example.com"))
            .await
            .expect("user email should resolve"),
        Some("user-2".to_string())
    );
}

#[tokio::test]
async fn resolve_assignee_id_rejects_missing_and_ambiguous_matches() {
    let client = FakeLinearClient {
        users: vec![
            UserRef {
                id: "user-1".to_string(),
                name: "Taylor".to_string(),
                email: Some("taylor@example.com".to_string()),
            },
            UserRef {
                id: "user-2".to_string(),
                name: "Taylor".to_string(),
                email: Some("taylor+2@example.com".to_string()),
            },
        ],
        ..FakeLinearClient::default()
    };
    let service = LinearService::new(client, Some("MET".to_string()));

    let missing = service
        .resolve_assignee_id(Some("nobody@example.com"))
        .await
        .expect_err("missing user should fail");
    assert!(missing.to_string().contains("was not found in Linear"));

    let ambiguous = service
        .resolve_assignee_id(Some("Taylor"))
        .await
        .expect_err("ambiguous user should fail");
    assert!(
        ambiguous
            .to_string()
            .contains("matched multiple Linear users")
    );
    assert!(ambiguous.to_string().contains("user-1"));
    assert!(ambiguous.to_string().contains("user-2"));
}

#[tokio::test]
async fn ensure_issue_labels_exist_creates_only_missing_labels() {
    let client = FakeLinearClient {
        teams: vec![team("MET", &[("state-1", "Todo")])],
        issue_labels: vec![label("label-plan", "plan")],
        ..FakeLinearClient::default()
    };
    let service = LinearService::new(client.clone(), Some("MET".to_string()));

    service
        .ensure_issue_labels_exist(
            None,
            &[
                "plan".to_string(),
                "technical".to_string(),
                "TECHNICAL".to_string(),
            ],
        )
        .await
        .expect("labels should be reconciled");

    let created = client.created_issue_labels.lock().expect("mutex poisoned");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].team_id, "team-MET");
    assert_eq!(created[0].name, "technical");
}

#[derive(Clone)]
struct DuplicateThenVisibleLabelClient {
    teams: Vec<TeamSummary>,
    initial_labels: Vec<LabelRef>,
}

#[async_trait]
impl LinearClient for DuplicateThenVisibleLabelClient {
    async fn list_projects(&self, _limit: usize) -> Result<Vec<ProjectSummary>> {
        unreachable!("list_projects is not used in these tests")
    }

    async fn list_users(&self, _limit: usize) -> Result<Vec<UserRef>> {
        unreachable!("list_users is not used in these tests")
    }

    async fn list_issues(&self, _limit: usize) -> Result<Vec<IssueSummary>> {
        unreachable!("list_issues is not used in these tests")
    }

    async fn list_filtered_issues(&self, _filters: &IssueListFilters) -> Result<Vec<IssueSummary>> {
        unreachable!("list_filtered_issues is not used in these tests")
    }

    async fn list_issue_labels(&self, _team: Option<&str>) -> Result<Vec<LabelRef>> {
        let mut labels = self.initial_labels.clone();
        labels.push(label("label-technical", "technical"));
        Ok(labels)
    }

    async fn get_issue(&self, _issue_id: &str) -> Result<IssueSummary> {
        unreachable!("get_issue is not used in these tests")
    }

    async fn list_teams(&self) -> Result<Vec<TeamSummary>> {
        Ok(self.teams.clone())
    }

    async fn viewer(&self) -> Result<UserRef> {
        unreachable!("viewer is not used in these tests")
    }

    async fn create_issue(&self, _request: IssueCreateRequest) -> Result<IssueSummary> {
        unreachable!("create_issue is not used in these tests")
    }

    async fn create_issue_label(&self, _request: IssueLabelCreateRequest) -> Result<LabelRef> {
        Err(anyhow!("Linear request failed: duplicate label name"))
    }

    async fn update_issue(
        &self,
        _issue_id: &str,
        _request: IssueUpdateRequest,
    ) -> Result<IssueSummary> {
        unreachable!("update_issue is not used in these tests")
    }

    async fn create_comment(&self, _issue_id: &str, _body: String) -> Result<IssueComment> {
        unreachable!("create_comment is not used in these tests")
    }

    async fn update_comment(&self, _comment_id: &str, _body: String) -> Result<IssueComment> {
        unreachable!("update_comment is not used in these tests")
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

#[tokio::test]
async fn ensure_issue_labels_exist_ignores_duplicate_create_when_label_is_visible_on_retry() {
    let client = DuplicateThenVisibleLabelClient {
        teams: vec![team("MET", &[("state-1", "Todo")])],
        initial_labels: vec![label("label-plan", "plan")],
    };
    let service = LinearService::new(client, Some("MET".to_string()));

    service
        .ensure_issue_labels_exist(None, &["technical".to_string()])
        .await
        .expect("duplicate label race should be tolerated when the label already exists");
}

#[derive(Clone)]
struct DuplicateLabelClient {
    teams: Vec<TeamSummary>,
}

#[async_trait]
impl LinearClient for DuplicateLabelClient {
    async fn list_projects(&self, _limit: usize) -> Result<Vec<ProjectSummary>> {
        unreachable!("list_projects is not used in these tests")
    }

    async fn list_users(&self, _limit: usize) -> Result<Vec<UserRef>> {
        unreachable!("list_users is not used in these tests")
    }

    async fn list_issues(&self, _limit: usize) -> Result<Vec<IssueSummary>> {
        unreachable!("list_issues is not used in these tests")
    }

    async fn list_filtered_issues(&self, _filters: &IssueListFilters) -> Result<Vec<IssueSummary>> {
        unreachable!("list_filtered_issues is not used in these tests")
    }

    async fn list_issue_labels(&self, _team: Option<&str>) -> Result<Vec<LabelRef>> {
        Ok(Vec::new())
    }

    async fn get_issue(&self, _issue_id: &str) -> Result<IssueSummary> {
        unreachable!("get_issue is not used in these tests")
    }

    async fn list_teams(&self) -> Result<Vec<TeamSummary>> {
        Ok(self.teams.clone())
    }

    async fn viewer(&self) -> Result<UserRef> {
        unreachable!("viewer is not used in these tests")
    }

    async fn create_issue(&self, _request: IssueCreateRequest) -> Result<IssueSummary> {
        unreachable!("create_issue is not used in these tests")
    }

    async fn create_issue_label(&self, _request: IssueLabelCreateRequest) -> Result<LabelRef> {
        Err(anyhow!("Linear request failed: duplicate label name"))
    }

    async fn update_issue(
        &self,
        _issue_id: &str,
        _request: IssueUpdateRequest,
    ) -> Result<IssueSummary> {
        unreachable!("update_issue is not used in these tests")
    }

    async fn create_comment(&self, _issue_id: &str, _body: String) -> Result<IssueComment> {
        unreachable!("create_comment is not used in these tests")
    }

    async fn update_comment(&self, _comment_id: &str, _body: String) -> Result<IssueComment> {
        unreachable!("update_comment is not used in these tests")
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

#[tokio::test]
async fn ensure_issue_labels_exist_ignores_duplicate_create_when_linear_already_rejected_it() {
    let client = DuplicateLabelClient {
        teams: vec![team("MET", &[("state-1", "Todo")])],
    };
    let service = LinearService::new(client, Some("MET".to_string()));

    service
        .ensure_issue_labels_exist(None, &["technical".to_string()])
        .await
        .expect("duplicate label responses should not fail repo setup");
}

#[tokio::test]
async fn resolve_project_selector_strict_rejects_ambiguous_matches() {
    let client = FakeLinearClient {
        projects: vec![
            project("project-1", "MetaStack CLI", &["MET"]),
            project("project-2", "MetaStack CLI", &["MET", "OPS"]),
        ],
        ..FakeLinearClient::default()
    };
    let service = LinearService::new(client, None);

    let error = service
        .resolve_project_selector_strict("MetaStack CLI", Some("MET"))
        .await
        .expect_err("ambiguous projects should fail");

    assert!(
        error
            .to_string()
            .contains("matched multiple Linear projects")
    );
    assert!(error.to_string().contains("project-1"));
    assert!(error.to_string().contains("project-2"));
}

#[tokio::test]
async fn edit_issue_updates_requested_fields_after_loading_context() {
    let client = FakeLinearClient {
        issues: vec![issue("MET-11", "Todo", Some("project-1"), "MetaStack CLI")],
        all_issues: vec![issue("MET-11", "Todo", Some("project-1"), "MetaStack CLI")],
        teams: vec![team(
            "MET",
            &[("state-1", "Todo"), ("state-2", "In Progress")],
        )],
        projects: vec![project("project-1", "MetaStack CLI", &["MET"])],
        updated_issue: Some(issue(
            "MET-11",
            "In Progress",
            Some("project-1"),
            "MetaStack CLI",
        )),
        ..FakeLinearClient::default()
    };
    let service = LinearService::new(client.clone(), Some("MET".to_string()));

    let issue = service
        .edit_issue(IssueEditSpec {
            identifier: "MET-11".to_string(),
            title: Some("CLI Foundation".to_string()),
            description: None,
            project: Some("MetaStack CLI".to_string()),
            state: Some("In Progress".to_string()),
            priority: Some(1),
            estimate: None,
            labels: None,
            parent_identifier: None,
        })
        .await
        .expect("issue edit should succeed");

    assert_eq!(
        issue.state.as_ref().map(|state| state.name.as_str()),
        Some("In Progress")
    );
    let updates = client.update_requests.lock().expect("mutex poisoned");
    let (issue_id, request) = updates
        .first()
        .expect("an update request should be recorded");
    assert_eq!(issue_id, "issue-met-11");
    assert_eq!(request.title.as_deref(), Some("CLI Foundation"));
    assert_eq!(request.project_id.as_deref(), Some("project-1"));
    assert_eq!(request.state_id.as_deref(), Some("state-2"));
    assert_eq!(request.priority, Some(1));
    assert_eq!(request.estimate, None);
    assert_eq!(request.label_ids, None);
    assert_eq!(request.parent_id, None);
}

#[tokio::test]
async fn edit_issue_updates_labels_estimate_and_parent() {
    let mut parent = issue("MET-10", "Backlog", Some("project-1"), "MetaStack CLI");
    parent.title = "Parent issue".to_string();
    let client = FakeLinearClient {
        issues: vec![issue("MET-11", "Todo", Some("project-1"), "MetaStack CLI")],
        all_issues: vec![
            issue("MET-11", "Todo", Some("project-1"), "MetaStack CLI"),
            parent.clone(),
        ],
        issue_labels: vec![
            label("label-plan", "plan"),
            label("label-hygiene", "hygiene"),
        ],
        teams: vec![team("MET", &[("state-1", "Todo"), ("state-2", "Backlog")])],
        issue_detail: Some(parent),
        updated_issue: Some(issue("MET-11", "Todo", Some("project-1"), "MetaStack CLI")),
        ..FakeLinearClient::default()
    };
    let service = LinearService::new(client.clone(), Some("MET".to_string()));

    service
        .edit_issue(IssueEditSpec {
            identifier: "MET-11".to_string(),
            title: None,
            description: None,
            project: None,
            state: None,
            priority: None,
            estimate: Some(5.0),
            labels: Some(vec!["plan".to_string(), "hygiene".to_string()]),
            parent_identifier: Some("MET-10".to_string()),
        })
        .await
        .expect("issue edit should succeed");

    let updates = client.update_requests.lock().expect("mutex poisoned");
    let (_issue_id, request) = updates
        .first()
        .expect("an update request should be recorded");
    assert_eq!(request.estimate, Some(5.0));
    assert_eq!(
        request.label_ids.as_ref(),
        Some(&vec!["label-plan".to_string(), "label-hygiene".to_string()])
    );
    assert_eq!(request.parent_id.as_deref(), Some("issue-met-10"));
}

#[tokio::test]
async fn edit_issue_creates_missing_labels_and_uses_created_ids() {
    let client = FakeLinearClient {
        issues: vec![issue("MET-11", "Todo", Some("project-1"), "MetaStack CLI")],
        all_issues: vec![issue("MET-11", "Todo", Some("project-1"), "MetaStack CLI")],
        issue_labels: vec![label("label-plan", "plan")],
        teams: vec![team("MET", &[("state-1", "Todo"), ("state-2", "Backlog")])],
        updated_issue: Some(issue("MET-11", "Todo", Some("project-1"), "MetaStack CLI")),
        ..FakeLinearClient::default()
    };
    let service = LinearService::new(client.clone(), Some("MET".to_string()));

    service
        .edit_issue(IssueEditSpec {
            identifier: "MET-11".to_string(),
            title: None,
            description: None,
            project: None,
            state: None,
            priority: None,
            estimate: None,
            labels: Some(vec!["plan".to_string(), "feature".to_string()]),
            parent_identifier: None,
        })
        .await
        .expect("issue edit should create the missing label before update");

    let created = client.created_issue_labels.lock().expect("mutex poisoned");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].name, "feature");

    let updates = client.update_requests.lock().expect("mutex poisoned");
    let (_issue_id, request) = updates
        .first()
        .expect("an update request should be recorded");
    assert_eq!(
        request.label_ids.as_ref(),
        Some(&vec!["label-plan".to_string(), "label-feature".to_string()])
    );
}

#[tokio::test]
async fn upsert_workpad_comment_updates_existing_active_comment() {
    let client = FakeLinearClient::default();
    let service = LinearService::new(client.clone(), None);
    let mut issue = issue("MET-11", "Todo", Some("project-1"), "MetaStack CLI");
    issue.comments = vec![
        comment(
            "comment-resolved",
            "## Codex Workpad",
            Some("2026-03-15T00:00:00Z"),
        ),
        comment("comment-active", "## Codex Workpad\n\nold body", None),
    ];

    let updated = service
        .upsert_workpad_comment(&issue, "## Codex Workpad\n\nnew body".to_string())
        .await
        .expect("existing workpad should update");

    assert_eq!(updated.id, "comment-active");
    let updates = client.updated_comments.lock().expect("mutex poisoned");
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].0, "comment-active");
}
