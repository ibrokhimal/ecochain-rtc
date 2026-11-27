//! Collaboration is authorized by the room actor, not by the drawing UI.
mod common;
use ecochain_rtc_server::{auth::Role, protocol::ErrorCode};
use serde_json::json;

// State-oriented tests read snapshots explicitly; mutation replies themselves are
// compact deltas, exercised independently below and by the SDK merge tests.
struct WorkspaceClient(ecochain_rtc_server::bot::SignalClient);
impl WorkspaceClient {
    async fn request(
        &mut self,
        kind: &str,
        data: serde_json::Value,
    ) -> Result<serde_json::Value, ecochain_rtc_server::bot::BotError> {
        let result = self.0.request(kind, data).await?;
        if result.get("patch").is_some() {
            self.0.request("meeting.snapshot", json!({})).await
        } else {
            Ok(result)
        }
    }
    async fn request_with_id(
        &mut self,
        id: &str,
        kind: &str,
        data: serde_json::Value,
    ) -> Result<serde_json::Value, ecochain_rtc_server::bot::BotError> {
        self.0.request_with_id(id, kind, data).await
    }
    async fn drop_connection(self) {
        self.0.drop_connection().await;
    }
}

#[tokio::test]
async fn shared_snapshot_permissions_conflicts_and_late_join() {
    let server = common::start("").await;
    let mut host = WorkspaceClient(server.signal().await);
    let h = host
        .request(
            "join",
            json!({"token":server.token("work", "host", Role::Host)}),
        )
        .await
        .unwrap();
    assert!(h["capabilities"]
        .as_array()
        .unwrap()
        .contains(&json!("work_tools_v1")));
    let mut guest = WorkspaceClient(server.signal().await);
    guest
        .request(
            "join",
            json!({"token":server.token("work", "guest", Role::Participant)}),
        )
        .await
        .unwrap();
    let denied = guest
        .request(
            "meeting.action",
            json!({"action":{"type":"drawing.policy","mode":"everyone"}}),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code(), Some(ErrorCode::Forbidden));
    let state = host
        .request("meeting.action", json!({"action":{"type":"board.open"}}))
        .await
        .unwrap();
    let presentation = state["presentation"]["id"].as_str().unwrap().to_owned();
    let stroke = json!({"type":"object.add","presentation_id":presentation,"object":{"id":"stroke-1","tool":"pen","color":4294901760u64,"width":0.004,"points":[[0.1,0.2],[0.6,0.7]]}});
    let denied = guest
        .request("meeting.action", json!({"action":stroke}))
        .await
        .unwrap_err();
    assert_eq!(denied.code(), Some(ErrorCode::Forbidden));
    host.request(
        "meeting.action",
        json!({"action":{"type":"drawing.policy","mode":"everyone"}}),
    )
    .await
    .unwrap();
    let added = guest
        .request("meeting.action", json!({"action":stroke}))
        .await
        .unwrap();
    let author = added["objects"][0]["author_id"].clone();
    assert_ne!(author, h["participant"]["participant_id"]);
    let mut bad = stroke.clone();
    bad["object"]["id"] = json!("bad");
    bad["object"]["points"] = json!([[1.1, 0.0]]);
    assert_eq!(
        guest
            .request("meeting.action", json!({"action":bad}))
            .await
            .unwrap_err()
            .code(),
        Some(ErrorCode::BadRequest)
    );
    assert_eq!(
        guest
            .request(
                "meeting.action",
                json!({"action":{"type":"object.remove","id":"stroke-1","expected_revision":99}})
            )
            .await
            .unwrap_err()
            .code(),
        Some(ErrorCode::InvalidState)
    );
    let undone = guest
        .request(
            "meeting.action",
            json!({"action":{"type":"object.remove","id":"stroke-1","expected_revision":1}}),
        )
        .await
        .unwrap();
    assert_eq!(undone["objects"][0]["deleted"], true);
    let mut late = WorkspaceClient(server.signal().await);
    let joined = late
        .request(
            "join",
            json!({"token":server.token("work", "late", Role::Participant)}),
        )
        .await
        .unwrap();
    assert_eq!(joined["meeting"], undone);
    let snapshot = late.request("meeting.snapshot", json!({})).await.unwrap();
    assert_eq!(snapshot, undone);
}

