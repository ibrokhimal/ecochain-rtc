mod common;

use axum::{
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use ecochain_rtc_server::{
    archive, auth,
    config::{self, Config},
    storage::Archive,
    Server,
};
use reqwest::{Client, Method};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

const TASK_ID: &str = "task-launch";
const JIRA_ISSUE_KEY: &str = "WORK-81";

struct LocalProvider {
    origin: String,
    task: JoinHandle<()>,
}

impl LocalProvider {
    async fn serve(router: Router) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { origin, task }
    }
}

impl Drop for LocalProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Meeting {
    server: Server,
    config: Config,
    client: Client,
    storage: PathBuf,
    instance: String,
    reader: String,
    manager: String,
}

impl Meeting {
    async fn start(provider: &LocalProvider) -> Self {
        let storage =
            std::env::temp_dir().join(format!("ecochain-provider-{}", uuid::Uuid::new_v4()));
        let origin = &provider.origin;
        let secret = common::SECRET;
        let admin = common::ADMIN;
        let config = config::parse(
            &format!(
                r#"
mode = "development"
[http]
bind = "127.0.0.1:0"
[auth]
issuer = "ecochain-test"
token_secret = "{secret}"
admin_key = "{admin}"
[media]
udp_port = 0
listen = [{{ bind_ip = "127.0.0.1" }}]
[storage]
path = "vault"
master_key_env = "ARCHIVE_KEY"
[providers.summary]
endpoint = "{origin}/v1/chat/completions"
api_key_env = "SUMMARY_KEY"
model = "mock-summary"
[providers.jira]
base_url = "{origin}"
email = "provider-test@example.com"
api_token_env = "JIRA_KEY"
project_keys = ["WORK"]
issue_type_id = "10003"
"#
            ),
            &storage,
            &|name| match name {
                "ARCHIVE_KEY" => Some(STANDARD.encode([19; 32])),
                "SUMMARY_KEY" | "JIRA_KEY" => Some(format!("mock-{name}")),
                _ => None,
            },
        )
        .unwrap();
        let server = ecochain_rtc_server::start(config.clone()).await.unwrap();
        let instance = uuid::Uuid::new_v4().to_string();
        server
            .app
            .vault
            .as_ref()
            .unwrap()
            .append(&Archive {
                state: json!({
                    "instance_id": instance,
                    "room_id": "completed-meeting",
                    "revision": 8,
                    "notes": [{"id": "note-1", "text": "Agree on the launch handoff."}],
                    "decisions": [],
                    "tasks": [{"id": TASK_ID, "text": "Prepare launch handoff.", "assignee": "bob"}],
                }),
                members: ["alice".to_owned(), "bob".to_owned()].into(),
                updated_at: auth::unix_now(),
            })
            .unwrap();
        let reader = archive::issue(&config, &instance, "bob", 3600);
        let manager = archive::issue_manage(&config, &instance, "alice", 3600);
        Self {
            server,
            config,
            client: Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            storage,
            instance,
            reader,
            manager,
        }
    }

    async fn restart(&mut self) {
        self.server.task.abort();
        self.server = ecochain_rtc_server::start(self.config.clone())
            .await
            .unwrap();
    }

