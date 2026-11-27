use ecochain_rtc_server::storage::{remove_abandoned_worker_dirs, Vault};
use serde_json::json;
use std::{fs, path::PathBuf};

const MARKER: &str = ".ecochain-worker";
const WORKER_ID: &[u8] = b"ecochain-private-worker-v1";

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ecochain-recovery-{}", id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[test]
fn restart_recovers_interrupted_jobs_without_replaying_jira_or_changing_finished_jobs() {
    let root = Temp::new();
    let vault = Vault::open(&root.0, [42; 32], 86400).unwrap();
    let occurrence = id();
    let other_occurrence = id();
    let jobs = [
        ("transcript", "queued", "failed"),
        ("transcript", "running", "failed"),
        ("summary", "queued", "failed"),
        ("summary", "running", "failed"),
        ("recording", "starting", "failed"),
        ("recording", "recording", "failed"),
        ("recording", "processing", "failed"),
        ("jira", "pending", "uncertain"),
        ("jira", "created", "created"),
        ("jira", "draft", "draft"),
        ("jira", "uncertain", "uncertain"),
        ("summary", "confirmed", "confirmed"),
        ("summary", "ready", "ready"),
        ("recording", "failed", "failed"),
    ];
    let mut saved = Vec::new();
    for (index, (kind, status, expected)) in jobs.into_iter().enumerate() {
        let instance = if index % 2 == 0 {
            &occurrence
        } else {
            &other_occurrence
        };
        let id = id();
        let original = json!({"id":id,"kind":kind,"status":status,"operation_key":"keep-me","segments":[{"text":"partial"}]});
        vault.write_job(instance, &id, &original).unwrap();
        saved.push((instance, id, original, expected));
    }
    drop(vault);

    let reopened = Vault::open(&root.0, [42; 32], 86400).unwrap();
    assert_eq!(reopened.recover_interrupted_jobs().unwrap(), 8);
    drop(reopened);
    let recovered = Vault::open(&root.0, [42; 32], 86400).unwrap();
    assert_eq!(recovered.recover_interrupted_jobs().unwrap(), 0);
    for (instance, id, mut original, expected) in saved {
        let actual = recovered.read_job(instance, &id).unwrap();
        if original["status"] != expected {
            original["status"] = json!(expected);
            original["error"] = if expected == "uncertain" {
                json!("creation_result_unconfirmed_no_repeat_post")
            } else {
                json!("interrupted_by_restart")
            };
        }
        assert_eq!(actual, original);
    }
}

#[test]
fn job_count_limit_allows_rewriting_an_existing_job() {
    let root = Temp::new();
    let vault = Vault::open(&root.0, [42; 32], 86400).unwrap();
    let instance = id();
    let first = id();
    let job = json!({"status":"ready","kind":"summary"});
    vault.write_job(&instance, &first, &job).unwrap();
    for _ in 1..256 {
        vault.write_job(&instance, &id(), &job).unwrap();
    }

    let rejected = id();
    let error = vault.write_job(&instance, &rejected, &job).unwrap_err();
    assert!(error.to_string().contains("too many jobs"));
    assert!(vault.read_job(&instance, &rejected).is_err());
    let update = json!({"status":"confirmed","kind":"summary"});
    vault.write_job(&instance, &first, &update).unwrap();
    assert_eq!(vault.read_job(&instance, &first).unwrap(), update);
    assert_eq!(vault.jobs(&instance).unwrap().len(), 256);
    vault.write_job(&id(), &rejected, &job).unwrap();
}

#[test]
fn aggregate_metadata_limit_counts_growth_without_double_counting_replacements() {
    let root = Temp::new();
    let vault = Vault::open(&root.0, [42; 32], 86400).unwrap();
    let instance = id();
    let first = id();
    let large = json!({"text":"x".repeat(3500 * 1024)});
    vault.write_job(&instance, &first, &large).unwrap();
    for _ in 1..4 {
        vault.write_job(&instance, &id(), &large).unwrap();
    }
    vault.write_job(&instance, &first, &large).unwrap();

    let second = id();
    let small = json!({"status":"queued","kind":"transcript"});
    vault.write_job(&instance, &second, &small).unwrap();
    let error = vault.write_job(&instance, &second, &large).unwrap_err();
    assert!(error.to_string().contains("aggregate job metadata"));
    assert_eq!(vault.read_job(&instance, &second).unwrap(), small);
    let error = vault.write_job(&instance, &id(), &large).unwrap_err();
    assert!(error.to_string().contains("aggregate job metadata"));
    vault.write_job(&instance, &first, &small).unwrap();
    vault.write_job(&instance, &second, &large).unwrap();
    assert_eq!(vault.jobs(&instance).unwrap().len(), 5);
}

#[test]
fn quota_reserves_space_for_small_jobs_to_save_a_terminal_status() {
    let root = Temp::new();
    let vault = Vault::open(&root.0, [42; 32], 86400).unwrap();
    let instance = id();
    let pending = id();
    vault
        .write_job(
            &instance,
            &pending,
            &json!({"kind":"recording","status":"starting"}),
        )
        .unwrap();

    // Four encrypted files of 4 MiB minus 256 bytes each leave one small-job slot.
    let filler = json!({"text":"x".repeat(4 * 1024 * 1024 - 363)});
    for _ in 0..4 {
        vault.write_job(&instance, &id(), &filler).unwrap();
    }
    let error = vault
        .write_job(
            &instance,
            &id(),
            &json!({"kind":"summary","status":"queued"}),
        )
        .unwrap_err();
    assert!(error.to_string().contains("aggregate job metadata"));
    let failed = json!({"kind":"recording","status":"failed","error":"recording_failed"});
    vault.write_job(&instance, &pending, &failed).unwrap();
    assert_eq!(vault.read_job(&instance, &pending).unwrap(), failed);
}

