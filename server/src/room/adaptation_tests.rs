//! Room-level adaptation fixtures; never compiled into the production server.
use super::participant::{OutState, OutTrack, Participant, PublishedTrack, Subscriber};
use super::*;
use crate::{
    auth::Role,
    protocol::{Quality, TrackSource},
    sfu::{
        layers::Allocation,
        peer::{Peer, PeerKind},
    },
};
use std::{path::Path, sync::Arc};
use str0m::media::{MediaKind, Mid, Rid};

async fn fixture() -> Room {
    crate::install_crypto();
    let config = Arc::new(
        crate::config::parse(
            r#"
mode = "development"
[http]
bind = "127.0.0.1:0"
[auth]
issuer = "test"
token_secret = "test-secret-test-secret-test-secret"
admin_key = "test-admin-key-test"
[rooms]
auto_create = true
[media]
udp_port = 0
listen = [{ bind_ip = "127.0.0.1" }]
"#,
            Path::new("."),
            &|_| None,
        )
        .unwrap(),
    );
    let metrics = Arc::new(Metrics::default());
    let sockets = MediaSockets::bind(
        &[("127.0.0.1:0".parse().unwrap(), None, false)],
        metrics.clone(),
    )
    .await
    .unwrap();
    let (packets_tx, _) = mpsc::channel(100);
    Room {
        test_estimates: HashMap::new(),
        id: "adaptation".into(),
        max_participants: 25,
        services: RoomServices {
            config,
            metrics,
            sockets,
            rooms: Weak::new(),
        },
        packets_tx,
        participants: vec![],
        routes: HashMap::new(),
        forwarding: HashMap::new(),
        slots: HashMap::new(),
        forwarding_dirty: false,
        empty_since: None,
        empty_ttl_override: None,
        audio: AudioSelector::new(3),
        audio_generation: 0,
        speakers: vec![],
        last_speakers_sent: None,
        speakers_due: None,
        keyframes_due: None,
        ended: false,
    }
}

fn participant(id: &str) -> Participant {
    let claims = Claims {
        iss: "test".into(),
        sub: id.into(),
        room: "adaptation".into(),
        name: None,
        role: Role::Participant,
        can_publish: true,
        exp: u64::MAX,
        iat: 0,
        nbf: None,
    };
    let (tx, _) = mpsc::channel(100);
    Participant::new(id.into(), &claims, id.into(), Connection { id: 1, tx })
}

fn track(id: &str, source: TrackSource, muted: bool) -> PublishedTrack {
    PublishedTrack {
        id: id.into(),
        mid: Mid::from(id),
        kind: if source.is_video() {
            MediaKind::Video
        } else {
            MediaKind::Audio
        },
        source,
        muted,
        layers: if source == TrackSource::Camera {
            vec![Rid::from("q"), Rid::from("h"), Rid::from("f")]
        } else {
            vec![]
        },
        keyframes: HashMap::new(),
        orientation: None,
    }
}

fn out(publisher: &str, t: &PublishedTrack, quality: Quality) -> OutTrack {
    OutTrack {
        track_id: t.id.clone(),
        publisher_id: publisher.into(),
        kind: t.kind,
        source: t.source,
        state: OutState::ToOpen,
        quality,
        video: Default::default(),
        frames_written: 0,
        timeline: Default::default(),
    }
}

fn add_receiver(room: &mut Room) {
    let mut receiver = participant("receiver");
    let peer = Peer::new(
        PeerKind::Subscriber,
        &room.services.sockets,
        &room.packets_tx,
        Instant::now(),
    );
    let mut sub = Subscriber::new(peer);
    for publisher in &room.participants {
        for t in &publisher.tracks {
            if t.kind == MediaKind::Video {
                receiver
                    .video_subscriptions
                    .insert(t.id.clone(), Quality::Medium);
            }
            sub.out.push(out(&publisher.id, t, Quality::Medium));
        }
    }
    receiver.subscriber = Some(sub);
    room.participants.push(receiver);
}

#[tokio::test]
async fn audio_budget_excludes_muted_own_and_unsubscribed_tracks() {
    let mut room = fixture().await;
    let mut publisher = participant("publisher");
    publisher.tracks = vec![
        track("mic", TrackSource::Microphone, false),
        track("muted", TrackSource::Microphone, true),
        track("screen-a", TrackSource::ScreenAudio, false),
        track("unsub", TrackSource::Microphone, false),
    ];
    room.participants.push(publisher);
    add_receiver(&mut room);
    room.participants[1]
        .tracks
        .push(track("own", TrackSource::Microphone, false));
    room.participants[1].muted_audio.insert("unsub".into());
    assert_eq!(room.audio_demand_counts(1), (1, 1));
    room.mute(0, TrackSource::Microphone, true).unwrap();
    assert_eq!(room.audio_demand_counts(1), (0, 1));
    room.participants[0]
        .tracks
        .retain(|t| t.source != TrackSource::ScreenAudio);
    assert_eq!(room.audio_demand_counts(1), (0, 0));
}

