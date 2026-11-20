//! Load generator: N headless participants in one room.
//!
//! By default every bot publishes Opus-sized audio (50 frames/s) and a
//! VP8-sized camera track with three simulcast layers, and subscribes to up
//! to `--max-video` remote videos plus all audio. The SFU does not decode, so
//! synthetic payloads load it exactly like real media of the same size and
//! rate.
//!
//! Scenario flags describe a meeting instead: the first `--publishers` bots
//! speak (audio and three-layer video), the next `--cameras` only send
//! video, the next `--muted-mics` negotiate a muted microphone, the rest
//! only listen. Probes measure how fast a listener gets the first frame
//! after switching a video subscription (`--switch-every-secs`) and how fast
//! others hear a listener who turns the microphone on (`--unmute-probe`).
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
    io::{Read, Write},
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
    /// Local IP the bots bind their UDP sockets to. Use the LAN address
    /// when the server runs on another machine.
    #[arg(long, default_value = "127.0.0.1")]
    bind_ip: std::net::IpAddr,
    /// Bots that publish audio and three-layer video (default: everybody).
    #[arg(long)]
    publishers: Option<usize>,
    /// Bots after the publishers that send only a camera.
    #[arg(long, default_value_t = 0)]
    cameras: usize,
    /// Bots after the cameras that negotiate a muted microphone.
    #[arg(long, default_value_t = 0)]
    muted_mics: usize,
    /// The first this many video subscriptions of each bot are "high", the
    /// rest "low" (default: all "high").
    #[arg(long, default_value_t = usize::MAX)]
    high_video: usize,
    /// Every this many seconds some listeners swap one video subscription
    /// and the time to the first frame is measured (0 = off).
    #[arg(long, default_value_t = 0)]
    switch_every_secs: u64,
    /// One listener turns its microphone on during the measurement and the
    /// time until the others hear it is measured.
    #[arg(long)]
    unmute_probe: bool,
    /// Audio tracks each receiver is expected to get (default: the number of
    /// publishers other than the receiver).
    #[arg(long)]
    expect_audio: Option<usize>,
    /// `/metrics` URL of the server, to report how busy the room task was.
    #[arg(long)]
    metrics_url: Option<String>,
    /// Admin key for `/metrics` when the server runs in production mode.
    #[arg(long)]
    admin_key: Option<String>,
}

/// Listeners that swap a video subscription per `--switch-every-secs` tick.
const SWITCHING_LISTENERS: usize = 5;

/// What a bot does in the meeting.
#[derive(Clone, Copy, PartialEq)]
enum Part {
    Publisher,
    Camera,
    MutedMic,
    Listener,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config = config::load(&args.config)?;
    let publishers = args.publishers.unwrap_or(args.participants);
    anyhow::ensure!(
        publishers + args.cameras + args.muted_mics <= args.participants,
        "publishers + cameras + muted-mics must not exceed participants"
    );
    let part = |i: usize| match i {
        i if i < publishers => Part::Publisher,
        i if i < publishers + args.cameras => Part::Camera,
        i if i < publishers + args.cameras + args.muted_mics => Part::MutedMic,
        _ => Part::Listener,
    };
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
            bind_ip: args.bind_ip,
            audio: matches!(part(i), Part::Publisher | Part::MutedMic),
            video_kbps: matches!(part(i), Part::Publisher | Part::Camera).then_some(500),
            simulcast: true,
            max_video: args.max_video,
            audio_level: -25,
            audio_muted: part(i) == Part::MutedMic,
            high_video: args.high_video,
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
    let busy_before = fetch_busy(&args).await;
    let t0 = Instant::now();
    let deadline = t0 + Duration::from_secs(args.seconds);

