mod common;
use ecochain_rtc_server::{auth::Role, protocol::ErrorCode};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn lobby_has_no_media_or_workspace_before_host_admits() {
    let server = common::start("").await;
    let mut host = server.signal().await;
    host.request(
        "join",
        json!({"token":server.token("lobby","host",Role::Host)}),
    )
    .await
    .unwrap();
    host.request(
        "meeting.action",
        json!({"action":{"type":"lobby.configure","enabled":true}}),
    )
    .await
    .unwrap();
    let mut guest = server.signal().await;
    let waiting = guest
        .request(
            "join",
            json!({"token":server.token("lobby","guest",Role::Participant)}),
        )
        .await
        .unwrap();
    assert_eq!(waiting["waiting"], true);
    assert!(waiting.get("meeting").is_none());
    assert!(waiting.get("participants").is_none());
    assert!(waiting.get("archive_token").is_none());
    assert_eq!(
        guest
            .request("meeting.snapshot", json!({}))
            .await
            .unwrap_err()
            .code(),
        Some(ErrorCode::NotJoined)
    );
    assert_eq!(
        guest
            .request("publisher.offer", json!({"sdp":"forged","tracks":[]}))
            .await
            .unwrap_err()
            .code(),
        Some(ErrorCode::NotJoined)
    );
    let pending = host
        .request("meeting.action", json!({"action":{"type":"lobby.list"}}))
        .await
        .unwrap();
    let id = &pending["waiting"][0]["id"];
    host.request(
        "meeting.action",
        json!({"action":{"type":"lobby.admit","id":id}}),
    )
    .await
    .unwrap();
    let admitted = guest
        .wait_event("lobby.admitted", Duration::from_secs(3))
        .await
        .unwrap();
    assert!(admitted["data"]["participant"]["participant_id"].is_string());
    assert!(guest.request("meeting.snapshot", json!({})).await.is_ok());
}

#[tokio::test]
async fn locked_room_rejects_new_guests_but_allows_existing_session_resume() {
    let server = common::start("").await;
    let mut host = server.signal().await;
    host.request(
        "join",
        json!({"token":server.token("locked","host",Role::Host)}),
    )
    .await
    .unwrap();
    let mut member = server.signal().await;
    let joined = member
        .request(
            "join",
            json!({"token":server.token("locked","member",Role::Participant)}),
        )
        .await
        .unwrap();
    assert_eq!(
        member
            .request(
                "meeting.action",
                json!({"action":{"type":"room.lock","locked":true}})
            )
            .await
            .unwrap_err()
            .code(),
        Some(ErrorCode::Forbidden)
    );
    host.request(
        "meeting.action",
        json!({"action":{"type":"room.lock","locked":true}}),
    )
    .await
    .unwrap();
    let mut stranger = server.signal().await;
    assert_eq!(
        stranger
            .request(
                "join",
                json!({"token":server.token("locked","stranger",Role::Participant)})
            )
            .await
            .unwrap_err()
            .code(),
        Some(ErrorCode::Forbidden)
    );
    member.drop_connection().await;
    let mut resumed = server.signal().await;
    let join=resumed.request("join",json!({"token":server.token("locked","member",Role::Participant),"resume_session_id":joined["session_id"]})).await.unwrap();
    assert_eq!(join["resumed"], true);
}