#[tokio::test]
async fn room_reserves_audio_and_updates_speaker_priority() {
    let mut room = fixture().await;
    for id in ["a", "b"] {
        let mut publisher = participant(id);
        publisher.tracks = vec![
            track(&format!("{id}v"), TrackSource::Camera, false),
            track(&format!("{id}a"), TrackSource::Microphone, false),
        ];
        room.participants.push(publisher);
    }
    add_receiver(&mut room);
    room.participants[2]
        .subscriber
        .as_mut()
        .unwrap()
        .estimate_kbps = Some(400); // 340 - 128 = 212, one base fits.
    room.audio
        .observe("ba", Some(-10), true, false, Instant::now());
    room.allocate_layers(2);
    let videos: Vec<_> = room.participants[2]
        .subscriber
        .as_ref()
        .unwrap()
        .out
        .iter()
        .filter(|o| o.kind == MediaKind::Video)
        .map(|o| o.video.allocation)
        .collect();
    assert_eq!(videos, vec![Allocation::Paused, Allocation::Layer(0)]);
    room.mute(1, TrackSource::Camera, true).unwrap();
    assert_eq!(
        room.participants[2]
            .subscriber
            .as_ref()
            .unwrap()
            .out
            .iter()
            .find(|o| o.track_id == "bv")
            .unwrap()
            .video
            .allocation,
        Allocation::Paused
    );
}

async fn control(server: &crate::Server, id: &str, estimate: Option<Option<u64>>) -> Value {
    let (reply, response) = oneshot::channel();
    assert!(
        server
            .app
            .rooms
            .get("adaptive-media")
            .unwrap()
            .send(RoomCommand::TestAdaptation {
                participant_id: id.into(),
                estimate,
                reply,
            })
            .await
    );
    response.await.unwrap()
}

fn bot_options(
    server: &crate::Server,
    id: &str,
    publish: bool,
    simulcast: bool,
) -> crate::bot::BotOptions {
    let now = crate::auth::unix_now();
    let claims = Claims {
        iss: "test".into(),
        sub: id.into(),
        room: "adaptive-media".into(),
        name: None,
        role: Role::Participant,
        can_publish: true,
        exp: now + 3600,
        iat: now,
        nbf: None,
    };
    crate::bot::BotOptions {
        url: format!("ws://{}/v1/rtc", server.http_addr),
        token: crate::auth::issue(&claims, &server.app.config.auth.token_secret),
        bind_ip: "127.0.0.1".parse().unwrap(),
        audio: publish,
        video_kbps: publish.then_some(300),
        simulcast,
        max_video: 4,
        audio_level: -25,
        audio_muted: false,
        high_video: usize::MAX,
    }
}

fn layer_frames(bot: &crate::bot::Bot, publisher: &str, layer: usize) -> u64 {
    bot.stats()
        .video_frames_by_layer
        .get(&(publisher.into(), layer))
        .copied()
        .unwrap_or(0)
}

