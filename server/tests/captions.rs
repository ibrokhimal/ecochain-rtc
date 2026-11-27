mod common;
use ecochain_rtc_server::{auth::Role, protocol::ErrorCode};
use serde_json::json;
#[tokio::test]
async fn captions_require_host_and_configured_provider() {
    let server = common::start("").await;
    let mut guest = server.signal().await;
    guest
        .request(
            "join",
            json!({"token":server.token("captions","guest",Role::Participant)}),
        )
        .await
        .unwrap();
    assert_eq!(
        guest
            .request(
                "meeting.action",
                json!({"action":{"type":"captions.start"}})
            )
            .await
            .unwrap_err()
            .code(),
        Some(ErrorCode::Forbidden)
    );
    let mut host = server.signal().await;
    host.request(
        "join",
        json!({"token":server.token("captions","host",Role::Host)}),
    )
    .await
    .unwrap();
    assert_eq!(
        host.request(
            "meeting.action",
            json!({"action":{"type":"captions.start"}})
        )
        .await
        .unwrap_err()
        .code(),
        Some(ErrorCode::Unavailable)
    );
}
#[tokio::test]
#[ignore = "requires FFmpeg; HTTP STT is a local mock"]
async fn live_caption_stop_flushes_short_audio_and_keeps_server_known_speaker() {
    use axum::{body::Bytes, extract::State, http::HeaderMap, routing::post, Json, Router};
    use base64::Engine;
    use ecochain_rtc_server::{
        bot::{Bot, SignalClient},
        config::{RecordingConfig, Secret, StorageConfig},
        providers::SttConfig,
    };
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/stt", listener.local_addr().unwrap());
    let route=Router::new().route("/stt",post(|State(calls):State<Arc<AtomicUsize>>,headers:HeaderMap,body:Bytes|async move{assert_eq!(headers["authorization"],"Bearer mock-stt");let text=String::from_utf8_lossy(&body);assert!(text.contains("caption.wav"));assert!(text.contains("RIFF"));calls.fetch_add(1,Ordering::Relaxed);Json(json!({"text":"Kelishuv tayyor.","participant_id":"forged"}))})).with_state(calls.clone());
    let mock = tokio::spawn(async move {
        axum::serve(listener, route).await.unwrap();
    });
    let baseline = common::start("").await;
    let mut config = baseline.config.clone();
    let root = std::env::temp_dir().join(format!("ecochain-caption-{}", uuid::Uuid::new_v4()));
    config.storage = Some(StorageConfig {
        path: root.clone(),
        master_key: Secret::new(base64::engine::general_purpose::STANDARD.encode([4; 32])),
        retention_secs: 86400,
    });
    config.recording = Some(RecordingConfig {
        ffmpeg: "ffmpeg".into(),
        temp_path: root.join("work"),
        max_duration_secs: 60,
        max_bytes: 1024 * 1024,
    });
    config.providers.stt = Some(SttConfig {
        endpoint,
        api_key: Secret::new("mock-stt"),
        model: "test-model".into(),
        response_format: "json".into(),
        language: None,
    });
    let server = ecochain_rtc_server::start(config.clone()).await.unwrap();
    let url = format!("ws://{}/v1/rtc", server.http_addr);
    let mut host = SignalClient::connect(&url).await.unwrap();
    let joined = host
        .request(
            "join",
            json!({"token":common::token_with(&config,"captions","host",Role::Host,3600)}),
        )
        .await
        .unwrap();
    let mut options = baseline.bot("captions", "speaker");
    options.url = url;
    options.token = common::token_with(&config, "captions", "speaker", Role::Participant, 3600);
    options.video_kbps = None;
    let publisher = Bot::start(options).await.unwrap();
    host.request(
        "meeting.action",
        json!({"action":{"type":"captions.start"}}),
    )
    .await
    .unwrap();
    loop {
        let state = host
            .wait_event("captions.changed", Duration::from_secs(5))
            .await
            .unwrap();
        if state["data"]["status"] == "running" {
            break;
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while server.app.metrics.media_in_total.load(Ordering::Relaxed) < 10 {
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    host.request("meeting.action", json!({"action":{"type":"captions.stop"}}))
        .await
        .unwrap();
    let segment = host
        .wait_event("captions.segment", Duration::from_secs(8))
        .await
        .unwrap();
    assert_eq!(segment["data"]["text"], "Kelishuv tayyor.");
    assert_eq!(
        segment["data"]["participant_id"],
        publisher.stats().participant_id.unwrap()
    );
    loop {
        let state = host
            .wait_event("captions.changed", Duration::from_secs(5))
            .await
            .unwrap();
        assert_ne!(state["data"]["status"], "failed");
        if state["data"]["status"] == "ready" {
            break;
        }
    }
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let jobs = server
        .app
        .vault
        .as_ref()
        .unwrap()
        .jobs(joined["meeting"]["instance_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(jobs[0]["segments"][0]["text"], "Kelishuv tayyor.");
    publisher.leave().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    std::fs::remove_dir_all(root).unwrap();
    mock.abort();
}