    let mut heard = Vec::new();
    if args.unmute_probe {
        tokio::time::sleep(Duration::from_secs(2.min(args.seconds))).await;
        match bots
            .iter()
            .rposition(|b| b.stats().participant_id.is_some())
        {
            Some(probe) if part(probe) == Part::Listener => {
                heard = unmute_probe(&bots, probe).await;
            }
            _ => println!("unmute probe skipped: no listener bot (increase participants)"),
        }
    }
    let mut switched = Vec::new();
    let mut switch_missed = 0;
    if args.switch_every_secs > 0 {
        let mut round = 0;
        while Instant::now() + Duration::from_secs(args.switch_every_secs) < deadline {
            tokio::time::sleep(Duration::from_secs(args.switch_every_secs)).await;
            let (times, missed) = switch_probe(&bots, round, args.high_video).await;
            switched.extend(times);
            switch_missed += missed;
            round += 1;
        }
    }
    tokio::time::sleep_until(deadline.into()).await;
    let elapsed = t0.elapsed().as_secs_f64();
    let busy_after = fetch_busy(&args).await;
    let after: Vec<_> = bots.iter().map(Bot::stats).collect();

    let video_tracks = publishers + args.cameras;
    let mut audio_ratio = Vec::new();
    let mut video_ratio = Vec::new();
    let mut kbps = Vec::new();
    for (i, (b, a)) in before.iter().zip(&after).enumerate() {
        let own_audio = usize::from(part(i) == Part::Publisher);
        let own_video = usize::from(matches!(part(i), Part::Publisher | Part::Camera));
        let audio_tracks = args.expect_audio.unwrap_or(publishers - own_audio);
        let expected_audio = 50.0 * audio_tracks as f64;
        let expected_video = 25.0 * args.max_video.min(video_tracks - own_video) as f64;
        if expected_audio > 0.0 {
            audio_ratio.push((a.audio_frames - b.audio_frames) as f64 / elapsed / expected_audio);
        }
        if expected_video > 0.0 {
            video_ratio.push((a.video_frames - b.video_frames) as f64 / elapsed / expected_video);
        }
        kbps.push((a.bytes - b.bytes) as f64 * 8.0 / elapsed / 1000.0);
    }
    let stopped = after.iter().filter(|s| s.stopped.is_some()).count();
    let summary = |v: &[f64]| {
        if v.is_empty() {
            return (0.0, 0.0);
        }
        let min = v.iter().cloned().fold(f64::INFINITY, f64::min);
        let avg = v.iter().sum::<f64>() / v.len() as f64;
        (min, avg)
    };
    let (amin, aavg) = summary(&audio_ratio);
    let (vmin, vavg) = summary(&video_ratio);
    let (kmin, kavg) = summary(&kbps);
    println!(
        "participants={} publishers={} cameras={} muted_mics={} max_video={} seconds={:.0}",
        args.participants, publishers, args.cameras, args.muted_mics, args.max_video, elapsed
    );
    println!(
        "audio delivered  min={:.1}% avg={:.1}% (of the audio each receiver expects)",
        amin * 100.0,
        aavg * 100.0,
    );
    println!(
        "video delivered  min={:.1}% avg={:.1}% (of the video each receiver subscribed)",
        vmin * 100.0,
        vavg * 100.0,
    );
    println!("receive kbps     min={kmin:.0} avg={kavg:.0}");
    println!("bots stopped     {stopped}");
    if args.switch_every_secs > 0 {
        println!(
            "switch to first frame  {} (no frame in 5 s: {switch_missed})",
            percentiles(&mut switched)
        );
    }
    if args.unmute_probe {
        println!("first unmute heard     {}", percentiles(&mut heard));
    }
    if let (Some(before), Some(after)) = (busy_before, busy_after) {
        println!(
            "room busy              {:.1}% of one core",
            (after - before) / elapsed * 100.0
        );
    }
    for bot in bots {
        bot.leave().await;
    }
    Ok(())
}

