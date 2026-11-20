//! Shared helpers: start an in-process server on free ports and sign tokens.
#![allow(dead_code)]

use ecochain_rtc_server::{
    auth::{self, Claims, Role},
    bot::{BotOptions, SignalClient},
    config::{self, Config},
    Server,
};
use std::path::Path;

pub const SECRET: &str = "integration-secret-integration-secret";
pub const ADMIN: &str = "integration-admin-key";

pub struct TestServer {
    pub server: Server,
    pub config: Config,
}

impl TestServer {
    pub fn ws_url(&self) -> String {
        format!("ws://{}/v1/rtc", self.server.http_addr)
    }

    pub fn http_url(&self, path: &str) -> String {
        format!("http://{}{}", self.server.http_addr, path)
    }

    pub fn token(&self, room: &str, identity: &str, role: Role) -> String {
        token_with(&self.config, room, identity, role, 3600)
    }

    pub async fn signal(&self) -> SignalClient {
        SignalClient::connect(&self.ws_url())
            .await
            .expect("connect")
    }

    pub fn bot(&self, room: &str, identity: &str) -> BotOptions {
        BotOptions {
            url: self.ws_url(),
            token: self.token(room, identity, Role::Participant),
            bind_ip: "127.0.0.1".parse().unwrap(),
            audio: true,
            video_kbps: Some(300),
            simulcast: false,
            max_video: 4,
            audio_level: -25,
            audio_muted: false,
            high_video: usize::MAX,
        }
    }

    /// Like [`Self::metric`] for values that are not whole numbers; -1.0 when absent.
    pub fn metric_f64(&self, name: &str) -> f64 {
        let text = self.server.app.metrics.render();
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{name} ")))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(-1.0)
    }

    pub fn metric(&self, name: &str) -> i64 {
        let text = self.server.app.metrics.render();
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{name} ")))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(-1)
    }
}

pub fn token_with(config: &Config, room: &str, identity: &str, role: Role, ttl: i64) -> String {
    let now = auth::unix_now();
    let claims = Claims {
        iss: config.auth.issuer.clone(),
        sub: identity.into(),
        room: room.into(),
        name: Some(identity.to_uppercase()),
        role,
        can_publish: true,
        exp: (now as i64 + ttl) as u64,
        iat: now,
        nbf: None,
    };
    auth::issue(&claims, &config.auth.token_secret)
}

/// Starts a development-mode server. `extra` is appended to the `[rooms]` table.
pub async fn start(rooms: &str) -> TestServer {
    start_config(test_config(rooms)).await
}

/// Like [`start`], but the media socket is treated as automatically chosen,
/// so the test can move it with `MediaSockets::move_primary`. The background
/// watcher is off: the test decides when the address "changes".
pub async fn start_following_primary() -> TestServer {
    let mut config = test_config("");
    config.media.listen[0].auto = true;
    config.media.follow_primary = false;
    start_config(config).await
}

async fn start_config(config: Config) -> TestServer {
    let server = ecochain_rtc_server::start(config.clone())
        .await
        .expect("start");
    TestServer { server, config }
}

fn test_config(rooms: &str) -> Config {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".into()))
        .with_test_writer()
        .try_init();
    let defaults: String = [("auto_create", "true"), ("empty_room_ttl_secs", "1")]
        .iter()
        .filter(|(key, _)| !rooms.contains(key))
        .map(|(key, value)| format!("{key} = {value}\n"))
        .collect();
    let text = format!(
        r#"
mode = "development"
[http]
bind = "127.0.0.1:0"
[auth]
issuer = "ecochain-test"
token_secret = "{SECRET}"
admin_key = "{ADMIN}"
[rooms]
{rooms}
{defaults}
[media]
udp_port = 0
listen = [{{ bind_ip = "127.0.0.1" }}]
"#
    );
    config::parse(&text, Path::new("."), &|_| None).expect("config")
}