#[tokio::test]
async fn workspace_votes_and_note_conflicts_are_authoritative() {
    let server = common::start("").await;
    let mut host = WorkspaceClient(server.signal().await);
    host.request(
        "join",
        json!({"token":server.token("office", "host", Role::Host)}),
    )
    .await
    .unwrap();
    let mut guest = WorkspaceClient(server.signal().await);
    guest
        .request(
            "join",
            json!({"token":server.token("office", "guest", Role::Participant)}),
        )
        .await
        .unwrap();
    host.request("meeting.action", json!({"action":{"type":"poll.create","id":"p1","question":"Release?","options":["Yes","No"]}})).await.unwrap();
    for _ in 0..2 {
        guest
            .request(
                "meeting.action",
                json!({"action":{"type":"poll.vote","id":"p1","option":0}}),
            )
            .await
            .unwrap();
    }
    let s = host.request("meeting.snapshot", json!({})).await.unwrap();
    assert_eq!(s["polls"][0]["votes"].as_object().unwrap().len(), 1);
    guest
        .request(
            "meeting.action",
            json!({"action":{"type":"note.put","id":"n1","text":"Decision","expected_revision":0}}),
        )
        .await
        .unwrap();
    assert_eq!(
        host.request(
            "meeting.action",
            json!({"action":{"type":"note.put","id":"n1","text":"Overwrite","expected_revision":0}})
        )
        .await
        .unwrap_err()
        .code(),
        Some(ErrorCode::InvalidState)
    );
}

#[tokio::test]
async fn dedup_resume_and_revocation_do_not_repeat_or_allow_stale_actions() {
    let server = common::start("").await;
    let mut host = WorkspaceClient(server.signal().await);
    host.request(
        "join",
        json!({"token":server.token("dedup","host",Role::Host)}),
    )
    .await
    .unwrap();
    let mut guest = WorkspaceClient(server.signal().await);
    let joined = guest
        .request(
            "join",
            json!({"token":server.token("dedup","guest",Role::Participant)}),
        )
        .await
        .unwrap();
    let board = host
        .request("meeting.action", json!({"action":{"type":"board.open"}}))
        .await
        .unwrap();
    host.request(
        "meeting.action",
        json!({"action":{"type":"drawing.policy","mode":"everyone"}}),
    )
    .await
    .unwrap();
    let a = json!({"action":{"type":"object.add","presentation_id":board["presentation"]["id"],"object":{"id":"once","tool":"pen","width":0.004,"color":0xfff00000u64,"points":[[0.1,0.2]],"author_id":"forged"}}});
    let once = guest
        .request_with_id("same-op", "meeting.action", a.clone())
        .await
        .unwrap();
    let twice = guest
        .request_with_id("same-op", "meeting.action", a.clone())
        .await
        .unwrap();
    assert_eq!(once, twice);
    assert_eq!(
        once["patch"]["collections"]["objects"]["upsert"][0]["author_id"],
        joined["participant"]["participant_id"]
    );
    assert_eq!(guest.request("meeting.action",json!({"action":{"type":"object.clear","presentation_id":board["presentation"]["id"]}})).await.unwrap_err().code(),Some(ErrorCode::Forbidden));
    let canonical = guest.request("meeting.snapshot", json!({})).await.unwrap();
    guest.drop_connection().await;
    let mut resumed = WorkspaceClient(server.signal().await);
    let resume=resumed.request("join",json!({"token":server.token("dedup","guest",Role::Participant),"resume_session_id":joined["session_id"]})).await.unwrap();
    assert_eq!(resume["meeting"], canonical);
    assert_eq!(
        resumed
            .request_with_id("same-op", "meeting.action", a)
            .await
            .unwrap(),
        once
    );
    host.request(
        "meeting.action",
        json!({"action":{"type":"drawing.policy","mode":"presenter"}}),
    )
    .await
    .unwrap();
    assert_eq!(
        resumed
            .request(
                "meeting.action",
                json!({"action":{"type":"object.remove","id":"once","expected_revision":1}})
            )
            .await
            .unwrap_err()
            .code(),
        Some(ErrorCode::Forbidden)
    );
}

