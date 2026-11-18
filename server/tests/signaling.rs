//! Signaling regressions: authentication, capacity, duplicates, resume, host
//! actions. Every test runs against a real in-process server.

mod common;

use ecochain_rtc_server::{auth::Role, protocol::ErrorCode};
use serde_json::json;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn rejects_invalid_and_foreign_tokens() {
    let server = common::start("").await;

    let mut client = server.signal().await;
    let error = client
        .request("join", json!({"token": "not-a-token"}))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::Unauthorized));
    assert_eq!(client.wait_closed(WAIT).await, Some(4003));

    // Correctly signed, but by somebody else's secret.
    let mut foreign = server.config.clone();
    foreign.auth.token_secret = ecochain_rtc_server::config::Secret::new("x".repeat(40));
    let token = common::token_with(&foreign, "room-a", "mallory", Role::Host, 3600);
    let mut client = server.signal().await;
    let error = client
        .request("join", json!({"token": token}))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::Unauthorized));

    // Valid token for room-a used to enter room-b.
    let token = server.token("room-a", "alice", Role::Participant);
    let mut client = server.signal().await;
    let error = client
        .request("join", json!({"token": token, "room": "room-b"}))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::Forbidden));

    let expired = common::token_with(&server.config, "room-a", "alice", Role::Participant, -600);
    let mut client = server.signal().await;
    let error = client
        .request("join", json!({"token": expired}))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::TokenExpired));

    // Requests before join are refused.
    let mut client = server.signal().await;
    let error = client.request("ping", json!({})).await.unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::NotJoined));

    assert_eq!(server.metric("ecochain_rtc_participants_active"), 0);
}

#[tokio::test]
async fn rejects_unsupported_protocol_version() {
    let server = common::start("").await;
    let mut client = server.signal().await;
    client
        .send_raw(r#"{"v":2,"type":"join","id":"1","data":{"token":"x"}}"#)
        .await
        .unwrap();
    let frame = client.next_frame(WAIT).await.unwrap();
    assert_eq!(frame["error"]["code"], "unsupported_version");
    assert_eq!(client.wait_closed(WAIT).await, Some(4000));
}

#[tokio::test]
async fn room_full_is_rejected() {
    let server = common::start("default_max_participants = 2").await;
    let mut clients = Vec::new();
    for name in ["a", "b"] {
        let mut client = server.signal().await;
        client
            .request(
                "join",
                json!({"token": server.token("full", name, Role::Participant)}),
            )
            .await
            .unwrap();
        clients.push(client);
    }
    let mut third = server.signal().await;
    let error = third
        .request(
            "join",
            json!({"token": server.token("full", "c", Role::Participant)}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::RoomFull));

    // A seat frees up after somebody leaves.
    clients[0].request("leave", json!({})).await.unwrap();
    let mut fourth = server.signal().await;
    fourth
        .request(
            "join",
            json!({"token": server.token("full", "c", Role::Participant)}),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn admin_api_sets_max_participants() {
    let server = common::start("auto_create = false").await;
    let client = reqwest_like_post(
        &server,
        "/v1/rooms",
        json!({"room_id": "board", "max_participants": 1}),
    )
    .await;
    assert_eq!(client, 201);

    let mut a = server.signal().await;
    a.request(
        "join",
        json!({"token": server.token("board", "a", Role::Host)}),
    )
    .await
    .unwrap();
    let mut b = server.signal().await;
    let error = b
        .request(
            "join",
            json!({"token": server.token("board", "b", Role::Participant)}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::RoomFull));

    let mut c = server.signal().await;
    let error = c
        .request(
            "join",
            json!({"token": server.token("missing", "c", Role::Participant)}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::RoomNotFound));
}

/// Minimal HTTP POST without an HTTP client dependency.
async fn reqwest_like_post(
    server: &common::TestServer,
    path: &str,
    body: serde_json::Value,
) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(server.server.http_addr)
        .await
        .unwrap();
    let body = body.to_string();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: test\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        common::ADMIN,
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response[9..12].parse().unwrap()
}

#[tokio::test]
async fn second_connection_of_same_identity_replaces_the_first() {
    let server = common::start("").await;
    let mut observer = server.signal().await;
    observer
        .request(
            "join",
            json!({"token": server.token("dup", "observer", Role::Participant)}),
        )
        .await
        .unwrap();

    let mut first = server.signal().await;
    let joined = first
        .request(
            "join",
            json!({"token": server.token("dup", "alice", Role::Participant)}),
        )
        .await
        .unwrap();
    let id = joined["participant"]["participant_id"].clone();
    let event = observer
        .wait_event("participant.joined", WAIT)
        .await
        .unwrap();
    assert_eq!(event["data"]["participant"]["participant_id"], id);

    // Same identity, no resume id (for example the app was reinstalled).
    let mut second = server.signal().await;
    let joined = second
        .request(
            "join",
            json!({"token": server.token("dup", "alice", Role::Participant)}),
        )
        .await
        .unwrap();
    assert_eq!(
        joined["participant"]["participant_id"], id,
        "same seat, same id"
    );
    assert_eq!(joined["resumed"], false);

    first.wait_event("session.replaced", WAIT).await.unwrap();
    assert_eq!(first.wait_closed(WAIT).await, Some(4001));

    // The observer never sees alice twice.
    let event = observer
        .wait_event("participant.updated", WAIT)
        .await
        .unwrap();
    assert_eq!(event["data"]["participant"]["participant_id"], id);
    assert!(observer
        .wait_event("participant.joined", Duration::from_millis(500))
        .await
        .is_err());
    assert_eq!(server.metric("ecochain_rtc_participants_active"), 2);
}

#[tokio::test]
async fn resume_keeps_the_session_and_grace_expiry_removes_it() {
    let server = common::start("reconnect_grace_secs = 2").await;
    let mut observer = server.signal().await;
    observer
        .request(
            "join",
            json!({"token": server.token("resume", "observer", Role::Participant)}),
        )
        .await
        .unwrap();

    let token = server.token("resume", "bob", Role::Participant);
    let mut bob = server.signal().await;
    let joined = bob.request("join", json!({"token": token})).await.unwrap();
    let session = joined["session_id"].as_str().unwrap().to_owned();
    let id = joined["participant"]["participant_id"].clone();
    observer
        .wait_event("participant.joined", WAIT)
        .await
        .unwrap();

    bob.drop_connection().await;
    let event = observer
        .wait_event("participant.updated", WAIT)
        .await
        .unwrap();
    assert_eq!(event["data"]["participant"]["connected"], false);

    let mut bob = server.signal().await;
    let joined = bob
        .request(
            "join",
            json!({"token": token, "resume_session_id": session}),
        )
        .await
        .unwrap();
    assert_eq!(joined["resumed"], true);
    assert_eq!(joined["participant"]["participant_id"], id);
    let event = observer
        .wait_event("participant.updated", WAIT)
        .await
        .unwrap();
    assert_eq!(event["data"]["participant"]["connected"], true);
    assert_eq!(server.metric("ecochain_rtc_resumes_total"), 1);

    // Now drop for longer than the grace period.
    bob.drop_connection().await;
    let event = observer
        .wait_event("participant.left", Duration::from_secs(6))
        .await
        .unwrap();
    assert_eq!(event["data"]["reason"], "timeout");
    assert_eq!(server.metric("ecochain_rtc_participants_active"), 1);
}

#[tokio::test]
async fn host_ends_meeting_and_late_joins_are_refused() {
    let server = common::start("").await;
    let mut guest = server.signal().await;
    guest
        .request(
            "join",
            json!({"token": server.token("end", "guest", Role::Participant)}),
        )
        .await
        .unwrap();
    let error = guest.request("room.end", json!({})).await.unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::Forbidden));

    let mut host = server.signal().await;
    host.request(
        "join",
        json!({"token": server.token("end", "host", Role::Host)}),
    )
    .await
    .unwrap();
    host.request("room.end", json!({})).await.unwrap();

    let event = guest.wait_event("room.ended", WAIT).await.unwrap();
    assert_eq!(event["data"]["reason"], "ended_by_host");
    assert_eq!(guest.wait_closed(WAIT).await, Some(4004));
    assert_eq!(host.wait_closed(WAIT).await, Some(4004));

    let mut late = server.signal().await;
    let error = late
        .request(
            "join",
            json!({"token": server.token("end", "guest", Role::Participant)}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::RoomEnded));
}

