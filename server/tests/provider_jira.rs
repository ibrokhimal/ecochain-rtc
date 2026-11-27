use axum::{
    extract::{Path, Query},
    http::{header, HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use ecochain_rtc_server::{
    config::Secret,
    providers::{
        client,
        jira::{
            build_payload, create_issue, find_existing, operation_key, projects, JiraCreateError,
            JiraPreview,
        },
        JiraConfig,
    },
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::{net::TcpListener, task::JoinHandle};

const EMAIL: &str = "integration@example.com";
const TOKEN: &str = "mock-private-api-token";
const INSTANCE: &str = "meeting-1";
const TASK_ID: &str = "task-1";
const OPERATION: &str = "fbee85dfc328d84bf4fc7002ee3125d46be3df679170431e2af6df0aef1c45a9";

struct MockJira {
    config: JiraConfig,
    task: JoinHandle<()>,
}

impl MockJira {
    async fn serve(router: Router) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = config(format!("http://{}", listener.local_addr().unwrap()));
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { config, task }
    }
}

impl Drop for MockJira {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn config(base_url: impl Into<String>) -> JiraConfig {
    JiraConfig {
        base_url: base_url.into(),
        email: EMAIL.into(),
        api_token: Secret::new(TOKEN),
        project_keys: vec!["WORK".into(), "NEXT".into()],
        issue_type_id: "10007".into(),
        issue_type_name: "Task".into(),
        identity_account_ids: [("person-1".into(), "559:account".into())].into(),
    }
}

fn preview() -> JiraPreview {
    JiraPreview {
        task_id: TASK_ID.into(),
        project_key: "WORK".into(),
        summary: "Schedule the launch".into(),
        description: "Coordinate launch coverage.\n\nConfirm owners.".into(),
        assignee_identity: Some("person-1".into()),
        due_date: Some("2028-02-29".into()),
    }
}

fn meeting_property() -> Value {
    json!({
        "key": "ecochain_meeting",
        "value": {
            "instance_id": INSTANCE,
            "task_id": TASK_ID,
            "operation_key": OPERATION
        }
    })
}

fn assert_basic_auth(headers: &HeaderMap) {
    assert_eq!(
        headers[header::AUTHORIZATION],
        format!("Basic {}", STANDARD.encode(format!("{EMAIL}:{TOKEN}")))
    );
}

#[test]
fn payload_contains_adf_assignee_due_date_and_stable_reference() {
    let config = config("https://example.atlassian.net");
    let preview = preview();
    let payload = build_payload(&config, INSTANCE, &preview).unwrap();
    assert_eq!(
        payload,
        json!({
            "fields": {
                "project": { "key": "WORK" },
                "issuetype": { "id": "10007" },
                "summary": "Schedule the launch",
                "description": {
                    "type": "doc",
                    "version": 1,
                    "content": [
                        { "type": "paragraph", "content": [{ "type": "text", "text": "Coordinate launch coverage." }] },
                        { "type": "paragraph" },
                        { "type": "paragraph", "content": [{ "type": "text", "text": "Confirm owners." }] },
                    ],
                },
                "assignee": { "accountId": "559:account" },
                "duedate": "2028-02-29",
                "labels": [format!("ecochain-{OPERATION}")],
            },
            "properties": [meeting_property()],
        })
    );
    assert_eq!(operation_key(INSTANCE, TASK_ID), OPERATION);
    assert_ne!(operation_key("ab", "c"), operation_key("a", "bc"));
}

#[test]
fn payload_explicitly_unassigns_unmapped_identity() {
    let config = config("https://example.atlassian.net");
    let preview = JiraPreview {
        assignee_identity: Some("559:account".into()),
        due_date: None,
        ..preview()
    };
    let payload = build_payload(&config, INSTANCE, &preview).unwrap();
    assert_eq!(payload["fields"].get("assignee"), Some(&Value::Null));
    assert!(payload["fields"].get("duedate").is_none());
}

#[test]
fn payload_rejects_disallowed_project_and_invalid_required_fields() {
    let config = config("https://example.atlassian.net");
    for preview in [
        JiraPreview {
            project_key: "OTHER".into(),
            ..preview()
        },
        JiraPreview {
            summary: "é".repeat(256),
            ..preview()
        },
        JiraPreview {
            summary: "  ".into(),
            ..preview()
        },
        JiraPreview {
            due_date: Some("2027-02-29".into()),
            ..preview()
        },
    ] {
        assert!(build_payload(&config, INSTANCE, &preview).is_err());
    }
}

#[tokio::test]
async fn projects_paginates_and_filters_to_the_allowlist() {
    let server = MockJira::serve(Router::new().route(
        "/rest/api/3/project/search",
        get(
            |headers: HeaderMap, Query(query): Query<HashMap<String, String>>| async move {
                assert_basic_auth(&headers);
                let start_at: usize = query["startAt"].parse().unwrap();
                let value = match start_at {
                    0 => json!({
                        "startAt": 0, "maxResults": 2, "total": 3, "isLast": false,
                        "nextPage": "http://untrusted.invalid/rest/api/3/project/search?startAt=2",
                        "values": [{"key":"WORK","name":"Work"}, {"key":"OUTSIDE","name":"Other"}]
                    }),
                    2 => json!({
                        "startAt": 2, "maxResults": 2, "total": 3, "isLast": true,
                        "values": [{"key":"NEXT","name":"Next"}]
                    }),
                    _ => panic!("unexpected project page {start_at}"),
                };
                Json(value)
            },
        ),
    ))
    .await;
    let result = projects(&client().unwrap(), &server.config).await.unwrap();
    let mut result: Vec<_> = result.into_iter().map(|p| (p.key, p.name)).collect();
    result.sort();
    assert_eq!(
        result,
        [
            ("NEXT".into(), "Next".into()),
            ("WORK".into(), "Work".into())
        ]
    );
}

#[tokio::test]
async fn project_pagination_stops_when_provider_never_finishes() {
    let calls = Arc::new(AtomicUsize::new(0));
    let serve_calls = calls.clone();
    let server = MockJira::serve(Router::new().route(
        "/rest/api/3/project/search",
        get(move |Query(query): Query<HashMap<String, String>>| {
            let count = serve_calls.fetch_add(1, Ordering::SeqCst);
            async move {
                assert!(count < 20, "unbounded project pagination");
                Json(json!({
                    "startAt": query["startAt"].parse::<usize>().unwrap(), "maxResults": 1,
                    "total": 40, "isLast": false, "values": [{"key":"OTHER","name":"Other"}]
                }))
            }
        }),
    ))
    .await;
    assert!(projects(&client().unwrap(), &server.config).await.is_err());
    assert!((2..20).contains(&calls.load(Ordering::SeqCst)));
}

#[tokio::test]
async fn reconciliation_rejects_a_project_key_with_jql_injection_even_if_configured() {
    let key = "WORK\" OR project = \"OTHER";
    let calls = Arc::new(AtomicUsize::new(0));
    let serve_calls = calls.clone();
    let mut server = MockJira::serve(Router::new().route(
        "/rest/api/3/search/jql",
        post(move || async move {
            serve_calls.fetch_add(1, Ordering::SeqCst);
            Json(json!({ "issues": [], "isLast": true }))
        }),
    ))
    .await;
    server.config.project_keys.push(key.into());
    assert!(
        find_existing(&client().unwrap(), &server.config, key, INSTANCE, TASK_ID)
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn reconciliation_uses_exact_property_and_rejects_duplicate_verified_issues() {
    for duplicate in [false, true] {
        let server = MockJira::serve(
            Router::new()
                .route(
                    "/rest/api/3/search/jql",
                    post(|headers: HeaderMap, Json(payload): Json<Value>| async move {
                        assert_basic_auth(&headers);
                        assert_eq!(
                            payload["jql"],
                            format!("project = \"WORK\" AND labels = \"ecochain-{OPERATION}\"")
                        );
                        let page = if payload.get("nextPageToken").is_none() {
                            json!({
                                "issues": [{"id":"10001", "key":"WORK-1", "self":"https://untrusted.invalid"}],
                                "isLast": false, "nextPageToken": "second"
                            })
                        } else {
                            assert_eq!(payload["nextPageToken"], "second");
                            json!({ "issues": [{"id":"10002", "key":"WORK-2"}], "isLast": true })
                        };
                        Json(page)
                    }),
                )
                .route(
                    "/rest/api/3/issue/{key}/properties/ecochain_meeting",
                    get(move |headers: HeaderMap, Path(key): Path<String>| async move {
                        assert_basic_auth(&headers);
                        let mut property = meeting_property();
                        if key == "WORK-1" && !duplicate {
                            property["value"]["task_id"] = json!("other-task");
                        }
                        Json(property)
                    }),
                ),
        )
        .await;
        let result = find_existing(
            &client().unwrap(),
            &server.config,
            "WORK",
            INSTANCE,
            TASK_ID,
        )
        .await;
        if duplicate {
            assert!(result.is_err());
        } else {
            let issue = result.unwrap().unwrap();
            assert_eq!(issue.id, "10002");
            assert_eq!(issue.key, "WORK-2");
            assert_eq!(
                issue.url,
                format!("{}/browse/WORK-2", server.config.base_url)
            );
        }
    }
}

#[tokio::test]
async fn reconciliation_stops_when_provider_never_finishes() {
    let calls = Arc::new(AtomicUsize::new(0));
    let serve_calls = calls.clone();
    let server = MockJira::serve(Router::new().route(
        "/rest/api/3/search/jql",
        post(move || {
            let count = serve_calls.fetch_add(1, Ordering::SeqCst);
            async move {
                assert!(count < 20, "unbounded issue pagination");
                Json(json!({
                    "issues": [], "isLast": false, "nextPageToken": count.to_string(),
                }))
            }
        }),
    ))
    .await;
    assert!(find_existing(
        &client().unwrap(),
        &server.config,
        "WORK",
        INSTANCE,
        TASK_ID
    )
    .await
    .is_err());
    assert!((2..20).contains(&calls.load(Ordering::SeqCst)));
}

#[tokio::test]
async fn create_uses_basic_auth_and_constructs_browse_url_from_configured_origin() {
    let server = MockJira::serve(Router::new().route(
        "/rest/api/3/issue",
        post(
            |headers: HeaderMap, Json(payload): Json<Value>| async move {
                assert_basic_auth(&headers);
                assert_eq!(payload["properties"], json!([meeting_property()]));
                (
                    StatusCode::CREATED,
                    Json(json!({"id":"10001", "key":"WORK-1", "self":"https://untrusted.invalid"})),
                )
            },
        ),
    ))
    .await;
    let payload = build_payload(&server.config, INSTANCE, &preview()).unwrap();
    let issue = create_issue(&client().unwrap(), &server.config, &payload)
        .await
        .unwrap();
    assert_eq!(issue.id, "10001");
    assert_eq!(issue.key, "WORK-1");
    assert_eq!(
        issue.url,
        format!("{}/browse/WORK-1", server.config.base_url)
    );
}

#[tokio::test]
async fn create_classifies_failure_without_echoing_provider_data() {
    for (status, body, definitive) in [
        (StatusCode::BAD_REQUEST, TOKEN, true),
        (StatusCode::BAD_GATEWAY, TOKEN, false),
        (StatusCode::CREATED, TOKEN, false),
        (
            StatusCode::CREATED,
            r#"{"id":"10001","key":"WORK-1/../../bad"}"#,
            false,
        ),
        (
            StatusCode::CREATED,
            r#"{"id":"10001","key":"NEXT-1"}"#,
            false,
        ),
    ] {
        let server = MockJira::serve(Router::new().route(
            "/rest/api/3/issue",
            post(move || async move { (status, body) }),
        ))
        .await;
        let payload = build_payload(&server.config, INSTANCE, &preview()).unwrap();
        let error = create_issue(&client().unwrap(), &server.config, &payload)
            .await
            .unwrap_err();
        assert_eq!(matches!(error, JiraCreateError::Definitive(_)), definitive);
        assert_eq!(matches!(error, JiraCreateError::Uncertain(_)), !definitive);
        assert!(!format!("{error:?} {error}").contains(TOKEN));
        assert!(!format!("{error:?} {error}").contains(EMAIL));
    }
}

#[tokio::test]
async fn oversized_create_response_is_uncertain() {
    for length in [0, 2 * 1024 * 1024] {
        let server = MockJira::serve(Router::new().route(
            "/rest/api/3/issue",
            post(move || async move {
                (
                    StatusCode::CREATED,
                    Json(json!({"id":"10001", "key":"WORK-1", "unrelated": " ".repeat(length)})),
                )
            }),
        ))
        .await;
        let payload = build_payload(&server.config, INSTANCE, &preview()).unwrap();
        let result = create_issue(&client().unwrap(), &server.config, &payload).await;
        if length == 0 {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(JiraCreateError::Uncertain(_))));
        }
    }
}