#[tokio::test]
async fn pointer_is_ephemeral_and_paused_board_rejects_drawing() {
    let server = common::start("").await;
    let mut host = WorkspaceClient(server.signal().await);
    host.request(
        "join",
        json!({"token":server.token("pointer","host",Role::Host)}),
    )
    .await
    .unwrap();
    let board = host
        .request("meeting.action", json!({"action":{"type":"board.open"}}))
        .await
        .unwrap();
    let point = json!({"action":{"type":"pointer","presentation_id":board["presentation"]["id"],"point":[0.4,0.6],"laser":true}});
    host.request("meeting.action", point.clone()).await.unwrap();
    assert_eq!(
        host.request("meeting.snapshot", json!({})).await.unwrap(),
        board
    );
    host.request(
        "meeting.action",
        json!({"action":{"type":"presentation.pause","paused":true}}),
    )
    .await
    .unwrap();
    assert_eq!(
        host.request("meeting.action", point)
            .await
            .unwrap_err()
            .code(),
        Some(ErrorCode::Forbidden)
    );
}

#[tokio::test]
async fn workspace_bounds_and_task_assignees_are_validated() {
    let server = common::start("").await;
    let mut host = WorkspaceClient(server.signal().await);
    host.request(
        "join",
        json!({"token":server.token("validation","host",Role::Host)}),
    )
    .await
    .unwrap();
    let invalid = json!({"action":{"type":"task.put","id":"task","text":"Ship","assignee":"outside-room","due_date":"2026-02-31","expected_revision":0}});
    assert_eq!(
        host.request("meeting.action", invalid)
            .await
            .unwrap_err()
            .code(),
        Some(ErrorCode::BadRequest)
    );
    let large = json!({"action":{"type":"note.put","id":"large","text":"x".repeat(33000),"expected_revision":0}});
    assert_eq!(
        host.request("meeting.action", large)
            .await
            .unwrap_err()
            .code(),
        Some(ErrorCode::LimitExceeded)
    );
    assert_eq!(
        host.request("meeting.snapshot", json!({})).await.unwrap()["revision"],
        0
    );
}

#[tokio::test]
async fn durable_ack_is_readable_and_disk_failure_does_not_commit_workspace() {
    use base64::Engine;
    use ecochain_rtc_server::config::{Secret, StorageConfig};
    let baseline = common::start("").await;
    let mut config = baseline.config.clone();
    let root = std::env::temp_dir().join(format!("ecochain-ack-{}", uuid::Uuid::new_v4()));
    config.storage = Some(StorageConfig {
        path: root.clone(),
        master_key: Secret::new(base64::engine::general_purpose::STANDARD.encode([7; 32])),
        retention_secs: 86400,
    });
    let server = ecochain_rtc_server::start(config.clone()).await.unwrap();
    let client = ecochain_rtc_server::bot::SignalClient::connect(&format!(
        "ws://{}/v1/rtc",
        server.http_addr
    ))
    .await
    .unwrap();
    let mut client = WorkspaceClient(client);
    let joined = client
        .request(
            "join",
            json!({"token":common::token_with(&config,"durable","host",Role::Host,3600)}),
        )
        .await
        .unwrap();
    let saved=client.request("meeting.action",json!({"action":{"type":"note.put","id":"note","text":"Committed","expected_revision":0}})).await.unwrap();
    let instance = joined["meeting"]["instance_id"].as_str().unwrap();
    assert_eq!(
        server
            .app
            .vault
            .as_ref()
            .unwrap()
            .read(instance)
            .unwrap()
            .state,
        saved
    );
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::write(&root, b"not a directory").unwrap();
    let error=client.request("meeting.action",json!({"action":{"type":"note.put","id":"note","text":"Must not commit","expected_revision":1}})).await.unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::Unavailable));
    assert_eq!(
        client.request("meeting.snapshot", json!({})).await.unwrap(),
        saved
    );
    std::fs::remove_file(root).unwrap();
}

#[tokio::test]
async fn mutation_reply_is_a_bounded_patch_not_the_entire_workspace_history() {
    let server = common::start("").await;
    let mut host = server.signal().await;
    host.request(
        "join",
        json!({"token":server.token("patch","host",Role::Host)}),
    )
    .await
    .unwrap();
    host.request("meeting.action",json!({"action":{"type":"note.put","id":"first","text":"x".repeat(7000),"expected_revision":0}})).await.unwrap();
    let delta = host
        .request(
            "meeting.action",
            json!({"action":{"type":"note.put","id":"second","text":"new","expected_revision":0}}),
        )
        .await
        .unwrap();
    assert!(delta.to_string().len() < 1000);
    assert_eq!(
        delta["patch"]["collections"]["notes"]["upsert"][0]["id"],
        "second"
    );
}