#[tokio::test]
async fn host_can_remove_participant_and_duplicate_requests_are_not_repeated() {
    let server = common::start("").await;
    let mut host = server.signal().await;
    host.request(
        "join",
        json!({"token": server.token("kick", "host", Role::Host)}),
    )
    .await
    .unwrap();
    let mut guest = server.signal().await;
    let joined = guest
        .request(
            "join",
            json!({"token": server.token("kick", "guest", Role::Participant)}),
        )
        .await
        .unwrap();
    let guest_id = joined["participant"]["participant_id"].clone();

    let data = json!({"participant_id": guest_id});
    host.request_with_id("kick-1", "participant.remove", data.clone())
        .await
        .unwrap();
    // A retry with the same request id gets the cached success instead of
    // "unknown participant".
    host.request_with_id("kick-1", "participant.remove", data)
        .await
        .unwrap();

    guest.wait_event("removed", WAIT).await.unwrap();
    assert_eq!(guest.wait_closed(WAIT).await, Some(4005));
    let left = host.wait_event("participant.left", WAIT).await.unwrap();
    assert_eq!(left["data"]["reason"], "removed");

    // The guest's token is still valid, but the room remembers the removal.
    let mut again = server.signal().await;
    let error = again
        .request(
            "join",
            json!({"token": server.token("kick", "guest", Role::Participant)}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(ErrorCode::Forbidden));
}

#[tokio::test]
async fn leaving_cleans_up_room_and_metrics() {
    let server = common::start("").await;
    let mut a = server.signal().await;
    a.request(
        "join",
        json!({"token": server.token("clean", "a", Role::Participant)}),
    )
    .await
    .unwrap();
    let mut b = server.signal().await;
    b.request(
        "join",
        json!({"token": server.token("clean", "b", Role::Participant)}),
    )
    .await
    .unwrap();
    assert_eq!(server.metric("ecochain_rtc_participants_active"), 2);
    assert_eq!(server.metric("ecochain_rtc_rooms_active"), 1);

    a.request("leave", json!({})).await.unwrap();
    assert_eq!(a.wait_closed(WAIT).await, Some(1000));
    let left = b.wait_event("participant.left", WAIT).await.unwrap();
    assert_eq!(left["data"]["reason"], "left");
    b.request("leave", json!({})).await.unwrap();

    // Empty room TTL is 1 s in tests.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(server.metric("ecochain_rtc_participants_active"), 0);
    assert_eq!(server.metric("ecochain_rtc_rooms_active"), 0);
    assert_eq!(server.metric("ecochain_rtc_connections_active"), 0);
}
