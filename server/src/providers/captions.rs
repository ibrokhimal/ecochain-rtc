//! Bounded live captions. Only short, private Opus chunks leave the spool for STT.
use super::{SttConfig, TranscriptSegment};
use crate::{
    config::RecordingConfig,
    recording::{mux::MatroskaWriter, RecordingCodec},
    room::RoomCommand,
    storage::Vault,
};
use anyhow::{ensure, Result};
use futures_util::{stream::FuturesUnordered, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, File},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    time::{Duration, Instant},
};

/// One audible Opus frame, already authorized by the SFU's mute/audio selector.
pub struct CaptionFrame {
    pub participant_id: String,
    pub name: String,
    pub time_ms: u64,
    pub data: Arc<[u8]>,
}
struct Shared {
    stop: AtomicBool,
    failed: AtomicBool,
    bytes: AtomicUsize,
}
/// Actor-side nonblocking caption capture handle.
pub struct CaptionHandle {
    pub id: String,
    pub start: Instant,
    tx: SyncSender<(usize, CaptionFrame)>,
    shared: Arc<Shared>,
}
impl CaptionHandle {
    /// Starts a short-chunk spool and a four-request HTTP supervisor.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        config: RecordingConfig,
        stt: SttConfig,
        vault: Arc<Vault>,
        instance: String,
        commands: tokio::sync::mpsc::Sender<RoomCommand>,
        slots: Arc<tokio::sync::Semaphore>,
    ) -> Result<Self> {
        let id = uuid::Uuid::new_v4().to_string();
        let start = Instant::now();
        let epoch_ms = epoch_ms();
        let (tx, rx) = mpsc::sync_channel(256);
        let (chunks, chunk_rx) = tokio::sync::mpsc::channel(8);
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            bytes: AtomicUsize::new(0),
        });
        let worker_shared = shared.clone();
        let task_shared = shared.clone();
        let worker_config = config.clone();
        let task_id = id.clone();
        tokio::spawn(async move {
            let mut segments = Vec::new();
            let result = supervise(
                stt,
                &vault,
                &instance,
                &task_id,
                epoch_ms,
                &commands,
                slots,
                chunk_rx,
                &mut segments,
            )
            .await;
            if result.is_err() {
                task_shared.failed.store(true, Ordering::Release);
                tracing::warn!("live transcription failed");
            }
            task_shared.stop.store(true, Ordering::Release);
            let job = json!({"id":task_id,"kind":"transcript","mode":"live","status":if result.is_ok(){"ready"}else{"failed"},"started_at_ms":epoch_ms,"segments":segments,"error":if result.is_ok(){Value::Null}else{json!("transcription_failed")}});
            let v = vault.clone();
            let room = instance.clone();
            let job_id = task_id.clone();
            let stored = job.clone();
            let saved =
                tokio::task::spawn_blocking(move || v.write_job(&room, &job_id, &stored)).await;
            let job = if matches!(saved, Ok(Ok(()))) {
                job
            } else {
                let failed = json!({"id":task_id,"kind":"transcript","mode":"live","status":"failed","started_at_ms":epoch_ms,"segments":[],"error":"transcript_storage_limit"});
                let v = vault.clone();
                let room = instance.clone();
                let job_id = task_id.clone();
                let stored = failed.clone();
                let _ =
                    tokio::task::spawn_blocking(move || v.write_job(&room, &job_id, &stored)).await;
                failed
            };
            let status = json!({"id":task_id,"status":job["status"],"started_at_ms":epoch_ms,"error":job["error"]});
            let _ = commands
                .send(RoomCommand::CaptionsStatus { state: status })
                .await;
        });
        std::thread::Builder::new()
            .name("ecochain-captions".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    spool(&worker_config, start, rx, &chunks, &worker_shared)
                }));
                let success = matches!(result, Ok(Ok(())));
                if !success {
                    worker_shared.failed.store(true, Ordering::Release);
                }
                let _ = chunks.blocking_send(Message::End(success));
            })?;
        Ok(Self {
            id,
            start,
            tx,
            shared,
        })
    }
    /// Queues an audible frame; input overflow ends captions without affecting media.
    pub fn frame(&self, frame: CaptionFrame) {
        if self.shared.stop.load(Ordering::Acquire) || self.shared.failed.load(Ordering::Acquire) {
            return;
        }
        let bytes = frame.data.len() + 256;
        if self
            .shared
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(bytes).filter(|n| *n <= 8 * 1024 * 1024)
            })
            .is_err()
        {
            self.shared.failed.store(true, Ordering::Release);
            return;
        }
        if self.tx.try_send((bytes, frame)).is_err() {
            self.shared.bytes.fetch_sub(bytes, Ordering::AcqRel);
            self.shared.failed.store(true, Ordering::Release);
        }
    }
    /// Stops capture; already accepted short chunks finish processing.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
    }
}
impl Drop for CaptionHandle {
    fn drop(&mut self) {
        self.stop();
    }
}
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Chunk {
    writer: MatroskaWriter,
    path: PathBuf,
    participant_id: String,
    name: String,
    start_ms: u64,
    end_ms: u64,
}
struct ReadyChunk {
    path: PathBuf,
    participant_id: String,
    name: String,
    start_ms: u64,
    end_ms: u64,
    _temp: Arc<Temp>,
}
enum Message {
    Ready,
    Chunk(ReadyChunk),
    End(bool),
}
fn spool(
    config: &RecordingConfig,
    start: Instant,
    rx: Receiver<(usize, CaptionFrame)>,
    tx: &tokio::sync::mpsc::Sender<Message>,
    shared: &Arc<Shared>,
) -> Result<()> {
    let work = Arc::new(Temp(crate::storage::create_private_worker_dir(
        &config.temp_path,
    )?));
    ensure!(
        Command::new(&config.ffmpeg)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success(),
        "caption FFmpeg unavailable"
    );
    tx.blocking_send(Message::Ready)?;
    let mut active: HashMap<String, Chunk> = HashMap::new();
    let mut sequence = 0u64;
    let mut input_bytes = 0u64;
    loop {
        ensure!(
            !shared.failed.load(Ordering::Acquire),
            "caption queue overflow"
        );
        if start.elapsed().as_secs() >= config.max_duration_secs {
            shared.stop.store(true, Ordering::Release);
        }
        let now = start.elapsed().as_millis() as u64;
        let expired: Vec<_> = active
            .iter()
            .filter(|(_, c)| now.saturating_sub(c.start_ms) >= 6000)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            send_chunk(active.remove(&id).unwrap(), config, tx, work.clone())?;
        }
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok((bytes, frame)) => {
                shared.bytes.fetch_sub(bytes, Ordering::AcqRel);
                input_bytes += bytes as u64;
                ensure!(
                    input_bytes <= config.max_bytes,
                    "caption byte budget exceeded"
                );
                if !active.contains_key(&frame.participant_id) {
                    ensure!(active.len() < 150, "caption source limit exceeded");
                    let path = work.0.join(format!("audio-{sequence}.mkv"));
                    sequence += 1;
                    active.insert(
                        frame.participant_id.clone(),
                        Chunk {
                            writer: MatroskaWriter::new(&path, RecordingCodec::Opus, &frame.data)?,
                            path,
                            participant_id: frame.participant_id.clone(),
                            name: frame.name.clone(),
                            start_ms: frame.time_ms,
                            end_ms: frame.time_ms,
                        },
                    );
                }
                let chunk = active.get_mut(&frame.participant_id).unwrap();
                let ms = frame.time_ms.max(chunk.end_ms.saturating_sub(20));
                chunk.writer.frame(ms - chunk.start_ms, &frame.data, true)?;
                chunk.end_ms = ms + 20;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    // Capture stops immediately; frames still queued at stop were not yet processed.
    while let Ok((bytes, frame)) = rx.try_recv() {
        shared.bytes.fetch_sub(bytes, Ordering::AcqRel);
        input_bytes += bytes as u64;
        ensure!(
            input_bytes <= config.max_bytes,
            "caption byte budget exceeded"
        );
        if !active.contains_key(&frame.participant_id) {
            ensure!(active.len() < 150, "caption source limit exceeded");
            let path = work.0.join(format!("audio-{sequence}.mkv"));
            sequence += 1;
            active.insert(
                frame.participant_id.clone(),
                Chunk {
                    writer: MatroskaWriter::new(&path, RecordingCodec::Opus, &frame.data)?,
                    path,
                    participant_id: frame.participant_id.clone(),
                    name: frame.name.clone(),
                    start_ms: frame.time_ms,
                    end_ms: frame.time_ms,
                },
            );
        }
        let chunk = active.get_mut(&frame.participant_id).unwrap();
        let ms = frame.time_ms.max(chunk.end_ms.saturating_sub(20));
        chunk.writer.frame(ms - chunk.start_ms, &frame.data, true)?;
        chunk.end_ms = ms + 20;
    }
    for (_, chunk) in active {
        send_chunk(chunk, config, tx, work.clone())?;
    }
    Ok(())
}
fn send_chunk(
    chunk: Chunk,
    config: &RecordingConfig,
    tx: &tokio::sync::mpsc::Sender<Message>,
    temp: Arc<Temp>,
) -> Result<()> {
    chunk.writer.finish()?;
    let wav = chunk.path.with_extension("wav");
    let log = File::create(chunk.path.with_extension("log"))?;
    let status = Command::new(&config.ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&chunk.path)
        .args([
            "-af",
            "aresample=async=1:first_pts=0",
            "-ar",
            "16000",
            "-ac",
            "1",
            "-c:a",
            "pcm_s16le",
        ])
        .arg(&wav)
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .status()?;
    ensure!(status.success(), "caption audio conversion failed");
    let _ = fs::remove_file(chunk.path);
    tx.try_send(Message::Chunk(ReadyChunk {
        path: wav,
        participant_id: chunk.participant_id,
        name: chunk.name,
        start_ms: chunk.start_ms,
        end_ms: chunk.end_ms,
        _temp: temp,
    }))
    .map_err(|_| anyhow::anyhow!("caption provider backlog is full"))?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn supervise(
    stt: SttConfig,
    vault: &Arc<Vault>,
    instance: &str,
    id: &str,
    epoch: u64,
    commands: &tokio::sync::mpsc::Sender<RoomCommand>,
    slots: Arc<tokio::sync::Semaphore>,
    mut rx: tokio::sync::mpsc::Receiver<Message>,
    segments: &mut Vec<TranscriptSegment>,
) -> Result<()> {
    let client = super::client()?;
    let mut pending = FuturesUnordered::new();
    let mut ended = false;
    let mut chars = 0usize;
    loop {
        if ended && pending.is_empty() {
            break;
        }
        tokio::select! {
            message=rx.recv(),if !ended&&pending.len()<4=>match message{
                Some(Message::Ready)=>{
                    save_transcript(vault,instance,id,epoch,"running",segments).await?;
                    let _=commands.send(RoomCommand::CaptionsStatus{state:json!({"id":id,"status":"running","started_at_ms":epoch})}).await;
                },
                Some(Message::Chunk(chunk))=>pending.push(transcribe_chunk(client.clone(),stt.clone(),slots.clone(),chunk)),
                Some(Message::End(success))=>{ensure!(success,"caption spool failed");ended=true;},None=>ended=true,
            },
            Some(result)=pending.next(),if !pending.is_empty()=>{
                for segment in result?{
                    ensure!(segment.text.len()<=4000,"caption segment too large");chars+=segment.text.len();ensure!(chars<=1024*1024&&segments.len()<10000,"transcript limit reached");
                    let _=commands.send(RoomCommand::CaptionSegment{job_id:id.to_owned(),segment:serde_json::to_value(&segment)?}).await;segments.push(segment);
                }
                segments.sort_by_key(|s|s.start_ms);save_transcript(vault,instance,id,epoch,"running",segments).await?;
            }
        }
    }
    Ok(())
}
async fn transcribe_chunk(
    client: reqwest::Client,
    stt: SttConfig,
    slots: Arc<tokio::sync::Semaphore>,
    chunk: ReadyChunk,
) -> Result<Vec<TranscriptSegment>> {
    let _permit = tokio::time::timeout(Duration::from_secs(15), slots.acquire_owned()).await??;
    let path = chunk.path.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        let bytes = fs::read(path)?;
        ensure!(bytes.len() <= 2 * 1024 * 1024, "caption chunk too large");
        Ok::<_, anyhow::Error>(bytes)
    })
    .await??;
    let result = super::speech::transcribe(
        &client,
        &stt,
        bytes,
        "caption.wav",
        chunk.start_ms,
        chunk.end_ms.saturating_sub(chunk.start_ms).max(1),
        &chunk.participant_id,
        &chunk.name,
    )
    .await;
    let path = chunk.path.clone();
    let _ = tokio::task::spawn_blocking(move || fs::remove_file(path)).await;
    result
}
async fn save_transcript(
    vault: &Arc<Vault>,
    instance: &str,
    id: &str,
    epoch: u64,
    status: &str,
    segments: &[TranscriptSegment],
) -> Result<()> {
    let job = json!({"id":id,"kind":"transcript","mode":"live","status":status,"started_at_ms":epoch,"segments":segments});
    let v = vault.clone();
    let instance = instance.to_owned();
    let id = id.to_owned();
    tokio::task::spawn_blocking(move || v.write_job(&instance, &id, &job)).await??;
    Ok(())
}
fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
