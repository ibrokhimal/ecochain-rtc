use ecochain_rtc_server::recording::{mux::MatroskaWriter, RecordingCodec};
use std::{fs, path::Path, process::Command};
fn run(program: &str, args: &[&str]) {
    let result = Command::new(program)
        .args(args)
        .output()
        .expect("recording tests require installed FFmpeg tools");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
fn probe(path: &Path) -> serde_json::Value {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
#[test]
#[ignore = "requires FFmpeg with libx264/libvpx and ffprobe"]
fn timestamped_h264_vp8_and_opus_spools_decode_to_playable_media() {
    let dir = std::env::temp_dir().join(format!("ecochain-mux-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    let raw = dir.join("input.h264");
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x48:rate=10",
            "-frames:v",
            "1",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-f",
            "h264",
            raw.to_str().unwrap(),
        ],
    );
    let data = fs::read(raw).unwrap();
    let path = dir.join("h264.mkv");
    let mut writer = MatroskaWriter::new(&path, RecordingCodec::H264, &data).unwrap();
    writer.frame(0, &data, true).unwrap();
    writer.finish().unwrap();
    let info = probe(&path);
    assert_eq!(info["streams"][0]["codec_name"], "h264");
    assert_eq!(info["streams"][0]["width"], 64);
    assert_eq!(info["streams"][0]["height"], 48);
    let out = dir.join("h264.mp4");
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-i",
            path.to_str().unwrap(),
            "-c:v",
            "copy",
            "-movflags",
            "+faststart",
            out.to_str().unwrap(),
        ],
    );
    assert_eq!(probe(&out)["streams"][0]["codec_name"], "h264");
    let raw = dir.join("input.ivf");
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x48:rate=10",
            "-frames:v",
            "4",
            "-c:v",
            "libvpx",
            "-deadline",
            "realtime",
            "-f",
            "ivf",
            raw.to_str().unwrap(),
        ],
    );
    let bytes = fs::read(raw).unwrap();
    let mut pos = 32;
    let mut frames = Vec::new();
    while pos + 12 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 12;
        frames.push(&bytes[pos..pos + len]);
        pos += len;
    }
    let path = dir.join("vp8.mkv");
    let mut writer = MatroskaWriter::new(&path, RecordingCodec::Vp8, frames[0]).unwrap();
    for (i, frame) in frames.iter().enumerate() {
        writer.frame(i as u64 * 100, frame, i == 0).unwrap();
    }
    writer.finish().unwrap();
    let out = dir.join("vp8.mp4");
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-i",
            path.to_str().unwrap(),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
            out.to_str().unwrap(),
        ],
    );
    let info = probe(&out);
    assert_eq!(info["streams"][0]["width"], 64);
    let path = dir.join("audio.mkv");
    let silence = [0xf8, 0xff, 0xfe];
    let mut writer = MatroskaWriter::new(&path, RecordingCodec::Opus, &silence).unwrap();
    for i in 0..20 {
        writer.frame(i * 20, &silence, true).unwrap();
    }
    writer.finish().unwrap();
    let out = dir.join("audio.m4a");
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-i",
            path.to_str().unwrap(),
            "-c:a",
            "aac",
            out.to_str().unwrap(),
        ],
    );
    let info = probe(&out);
    assert_eq!(info["streams"][0]["codec_name"], "aac");
    let duration = info["format"]["duration"]
        .as_str()
        .unwrap()
        .parse::<f64>()
        .unwrap();
    assert!((0.3..0.6).contains(&duration));
    fs::remove_dir_all(dir).unwrap();
}

mod common;
#[tokio::test]
#[ignore = "requires FFmpeg with AAC encoder"]
async fn sfu_worker_finalizes_encrypted_audio_and_annotation_history() {
    use base64::Engine;
    use ecochain_rtc_server::{
        auth::Role,
        bot::{Bot, SignalClient},
        config::{RecordingConfig, Secret, StorageConfig},
    };
    use serde_json::json;
    use std::time::Duration;
    let baseline = common::start("").await;
    let mut config = baseline.config.clone();
    let root = std::env::temp_dir().join(format!("ecochain-recorder-{}", uuid::Uuid::new_v4()));
    config.storage = Some(StorageConfig {
        path: root.clone(),
        master_key: Secret::new(base64::engine::general_purpose::STANDARD.encode([3; 32])),
        retention_secs: 86400,
    });
    config.recording = Some(RecordingConfig {
        ffmpeg: "ffmpeg".into(),
        temp_path: root.join("work"),
        max_duration_secs: 60,
        max_bytes: 1024 * 1024,
    });
    let server = ecochain_rtc_server::start(config.clone()).await.unwrap();
    let url = format!("ws://{}/v1/rtc", server.http_addr);
    let mut host = SignalClient::connect(&url).await.unwrap();
    let joined = host
        .request(
            "join",
            json!({"token":common::token_with(&config,"record","host",Role::Host,3600)}),
        )
        .await
        .unwrap();
    let mut source = baseline.bot("record", "speaker");
    source.url = url;
    source.token = common::token_with(&config, "record", "speaker", Role::Participant, 3600);
    source.video_kbps = None;
    let publisher = Bot::start(source).await.unwrap();
    host.request(
        "meeting.action",
        json!({"action":{"type":"recording.start"}}),
    )
    .await
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let e = host
            .wait_event("recording.changed", Duration::from_secs(3))
            .await
            .unwrap();
        if e["data"]["status"] == "recording" {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
    }
    while server
        .app
        .metrics
        .media_in_total
        .load(std::sync::atomic::Ordering::Relaxed)
        < 10
    {
        assert!(
            std::time::Instant::now() < deadline,
            "publisher did not deliver audio: {:?}",
            publisher.stats()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(450)).await;
    host.request("meeting.action",json!({"action":{"type":"note.put","id":"note","text":"Recorded decision","expected_revision":0}})).await.unwrap();
    host.request(
        "meeting.action",
        json!({"action":{"type":"recording.stop"}}),
    )
    .await
    .unwrap();
    let ready = loop {
        let e = host
            .wait_event("recording.changed", Duration::from_secs(8))
            .await
            .unwrap();
        assert_ne!(e["data"]["status"], "failed");
        if e["data"]["status"] == "ready" {
            break e["data"].clone();
        }
        assert!(std::time::Instant::now() < deadline);
    };
    let vault = server.app.vault.as_ref().unwrap();
    let instance = joined["meeting"]["instance_id"].as_str().unwrap();
    assert!(!ready["audio_sources"].as_array().unwrap().is_empty());
    let id = ready["audio_file_id"].as_str().unwrap();
    let (length, _) = vault.stream_metadata(instance, id).unwrap();
    let bytes = vault.read_range(instance, id, 0, length as usize).unwrap();
    let path = root.join("verified.m4a");
    fs::write(&path, bytes).unwrap();
    assert_eq!(probe(&path)["streams"][0]["codec_name"], "aac");
    let events = ready["events_file_id"].as_str().unwrap();
    let (len, _) = vault.stream_metadata(instance, events).unwrap();
    let bytes = vault.read_range(instance, events, 0, len as usize).unwrap();
    assert!(String::from_utf8(bytes)
        .unwrap()
        .contains("Recorded decision"));
    publisher.leave().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(fs::read_dir(root.join("work")).unwrap().count(), 0);
    fs::remove_dir_all(root).unwrap();
}