#[test]
fn restart_recovers_a_transcript_at_the_single_job_size_limit() {
    let root = Temp::new();
    let vault = Vault::open(&root.0, [42; 32], 86400).unwrap();
    let instance = id();
    let id = id();
    let mut job = json!({"kind":"transcript","status":"running","text":""});
    let bytes_remaining = 4 * 1024 * 1024 - serde_json::to_vec(&job).unwrap().len();
    job["text"] = json!("x".repeat(bytes_remaining));
    vault.write_job(&instance, &id, &job).unwrap();

    assert_eq!(vault.recover_interrupted_jobs().unwrap(), 1);
    let recovered = vault.read_job(&instance, &id).unwrap();
    assert_eq!(recovered["status"], "failed");
    assert_eq!(recovered["text"], job["text"]);
}

#[cfg(unix)]
#[test]
fn cleanup_only_removes_private_worker_directories_with_an_exact_regular_marker() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let root = Temp::new();
    let workers = root.0.join("work");
    fs::create_dir(&workers).unwrap();
    let marked_dir = |name: &str, mode, marker: Option<&[u8]>| {
        let dir = workers.join(name);
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(mode)).unwrap();
        if let Some(bytes) = marker {
            fs::write(dir.join(MARKER), bytes).unwrap();
        }
        fs::write(dir.join("fixture"), b"temporary worker data").unwrap();
        dir
    };
    let abandoned = marked_dir(&id(), 0o700, Some(WORKER_ID));
    let missing = marked_dir(&id(), 0o700, None);
    let mismatch = marked_dir(&id(), 0o700, Some(b"ecochain-private-worker-v1\n"));
    let public = marked_dir(&id(), 0o755, Some(WORKER_ID));
    let arbitrary_name = marked_dir("unrelated", 0o700, Some(WORKER_ID));
    let marker_symlink = marked_dir(&id(), 0o700, None);
    symlink(arbitrary_name.join(MARKER), marker_symlink.join(MARKER)).unwrap();
    let dir_symlink = workers.join(id());
    symlink(&arbitrary_name, &dir_symlink).unwrap();
    symlink(&arbitrary_name, abandoned.join("must-not-follow")).unwrap();

    assert_eq!(remove_abandoned_worker_dirs(&workers).unwrap(), 1);
    assert_eq!(remove_abandoned_worker_dirs(&workers).unwrap(), 0);
    assert_eq!(
        remove_abandoned_worker_dirs(&workers.join("missing")).unwrap(),
        0
    );
    assert!(!abandoned.exists());
    for preserved in [
        missing,
        mismatch,
        public,
        arbitrary_name,
        marker_symlink,
        dir_symlink,
    ] {
        assert_eq!(
            fs::read(preserved.join("fixture")).unwrap(),
            b"temporary worker data"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn recording_uses_marked_workdir_and_persists_capture_epoch() {
    use ecochain_rtc_server::{
        config::RecordingConfig, recording::RecordingHandle, room::RoomCommand,
    };
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    let root = Temp::new();
    let vault = Vault::open(root.0.join("vault"), [42; 32], 86400).unwrap();
    let instance = id();
    let work = root.0.join("work");
    let mock = root.0.join("mock-ffmpeg");
    fs::write(&mock, "#!/bin/sh\n[ \"$1\" = \"-version\" ]\n").unwrap();
    fs::set_permissions(&mock, fs::Permissions::from_mode(0o700)).unwrap();
    let epoch_ms = || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    };
    let before = epoch_ms();
    let (send, mut receive) = tokio::sync::mpsc::channel(8);
    let handle = RecordingHandle::start(
        RecordingConfig {
            ffmpeg: mock,
            temp_path: work.clone(),
            max_duration_secs: 60,
            max_bytes: 1024 * 1024,
        },
        vault.clone(),
        instance.clone(),
        json!({}),
        send,
    )
    .unwrap();
    let RoomCommand::RecordingStatus { state } =
        tokio::time::timeout(Duration::from_secs(5), receive.recv())
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("expected recording status");
    };
    assert_eq!(state["status"], "recording");
    let started_at_ms = state["started_at_ms"].as_u64().unwrap();
    assert!((before..=epoch_ms()).contains(&started_at_ms));
    assert_eq!(state["started_at"], started_at_ms / 1000);
    assert_eq!(vault.read_job(&instance, &handle.id).unwrap(), state);
    let worker = fs::read_dir(&work).unwrap().next().unwrap().unwrap().path();
    assert_eq!(
        fs::metadata(&worker).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(fs::read(worker.join(MARKER)).unwrap(), WORKER_ID);

    handle.stop();
    let failed = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(command) = receive.recv().await {
            if let RoomCommand::RecordingStatus { state } = command {
                if state["status"] == "failed" {
                    return state;
                }
            }
        }
        panic!("expected mocked finalization failure");
    })
    .await
    .unwrap();
    assert_eq!(failed["started_at_ms"], started_at_ms);
    assert_eq!(vault.read_job(&instance, &handle.id).unwrap(), failed);
    assert!(!worker.exists());
}
