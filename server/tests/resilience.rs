//! Network failure scenarios with time-to-audio measurements.
//!
//! These use the headless bot on loopback, so they measure the server and
//! protocol behavior (resume, ICE restart, peer rebuild), not radio or OS
//! network stacks. Run with `--nocapture` to see the timings.

mod common;

use ecochain_rtc_server::bot::{Bot, BotStats};
use std::time::{Duration, Instant};

/// Waits until `bot` receives audio that was sent after `since`, and
/// returns how long that took.
async fn audio_recovery(bot: &Bot, since: Instant) -> Duration {
    let ok = bot
        .wait_for(Duration::from_secs(30), |s| {
            s.last_audio
                .is_some_and(|t| t > since + Duration::from_millis(100))
        })
        .await;
    assert!(ok, "audio did not recover: {:?}", bot.stats());
    bot.stats().last_audio.unwrap() - since
}

async fn pair(server: &common::TestServer, room: &str) -> (Bot, Bot) {
    let alice = Bot::start(server.bot(room, "alice")).await.unwrap();
    let bob = Bot::start(server.bot(room, "bob")).await.unwrap();
    let flowing = |s: &BotStats| s.audio_frames > 25;
    assert!(alice.wait_for(Duration::from_secs(10), flowing).await);
    assert!(bob.wait_for(Duration::from_secs(10), flowing).await);
    (alice, bob)
}

fn single_seat(bot: &Bot) {
    let remote = bot.stats().remote_participants;
    assert_eq!(
        remote.len(),
        1,
        "participant must not be duplicated: {remote:?}"
    );
}

async fn outage(seconds: u64) {
    let server = common::start("").await;
    let (alice, bob) = pair(&server, &format!("outage-{seconds}")).await;
    alice.outage(Duration::from_secs(seconds));
    tokio::time::sleep(Duration::from_secs(seconds)).await;
    let back = Instant::now();
    let to_bob = audio_recovery(&bob, back).await;
    let to_alice = audio_recovery(&alice, back).await;
    println!(
        "RESULT outage={seconds}s audio_recovery alice->bob={:?} bob->alice={:?} resumes={}",
        to_bob,
        to_alice,
        alice.stats().resumes
    );
    single_seat(&alice);
    single_seat(&bob);
    assert_eq!(server.metric("ecochain_rtc_participants_active"), 2);
    alice.leave().await;
    bob.leave().await;
}

#[tokio::test]
async fn outage_5_seconds() {
    outage(5).await;
}

#[tokio::test]
async fn outage_15_seconds() {
    outage(15).await;
}

#[tokio::test]
async fn network_switch_restarts_ice() {
    let server = common::start("").await;
    let (alice, bob) = pair(&server, "switch").await;
    let switched = Instant::now();
    alice.switch_network();
    let to_bob = audio_recovery(&bob, switched).await;
    let to_alice = audio_recovery(&alice, switched).await;
    println!("RESULT network_switch audio_recovery alice->bob={to_bob:?} bob->alice={to_alice:?}");
    single_seat(&bob);
    alice.leave().await;
    bob.leave().await;
}

#[tokio::test]
async fn signaling_drop_keeps_media_flowing() {
    let server = common::start("").await;
    let (alice, bob) = pair(&server, "ws-drop").await;
    let before = bob.stats().audio_frames;
    alice.drop_signaling();
    assert!(
        alice
            .wait_for(Duration::from_secs(10), |s| s.resumes == 1)
            .await,
        "{:?}",
        alice.stats()
    );
    // Audio never stopped: ~50 frames per second keep arriving.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(bob.stats().audio_frames > before + 40, "{:?}", bob.stats());
    single_seat(&bob);
    println!("RESULT signaling_drop resumed=true media_uninterrupted=true");
    alice.leave().await;
    bob.leave().await;
}

#[tokio::test]
async fn server_address_change_moves_media() {
    let Some(lan) = ecochain_rtc_server::config::primary_lan_ip() else {
        eprintln!("SKIP: no LAN address");
        return;
    };
    let server = common::start_following_primary().await;
    let sockets = &server.server.sockets;
    let before = sockets.advertised();
    let (alice, bob) = pair(&server, "server-moves").await;

    // The server's network changes: its media socket moves to another address.
    let mut changes = sockets.subscribe();
    assert!(sockets.move_primary(lan).unwrap());
    changes.changed().await.unwrap();
    let after = sockets.advertised();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].ip(), lan);
    assert_eq!(after[0].port(), before[0].port(), "port is kept");
    assert!(
        !sockets.move_primary(lan).unwrap(),
        "same address is a no-op"
    );

    // Clients notice the broken path and restart ICE (the SDK's ICE monitor);
    // the restart negotiation carries the new server candidate.
    let moved = Instant::now();
    alice.switch_network();
    bob.switch_network();
    let to_bob = audio_recovery(&bob, moved).await;
    let to_alice = audio_recovery(&alice, moved).await;
    println!(
        "RESULT server_address_change audio_recovery alice->bob={to_bob:?} bob->alice={to_alice:?}"
    );
    single_seat(&alice);
    single_seat(&bob);
    alice.leave().await;
    bob.leave().await;
}
