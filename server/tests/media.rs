//! End-to-end media through the SFU with real ICE, DTLS-SRTP and RTP.

mod common;

use ecochain_rtc_server::bot::{Bot, BotOptions, BotStats};
use std::time::{Duration, Instant};

#[tokio::test]
async fn audio_and_video_flow_between_two_participants() {
    let server = common::start("").await;
    let alice = Bot::start(server.bot("media", "alice")).await.unwrap();
    let bob = Bot::start(server.bot("media", "bob")).await.unwrap();

    let both = |s: &ecochain_rtc_server::bot::BotStats| {
        s.audio_frames > 50 && s.video_frames > 25 && s.video_keyframes > 0
    };
    assert!(
        bob.wait_for(Duration::from_secs(10), both).await,
        "bob: {:?}",
        bob.stats()
    );
    assert!(
        alice.wait_for(Duration::from_secs(10), both).await,
        "alice: {:?}",
        alice.stats()
    );

    alice.leave().await;
    bob.leave().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(server.metric("ecochain_rtc_participants_active"), 0);
}

#[tokio::test]
async fn simulcast_publisher_reaches_subscriber() {
    let server = common::start("").await;
    let mut options = server.bot("simulcast", "alice");
    options.simulcast = true;
    let alice = Bot::start(options).await.unwrap();
    let bob = Bot::start(server.bot("simulcast", "bob")).await.unwrap();

    assert!(
        bob.wait_for(Duration::from_secs(10), |s| s.video_frames > 50
            && s.video_keyframes > 0)
            .await,
        "bob: {:?}",
        bob.stats()
    );
    // One layer is forwarded, not all three: about 25 frames per second.
    let before = bob.stats().video_frames;
    let start = Instant::now();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let rate = (bob.stats().video_frames - before) as f64 / start.elapsed().as_secs_f64();
    assert!((15.0..=35.0).contains(&rate), "video fps {rate}");
    alice.leave().await;
    bob.leave().await;
}

#[tokio::test]
async fn audio_only_participant_without_camera() {
    let server = common::start("").await;
    let mut options = server.bot("audio-only", "alice");
    options.video_kbps = None;
    let alice = Bot::start(options).await.unwrap();
    let mut options = server.bot("audio-only", "bob");
    options.audio = false;
    options.video_kbps = None;
    let bob = Bot::start(options).await.unwrap();
    assert!(
        bob.wait_for(Duration::from_secs(10), |s| s.audio_frames > 50)
            .await,
        "{:?}",
        bob.stats()
    );
    assert_eq!(bob.stats().video_frames, 0);
    alice.leave().await;
    bob.leave().await;
}

#[tokio::test]
async fn bot_counts_frames_per_participant() {
    let server = common::start("").await;
    let alice = Bot::start(server.bot("per-participant", "alice"))
        .await
        .unwrap();
    let mut options = server.bot("per-participant", "bob");
    options.audio = false;
    options.video_kbps = None;
    let bob = Bot::start(options).await.unwrap();
    let alice_id = alice.stats().participant_id.unwrap();

    let counted = |s: &ecochain_rtc_server::bot::BotStats| {
        s.audio_frames_by_participant
            .get(&alice_id)
            .is_some_and(|n| *n > 50)
            && s.video_frames_by_participant
                .get(&alice_id)
                .is_some_and(|n| *n > 25)
    };
    assert!(
        bob.wait_for(Duration::from_secs(10), counted).await,
        "bob: {:?}",
        bob.stats()
    );
    assert!(bob
        .stats()
        .remote_tracks
        .iter()
        .any(|(p, _, kind)| *p == alice_id && kind == "video"));
    alice.leave().await;
    bob.leave().await;
}

#[tokio::test]
async fn room_busy_metric_grows() {
    let server = common::start("").await;
    let alice = Bot::start(server.bot("busy", "alice")).await.unwrap();
    let bob = Bot::start(server.bot("busy", "bob")).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let busy = server.metric_f64("ecochain_rtc_room_busy_seconds_total");
    assert!(busy > 0.0, "busy seconds {busy}");
    alice.leave().await;
    bob.leave().await;
}

