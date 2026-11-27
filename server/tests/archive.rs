mod common;
use base64::{engine::general_purpose::STANDARD, Engine};
use ecochain_rtc_server::{
    archive,
    auth::{self, Role},
    config,
    storage::Archive,
};
use reqwest::{header, Client, Response, StatusCode, Url};
use serde_json::{json, Value};
use std::{fs, path::PathBuf, time::Duration};

#[tokio::test]
async fn archive_tokens_cannot_be_used_for_live_rooms_or_other_occurrences() {
    let server = common::start("").await;
    let instance = uuid::Uuid::new_v4().to_string();
    let token = archive::issue(&server.config, &instance, "alice", 3600);
    assert_eq!(
        archive::verify(&server.config, &token, &instance)
            .unwrap()
            .sub,
        "alice"
    );
    assert!(archive::verify(&server.config, &token, &uuid::Uuid::new_v4().to_string()).is_err());
    assert!(auth::verify(
        &token,
        &server.config.auth.token_secret,
        &server.config.auth.issuer,
        auth::unix_now()
    )
    .is_err());
    let live = server.token(&instance, "alice", Role::Host);
    assert!(archive::verify(&server.config, &live, &instance).is_err());
}
#[test]
fn rejects_images_with_unbounded_dimensions_or_wrong_type() {
    let mut png = vec![0; 33];
    png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    png[12..16].copy_from_slice(b"IHDR");
    png[16..20].copy_from_slice(&1920u32.to_be_bytes());
    png[20..24].copy_from_slice(&1080u32.to_be_bytes());
    assert_eq!(
        archive::image_dimensions(&png).unwrap(),
        ("image/png", 1920, 1080)
    );
    png[16..20].copy_from_slice(&100000u32.to_be_bytes());
    assert!(archive::image_dimensions(&png).is_err());
    assert!(archive::image_dimensions(b"<svg></svg>").is_err());
}

const PRIVATE_MARKER: &[u8] = b"private recording fixture, stored encrypted";

struct RecordedArchive {
    server: common::TestServer,
    client: Client,
    root: PathBuf,
    instance: String,
    other_instance: String,
    recording: String,
    file: String,
    token: String,
    plaintext: Vec<u8>,
}

impl RecordedArchive {
    async fn start() -> Self {
        let root =
            std::env::temp_dir().join(format!("ecochain-archive-http-{}", uuid::Uuid::new_v4()));
        let secret = common::SECRET;
        let admin = common::ADMIN;
        let config = config::parse(
            &format!(
                r#"
mode = "development"
[http]
bind = "127.0.0.1:0"
[auth]
issuer = "ecochain-test"
token_secret = "{secret}"
admin_key = "{admin}"
[media]
udp_port = 0
listen = [{{ bind_ip = "127.0.0.1" }}]
[storage]
path = "vault"
master_key_env = "ARCHIVE_KEY"
"#
            ),
            &root,
            &|name| (name == "ARCHIVE_KEY").then(|| STANDARD.encode([8; 32])),
        )
        .unwrap();
        let server = ecochain_rtc_server::start(config.clone()).await.unwrap();
        let vault = server.app.vault.as_ref().unwrap();
        let instance = uuid::Uuid::new_v4().to_string();
        let other_instance = uuid::Uuid::new_v4().to_string();
        for id in [&instance, &other_instance] {
            vault
                .append(&Archive {
                    state: json!({"instance_id": id, "revision": 1}),
                    members: ["alice".to_owned()].into(),
                    updated_at: auth::unix_now(),
                })
                .unwrap();
        }
        let mut plaintext: Vec<_> = (0..1024 * 1024 + 257).map(|i| (i % 251) as u8).collect();
        plaintext[65_540..65_540 + PRIVATE_MARKER.len()].copy_from_slice(PRIVATE_MARKER);
        let source = root.join("fixture.m4a");
        fs::write(&source, &plaintext).unwrap();
        let file = vault.put_file(&instance, &source, "audio/mp4").unwrap();
        fs::remove_file(source).unwrap();
        let recording = uuid::Uuid::new_v4().to_string();
        vault
            .write_job(
                &instance,
                &recording,
                &json!({
                    "id": recording, "kind": "recording", "status": "ready",
                    "audio_file_id": file, "duration_ms": 750, "tracks": []
                }),
            )
            .unwrap();
        Self {
            token: archive::issue(&config, &instance, "alice", 3600),
            server: common::TestServer { server, config },
            client: Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            root,
            instance,
            other_instance,
            recording,
            file,
            plaintext,
        }
    }

