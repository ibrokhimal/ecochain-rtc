//! Load generator: N headless participants in one room.
//!
//! Every bot publishes Opus-sized audio (50 frames/s) and a VP8-sized camera
//! track with three simulcast layers, and subscribes to up to `--max-video`
//! remote videos plus all audio. The SFU does not decode, so synthetic
//! payloads load it exactly like real media of the same size and rate.
//!
//! ```text
//! cargo run --release --bin loadtest -- --config config/development.toml --participants 25
//! ```

use clap::Parser;
use ecochain_rtc_server::{
    auth::{self, Claims, Role},
    bot::{Bot, BotOptions},
    config,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Parser)]
struct Args {
    /// Server configuration (used only to sign tokens).
    #[arg(long, default_value = "config/development.toml")]
    config: PathBuf,
    /// Signaling URL.
    #[arg(long, default_value = "ws://127.0.0.1:7880/v1/rtc")]
    url: String,
    /// Number of participants.
    #[arg(long, default_value_t = 25)]
    participants: usize,
    /// Remote videos each participant receives.
    #[arg(long, default_value_t = 8)]
    max_video: usize,
    /// Measurement duration in seconds (after everybody joined).
    #[arg(long, default_value_t = 30)]
    seconds: u64,
    /// Room id.
    #[arg(long, default_value = "loadtest")]
    room: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config = config::load(&args.config)?;
    let mut bots = Vec::new();
    let started = Instant::now();
    for i in 0..args.participants {
        let now = auth::unix_now();
        let claims = Claims {
            iss: config.auth.issuer.clone(),
            sub: format!("load-{i}"),
            room: args.room.clone(),
            name: None,
            role: if i == 0 {
                Role::Host
            } else {
                Role::Participant
            },
            can_publish: true,
            exp: now + 3600,
            iat: now,
            nbf: None,
        };
        let options = BotOptions {
            url: args.url.clone(),
            token: auth::issue(&claims, &config.auth.token_secret),
            bind_ip: "127.0.0.1".parse()?,
            audio: true,
            video_kbps: Some(500),
            simulcast: true,
            max_video: args.max_video,
        };
        bots.push(
            Bot::start(options)
                .await
                .map_err(|e| anyhow::anyhow!("bot {i}: {e}"))?,
        );
    }
    println!(
        "joined {} participants in {:?}",
        bots.len(),
        started.elapsed()
    );

    // Let negotiation settle, then measure.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let before: Vec<_> = bots.iter().map(Bot::stats).collect();
    let t0 = Instant::now();
    tokio::time::sleep(Duration::from_secs(args.seconds)).await;
    let elapsed = t0.elapsed().as_secs_f64();
    let after: Vec<_> = bots.iter().map(Bot::stats).collect();

    let expected_audio = 50.0 * (args.participants - 1) as f64;
    let expected_video = 25.0 * args.max_video.min(args.participants - 1) as f64;
    let mut audio_ratio = Vec::new();
    let mut video_ratio = Vec::new();
    let mut kbps = Vec::new();
    for (b, a) in before.iter().zip(&after) {
        audio_ratio.push((a.audio_frames - b.audio_frames) as f64 / elapsed / expected_audio);
        video_ratio.push((a.video_frames - b.video_frames) as f64 / elapsed / expected_video);
        kbps.push((a.bytes - b.bytes) as f64 * 8.0 / elapsed / 1000.0);
    }
    let stopped = after.iter().filter(|s| s.stopped.is_some()).count();
    let summary = |v: &[f64]| {
        let min = v.iter().cloned().fold(f64::INFINITY, f64::min);
        let avg = v.iter().sum::<f64>() / v.len() as f64;
        (min, avg)
    };
    let (amin, aavg) = summary(&audio_ratio);
    let (vmin, vavg) = summary(&video_ratio);
    let (kmin, kavg) = summary(&kbps);
    println!(
        "participants={} max_video={} seconds={:.0}",
        args.participants, args.max_video, elapsed
    );
    println!(
        "audio delivered  min={:.1}% avg={:.1}% (of {:.0} frames/s per receiver)",
        amin * 100.0,
        aavg * 100.0,
        expected_audio
    );
    println!(
        "video delivered  min={:.1}% avg={:.1}% (of {:.0} frames/s per receiver)",
        vmin * 100.0,
        vavg * 100.0,
        expected_video
    );
    println!("receive kbps     min={kmin:.0} avg={kavg:.0}");
    println!("bots stopped     {stopped}");
    for bot in bots {
        bot.leave().await;
    }
    Ok(())
}