#[tokio::test]
async fn real_media_uses_three_receiver_budgets_and_resumes_independently() {
    use crate::bot::Bot;
    let fixture = fixture().await;
    let server = crate::start((*fixture.services.config).clone())
        .await
        .unwrap();
    let publisher = Bot::start(bot_options(&server, "publisher", true, true))
        .await
        .unwrap();
    let publisher_id = publisher.stats().participant_id.unwrap();
    let mut receivers = vec![];
    for (i, budget) in [300, 800, 2500].into_iter().enumerate() {
        let bot = Bot::start(bot_options(&server, &format!("r{i}"), false, false))
            .await
            .unwrap();
        control(
            &server,
            &bot.stats().participant_id.unwrap(),
            Some(Some(budget)),
        )
        .await;
        receivers.push(bot);
    }
    for (layer, bot) in receivers.iter().enumerate() {
        assert!(
            bot.wait_for(Duration::from_secs(12), |s| s
                .video_frames_by_layer
                .get(&(publisher_id.clone(), layer))
                .is_some_and(|n| *n > 25))
                .await,
            "layer {layer}: {:?}",
            bot.stats()
        );
    }
    let before: Vec<_> = receivers.iter().map(|b| b.stats()).collect();
    tokio::time::sleep(Duration::from_millis(800)).await;
    for (layer, bot) in receivers.iter().enumerate() {
        for other in 0..3 {
            let a = before[layer]
                .video_frames_by_layer
                .get(&(publisher_id.clone(), other))
                .copied()
                .unwrap_or(0);
            let b = layer_frames(bot, &publisher_id, other);
            if layer == other {
                assert!(b > a + 10);
            } else {
                assert_eq!(b, a, "unexpected layer {other} for receiver {layer}");
            }
        }
    }
    let id = receivers[0].stats().participant_id.unwrap();
    control(&server, &id, Some(Some(0))).await;
    tokio::time::sleep(Duration::from_millis(200)).await; // drain previously queued packets
    let paused = receivers[0].stats();
    let other_frames = [
        receivers[1].stats().video_frames,
        receivers[2].stats().video_frames,
    ];
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(receivers[0].stats().video_frames, paused.video_frames);
    assert!(receivers[0].stats().audio_frames > paused.audio_frames + 20);
    for i in 1..3 {
        assert!(receivers[i].stats().video_frames > other_frames[i - 1] + 10);
    }
    control(&server, &id, Some(Some(2500))).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        receivers[0].stats().video_frames,
        paused.video_frames,
        "recovery bypassed hold"
    );
    assert!(
        receivers[0]
            .wait_for(Duration::from_secs(6), |s| s
                .video_frames_by_layer
                .get(&(publisher_id.clone(), 2))
                .is_some_and(|n| *n > 20)
                && s.video_keyframes > paused.video_keyframes)
            .await,
        "{:?}",
        receivers[0].stats()
    );
    for bot in receivers {
        bot.leave().await;
    }
    publisher.leave().await;
    server.task.abort();
}

#[tokio::test]
async fn shaped_transport_recovers_from_all_video_paused_with_real_twcc() {
    use crate::bot::{test_transport::Link, Bot};
    let fixture = fixture().await;
    let server = crate::start((*fixture.services.config).clone())
        .await
        .unwrap();
    let publisher = Bot::start(bot_options(&server, "publisher", true, true))
        .await
        .unwrap();
    let link = Link::new(4_000_000).await;
    let shaped = Bot::start_shaped(bot_options(&server, "shaped", false, false), link.clone())
        .await
        .unwrap();
    let normal = Bot::start(bot_options(&server, "normal", false, false))
        .await
        .unwrap();
    assert!(
        shaped
            .wait_for(Duration::from_secs(20), |s| s.video_frames > 30)
            .await,
        "startup: {:?}",
        shaped.stats()
    );
    let id = shaped.stats().participant_id.unwrap();
    link.set_capacity(120_000);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let state = control(&server, &id, None).await;
        if state["estimate_kbps"].as_u64().is_some_and(|v| v < 250)
            && state["allocations"][0] == "Paused"
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no congestion reaction: {state}; {:?}",
            shaped.stats()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = shaped.stats();
    let forwarded_before = control(&server, &id, None).await["video_frames_written"].clone();
    let normal_before = normal.stats().video_frames;
    tokio::time::sleep(Duration::from_secs(1)).await;
    // RTX and already-buffered/depacketized frames can arrive after policy pause.
    // Assert that the real SFU stops writing new frames, not that the client
    // discards frames already in flight before the pause.
    assert_eq!(
        control(&server, &id, None).await["video_frames_written"],
        forwarded_before,
        "SFU wrote video while paused"
    );
    assert!(
        shaped.stats().audio_frames > before.audio_frames + 5,
        "audio stopped under shaped link"
    );
    assert!(
        normal.stats().video_frames > normal_before + 10,
        "other subscriber affected"
    );
    link.set_capacity(4_000_000);
    assert!(
        shaped
            .wait_for(Duration::from_secs(25), |s| s.video_frames
                > before.video_frames + 25
                && s.video_keyframes > before.video_keyframes)
            .await,
        "did not recover from full video pause: {:?}, state {}",
        shaped.stats(),
        control(&server, &id, None).await
    );
    let publisher_id = publisher.stats().participant_id.unwrap();
    let high_before = before
        .video_frames_by_layer
        .get(&(publisher_id.clone(), 2))
        .copied()
        .unwrap_or(0);
    assert!(
        shaped
            .wait_for(Duration::from_secs(25), |s| s
                .video_frames_by_layer
                .get(&(publisher_id.clone(), 2))
                .copied()
                .unwrap_or(0)
                > high_before + 20)
            .await,
        "quality did not recover to high: {:?}, state {}",
        shaped.stats(),
        control(&server, &id, None).await
    );
    eprintln!("real TWCC recovery: {}", control(&server, &id, None).await);
    shaped.leave().await;
    normal.leave().await;
    publisher.leave().await;
    server.task.abort();
}