/// How long a probe waits for media before it counts as missed.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Bot `probe` turns its microphone on; returns, per other bot, the time
/// until its first audio frame from the probe arrived.
async fn unmute_probe(bots: &[Bot], probe: usize) -> Vec<Duration> {
    let probe_id = bots[probe].stats().participant_id.unwrap_or_default();
    let heard_from = |bot: &Bot| {
        bot.stats()
            .audio_frames_by_participant
            .get(&probe_id)
            .copied()
            .unwrap_or(0)
    };
    let baseline: Vec<u64> = bots.iter().map(heard_from).collect();
    let started = Instant::now();
    bots[probe].enable_audio();
    let mut times = vec![None; bots.len()];
    while started.elapsed() < PROBE_TIMEOUT && times.iter().flatten().count() + 1 < bots.len() {
        for (i, bot) in bots.iter().enumerate() {
            if i != probe && times[i].is_none() && heard_from(bot) > baseline[i] {
                times[i] = Some(started.elapsed());
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    times.into_iter().flatten().collect()
}

/// Some listeners swap their oldest video subscription for a track they are
/// not watching; returns the time to the first frame of each swap and how
/// many got none in time.
async fn switch_probe(bots: &[Bot], round: usize, high_video: usize) -> (Vec<Duration>, usize) {
    let quality = if high_video == usize::MAX {
        "high"
    } else {
        "low"
    };
    let mut pending = Vec::new();
    for k in 0..SWITCHING_LISTENERS.min(bots.len()) {
        let index = (round * SWITCHING_LISTENERS + k) % bots.len();
        let bot = &bots[index];
        let stats = bot.stats();
        let Some(old) = stats.subscribed_video.first().cloned() else {
            continue;
        };
        let Some((participant, track, _)) = stats
            .remote_tracks
            .iter()
            .find(|(_, track, kind)| kind == "video" && !stats.subscribed_video.contains(track))
        else {
            continue;
        };
        let baseline = stats
            .video_frames_by_participant
            .get(participant)
            .copied()
            .unwrap_or(0);
        bot.unsubscribe_video(old);
        bot.subscribe_video(track.clone(), quality);
        pending.push((index, participant.clone(), baseline, Instant::now()));
    }
    let mut times = Vec::new();
    let deadline = Instant::now() + PROBE_TIMEOUT;
    while !pending.is_empty() && Instant::now() < deadline {
        pending.retain(|(index, participant, baseline, started)| {
            let frames = bots[*index]
                .stats()
                .video_frames_by_participant
                .get(participant)
                .copied()
                .unwrap_or(0);
            if frames > *baseline {
                times.push(started.elapsed());
                false
            } else {
                true
            }
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (times, pending.len())
}

fn percentiles(times: &mut [Duration]) -> String {
    if times.is_empty() {
        return "no samples".into();
    }
    times.sort();
    let at = |p: usize| times[(times.len() * p).div_ceil(100).saturating_sub(1)];
    format!(
        "p50={}ms p95={}ms (n={})",
        at(50).as_millis(),
        at(95).as_millis(),
        times.len()
    )
}

/// Total room busy time in seconds from `--metrics-url`, if reachable.
async fn fetch_busy(args: &Args) -> Option<f64> {
    let url = args.metrics_url.clone()?;
    let key = args.admin_key.clone();
    let body = tokio::task::spawn_blocking(move || http_get(&url, key.as_deref()))
        .await
        .ok()?
        .map_err(|e| eprintln!("metrics: {e}"))
        .ok()?;
    body.lines()
        .find_map(|l| l.strip_prefix("ecochain_rtc_room_busy_seconds_total "))
        .and_then(|v| v.trim().parse().ok())
}

/// Minimal HTTP/1.1 GET for `http://host:port/path` (no TLS, no redirects).
fn http_get(url: &str, admin_key: Option<&str>) -> std::io::Result<String> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "only http:// URLs")
    })?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let mut stream = std::net::TcpStream::connect(host)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let auth = admin_key
        .map(|k| format!("Authorization: Bearer {k}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "GET /{path} HTTP/1.1\r\nHost: {host}\r\n{auth}Connection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response
        .split_once("\r\n\r\n")
        .map_or(response.clone(), |(_, body)| body.to_owned()))
}