#[tokio::test]
async fn media_keeps_flowing_after_another_participant_leaves() {
    let server = common::start("").await;
    let a = Bot::start(server.bot("leaver", "a")).await.unwrap();
    let b = Bot::start(server.bot("leaver", "b")).await.unwrap();
    let c = Bot::start(server.bot("leaver", "c")).await.unwrap();
    let flowing = |s: &ecochain_rtc_server::bot::BotStats| {
        s.audio_frames_by_participant.len() == 2 && s.audio_frames > 50
    };
    for bot in [&a, &b, &c] {
        assert!(
            bot.wait_for(Duration::from_secs(10), flowing).await,
            "{:?}",
            bot.stats()
        );
    }
    let a_id = a.stats().participant_id.clone().unwrap();
    let c_id = c.stats().participant_id.clone().unwrap();
    let a_from_c = a.stats().audio_frames_by_participant[&c_id];
    let a_video_from_c = a
        .stats()
        .video_frames_by_participant
        .get(&c_id)
        .copied()
        .unwrap_or(0);
    let c_from_a = c.stats().audio_frames_by_participant[&a_id];
    let c_video_from_a = c
        .stats()
        .video_frames_by_participant
        .get(&a_id)
        .copied()
        .unwrap_or(0);

    b.leave().await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let a_stats = a.stats();
    let c_stats = c.stats();
    assert!(a_stats.audio_frames_by_participant[&c_id] > a_from_c + 40);
    assert!(c_stats.audio_frames_by_participant[&a_id] > c_from_a + 40);
    assert!(a_stats.video_frames_by_participant[&c_id] > a_video_from_c);
    assert!(c_stats.video_frames_by_participant[&a_id] > c_video_from_a);
    a.leave().await;
    c.leave().await;
}

#[tokio::test]
async fn subscriber_gets_track_only_after_answer_and_stops_after_unsubscribe() {
    let server = common::start("").await;
    let alice = Bot::start(server.bot("fwd-index", "alice")).await.unwrap();
    let mut options = server.bot("fwd-index", "bob");
    options.audio = false;
    options.video_kbps = None;
    let bob = Bot::start(options).await.unwrap();
    let alice_id = alice.stats().participant_id.unwrap();

    let got_video = |s: &ecochain_rtc_server::bot::BotStats| {
        s.video_frames_by_participant
            .get(&alice_id)
            .is_some_and(|n| *n > 10)
    };
    assert!(
        bob.wait_for(Duration::from_secs(10), got_video).await,
        "bob: {:?}",
        bob.stats()
    );
    let track_id = bob
        .stats()
        .remote_tracks
        .iter()
        .find(|(p, _, kind)| *p == alice_id && kind == "video")
        .map(|(_, track, _)| track.clone())
        .expect("alice video track");

    bob.unsubscribe_video(track_id);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let frozen = bob.stats().video_frames_by_participant[&alice_id];
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        bob.stats().video_frames_by_participant[&alice_id],
        frozen,
        "frames still arriving after unsubscribe"
    );
    alice.leave().await;
    bob.leave().await;
}

/// An audio-only bot that speaks at `level` dBov and receives everything.
fn speaker(server: &common::TestServer, room: &str, identity: &str, level: i8) -> BotOptions {
    let mut options = server.bot(room, identity);
    options.video_kbps = None;
    options.audio_level = level;
    options
}

/// A bot that publishes nothing.
fn listener(server: &common::TestServer, room: &str, identity: &str) -> BotOptions {
    let mut options = server.bot(room, identity);
    options.audio = false;
    options.video_kbps = None;
    options
}

fn frames_from(stats: &BotStats, participant_id: &str) -> u64 {
    stats
        .audio_frames_by_participant
        .get(participant_id)
        .copied()
        .unwrap_or(0)
}

