//! Host-reviewed processing jobs and a durable Jira outbox.
use super::{
    jira::{self, JiraCreateError, JiraPreview},
    ParticipantLabel, SummaryConfirmation, SummaryDraft, TranscriptSegment,
};
use crate::{archive, protocol::ErrorCode, room::RoomCommand, storage::Vault, App};
use anyhow::{ensure, Context, Result};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};

/// Processing routes require management scope; reads remain archive-scoped.
pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/v1/archives/{instance}/transcripts", post(transcript))
        .route("/v1/archives/{instance}/summaries", post(summary))
        .route(
            "/v1/archives/{instance}/summaries/confirm",
            post(confirm_summary),
        )
        .route("/v1/archives/{instance}/jira/projects", get(projects))
        .route("/v1/archives/{instance}/jira/preview", post(preview))
        .route("/v1/archives/{instance}/jira/{id}/submit", post(submit))
}
fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error":message}))).into_response()
}
async fn guard(app: &App, key: String) -> tokio::sync::OwnedMutexGuard<()> {
    let lock = {
        let mut locks = app.job_locks.lock().unwrap();
        locks.retain(|_, v| v.strong_count() > 0);
        let lock = locks
            .get(&key)
            .and_then(std::sync::Weak::upgrade)
            .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(())));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    };
    lock.lock_owned().await
}
async fn read_job(vault: Arc<Vault>, instance: String, id: String) -> Result<Value> {
    tokio::task::spawn_blocking(move || vault.read_job(&instance, &id)).await?
}
async fn write_job(vault: Arc<Vault>, instance: String, id: String, value: Value) -> Result<()> {
    tokio::task::spawn_blocking(move || vault.write_job(&instance, &id, &value)).await?
}
async fn read_archive(vault: Arc<Vault>, instance: String) -> Result<crate::storage::Archive> {
    tokio::task::spawn_blocking(move || vault.read(&instance)).await?
}
fn stable_id(kind: &str, instance: &str, source: &str) -> String {
    let digest = Sha256::digest(format!("{kind}/{instance}/{source}"));
    uuid::Uuid::from_bytes(digest[..16].try_into().unwrap()).to_string()
}
#[derive(Deserialize)]
struct TranscriptRequest {
    recording_id: String,
}
async fn transcript(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(instance): Path<String>,
    Json(body): Json<TranscriptRequest>,
) -> Response {
    if let Err(e) = archive::authorized_manage(&app, &headers, &instance).await {
        return e;
    }
    let Some(stt) = app.config.providers.stt.clone() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "STT provider not configured",
        );
    };
    let Some(worker) = app.config.recording.clone() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "audio worker not configured",
        );
    };
    let vault = app.vault.clone().unwrap();
    let id = stable_id("transcript", &instance, &body.recording_id);
    let _guard = guard(&app, format!("{instance}/{id}")).await;
    if let Ok(job) = read_job(vault.clone(), instance.clone(), id.clone()).await {
        if ["queued", "running", "ready"]
            .iter()
            .any(|s| job["status"] == *s)
        {
            return Json(job).into_response();
        }
    }
    let recording = match read_job(vault.clone(), instance.clone(), body.recording_id.clone()).await
    {
        Ok(job) if job["kind"] == "recording" && job["status"] == "ready" => job,
        _ => return error(StatusCode::BAD_REQUEST, "ready recording required"),
    };
    let permit = match app.provider_slots.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "processing capacity is busy",
            )
        }
    };
    let job = json!({"id":id,"kind":"transcript","mode":"recording","recording_id":body.recording_id,"status":"queued","segments":[],"started_at_ms":recording["started_at_ms"]});
    if write_job(vault.clone(), instance.clone(), id.clone(), job.clone())
        .await
        .is_err()
    {
        return error(StatusCode::SERVICE_UNAVAILABLE, "job could not be saved");
    }
    tokio::spawn(async move {
        let _permit = permit;
        let mut segments = Vec::new();
        let result = batch_transcript(
            &vault,
            &instance,
            &id,
            &recording,
            &worker,
            &stt,
            &mut segments,
        )
        .await;
        let terminal = json!({"id":id,"kind":"transcript","mode":"recording","recording_id":body.recording_id,"status":if result.is_ok(){"ready"}else{"failed"},"segments":segments,"started_at_ms":recording["started_at_ms"],"error":if result.is_ok(){Value::Null}else{json!("transcription_failed")}});
        if let Err(error) = result {
            tracing::warn!(error=%error,"recording transcription failed");
        }
        if write_job(vault.clone(), instance.clone(), id.clone(), terminal)
            .await
            .is_err()
        {
            let _=write_job(vault,instance,id.clone(),json!({"id":id,"kind":"transcript","status":"failed","error":"transcript_storage_limit","segments":[]})).await;
        }
    });
    (StatusCode::ACCEPTED, Json(job)).into_response()
}
struct WorkDir(PathBuf);
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
async fn batch_transcript(
    vault: &Arc<Vault>,
    instance: &str,
    id: &str,
    recording: &Value,
    worker: &crate::config::RecordingConfig,
    stt: &super::SttConfig,
    segments: &mut Vec<TranscriptSegment>,
) -> Result<()> {
    let root = worker.temp_path.clone();
    let work = tokio::task::spawn_blocking(move || {
        let path = crate::storage::create_private_worker_dir(&root)?;
        Ok::<_, anyhow::Error>(WorkDir(path))
    })
    .await??;
    let client = super::client()?;
    let sources = recording["audio_sources"]
        .as_array()
        .context("recording audio sources missing")?;
    ensure!(sources.len() <= 4096, "too many audio segments");
    let mut text_bytes = 0;
    for (index, source) in sources.iter().enumerate() {
        let file = source["file_id"]
            .as_str()
            .context("audio file id missing")?
            .to_owned();
        let start = source["start_ms"].as_u64().context("audio start missing")?;
        let end = source["end_ms"].as_u64().context("audio end missing")?;
        if end <= start {
            continue;
        }
        let input = work.0.join(format!("source-{index}.m4a"));
        let output = input.clone();
        let v = vault.clone();
        let room = instance.to_owned();
        tokio::task::spawn_blocking(move || {
            let (length, _) = v.stream_metadata(&room, &file)?;
            let mut target = File::create(output)?;
            let mut offset = 0;
            while offset < length {
                let count = (length - offset).min(1024 * 1024) as usize;
                target.write_all(&v.read_range(&room, &file, offset, count)?)?;
                offset += count as u64;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        let mut offset = 0u64;
        while offset < end - start {
            let duration = (end - start - offset).min(15000);
            let wav = work.0.join("chunk.wav");
            let source_path = input.clone();
            let target = wav.clone();
            let executable = worker.ffmpeg.clone();
            let bytes = tokio::task::spawn_blocking(move || {
                let status = Command::new(executable)
                    .args([
                        "-nostdin",
                        "-hide_banner",
                        "-loglevel",
                        "error",
                        "-y",
                        "-ss",
                        &format!("{:.3}", offset as f64 / 1000.0),
                        "-i",
                    ])
                    .arg(source_path)
                    .args([
                        "-t",
                        &format!("{:.3}", duration as f64 / 1000.0),
                        "-ar",
                        "16000",
                        "-ac",
                        "1",
                        "-c:a",
                        "pcm_s16le",
                    ])
                    .arg(&target)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()?;
                ensure!(status.success(), "audio chunk extraction failed");
                let bytes = fs::read(target)?;
                ensure!(bytes.len() <= 2 * 1024 * 1024, "audio chunk too large");
                Ok::<_, anyhow::Error>(bytes)
            })
            .await??;
            let added = super::speech::transcribe(
                &client,
                stt,
                bytes,
                "recording.wav",
                start + offset,
                duration,
                source["participant_id"].as_str().unwrap_or(""),
                source["name"].as_str().unwrap_or("Noma’lum"),
            )
            .await?;
            for segment in added {
                text_bytes += segment.text.len();
                ensure!(
                    text_bytes <= 1024 * 1024 && segments.len() < 10000,
                    "transcript limit reached"
                );
                segments.push(segment);
            }
            segments.sort_by_key(|s| s.start_ms);
            write_job(vault.clone(),instance.to_owned(),id.to_owned(),json!({"id":id,"kind":"transcript","mode":"recording","recording_id":recording["id"],"status":"running","started_at_ms":recording["started_at_ms"],"segments":segments})).await?;
            offset += duration;
        }
        let path = input;
        let _ = tokio::task::spawn_blocking(move || fs::remove_file(path)).await;
    }
    Ok(())
}
#[derive(Deserialize)]
struct SummaryRequest {
    transcript_job_id: Option<String>,
}
async fn summary(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(instance): Path<String>,
    Json(body): Json<SummaryRequest>,
) -> Response {
    if let Err(e) = archive::authorized_manage(&app, &headers, &instance).await {
        return e;
    }
    let Some(config) = app.config.providers.summary.clone() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "summary provider not configured",
        );
    };
    let vault = app.vault.clone().unwrap();
    let archive = match read_archive(vault.clone(), instance.clone()).await {
        Ok(a) => a,
        Err(_) => return error(StatusCode::NOT_FOUND, "archive not found"),
    };
    let segments: Vec<TranscriptSegment> = if let Some(source) = &body.transcript_job_id {
        match read_job(vault.clone(), instance.clone(), source.clone()).await {
            Ok(job) if job["kind"] == "transcript" && job["status"] == "ready" => {
                match serde_json::from_value(job["segments"].clone()) {
                    Ok(s) => s,
                    Err(_) => return error(StatusCode::BAD_REQUEST, "invalid transcript"),
                }
            }
            _ => return error(StatusCode::BAD_REQUEST, "ready transcript required"),
        }
    } else {
        ["notes", "decisions", "tasks"]
            .into_iter()
            .flat_map(|key| archive.state[key].as_array().into_iter().flatten())
            .filter_map(|item| item["text"].as_str())
            .map(|text| TranscriptSegment {
                start_ms: 0,
                end_ms: 0,
                participant_id: String::new(),
                name: "Uchrashuv qaydlari".into(),
                text: text.to_owned(),
            })
            .collect()
    };
    if segments.is_empty() || segments.iter().map(|s| s.text.len()).sum::<usize>() > 1024 * 1024 {
        return error(
            StatusCode::BAD_REQUEST,
            "summary input must be 1 byte..1 MiB",
        );
    }
    let permit = match app.provider_slots.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "processing capacity is busy",
            )
        }
    };
    let id = uuid::Uuid::new_v4().to_string();
    let job = json!({"id":id,"kind":"summary","status":"queued","source_revision":archive.state["revision"],"transcript_job_id":body.transcript_job_id});
    if write_job(vault.clone(), instance.clone(), id.clone(), job.clone())
        .await
        .is_err()
    {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "summary job could not be saved",
        );
    }
    let people = archive
        .members
        .iter()
        .map(|identity| ParticipantLabel {
            identity: identity.clone(),
            name: identity.clone(),
        })
        .collect::<Vec<_>>();
    let source_revision = archive.state["revision"].clone();
    tokio::spawn(async move {
        let _permit = permit;
        let result = make_summary(config, &segments, &people).await;
        let terminal = match result {
            Ok(draft) => {
                json!({"id":id,"kind":"summary","status":"draft","source_revision":source_revision,"draft":draft})
            }
            Err(error) => {
                tracing::warn!(error=%error,"summary generation failed");
                json!({"id":id,"kind":"summary","status":"failed","error":"summary_failed"})
            }
        };
        if let Err(error) = write_job(vault.clone(), instance.clone(), id.clone(), terminal).await {
            tracing::warn!(error=%error,"summary terminal save failed");
            if let Err(error) = write_job(
                vault,
                instance,
                id.clone(),
                json!({"id":id,"kind":"summary","status":"failed","error":"summary_storage_limit"}),
            )
            .await
            {
                tracing::error!(error=%error,"summary terminal failure could not be saved");
            }
        }
    });
    (StatusCode::ACCEPTED, Json(job)).into_response()
}
async fn make_summary(
    config: super::SummaryConfig,
    segments: &[TranscriptSegment],
    people: &[ParticipantLabel],
) -> Result<SummaryDraft> {
    let client = super::client()?;
    let mut groups: Vec<Vec<TranscriptSegment>> = Vec::new();
    let mut group = Vec::new();
    let mut bytes = 0;
    for segment in segments {
        let size = serde_json::to_vec(segment)?.len();
        ensure!(size <= 48 * 1024, "utterance too large");
        if bytes + size > 48 * 1024 && !group.is_empty() {
            groups.push(std::mem::take(&mut group));
            bytes = 0;
        }
        group.push(segment.clone());
        bytes += size;
    }
    if !group.is_empty() {
        groups.push(group);
    }
    let mut drafts = Vec::new();
    for group in groups {
        let draft = super::speech::summarize(&client, &config, &group, people).await?;
        ensure!(
            serde_json::to_vec(&draft)?.len() <= 64 * 1024,
            "summary is too verbose for aggregation"
        );
        drafts.push(draft);
    }
    while drafts.len() > 1 {
        let mut reduced = Vec::new();
        for pair in drafts.chunks(2) {
            if pair.len() == 1 {
                reduced.push(pair[0].clone());
                continue;
            }
            let rows = pair
                .iter()
                .map(|draft| {
                    Ok(TranscriptSegment {
                        start_ms: 0,
                        end_ms: 0,
                        participant_id: String::new(),
                        name: "Qisman xulosa (loyiha)".into(),
                        text: serde_json::to_string(draft)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let draft = super::speech::summarize(&client, &config, &rows, people).await?;
            ensure!(
                serde_json::to_vec(&draft)?.len() <= 64 * 1024,
                "summary aggregation too large"
            );
            reduced.push(draft);
        }
        drafts = reduced;
    }
    drafts.pop().context("empty summary")
}
#[derive(Deserialize)]
struct ConfirmRequest {
    confirmed: bool,
    confirmation: SummaryConfirmation,
}
async fn confirm_summary(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(instance): Path<String>,
    Json(body): Json<ConfirmRequest>,
) -> Response {
    let manager = match archive::authorized_manage(&app, &headers, &instance).await {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !body.confirmed {
        return error(
            StatusCode::BAD_REQUEST,
            "explicit draft confirmation required",
        );
    }
    let vault = app.vault.clone().unwrap();
    let _guard = guard(&app, format!("{instance}/{}", body.confirmation.job_id)).await;
    let mut job = match read_job(
        vault.clone(),
        instance.clone(),
        body.confirmation.job_id.clone(),
    )
    .await
    {
        Ok(j)
            if j["kind"] == "summary"
                && ["draft", "confirmed"].iter().any(|s| j["status"] == *s) =>
        {
            j
        }
        _ => return error(StatusCode::BAD_REQUEST, "summary draft not found"),
    };
    let archive = match read_archive(vault.clone(), instance.clone()).await {
        Ok(a) => a,
        Err(_) => return error(StatusCode::NOT_FOUND, "archive unavailable"),
    };
    if let Err(error) = super::confirmed_snapshot(
        &archive.state,
        &archive.members,
        &manager.sub,
        &body.confirmation,
    ) {
        return error_response(error.code, &error.message);
    }
    let mut applied_live = false;
    if let Some(room) = archive.state["room_id"]
        .as_str()
        .and_then(|id| app.rooms.get(id))
    {
        let (reply, result) = tokio::sync::oneshot::channel();
        if room
            .send(RoomCommand::ConfirmSummary {
                instance: instance.clone(),
                identity: manager.sub.clone(),
                confirmation: body.confirmation.clone(),
                reply,
            })
            .await
        {
            match tokio::time::timeout(Duration::from_secs(15), result).await {
                Ok(Ok(Ok(_))) => applied_live = true,
                Ok(Ok(Err(e))) if e.code == ErrorCode::RoomNotFound => {}
                Ok(Ok(Err(e))) => return error_response(e.code, &e.message),
                _ => {
                    return error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "summary confirmation interrupted; refresh before retry",
                    )
                }
            }
        }
    }
    if !applied_live {
        let v = vault.clone();
        let room = instance.clone();
        let identity = manager.sub;
        let confirmation = body.confirmation.clone();
        if !matches!(
            tokio::task::spawn_blocking(move || {
                v.update_archive(&room, |archive| {
                    archive.state = super::confirmed_snapshot(
                        &archive.state,
                        &archive.members,
                        &identity,
                        &confirmation,
                    )
                    .map_err(|e| anyhow::anyhow!(e.message))?;
                    Ok(())
                })
            })
            .await,
            Ok(Ok(_))
        ) {
            return error(
                StatusCode::CONFLICT,
                "workspace changed; refresh and review again",
            );
        }
    }
    job["status"] = json!("confirmed");
    job["draft"] = serde_json::to_value(body.confirmation.draft).unwrap();
    if write_job(vault, instance, body.confirmation.job_id, job.clone())
        .await
        .is_err()
    {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "confirmation stored; refresh before retry",
        );
    }
    Json(job).into_response()
}
fn error_response(code: ErrorCode, message: &str) -> Response {
    error(
        match code {
            ErrorCode::InvalidState => StatusCode::CONFLICT,
            ErrorCode::Forbidden => StatusCode::FORBIDDEN,
            ErrorCode::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::BAD_REQUEST,
        },
        message,
    )
}
async fn projects(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(instance): Path<String>,
) -> Response {
    if let Err(e) = archive::authorized_manage(&app, &headers, &instance).await {
        return e;
    }
    let Some(config) = &app.config.providers.jira else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "Jira not configured");
    };
    let _permit = match app.provider_slots.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "provider capacity busy"),
    };
    let client = match super::client() {
        Ok(c) => c,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "provider unavailable"),
    };
    match jira::projects(&client, config).await {
        Ok(projects) => {
            Json(json!({"projects":projects,"issue_type":config.issue_type_name})).into_response()
        }
        Err(_) => error(StatusCode::BAD_GATEWAY, "Jira projects unavailable"),
    }
}
async fn preview(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(instance): Path<String>,
    Json(body): Json<JiraPreview>,
) -> Response {
    if let Err(e) = archive::authorized_manage(&app, &headers, &instance).await {
        return e;
    }
    let Some(config) = &app.config.providers.jira else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "Jira not configured");
    };
    let vault = app.vault.clone().unwrap();
    let archive = match read_archive(vault.clone(), instance.clone()).await {
        Ok(a) => a,
        Err(_) => return error(StatusCode::NOT_FOUND, "archive unavailable"),
    };
    if !archive.state["tasks"]
        .as_array()
        .is_some_and(|tasks| tasks.iter().any(|task| task["id"] == body.task_id))
    {
        return error(StatusCode::BAD_REQUEST, "existing meeting task required");
    }
    let payload = match jira::build_payload(config, &instance, &body) {
        Ok(p) => p,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid Jira task fields"),
    };
    let key = jira::operation_key(&instance, &body.task_id);
    let id = stable_id("jira", &instance, &body.task_id);
    let _guard = guard(&app, format!("{instance}/{id}")).await;
    match read_job(vault.clone(), instance.clone(), id.clone()).await {
        Ok(job)
            if ["pending", "uncertain", "created"]
                .iter()
                .any(|s| job["status"] == *s) =>
        {
            return Json(job).into_response()
        }
        Ok(_) => {}
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "existing Jira outbox unavailable; preview preserved",
            )
        }
    }
    let job = json!({"id":id,"kind":"jira","status":"draft","task_id":body.task_id,"project_key":body.project_key,"preview":body,"preview_hash":format!("{:x}",Sha256::digest(serde_json::to_vec(&json!({"origin":config.base_url.trim_end_matches('/'),"payload":payload})).unwrap())),"payload":payload,"operation_key":key,"origin":config.base_url.trim_end_matches('/'),"issue_type":config.issue_type_name});
    match write_job(vault, instance, id, job.clone()).await {
        Ok(()) => Json(job).into_response(),
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Jira preview could not be saved",
        ),
    }
}
#[derive(Deserialize)]
struct SubmitRequest {
    confirmed: bool,
    preview_hash: String,
}
async fn submit(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path((instance, id)): Path<(String, String)>,
    Json(body): Json<SubmitRequest>,
) -> Response {
    if let Err(e) = archive::authorized_manage(&app, &headers, &instance).await {
        return e;
    }
    if !body.confirmed {
        return error(
            StatusCode::BAD_REQUEST,
            "explicit Jira preview confirmation required",
        );
    }
    let Some(config) = &app.config.providers.jira else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "Jira not configured");
    };
    let vault = app.vault.clone().unwrap();
    let _guard = guard(&app, format!("{instance}/{id}")).await;
    let mut job = match read_job(vault.clone(), instance.clone(), id.clone()).await {
        Ok(j) if j["kind"] == "jira" => j,
        _ => return error(StatusCode::NOT_FOUND, "Jira preview not found"),
    };
    if job["preview_hash"].as_str() != Some(body.preview_hash.as_str()) {
        return error(
            StatusCode::CONFLICT,
            "Jira preview changed; review the current fields again",
        );
    }
    if job["status"] == "created" {
        return Json(job).into_response();
    }
    if job["origin"].as_str() != Some(config.base_url.trim_end_matches('/')) {
        return error(
            StatusCode::CONFLICT,
            "Jira destination changed; review a new preview",
        );
    }
    let client = match super::client() {
        Ok(c) => c,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "provider unavailable"),
    };
    let _permit = match app.provider_slots.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "provider capacity busy"),
    };
    if job["status"] == "pending" || job["status"] == "uncertain" {
        match jira::find_existing(
            &client,
            config,
            job["project_key"].as_str().unwrap_or(""),
            &instance,
            job["task_id"].as_str().unwrap_or(""),
        )
        .await
        {
            Ok(Some(issue)) => {
                job["status"] = json!("created");
                job.as_object_mut().unwrap().remove("error");
                job["issue"] = serde_json::to_value(issue).unwrap();
            }
            _ => {
                job["status"] = json!("uncertain");
                job["error"] = json!("creation_result_unconfirmed_no_repeat_post");
            }
        }
    } else if job["status"] == "draft" {
        job["status"] = json!("pending");
        if write_job(vault.clone(), instance.clone(), id.clone(), job.clone())
            .await
            .is_err()
        {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Jira outbox could not be saved",
            );
        }
        match jira::create_issue(&client, config, &job["payload"]).await {
            Ok(issue) => {
                job["status"] = json!("created");
                job.as_object_mut().unwrap().remove("error");
                job["issue"] = serde_json::to_value(issue).unwrap();
            }
            Err(JiraCreateError::Definitive(_)) => {
                job["status"] = json!("failed");
                job["error"] = json!("jira_rejected_request");
            }
            Err(JiraCreateError::Uncertain(_)) => {
                job["status"] = json!("uncertain");
                job["error"] = json!("creation_result_unconfirmed_no_repeat_post");
            }
        }
    } else {
        return error(
            StatusCode::CONFLICT,
            "review a corrected preview before resubmitting",
        );
    }
    if write_job(vault, instance, id, job.clone()).await.is_err() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Jira result not confirmed locally; refresh before retry",
        );
    }
    (
        if job["status"] == "uncertain" {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        },
        Json(job),
    )
        .into_response()
}
