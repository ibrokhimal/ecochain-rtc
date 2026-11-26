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
