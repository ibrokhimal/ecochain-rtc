//! End-to-end media through the SFU with real ICE, DTLS-SRTP and RTP.

mod common;

use ecochain_rtc_server::bot::Bot;
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
