use httpmock::Method::POST;
use httpmock::MockServer;
use serde_json::json;

use crate::config::LinearConfig;
use crate::linear::{
    IssueAssigneeFilter, IssueCreateRequest, IssueLabelCreateRequest, IssueListFilters,
    IssueUpdateRequest, LinearClient, ReqwestLinearClient,
};

#[tokio::test]
async fn reqwest_client_honors_issue_limit_without_extra_pages() {
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let first_page = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues")
            .body_includes("\"first\":1")
            .body_includes("\"after\":null");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [issue_node("MET-11")],
                    "pageInfo": {
                        "hasNextPage": true,
                        "endCursor": "cursor-1"
                    }
                }
            }
        }));
    });
    let second_page = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("\"after\":\"cursor-1\"");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [issue_node("MET-12")],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        }));
    });
    let client = client(api_url);

    let issues = client.list_issues(1).await.expect("issue page should load");

    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].identifier, "MET-11");
    first_page.assert_calls(1);
    second_page.assert_calls(0);
}

#[tokio::test]
async fn reqwest_client_sends_server_side_issue_filters() {
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let filtered_page = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues")
            .body_includes("\"team\":{\"key\":{\"eq\":\"MET\"}}")
            .body_includes("\"project\":{\"id\":{\"eq\":\"project-1\"}}")
            .body_includes("\"state\":{\"name\":{\"eq\":\"Todo\"}}");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [issue_node("MET-11")],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        }));
    });
    let client = client(api_url);

    let issues = client
        .list_filtered_issues(&IssueListFilters {
            team: Some("MET".to_string()),
            project: None,
            project_id: Some("project-1".to_string()),
            state: Some("Todo".to_string()),
            assignee: IssueAssigneeFilter::ViewerOrUnassigned {
                viewer_id: "viewer-1".to_string(),
            },
            limit: 5,
        })
        .await
        .expect("filtered issues should load");

    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].identifier, "MET-11");
    filtered_page.assert_calls(1);
}

#[tokio::test]
async fn reqwest_client_lists_issue_labels_for_team() {
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let labels_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query IssueLabels")
            .body_includes("\"key\":{\"eq\":\"MET\"}");
        then.status(200).json_body(json!({
            "data": {
                "issueLabels": {
                    "nodes": [
                        {
                            "id": "label-plan",
                            "name": "plan"
                        }
                    ],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                }
            }
        }));
    });
    let client = client(api_url);

    let labels = client
        .list_issue_labels(Some("MET"))
        .await
        .expect("labels should load");

    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].id, "label-plan");
    assert_eq!(labels[0].name, "plan");
    labels_mock.assert_calls(1);
}

#[tokio::test]
async fn reqwest_client_sends_assignee_and_label_ids_when_creating_issue() {
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let create_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue")
            .body_includes("\"assigneeId\":\"user-1\"")
            .body_includes("\"labelIds\":[\"label-plan\"]");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": issue_node("MET-41")
                }
            }
        }));
    });
    let client = client(api_url);

    let issue = client
        .create_issue(IssueCreateRequest {
            team_id: "team-1".to_string(),
            title: "Create labeled issue".to_string(),
            description: Some("Description".to_string()),
            project_id: Some("project-1".to_string()),
            parent_id: None,
            state_id: Some("state-1".to_string()),
            priority: Some(2),
            assignee_id: Some("user-1".to_string()),
            label_ids: vec!["label-plan".to_string()],
        })
        .await
        .expect("issue creation should succeed");

    assert_eq!(issue.identifier, "MET-41");
    create_mock.assert_calls(1);
}

#[tokio::test]
async fn reqwest_client_allows_unassigned_issue_creation() {
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let create_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue")
            .body_includes("\"labelIds\":[]");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": issue_node("MET-42")
                }
            }
        }));
    });
    let client = client(api_url);

    let issue = client
        .create_issue(IssueCreateRequest {
            team_id: "team-1".to_string(),
            title: "Create unassigned issue".to_string(),
            description: Some("Description".to_string()),
            project_id: Some("project-1".to_string()),
            parent_id: None,
            state_id: Some("state-1".to_string()),
            priority: Some(2),
            assignee_id: None,
            label_ids: Vec::new(),
        })
        .await
        .expect("issue creation should succeed");

    assert_eq!(issue.identifier, "MET-42");
    create_mock.assert_calls(1);
}

#[tokio::test]
async fn reqwest_client_creates_issue_labels() {
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let create_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssueLabel")
            .body_includes("\"teamId\":\"team-1\"")
            .body_includes("\"name\":\"technical\"");
        then.status(200).json_body(json!({
            "data": {
                "issueLabelCreate": {
                    "success": true,
                    "issueLabel": {
                        "id": "label-technical",
                        "name": "technical"
                    }
                }
            }
        }));
    });
    let client = client(api_url);

    let label = client
        .create_issue_label(IssueLabelCreateRequest {
            team_id: "team-1".to_string(),
            name: "technical".to_string(),
        })
        .await
        .expect("issue label creation should succeed");

    assert_eq!(label.id, "label-technical");
    assert_eq!(label.name, "technical");
    create_mock.assert_calls(1);
}