    fn url(&self, instance: &str, suffix: &str) -> String {
        self.server
            .http_url(&format!("/v1/archives/{instance}{suffix}"))
    }

    fn file_url(&self, instance: &str) -> String {
        self.url(instance, &format!("/files/{}", self.file))
    }

    async fn playback_link(&self) -> Url {
        let response = self
            .client
            .post(self.url(&self.instance, "/playback-link"))
            .bearer_auth(&self.token)
            .json(&json!({"recording_id": self.recording}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        let link = body["url"].as_str().unwrap();
        assert!(!link.contains(&self.token));
        Url::parse(&self.server.http_url(link)).unwrap()
    }

    async fn exchange(&self, instance: &str, handoff: &str) -> Response {
        self.client
            .post(self.url(instance, "/playback-session"))
            .json(&json!({"handoff": handoff}))
            .send()
            .await
            .unwrap()
    }
}

impl Drop for RecordedArchive {
    fn drop(&mut self) {
        self.server.server.task.abort();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn handoff(link: &Url) -> &str {
    link.fragment().unwrap().strip_prefix("handoff=").unwrap()
}

#[tokio::test]
async fn archive_file_requires_bearer_access_for_the_admitted_instance() {
    let fixture = RecordedArchive::start().await;
    let path = fixture.file_url(&fixture.instance);
    let outsider = archive::issue(&fixture.server.config, &fixture.instance, "outside", 3600);
    let other = archive::issue(
        &fixture.server.config,
        &fixture.other_instance,
        "alice",
        3600,
    );
    let live = fixture.server.token(&fixture.instance, "alice", Role::Host);
    for (token, expected) in [
        ("", StatusCode::UNAUTHORIZED),
        (other.as_str(), StatusCode::UNAUTHORIZED),
        (live.as_str(), StatusCode::UNAUTHORIZED),
        (outsider.as_str(), StatusCode::FORBIDDEN),
    ] {
        let response = fixture
            .client
            .get(&path)
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    let wrong_occurrence = fixture
        .client
        .get(fixture.file_url(&fixture.other_instance))
        .bearer_auth(&fixture.token)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_occurrence.status(), StatusCode::UNAUTHORIZED);

    let wrong_file = fixture
        .client
        .get(fixture.file_url(&fixture.other_instance))
        .bearer_auth(other)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_file.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        fixture
            .client
            .get(path)
            .bearer_auth(&fixture.token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn archive_http_serves_decrypted_ranges_across_encrypted_chunks() {
    let fixture = RecordedArchive::start().await;
    let encrypted = fs::read(
        fixture
            .root
            .join("vault")
            .join(&fixture.instance)
            .join(format!("{}.stream", fixture.file)),
    )
    .unwrap();
    assert!(encrypted.len() > fixture.plaintext.len());
    assert!(!encrypted
        .windows(PRIVATE_MARKER.len())
        .any(|bytes| bytes == PRIVATE_MARKER));

    let len = fixture.plaintext.len();
    let path = fixture.file_url(&fixture.instance);
    for (range, first, last) in [
        ("bytes=65490-65600", 65_490, 65_600),
        ("bytes=-47", len - 47, len - 1),
        ("bytes=65500-", 65_500, len - 1),
    ] {
        let response = fixture
            .client
            .get(&path)
            .bearer_auth(&fixture.token)
            .header(header::RANGE, range)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers()[header::CONTENT_RANGE],
            format!("bytes {first}-{last}/{len}")
        );
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH],
            (last - first + 1).to_string()
        );
        assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/mp4");
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "private, no-store"
        );
        assert_eq!(
            response.bytes().await.unwrap().as_ref(),
            &fixture.plaintext[first..=last]
        );
    }

    let response = fixture
        .client
        .get(&path)
        .bearer_auth(&fixture.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(header::CONTENT_RANGE).is_none());
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        fixture.plaintext.as_slice()
    );

    for invalid in [
        format!("bytes={len}-"),
        "bytes=-0".into(),
        "bytes=1-5,8-9".into(),
    ] {
        let response = fixture
            .client
            .get(&path)
            .bearer_auth(&fixture.token)
            .header(header::RANGE, invalid)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            response.headers()[header::CONTENT_RANGE],
            format!("bytes */{len}")
        );
        assert!(response.bytes().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn one_concurrent_playback_exchange_sets_a_cookie_scoped_to_the_archive() {
    let fixture = RecordedArchive::start().await;
    let link = fixture.playback_link().await;
    assert_eq!(
        link.path(),
        format!("/v1/archives/{}/player", fixture.instance)
    );
    assert_eq!(
        link.query_pairs().collect::<Vec<_>>(),
        [("recording".into(), fixture.recording.as_str().into())]
    );
    let handoff = handoff(&link);
    assert!(uuid::Uuid::parse_str(handoff).is_ok());
    let (first, second) = tokio::join!(
        fixture.exchange(&fixture.instance, handoff),
        fixture.exchange(&fixture.instance, handoff),
    );
    let (success, consumed) = if first.status() == StatusCode::OK {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(success.status(), StatusCode::OK);
    assert_eq!(consumed.status(), StatusCode::UNAUTHORIZED);
    assert!(consumed.headers().get(header::SET_COOKIE).is_none());

    let cookie = success.headers()[header::SET_COOKIE].to_str().unwrap();
    let mut parts = cookie.split(';').map(str::trim);
    let credential = parts.next().unwrap();
    let attributes: Vec<_> = parts.collect();
    let path = format!("Path=/v1/archives/{}", fixture.instance);
    assert!(attributes.contains(&path.as_str()));
    assert!(attributes.contains(&"HttpOnly"));
    assert!(attributes.contains(&"SameSite=Strict"));
    let max_age = attributes
        .iter()
        .find_map(|part| part.strip_prefix("Max-Age="))
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!((1..=86400).contains(&max_age));

    let audio = fixture
        .client
        .get(fixture.file_url(&fixture.instance))
        .header(header::COOKIE, credential)
        .header(header::RANGE, "bytes=3-8")
        .send()
        .await
        .unwrap();
    assert_eq!(audio.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        audio.bytes().await.unwrap().as_ref(),
        &fixture.plaintext[3..=8]
    );
    let wrong_archive = fixture
        .client
        .get(fixture.url(&fixture.other_instance, ""))
        .header(header::COOKIE, credential)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_archive.status(), StatusCode::UNAUTHORIZED);
    let cookie_only_handoff = fixture
        .client
        .post(fixture.url(&fixture.instance, "/playback-link"))
        .header(header::COOKIE, credential)
        .json(&json!({"recording_id": fixture.recording}))
        .send()
        .await
        .unwrap();
    assert_eq!(cookie_only_handoff.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn expired_or_wrong_instance_playback_handoffs_cannot_set_a_cookie() {
    let fixture = RecordedArchive::start().await;
    let wrong_instance_link = fixture.playback_link().await;
    let wrong = fixture
        .exchange(&fixture.other_instance, handoff(&wrong_instance_link))
        .await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert!(wrong.headers().get(header::SET_COOKIE).is_none());

    let expired_link = fixture.playback_link().await;
    let expired = handoff(&expired_link);
    // Age the actual HTTP-issued handoff without a minute-long wall clock sleep.
    {
        let mut links = fixture.server.server.app.playback_links.lock().unwrap();
        let entry = links.get_mut(expired).unwrap();
        entry.0 = entry.0.checked_sub(Duration::from_secs(61)).unwrap();
    }
    let response = fixture.exchange(&fixture.instance, expired).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().get(header::SET_COOKIE).is_none());
    assert_eq!(
        fixture.exchange(&fixture.instance, expired).await.status(),
        StatusCode::UNAUTHORIZED
    );
}