    async fn request(
        &self,
        method: Method,
        suffix: &str,
        token: &str,
        payload: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = self
            .client
            .request(
                method,
                format!(
                    "http://{}/v1/archives/{}{suffix}",
                    self.server.http_addr, self.instance
                ),
            )
            .bearer_auth(token);
        if let Some(payload) = payload {
            request = request.json(&payload);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        (status, response.json().await.unwrap())
    }

    async fn post(&self, suffix: &str, payload: Value) -> (StatusCode, Value) {
        self.request(Method::POST, suffix, &self.manager, Some(payload))
            .await
    }

    async fn read(&self) -> Value {
        let (status, body) = self.request(Method::GET, "", &self.reader, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn preview(&self, summary: &str) -> Value {
        let (status, draft) = self.post("/jira/preview", jira_preview(summary)).await;
        assert_eq!(status, StatusCode::OK, "{draft}");
        draft
    }

    async fn submit(&self, preview: &Value, confirmed: bool) -> (StatusCode, Value) {
        self.post(
            &format!("/jira/{}/submit", preview["id"].as_str().unwrap()),
            json!({"confirmed": confirmed, "preview_hash": preview["preview_hash"]}),
        )
        .await
    }
}

impl Drop for Meeting {
    fn drop(&mut self) {
        self.server.task.abort();
        let _ = fs::remove_dir_all(&self.storage);
    }
}

fn jira_preview(summary: &str) -> Value {
    json!({
        "task_id": TASK_ID,
        "project_key": "WORK",
        "summary": summary,
        "description": "The release coordinator needs a handoff.",
        "assignee_identity": "bob",
        "due_date": "2028-02-29",
    })
}

fn summary_review(job: &Value, confirmed: bool) -> Value {
    json!({
        "confirmed": confirmed,
        "confirmation": {
            "job_id": job["id"],
            "expected_revision": 8,
            "draft": {
                "summary": "Host-reviewed launch plan",
                "decisions": ["Keep the handoff on Tuesday."],
                "tasks": [{
                    "text": "Send the revised handoff checklist.",
                    "assignee": "bob",
                    "due_date": "2028-02-29",
                }],
            }
        }
    })
}

fn saved_job<'a>(archive: &'a Value, id: &Value) -> Option<&'a Value> {
    archive["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| &job["id"] == id)
}

#[tokio::test]
async fn provider_routes_require_management_scope_and_instance_membership() {
    let hits = Arc::new(AtomicUsize::new(0));
    let project_hits = hits.clone();
    let provider = LocalProvider::serve(Router::new().route(
        "/rest/api/3/project/search",
        get(move || async move {
            project_hits.fetch_add(1, Ordering::SeqCst);
            Json(json!({"startAt": 0, "isLast": true, "values": [{"key": "WORK", "name": "Work"}]}))
        }),
    ))
    .await;
    let meeting = Meeting::start(&provider).await;
    assert_eq!(meeting.read().await["revision"], 8);
    let other_instance = uuid::Uuid::new_v4().to_string();
    let wrong_instance = archive::issue_manage(&meeting.config, &other_instance, "alice", 3600);
    let live_host = common::token_with(
        &meeting.config,
        &meeting.instance,
        "alice",
        auth::Role::Host,
        3600,
    );
    let outsider = archive::issue_manage(&meeting.config, &meeting.instance, "outsider", 3600);
    for (token, want) in [
        ("", StatusCode::UNAUTHORIZED),
        (meeting.reader.as_str(), StatusCode::UNAUTHORIZED),
        (wrong_instance.as_str(), StatusCode::UNAUTHORIZED),
        (live_host.as_str(), StatusCode::UNAUTHORIZED),
        (outsider.as_str(), StatusCode::FORBIDDEN),
    ] {
        let (status, body) = meeting
            .request(Method::GET, "/jira/projects", token, None)
            .await;
        assert_eq!(status, want, "{body}");
    }
    let unknown_job = json!({"id": uuid::Uuid::new_v4().to_string()});
    let submit = format!("/jira/{}/submit", unknown_job["id"].as_str().unwrap());
    for (path, body) in [
        ("/transcripts", json!({"recording_id": unknown_job["id"]})),
        ("/summaries", json!({})),
        ("/summaries/confirm", summary_review(&unknown_job, true)),
        ("/jira/preview", jira_preview("Try with archive read scope")),
        (
            submit.as_str(),
            json!({"confirmed": true, "preview_hash": "0".repeat(64)}),
        ),
    ] {
        let (status, response) = meeting
            .request(Method::POST, path, &meeting.reader, Some(body))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} {response}");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    assert_eq!(meeting.read().await["jobs"], json!([]));

    let (status, body) = meeting
        .request(Method::GET, "/jira/projects", &meeting.manager, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["projects"], json!([{"key":"WORK","name":"Work"}]));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn summary_generation_stays_draft_until_explicit_revision_bound_confirmation() {
    let provider = LocalProvider::serve(Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let draft = json!({
                "summary": "Model draft launch plan",
                "decisions": ["Proposed handoff."],
                "tasks": [{"text": "Proposed checklist.", "assignee": "bob", "due_date": null}],
            });
            Json(json!({
                "id": "mock-completion",
                "object": "chat.completion",
                "created": 1,
                "model": "mock-summary",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": draft.to_string(), "refusal": null},
                    "finish_reason": "stop",
                }],
            }))
        }),
    ))
    .await;
    let meeting = Meeting::start(&provider).await;
    let initial = meeting.read().await;
    let (status, queued) = meeting.post("/summaries", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    assert_eq!(queued["status"], "queued");
    let generated = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let archive = meeting.read().await;
            if saved_job(&archive, &queued["id"]).is_some_and(|job| job["status"] != "queued") {
                break archive;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .expect("summary did not finish");
    let draft = saved_job(&generated, &queued["id"]).unwrap();
    assert_eq!(draft["status"], "draft", "{draft}");
    assert_eq!(draft["draft"]["summary"], "Model draft launch plan");
    for field in ["notes", "decisions", "tasks", "revision"] {
        assert_eq!(generated[field], initial[field], "{field}");
    }

    let confirm = "/summaries/confirm";
    let (status, body) = meeting.post(confirm, summary_review(draft, false)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let review = summary_review(draft, true);
    let mut stale = review.clone();
    stale["confirmation"]["expected_revision"] = json!(7);
    let (status, body) = meeting.post(confirm, stale).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(meeting.read().await, generated);

    let (status, confirmed) = meeting.post(confirm, review.clone()).await;
    assert_eq!(status, StatusCode::OK, "{confirmed}");
    assert_eq!(confirmed["status"], "confirmed");
    let applied = meeting.read().await;
    assert_eq!(applied["revision"], 9);
    assert_eq!(applied["notes"].as_array().unwrap().len(), 2);
    assert_eq!(applied["notes"][1]["text"], "Host-reviewed launch plan");
    assert_eq!(applied["notes"][1]["author_id"], "alice");
    assert_eq!(applied["decisions"].as_array().unwrap().len(), 1);
    assert_eq!(
        applied["decisions"][0]["text"],
        "Keep the handoff on Tuesday."
    );
    assert_eq!(applied["tasks"].as_array().unwrap().len(), 2);
    assert_eq!(
        applied["tasks"][1]["text"],
        "Send the revised handoff checklist."
    );
    assert_eq!(applied["tasks"][1]["assignee"], "bob");
    assert_eq!(applied["tasks"][1]["due_date"], "2028-02-29");

    let (status, replay) = meeting.post(confirm, review.clone()).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay, confirmed);
    let mut different = review;
    different["confirmation"]["draft"]["summary"] = json!("Different reviewed summary");
    let (status, body) = meeting.post(confirm, different).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(meeting.read().await, applied);
}

#[tokio::test]
async fn jira_submit_requires_the_preview_version_the_host_reviewed() {
    let creates = Arc::new(AtomicUsize::new(0));
    let provider_creates = creates.clone();
    let provider = LocalProvider::serve(Router::new().route(
        "/rest/api/3/issue",
        post(move |Json(payload): Json<Value>| async move {
            provider_creates.fetch_add(1, Ordering::SeqCst);
            assert_eq!(payload["fields"]["summary"], "Updated launch handoff");
            (
                StatusCode::CREATED,
                Json(json!({"id": "10081", "key": JIRA_ISSUE_KEY})),
            )
        }),
    ))
    .await;
    let meeting = Meeting::start(&provider).await;
    let old = meeting.preview("Original launch handoff").await;
    let current = meeting.preview("Updated launch handoff").await;
    let (status, body) = meeting.submit(&old, true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, body) = meeting.submit(&current, false).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(creates.load(Ordering::SeqCst), 0);

    let (status, created) = meeting.submit(&current, true).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["status"], "created");
    assert_eq!(created["issue"]["key"], JIRA_ISSUE_KEY);
    assert_eq!(creates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn concurrent_jira_confirms_keep_one_durable_result_and_one_external_create() {
    let creates = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider_creates = creates.clone();
    let provider_entered = entered.clone();
    let provider_release = release.clone();
    let provider = LocalProvider::serve(Router::new().route(
        "/rest/api/3/issue",
        post(move |Json(payload): Json<Value>| async move {
            provider_creates.fetch_add(1, Ordering::SeqCst);
            assert_eq!(payload["fields"]["summary"], "Approved launch handoff");
            provider_entered.notify_one();
            provider_release.notified().await;
            (
                StatusCode::CREATED,
                Json(json!({"id": "10081", "key": JIRA_ISSUE_KEY})),
            )
        }),
    ))
    .await;
    let meeting = Meeting::start(&provider).await;
    let preview = meeting.preview("Approved launch handoff").await;
    assert_eq!(preview["status"], "draft");
    let fail_unless_pending = async {
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        let archive = meeting.read().await;
        let pending = saved_job(&archive, &preview["id"]).unwrap();
        assert_eq!(pending["status"], "pending");
        release.notify_one();
    };
    let (first, concurrent, ()) = tokio::join!(
        meeting.submit(&preview, true),
        meeting.submit(&preview, true),
        fail_unless_pending,
    );
    for (status, body) in [&first, &concurrent] {
        assert_eq!(*status, StatusCode::OK, "{body}");
        assert_eq!(body["status"], "created");
        assert_eq!(body["issue"]["key"], JIRA_ISSUE_KEY);
    }
    assert_eq!(first, concurrent);
    assert_eq!(meeting.submit(&preview, true).await, first);
    let archived = meeting.read().await;
    assert_eq!(saved_job(&archived, &preview["id"]), Some(&first.1));
    assert_eq!(creates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn uncertain_jira_create_survives_restart_and_later_reconciles_without_another_post() {
    let creates = Arc::new(AtomicUsize::new(0));
    let searches = Arc::new(AtomicUsize::new(0));
    let indexed = Arc::new(AtomicBool::new(false));
    let property = Arc::new(Mutex::new(None::<Value>));
    let create_calls = creates.clone();
    let search_calls = searches.clone();
    let search_indexed = indexed.clone();
    let create_property = property.clone();
    let provider = LocalProvider::serve(
        Router::new()
            .route(
                "/rest/api/3/issue",
                post(move |Json(payload): Json<Value>| async move {
                    create_calls.fetch_add(1, Ordering::SeqCst);
                    *create_property.lock().unwrap() = payload["properties"].get(0).cloned();
                    StatusCode::BAD_GATEWAY
                }),
            )
            .route(
                "/rest/api/3/search/jql",
                post(move || async move {
                    search_calls.fetch_add(1, Ordering::SeqCst);
                    let issues = if search_indexed.load(Ordering::SeqCst) {
                        json!([{"id": "10081", "key": JIRA_ISSUE_KEY}])
                    } else {
                        json!([])
                    };
                    Json(json!({"isLast": true, "issues": issues}))
                }),
            )
            .route(
                "/rest/api/3/issue/WORK-81/properties/ecochain_meeting",
                get(move || async move { Json(property.lock().unwrap().clone().unwrap()) }),
            ),
    )
    .await;
    let mut meeting = Meeting::start(&provider).await;
    let preview = meeting.preview("Approved launch handoff").await;
    let (status, uncertain) = meeting.submit(&preview, true).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{uncertain}");
    assert_eq!(uncertain["status"], "uncertain");
    assert_eq!(creates.load(Ordering::SeqCst), 1);

    meeting.restart().await;
    let refreshed = meeting.preview("Attempted replacement").await;
    assert_eq!(refreshed, uncertain);
    let (status, still_uncertain) = meeting.submit(&preview, true).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{still_uncertain}");
    assert_eq!(still_uncertain["status"], "uncertain");
    assert_eq!(creates.load(Ordering::SeqCst), 1);

    indexed.store(true, Ordering::SeqCst);
    let (status, created) = meeting.submit(&preview, true).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["status"], "created");
    assert!(created["error"].is_null(), "{created}");
    assert_eq!(created["issue"]["key"], JIRA_ISSUE_KEY);
    assert_eq!(
        meeting.submit(&preview, true).await,
        (StatusCode::OK, created)
    );
    assert_eq!(creates.load(Ordering::SeqCst), 1);
    assert_eq!(searches.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn corrupted_jira_outbox_is_never_replaced_with_a_new_createable_draft() {
    let provider = LocalProvider::serve(Router::new()).await;
    let meeting = Meeting::start(&provider).await;
    let preview = meeting.preview("Previously reviewed").await;
    let id = preview["id"].as_str().unwrap();
    let path = meeting
        .config
        .storage
        .as_ref()
        .unwrap()
        .path
        .join(&meeting.instance)
        .join(format!("{id}.job"));
    let mut damaged = fs::read(&path).unwrap();
    let last = damaged.len() - 1;
    damaged[last] ^= 1;
    fs::write(&path, &damaged).unwrap();
    let (status,_)=meeting.post("/jira/preview",json!({"task_id":TASK_ID,"project_key":"WORK","summary":"New title","description":"Reviewed","assignee_identity":null,"due_date":null})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(fs::read(path).unwrap(), damaged);
}

#[tokio::test]
async fn summary_quota_failure_saves_a_terminal_status_instead_of_staying_queued() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let e = entered.clone();
    let r = release.clone();
    let provider = LocalProvider::serve(Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let e = e.clone();
            let r = r.clone();
            async move {
                e.notify_one();
                r.notified().await;
                let draft = json!({"summary":"s".repeat(16000),"decisions":[],"tasks":[]});
                Json(json!({"choices":[{"message":{"content":draft.to_string()}}]}))
            }
        }),
    ))
    .await;
    let meeting = Meeting::start(&provider).await;
    let (status, job) = meeting.post("/summaries", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let vault = meeting.server.app.vault.as_ref().unwrap();
    for _ in 0..4 {
        let id = uuid::Uuid::new_v4().to_string();
        vault
            .write_job(
                &meeting.instance,
                &id,
                &json!({"id":id,"status":"ready","blob":"x".repeat(3*1024*1024)}),
            )
            .unwrap();
    }
    let dir = meeting
        .config
        .storage
        .as_ref()
        .unwrap()
        .path
        .join(&meeting.instance);
    let used: u64 = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let e = e.unwrap();
            (e.path().extension().is_some_and(|x| x == "job"))
                .then(|| e.metadata().unwrap().len().max(1024))
        })
        .sum();
    let remaining = 16 * 1024 * 1024 - used as usize;
    let id = uuid::Uuid::new_v4().to_string();
    vault
        .write_job(
            &meeting.instance,
            &id,
            &json!({"id":id,"status":"ready","blob":"x".repeat(remaining-512)}),
        )
        .unwrap();
    release.notify_one();
    let mut terminal = Value::Null;
    for _ in 0..100 {
        terminal = vault
            .read_job(&meeting.instance, job["id"].as_str().unwrap())
            .unwrap();
        if terminal["status"] == "failed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        terminal["status"], "failed",
        "terminal storage quota must be visible"
    );
    assert_eq!(terminal["error"], "summary_storage_limit");
}