#[tokio::test]
async fn only_loudest_three_audio_tracks_are_forwarded() {
    let server = common::start("").await;
    let room = "loudest";
    // -28 dBov is above the speaking threshold (about -30.5), so only the
    // cap keeps it out: it is within the 6 dB swap margin of -20, and if it
    // got a slot first, each louder speaker evicts it.
    let mut speakers = Vec::new();
    for (name, level) in [("a", -10), ("b", -15), ("c", -20), ("quiet", -28)] {
        speakers.push(
            Bot::start(speaker(&server, room, name, level))
                .await
                .unwrap(),
        );
    }
    let ids: Vec<String> = speakers
        .iter()
        .map(|b| b.stats().participant_id.unwrap())
        .collect();
    let listener = Bot::start(listener(&server, room, "listener"))
        .await
        .unwrap();

    let loud = ids[..3].to_vec();
    assert!(
        listener
            .wait_for(Duration::from_secs(10), |s| loud
                .iter()
                .all(|id| frames_from(s, id) > 100))
            .await,
        "listener: {:?}",
        listener.stats()
    );
    let before = frames_from(&listener.stats(), &ids[3]);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after = frames_from(&listener.stats(), &ids[3]);
    assert!(
        after - before < 10,
        "quiet speaker forwarded: {before} -> {after}"
    );

    for bot in speakers {
        bot.leave().await;
    }
    listener.leave().await;
}

#[tokio::test]
async fn leaving_speaker_frees_audio_slot() {
    let server = common::start("").await;
    let room = "slot";
    let listener = Bot::start(listener(&server, room, "listener"))
        .await
        .unwrap();
    let mut first = Vec::new();
    for (name, level) in [("a", -10), ("b", -15), ("c", -20)] {
        first.push(
            Bot::start(speaker(&server, room, name, level))
                .await
                .unwrap(),
        );
    }
    let ids: Vec<String> = first
        .iter()
        .map(|b| b.stats().participant_id.unwrap())
        .collect();
    assert!(
        listener
            .wait_for(Duration::from_secs(10), |s| ids
                .iter()
                .all(|id| frames_from(s, id) > 50))
            .await,
        "listener: {:?}",
        listener.stats()
    );
    // Joins last and is less than the swap margin quieter: no slot.
    let fourth = Bot::start(speaker(&server, room, "d", -25)).await.unwrap();
    let fourth_id = fourth.stats().participant_id.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let before = frames_from(&listener.stats(), &fourth_id);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let after = frames_from(&listener.stats(), &fourth_id);
    assert!(
        after - before < 10,
        "fourth forwarded early: {before} -> {after}"
    );

    let mut first = first.into_iter();
    first.next().unwrap().leave().await;
    assert!(
        listener
            .wait_for(Duration::from_secs(2), |s| frames_from(s, &fourth_id)
                > after + 20)
            .await,
        "listener: {:?}",
        listener.stats()
    );

    for bot in first {
        bot.leave().await;
    }
    fourth.leave().await;
    listener.leave().await;
}

#[tokio::test]
async fn muted_audio_reaches_nobody() {
    let server = common::start("").await;
    let room = "muted";
    let mut options = speaker(&server, room, "speaker", -10);
    options.audio_muted = true;
    let speaker = Bot::start(options).await.unwrap();
    let listener = Bot::start(listener(&server, room, "listener"))
        .await
        .unwrap();
    // Wait for the listener's subscriber to be set up with the speaker's track.
    assert!(
        listener
            .wait_for(Duration::from_secs(10), |s| !s.remote_tracks.is_empty())
            .await,
        "listener: {:?}",
        listener.stats()
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(listener.stats().audio_frames, 0);
    speaker.leave().await;
    listener.leave().await;
}

#[tokio::test]
async fn loud_speaker_is_announced() {
    let server = common::start("").await;
    let room = "announce";
    let speaker = Bot::start(speaker(&server, room, "speaker", -10))
        .await
        .unwrap();
    let listener = Bot::start(listener(&server, room, "listener"))
        .await
        .unwrap();
    assert!(
        listener
            .wait_for(Duration::from_secs(5), |s| s
                .events
                .iter()
                .any(|e| e == "speakers.changed"))
            .await,
        "listener: {:?}",
        listener.stats()
    );
    speaker.leave().await;
    listener.leave().await;
}