#[tokio::test]
async fn reqwest_client_sends_estimate_labels_and_parent_when_updating_issue() {
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let update_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue")
            .body_includes("\"estimate\":5.0")
            .body_includes("\"labelIds\":[\"label-plan\",\"label-hygiene\"]")
            .body_includes("\"parentId\":\"issue-parent\"");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": issue_node("MET-41")
                }
            }
        }));
    });
    let client = client(api_url);

    let issue = client
        .update_issue(
            "issue-met-41",
            IssueUpdateRequest {
                title: None,
                description: None,
                project_id: None,
                state_id: None,
                priority: None,
                estimate: Some(5.0),
                label_ids: Some(vec!["label-plan".to_string(), "label-hygiene".to_string()]),
                parent_id: Some("issue-parent".to_string()),
            },
        )
        .await
        .expect("issue update should succeed");

    assert_eq!(issue.identifier, "MET-41");
    update_mock.assert_calls(1);
}

#[tokio::test]
async fn reqwest_client_fetches_parent_description_and_comment_attribution() {
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let issue_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issue")
            .body_includes("createdAt")
            .body_includes("user {")
            .body_includes("description")
            .body_includes("\"id\":\"issue-1\"");
        then.status(200).json_body(json!({
            "data": {
                "issue": {
                    "id": "issue-1",
                    "identifier": "MET-41",
                    "title": "Issue MET-41",
                    "description": "Main description",
                    "url": "https://linear.app/issues/MET-41",
                    "priority": 2,
                    "estimate": 3.0,
                    "updatedAt": "2026-03-14T16:00:00Z",
                    "team": {
                        "id": "team-1",
                        "key": "MET",
                        "name": "Metastack"
                    },
                    "project": {
                        "id": "project-1",
                        "name": "MetaStack CLI"
                    },
                    "assignee": null,
                    "labels": {
                        "nodes": []
                    },
                    "comments": {
                        "nodes": [{
                            "id": "comment-1",
                            "body": "Please localize this image",
                            "createdAt": "2026-03-17T13:37:00Z",
                            "user": {
                                "name": "Jane Reviewer"
                            },
                            "resolvedAt": null
                        }]
                    },
                    "state": {
                        "id": "state-1",
                        "name": "Todo",
                        "type": "unstarted"
                    },
                    "attachments": {
                        "nodes": []
                    },
                    "parent": {
                        "id": "parent-1",
                        "identifier": "MET-40",
                        "title": "Parent issue",
                        "url": "https://linear.app/issues/MET-40",
                        "description": "Parent issue description"
                    },
                    "children": {
                        "nodes": []
                    }
                }
            }
        }));
    });
    let client = client(api_url);

    let issue = client
        .get_issue("issue-1")
        .await
        .expect("issue detail should load");

    assert_eq!(
        issue
            .parent
            .as_ref()
            .and_then(|parent| parent.description.as_deref()),
        Some("Parent issue description")
    );
    assert_eq!(issue.comments.len(), 1);
    assert_eq!(
        issue.comments[0].created_at.as_deref(),
        Some("2026-03-17T13:37:00Z")
    );
    assert_eq!(
        issue.comments[0].user_name.as_deref(),
        Some("Jane Reviewer")
    );
    issue_mock.assert_calls(1);
}

#[test]
fn download_request_adds_raw_authorization_only_for_linear_upload_hosts() {
    let client = client("https://api.linear.app/graphql".to_string());
    let transport = super::graphql::GraphqlTransport::new(&client.config, &client.http);

    let linear_request = transport
        .build_download_request("https://uploads.linear.app/uploads/test.png")
        .expect("linear upload request")
        .build()
        .expect("request should build");
    let external_request = transport
        .build_download_request("https://example.com/test.png")
        .expect("external request")
        .build()
        .expect("request should build");

    assert_eq!(
        linear_request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("token")
    );
    assert!(
        !external_request
            .headers()
            .contains_key(reqwest::header::AUTHORIZATION)
    );
}

fn client(api_url: String) -> ReqwestLinearClient {
    ReqwestLinearClient::new(LinearConfig {
        api_key: "token".to_string(),
        api_url,
        default_team: None,
    })
    .expect("client should build")
}

fn issue_node(identifier: &str) -> serde_json::Value {
    json!({
        "id": identifier.to_ascii_lowercase(),
        "identifier": identifier,
        "title": format!("Issue {identifier}"),
        "description": format!("Description for {identifier}"),
        "url": format!("https://linear.app/issues/{identifier}"),
        "priority": 2,
        "estimate": 3.0,
        "updatedAt": "2026-03-14T16:00:00Z",
        "team": {
            "id": "team-1",
            "key": "MET",
            "name": "Metastack"
        },
        "project": {
            "id": "project-1",
            "name": "MetaStack CLI"
        },
        "assignee": null,
        "labels": {
            "nodes": []
        },
        "state": {
            "id": "state-1",
            "name": "Todo",
            "type": "unstarted"
        }
    })
}
