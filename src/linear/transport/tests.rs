use httpmock::Method::POST;
use httpmock::MockServer;
use serde_json::json;

use crate::config::LinearConfig;
use crate::linear::{
    IssueCreateRequest, IssueLabelCreateRequest, IssueListFilters, LinearClient,
    ReqwestLinearClient,
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
async fn reqwest_client_sends_label_ids_when_creating_issue() {
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let create_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue")
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
            label_ids: vec!["label-plan".to_string()],
        })
        .await
        .expect("issue creation should succeed");

    assert_eq!(issue.identifier, "MET-41");
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
