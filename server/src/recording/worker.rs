//! The blocking recording worker owns all spools, muxing and finalization.
use super::{mux::MatroskaWriter, RecordingCodec};
use crate::{
    config::RecordingConfig,
    room::RoomCommand,
    storage::{create_private_worker_dir, Vault},
};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const QUEUE_BYTES: usize = 32 * 1024 * 1024;
/// One authenticated encoded access unit copied by reference from the SFU.
pub struct RecordedFrame {
    /// Publisher track identity; never used as a filesystem path.
    pub track_id: String,
    /// Server-assigned publisher identity.
    pub participant_id: String,
    /// Display name for the playback UI.
    pub name: String,
    /// microphone, camera, screen or screen_audio.
    pub source: String,
    /// Actual negotiated codec of this frame.
    pub codec: RecordingCodec,
    /// Simulcast encoding identity; layer changes start a new independent segment.
    pub layer: String,
    /// Clockwise display rotation carried by the sender.
    pub rotation: u16,
    /// Server receive time relative to recording start.
    pub arrival_ms: u64,
    /// Sender-report wallclock plus RTP offset, when available.
    pub sender_ms: Option<f64>,
    /// Complete encoded frame; no decode occurs in the actor.
    pub data: Arc<[u8]>,
    /// Decodable keyframe flag.
    pub keyframe: bool,
    /// Whether this access unit follows an unbroken codec reference chain.
    pub contiguous: bool,
}
enum Input {
    Frame(RecordedFrame),
    Workspace(u64, Value),
}
struct Shared {
    stop: AtomicBool,
    stop_ms: AtomicU64,
    failure: AtomicU8,
    queued_bytes: AtomicUsize,
    finished: AtomicBool,
}
/// Nonblocking actor-side handle to a single recording worker.
pub struct RecordingHandle {
    pub id: String,
    pub start: Instant,
    tx: SyncSender<(usize, Input)>,
    shared: Arc<Shared>,
}
impl RecordingHandle {
    /// Starts a bounded worker and immediately returns control to the media actor.
    pub fn start(
        config: RecordingConfig,
        vault: Arc<Vault>,
        instance: String,
        workspace: Value,
        commands: tokio::sync::mpsc::Sender<RoomCommand>,
    ) -> Result<Self> {
        let id = uuid::Uuid::new_v4().to_string();
        let start = Instant::now();
        let started_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let (tx, rx) = mpsc::sync_channel(128);
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            stop_ms: AtomicU64::new(0),
            failure: AtomicU8::new(0),
            queued_bytes: AtomicUsize::new(0),
            finished: AtomicBool::new(false),
        });
        let worker_id = id.clone();
        let worker_shared = shared.clone();
        std::thread::Builder::new()
            .name("ecochain-recording".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(
                        &config,
                        &vault,
                        &instance,
                        &worker_id,
                        workspace,
                        &commands,
                        start,
                        started_at_ms,
                        rx,
                        &worker_shared,
                    )
                }));
                worker_shared.finished.store(true, Ordering::Release);
                if !matches!(result, Ok(Ok(()))) {
                    if let Ok(Err(error)) = result {
                        tracing::warn!(error = %error, "recording worker failed");
                    }
                    let mut job = recording_job(&worker_id, "failed", started_at_ms);
                    job["error"] = json!("recording_failed");
                    let _ = vault.write_job(&instance, &worker_id, &job);
                    let _ = commands.blocking_send(RoomCommand::RecordingStatus { state: job });
                }
            })?;
        Ok(Self {
            id,
            start,
            tx,
            shared,
        })
    }
    /// Requests finalization without waiting for queue drainage or FFmpeg.
    pub fn stop(&self) {
        if !self.shared.stop.load(Ordering::Acquire) {
            self.shared
                .stop_ms
                .store(self.start.elapsed().as_millis() as u64, Ordering::Release);
            self.shared.stop.store(true, Ordering::Release);
        }
    }
    /// Marks an unsupported codec as an explicit recording failure.
    pub fn unsupported(&self) {
        self.shared.failure.store(2, Ordering::Release);
    }
    /// Enqueues a frame without blocking; overflow fails the recording, not the call.
    pub fn frame(&self, frame: RecordedFrame) {
        let bytes = frame.data.len() + 512;
        self.enqueue(bytes, Input::Frame(frame));
    }
    /// Records only durable workspace events; laser and cursor events never enter history.
    pub fn workspace(&self, state: &Value) {
        let bytes = state.to_string().len();
        self.enqueue(
            bytes,
            Input::Workspace(self.start.elapsed().as_millis() as u64, state.clone()),
        );
    }
    fn enqueue(&self, bytes: usize, input: Input) {
        if self.shared.stop.load(Ordering::Acquire)
            || self.shared.finished.load(Ordering::Acquire)
            || self.shared.failure.load(Ordering::Acquire) != 0
        {
            return;
        }
        if self
            .shared
            .queued_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|v| *v <= QUEUE_BYTES)
            })
            .is_err()
        {
            self.shared.failure.store(1, Ordering::Release);
            return;
        }
        if self.tx.try_send((bytes, input)).is_err() {
            self.shared.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
            self.shared.failure.store(1, Ordering::Release);
        }
    }
}
impl Drop for RecordingHandle {
    fn drop(&mut self) {
        self.stop();
    }
}
struct PrivateTemp(PathBuf);
impl Drop for PrivateTemp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Segment {
    writer: Option<MatroskaWriter>,
    path: PathBuf,
    codec: RecordingCodec,
    layer: String,
    rotation: u16,
    track_id: String,
    participant_id: String,
    name: String,
    source: String,
    start_ms: u64,
    end_ms: u64,
    last_ms: u64,
}
#[derive(Default)]
struct ClockMap {
    offsets: HashMap<String, f64>,
    last: HashMap<String, u64>,
}
impl ClockMap {
    fn timestamp(&mut self, frame: &RecordedFrame) -> u64 {
        let mut ms = frame.arrival_ms;
        if let Some(sender) = frame.sender_ms.filter(|s| s.is_finite()) {
            let offset = self
                .offsets
                .entry(frame.participant_id.clone())
                .or_insert(frame.arrival_ms as f64 - sender);
            let mapped = sender + *offset;
            if mapped >= 0.0 && (mapped - frame.arrival_ms as f64).abs() < 2000.0 {
                ms = mapped.round() as u64;
            }
        }
        let key = format!("{}/{}", frame.participant_id, frame.track_id);
        let last = self.last.entry(key).or_insert(ms);
        ms = ms.max(*last);
        *last = ms;
        ms
    }
}
#[allow(clippy::too_many_arguments)]
fn run(
    config: &RecordingConfig,
    vault: &Arc<Vault>,
    instance: &str,
    id: &str,
    workspace: Value,
    commands: &tokio::sync::mpsc::Sender<RoomCommand>,
    start: Instant,
    started_at_ms: u64,
    rx: Receiver<(usize, Input)>,
    shared: &Arc<Shared>,
) -> Result<()> {
    vault.write_job(instance, id, &recording_job(id, "starting", started_at_ms))?;
    let work = PrivateTemp(create_private_worker_dir(&config.temp_path)?);
    ensure!(
        Command::new(&config.ffmpeg)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success(),
        "FFmpeg is unavailable"
    );
    let notify = |job: Value| -> Result<()> {
        vault.write_job(instance, id, &job)?;
        let _ = commands.blocking_send(RoomCommand::RecordingStatus { state: job });
        Ok(())
    };
    notify(recording_job(id, "recording", started_at_ms))?;
    let event_path = work.0.join("events.ndjson");
    let mut events = BufWriter::new(File::create(&event_path)?);
    serde_json::to_writer(&mut events, &json!({"time_ms":0,"state":workspace}))?;
    events.write_all(b"\n")?;
    let mut segments: Vec<Segment> = Vec::new();
    let mut active: HashMap<String, usize> = HashMap::new();
    let mut clocks = ClockMap::default();
    let mut h264_headers: HashMap<String, Vec<u8>> = HashMap::new();
    let mut total_bytes = 0u64;
    let mut event_bytes = 0usize;
    loop {
        ensure!(
            shared.failure.load(Ordering::Acquire) == 0,
            "recording input overflow or unsupported codec"
        );
        if start.elapsed().as_secs() >= config.max_duration_secs
            && !shared.stop.load(Ordering::Acquire)
        {
            shared
                .stop_ms
                .store(start.elapsed().as_millis() as u64, Ordering::Release);
            shared.stop.store(true, Ordering::Release);
        }
        let item = if shared.stop.load(Ordering::Acquire) {
            rx.try_recv().ok()
        } else {
            match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(v) => Some(v),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => None,
            }
        };
        let Some((bytes, input)) = item else {
            break;
        };
        shared.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
        total_bytes += bytes as u64;
        ensure!(
            total_bytes <= config.max_bytes,
            "recording byte budget exceeded"
        );
        match input {
            Input::Workspace(ms, value) => {
                let row = json!({"time_ms":ms,"change":value});
                let bytes = serde_json::to_vec(&row)?;
                event_bytes += bytes.len();
                ensure!(
                    event_bytes <= 64 * 1024 * 1024,
                    "recording annotation history exceeds 64 MiB"
                );
                events.write_all(&bytes)?;
                events.write_all(b"\n")?;
            }
            Input::Frame(frame) => {
                let key = format!("{}/{}", frame.participant_id, frame.track_id);
                let ms = clocks.timestamp(&frame);
                if frame.codec == RecordingCodec::H264 {
                    if let Some(headers) = super::mux::parameter_sets(&frame.data) {
                        h264_headers.insert(key.clone(), headers);
                    }
                }
                let needs_segment = active.get(&key).is_none_or(|i| {
                    let s = &segments[*i];
                    s.codec != frame.codec
                        || s.layer != frame.layer
                        || s.rotation != frame.rotation
                        || ms.saturating_sub(s.last_ms) > 500
                        || (!frame.contiguous && frame.codec != RecordingCodec::Opus)
                });
                if needs_segment {
                    if let Some(index) = active.remove(&key) {
                        if let Some(writer) = segments[index].writer.take() {
                            writer.finish()?;
                        }
                    }
                    if frame.codec != RecordingCodec::Opus && !frame.keyframe {
                        continue;
                    }
                    ensure!(segments.len() < 4096, "recording segment limit reached");
                    let mut first = Vec::new();
                    if frame.codec == RecordingCodec::H264 {
                        if let Some(headers) = h264_headers.get(&key) {
                            first.extend_from_slice(headers);
                        }
                    }
                    first.extend_from_slice(&frame.data);
                    let path = work.0.join(format!("track-{}.mkv", segments.len()));
                    let writer = MatroskaWriter::new(&path, frame.codec, &first)?;
                    active.insert(key.clone(), segments.len());
                    segments.push(Segment {
                        writer: Some(writer),
                        path,
                        codec: frame.codec,
                        layer: frame.layer.clone(),
                        rotation: frame.rotation,
                        track_id: frame.track_id.clone(),
                        participant_id: frame.participant_id.clone(),
                        name: frame.name.clone(),
                        source: frame.source.clone(),
                        start_ms: ms,
                        end_ms: ms,
                        last_ms: ms,
                    });
                }
                let Some(index) = active.get(&key).copied() else {
                    continue;
                };
                let segment = &mut segments[index];
                segment.writer.as_mut().unwrap().frame(
                    ms - segment.start_ms,
                    &frame.data,
                    frame.keyframe,
                )?;
                segment.last_ms = ms;
                segment.end_ms = ms
                    + if frame.codec == RecordingCodec::Opus {
                        20
                    } else {
                        40
                    };
            }
        }
    }
    events.flush()?;
    events.get_ref().sync_all()?;
    drop(events);
    let duration_ms = shared
        .stop_ms
        .load(Ordering::Acquire)
        .max(start.elapsed().as_millis() as u64)
        .max(1);
    for segment in &mut segments {
        if let Some(writer) = segment.writer.take() {
            writer.finish()?;
        }
    }
    let mut processing = recording_job(id, "processing", started_at_ms);
    processing["duration_ms"] = json!(duration_ms);
    notify(processing)?;
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut audio_sources = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        let output = work.0.join(format!(
            "output-{index}.{}",
            if segment.codec == RecordingCodec::Opus {
                "m4a"
            } else {
                "mp4"
            }
        ));
        let mut args = vec![
            "-fflags".into(),
            "+genpts".into(),
            "-i".into(),
            segment.path.to_string_lossy().into_owned(),
        ];
        if segment.codec == RecordingCodec::Opus {
            args.extend(strings(&["-vn", "-c:a", "aac", "-ar", "48000", "-ac", "2"]));
        } else if segment.codec == RecordingCodec::H264 {
            args.extend(strings(&["-an", "-c:v", "copy"]));
        } else {
            args.extend(strings(&[
                "-an",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "23",
                "-vf",
                "pad=ceil(iw/2)*2:ceil(ih/2)*2",
                "-pix_fmt",
                "yuv420p",
                "-fps_mode",
                "vfr",
            ]));
        }
        args.extend(strings(&[
            "-movflags",
            "+faststart",
            "-avoid_negative_ts",
            "make_zero",
        ]));
        args.push(output.to_string_lossy().into_owned());
        ffmpeg(config, &work.0, &args)?;
        let mime = if segment.codec == RecordingCodec::Opus {
            "audio/mp4"
        } else {
            "video/mp4"
        };
        let file_id = vault.put_file(instance, &output, mime)?;
        let item = json!({"file_id":file_id,"track_id":segment.track_id,"participant_id":segment.participant_id,"name":segment.name,"source":segment.source,"start_ms":segment.start_ms,"end_ms":segment.end_ms.min(duration_ms),"mime":mime,"rotation":segment.rotation});
        if segment.codec == RecordingCodec::Opus {
            audio.push((output, segment.start_ms));
            audio_sources.push(item);
        } else {
            video.push(item);
        }
    }
    let mixed = mix_audio(config, &work.0, audio, duration_ms)?;
    let audio_id = vault.put_file(instance, &mixed, "audio/mp4")?;
    let events_id = vault.put_file(instance, &event_path, "application/x-ndjson")?;
    let mut ready = recording_job(id, "ready", started_at_ms);
    ready["duration_ms"] = json!(duration_ms);
    ready["tracks"] = json!(video);
    ready["audio_sources"] = json!(audio_sources);
    ready["audio_file_id"] = json!(audio_id);
    ready["events_file_id"] = json!(events_id);
    notify(ready)?;
    Ok(())
}
fn recording_job(id: &str, status: &str, started_at_ms: u64) -> Value {
    json!({
        "id": id,
        "kind": "recording",
        "status": status,
        "started_at": started_at_ms / 1000,
        "started_at_ms": started_at_ms,
    })
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).to_owned()).collect()
}
fn ffmpeg(config: &RecordingConfig, work: &Path, args: &[String]) -> Result<()> {
    let log = File::create(work.join("ffmpeg.log"))?;
    let status = Command::new(&config.ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .status()
        .context("cannot run FFmpeg")?;
    if !status.success() {
        bail!("FFmpeg finalization failed ({status})");
    }
    Ok(())
}
fn mix_audio(
    config: &RecordingConfig,
    work: &Path,
    mut inputs: Vec<(PathBuf, u64)>,
    duration_ms: u64,
) -> Result<PathBuf> {
    let duration = format!("{:.3}", duration_ms as f64 / 1000.0);
    if inputs.is_empty() {
        let output = work.join("mixed.m4a");
        let mut args = strings(&[
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=48000:cl=stereo",
            "-t",
            &duration,
            "-c:a",
            "aac",
            "-movflags",
            "+faststart",
        ]);
        args.push(output.to_string_lossy().into_owned());
        ffmpeg(config, work, &args)?;
        return Ok(output);
    }
    let mut round = 0;
    loop {
        let mut mixed = Vec::new();
        for (batch, chunk) in inputs.chunks(16).enumerate() {
            let output = work.join(format!("mix-{round}-{batch}.m4a"));
            let mut args = Vec::new();
            let mut filter = String::new();
            let mut labels = String::new();
            for (index, (path, offset)) in chunk.iter().enumerate() {
                args.extend(strings(&["-i", &path.to_string_lossy()]));
                filter.push_str(&format!("[{index}:a]adelay={offset}:all=1[a{index}];"));
                labels.push_str(&format!("[a{index}]"));
            }
            filter.push_str(&format!("{labels}amix=inputs={}:duration=longest:normalize=0,alimiter=limit=0.95,apad=whole_dur={duration}[out]",chunk.len()));
            args.extend(strings(&[
                "-filter_complex",
                &filter,
                "-map",
                "[out]",
                "-t",
                &duration,
                "-c:a",
                "aac",
                "-movflags",
                "+faststart",
            ]));
            args.push(output.to_string_lossy().into_owned());
            ffmpeg(config, work, &args)?;
            mixed.push((output, 0));
        }
        if mixed.len() == 1 {
            return Ok(mixed.remove(0).0);
        }
        inputs = mixed;
        round += 1;
    }
}