#[tokio::test]
async fn real_single_encoding_resumes_with_keyframe_without_renegotiation() {
    use crate::bot::Bot;
    let fixture = fixture().await;
    let server = crate::start((*fixture.services.config).clone())
        .await
        .unwrap();
    let publisher = Bot::start(bot_options(&server, "publisher", true, false))
        .await
        .unwrap();
    let receiver = Bot::start(bot_options(&server, "single-receiver", false, false))
        .await
        .unwrap();
    let id = receiver.stats().participant_id.unwrap();
    control(&server, &id, Some(Some(2500))).await;
    assert!(
        receiver
            .wait_for(Duration::from_secs(8), |s| s.video_frames > 20)
            .await
    );
    control(&server, &id, Some(Some(0))).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = receiver.stats();
    let offers = before
        .events
        .iter()
        .filter(|e| *e == "subscriber.offer")
        .count();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(receiver.stats().video_frames, before.video_frames);
    control(&server, &id, Some(Some(2500))).await;
    assert!(
        receiver
            .wait_for(Duration::from_secs(6), |s| s.video_frames
                > before.video_frames + 20
                && s.video_keyframes > before.video_keyframes)
            .await
    );
    assert_eq!(
        receiver
            .stats()
            .events
            .iter()
            .filter(|e| *e == "subscriber.offer")
            .count(),
        offers,
        "bandwidth pause renegotiated the receiver"
    );
    receiver.leave().await;
    publisher.leave().await;
    server.task.abort();
}

#[tokio::test]
async fn review_unsubscribe_pauses_video_while_another_offer_is_pending() {
    let mut room = fixture().await;
    let mut publisher = participant("publisher");
    publisher
        .tracks
        .push(track("video", TrackSource::Camera, false));
    room.participants.push(publisher);
    add_receiver(&mut room);
    let sub = room.participants[1].subscriber.as_mut().unwrap();
    sub.estimate_kbps = Some(2500);
    sub.out[0].state = OutState::Open(Mid::from("video"));
    sub.out[0]
        .video
        .configure(Allocation::Layer(0), Some(Rid::from("q")));
    let mut change = sub.peer.rtc.sdp_api();
    change.add_channel("pending".into());
    let (_, pending) = change.apply().unwrap();
    sub.pending = Some(super::participant::PendingOffer {
        pending,
        revision: 1,
        event: json!({}),
        sent_at: Instant::now(),
    });
    room.update_subscriptions(
        1,
        &[crate::protocol::SubscriptionChange {
            track_id: "video".into(),
            subscribed: false,
            quality: Quality::High,
        }],
    )
    .unwrap();
    let sub = room.participants[1].subscriber.as_ref().unwrap();
    assert!(sub.pending.is_some());
    assert!(matches!(sub.out[0].state, OutState::Open(_)));
    assert_eq!(sub.out[0].video.allocation, Allocation::Paused);
}

#[tokio::test]
async fn review_fully_active_single_video_does_not_fill_link_with_padding() {
    use crate::bot::{test_transport::Link, Bot};
    let fixture = fixture().await;
    let server = crate::start((*fixture.services.config).clone())
        .await
        .unwrap();
    let publisher = Bot::start(bot_options(&server, "publisher", true, false))
        .await
        .unwrap();
    let link = Link::new(4_000_000).await;
    let receiver = Bot::start_shaped(bot_options(&server, "receiver", false, false), link.clone())
        .await
        .unwrap();
    control(
        &server,
        &receiver.stats().participant_id.unwrap(),
        Some(Some(2500)),
    )
    .await;
    assert!(
        receiver
            .wait_for(Duration::from_secs(8), |s| s.video_frames > 50)
            .await
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let before = link.delivered_bytes();
    let start = Instant::now();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let bps = ((link.delivered_bytes() - before) as f64 * 8.0) / start.elapsed().as_secs_f64();
    assert!(
        bps < 900_000.0,
        "300kbps camera still filled downlink with {bps:.0}bps of SRTP/padding"
    );
    receiver.leave().await;
    publisher.leave().await;
    server.task.abort();
}