#[tokio::test]
async fn host_mute_stops_legacy_sender_until_it_explicitly_unmutes() {
    use ecochain_rtc_server::bot::Bot;
    let server = common::start("").await;
    let mut host = server.signal().await;
    host.request(
        "join",
        json!({"token":server.token("muting","host",Role::Host)}),
    )
    .await
    .unwrap();
    let mut source = server.bot("muting", "source");
    source.video_kbps = None;
    let publisher = Bot::start(source).await.unwrap();
    let mut sink = server.bot("muting", "sink");
    sink.audio = false;
    sink.video_kbps = None;
    let receiver = Bot::start(sink).await.unwrap();
    assert!(
        receiver
            .wait_for(Duration::from_secs(5), |s| s.audio_frames > 10)
            .await
    );
    let id = publisher.stats().participant_id.unwrap();
    host.request(
        "meeting.action",
        json!({"action":{"type":"participant.mute","participant_id":id}}),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let stopped = receiver.stats().audio_frames;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(receiver.stats().audio_frames, stopped);
    publisher.set_audio_muted(false);
    assert!(
        receiver
            .wait_for(Duration::from_secs(3), |s| s.audio_frames > stopped + 5)
            .await
    );
    publisher.leave().await;
    receiver.leave().await;
}

#[tokio::test]
async fn presentation_handoff_is_an_offer_until_the_invitee_starts_capture() {
    let server = common::start("").await;
    let mut host = server.signal().await;
    let h = host
        .request(
            "join",
            json!({"token":server.token("handoff","host",Role::Host)}),
        )
        .await
        .unwrap();
    let mut guest = server.signal().await;
    let g = guest
        .request(
            "join",
            json!({"token":server.token("handoff","guest",Role::Participant)}),
        )
        .await
        .unwrap();
    host.request("meeting.action", json!({"action":{"type":"board.open"}}))
        .await
        .unwrap();
    assert_eq!(
        guest
            .request(
                "meeting.action",
                json!({"action":{"type":"presentation.begin"}})
            )
            .await
            .unwrap_err()
            .code(),
        Some(ErrorCode::Forbidden)
    );
    host.request("meeting.action",json!({"action":{"type":"presentation.offer","participant_id":g["participant"]["participant_id"]}})).await.unwrap();
    assert_eq!(
        guest.request("meeting.snapshot", json!({})).await.unwrap()["presentation"]["presenter_id"],
        h["participant"]["participant_id"]
    );
    guest
        .request(
            "meeting.action",
            json!({"action":{"type":"presentation.begin"}}),
        )
        .await
        .unwrap();
    let state = host.request("meeting.snapshot", json!({})).await.unwrap();
    assert_eq!(
        state["presentation"]["presenter_id"],
        g["participant"]["participant_id"]
    );
    assert!(state["presentation_offer"].is_null());
}

#[tokio::test]
async fn host_sharing_policy_requires_a_host_offer_and_can_close_an_invited_stage() {
    let server = common::start("").await;
    let mut host = server.signal().await;
    let h = host
        .request(
            "join",
            json!({"token":server.token("policy","host",Role::Host)}),
        )
        .await
        .unwrap();
    let mut guest = server.signal().await;
    let g = guest
        .request(
            "join",
            json!({"token":server.token("policy","guest",Role::Participant)}),
        )
        .await
        .unwrap();
    let action = json!({"action":{"type":"presentation.policy","mode":"host_or_invited"}});
    assert_eq!(
        guest
            .request("meeting.action", action.clone())
            .await
            .unwrap_err()
            .code(),
        Some(ErrorCode::Forbidden)
    );
    guest
        .request(
            "meeting.action",
            json!({"action":{"type":"presentation.begin"}}),
        )
        .await
        .unwrap();
    host.request("meeting.action", action).await.unwrap();
    assert!(
        host.request("meeting.snapshot", json!({})).await.unwrap()["presentation"].is_null(),
        "revocation must clear the public stage and stop the SDK capture"
    );
    for kind in ["presentation.begin", "board.open"] {
        assert_eq!(
            guest
                .request("meeting.action", json!({"action":{"type":kind}}))
                .await
                .unwrap_err()
                .code(),
            Some(ErrorCode::Forbidden)
        );
    }
    host.request("meeting.action",json!({"action":{"type":"presentation.offer","participant_id":g["participant"]["participant_id"]}})).await.unwrap();
    guest
        .request(
            "meeting.action",
            json!({"action":{"type":"presentation.begin"}}),
        )
        .await
        .unwrap();
    let state = guest.request("meeting.snapshot", json!({})).await.unwrap();
    assert_eq!(state["presentation"]["invited"], true);
    host.request(
        "meeting.action",
        json!({"action":{"type":"presentation.end"}}),
    )
    .await
    .unwrap();
    assert_eq!(
        guest
            .request(
                "meeting.action",
                json!({"action":{"type":"presentation.begin"}})
            )
            .await
            .unwrap_err()
            .code(),
        Some(ErrorCode::Forbidden)
    );
    host.request("meeting.action", json!({"action":{"type":"board.open"}}))
        .await
        .unwrap();
    let state = host.request("meeting.snapshot", json!({})).await.unwrap();
    assert_eq!(
        state["presentation"]["presenter_id"],
        h["participant"]["participant_id"]
    );
}
