//! Server configuration.
//!
//! Configuration is read from a TOML file. Secrets (token signing secret and
//! admin key) are normally read from environment variables so that they never
//! end up in a file that is committed or copied around. Inline secrets are only
//! accepted in [`Mode::Development`].

use anyhow::Context;
use serde::Deserialize;
use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

/// Whether the server runs with relaxed local-development rules or strict
/// production rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Allows plain `ws://`, inline secrets, the `/v1/dev/token` endpoint and
    /// public metrics. Never expose a development server to the internet.
    Development,
    /// Requires TLS (or an explicit TLS-terminating proxy), secrets from the
    /// environment and an admin key for metrics.
    Production,
}

/// A secret string that never prints its value.
///
/// `Debug` and `Display` are redacted so that secrets cannot leak through
/// `tracing` fields or error messages by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the secret value. Call sites should be easy to audit, so keep
    /// this the only way to read it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Raw file layout. Converted into [`Config`] by [`load`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    mode: Mode,
    http: HttpConfig,
    auth: FileAuthConfig,
    #[serde(default)]
    rooms: RoomsConfig,
    media: FileMediaConfig,
    storage: Option<FileStorageConfig>,
    recording: Option<FileRecordingConfig>,
    #[serde(default)]
    providers: FileProvidersConfig,
}

/// HTTP and WebSocket listener settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    /// Address for the HTTP/WebSocket listener, for example `0.0.0.0:7880`.
    pub bind: SocketAddr,
    /// PEM certificate chain. When set together with `tls_key` the listener
    /// serves HTTPS/WSS directly.
    pub tls_cert: Option<PathBuf>,
    /// PEM private key for `tls_cert`.
    pub tls_key: Option<PathBuf>,
    /// Set to `true` in production when a reverse proxy terminates TLS in
    /// front of this server. This is an explicit opt-in so that plain HTTP is
    /// never served in production by accident.
    #[serde(default)]
    pub behind_tls_proxy: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileAuthConfig {
    issuer: String,
    token_secret_env: Option<String>,
    token_secret: Option<String>,
    admin_key_env: Option<String>,
    admin_key: Option<String>,
}

/// Resolved authentication settings.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Expected `iss` claim of room tokens.
    pub issuer: String,
    /// HMAC-SHA256 secret shared with the backend that issues room tokens.
    pub token_secret: Secret,
    /// Key for the admin HTTP API (`Authorization: Bearer <key>`).
    pub admin_key: Secret,
}

/// Room limits and lifetimes.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RoomsConfig {
    /// Create a room on the first valid join. Convenient for development;
    /// in production rooms are usually created through `POST /v1/rooms`.
    pub auto_create: bool,
    /// `max_participants` for rooms created without an explicit value.
    pub default_max_participants: usize,
    /// Upper bound accepted by the admin API for `max_participants`.
    pub max_participants_limit: usize,
    /// How many video tracks (camera and screen) a single participant may
    /// receive at the same time. This is separate from `max_participants`:
    /// a 25 person room does not mean a phone decodes 24 videos.
    pub max_video_subscriptions: usize,
    /// How many audio tracks are forwarded at the same time: only the
    /// loudest this many speakers are heard, so a 100 person room does not
    /// send every receiver 99 audio streams. 1..=8.
    pub max_forwarded_audio: usize,
    /// How long a participant whose signaling socket dropped keeps its seat
    /// and media while waiting for a resume.
    pub reconnect_grace_secs: u64,
    /// How long an empty room stays alive before it is closed.
    pub empty_room_ttl_secs: u64,
    /// How long the id of an ended room is remembered, so that late retries
    /// receive `room_ended` instead of silently creating a new room.
    pub ended_room_memory_secs: u64,
    /// Minimum time a host removal blocks the identity from the room. The
    /// ban always lasts at least until the removed participant's token
    /// expires; this extends it for tokens the backend issues later.
    pub removal_memory_secs: u64,
}

impl Default for RoomsConfig {
    fn default() -> Self {
        Self {
            auto_create: false,
            default_max_participants: 25,
            max_participants_limit: 150,
            max_video_subscriptions: 9,
            max_forwarded_audio: 3,
            reconnect_grace_secs: 30,
            empty_room_ttl_secs: 60,
            ended_room_memory_secs: 24 * 3600,
            removal_memory_secs: 24 * 3600,
        }
    }
}

impl RoomsConfig {
    /// Reconnect grace period as a [`Duration`].
    pub fn reconnect_grace(&self) -> Duration {
        Duration::from_secs(self.reconnect_grace_secs)
    }

    /// Empty room lifetime as a [`Duration`].
    pub fn empty_room_ttl(&self) -> Duration {
        Duration::from_secs(self.empty_room_ttl_secs)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileMediaConfig {
    udp_port: u16,
    #[serde(default)]
    listen: Vec<MediaListen>,
    #[serde(default)]
    ice_servers: Vec<IceServer>,
    #[serde(default = "default_true")]
    follow_primary: bool,
    turn: Option<FileTurnConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileTurnConfig {
    urls: Vec<String>,
    secret_env: Option<String>,
    secret: Option<String>,
    #[serde(default = "default_turn_ttl_secs")]
    ttl_secs: u64,
}

/// 12 hours: the SDK reads `ice_servers` only when it (re)joins, so an ICE
/// restart after the credential expired could not allocate a relay. The TTL
/// must cover the longest meeting.
fn default_turn_ttl_secs() -> u64 {
    43_200
}

/// Ephemeral TURN credentials (coturn `use-auth-secret`, TURN REST API).
/// Every join gets its own time-limited username and credential derived from
/// `secret`, so no static TURN password ships to clients.
#[derive(Debug, Clone)]
pub struct TurnConfig {
    /// `turn:` / `turns:` URLs handed to clients.
    pub urls: Vec<String>,
    /// Shared secret; must equal coturn's `static-auth-secret`.
    pub secret: Secret,
    /// Credential lifetime in seconds (default 43200 = 12 h, minimum 60).
    /// Must cover the longest meeting: clients only get new credentials
    /// when they rejoin.
    pub ttl_secs: u64,
}

fn default_true() -> bool {
    true
}

/// One UDP media socket.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaListen {
    /// Local interface address to bind.
    pub bind_ip: IpAddr,
    /// Address announced to clients as the ICE host candidate. Use the public
    /// IP when the server sits behind 1:1 NAT (for example a cloud VM).
    pub advertise_ip: Option<IpAddr>,
    /// Picked automatically (empty `media.listen`). The server then follows
    /// the primary address when the machine changes networks.
    #[serde(skip)]
    pub auto: bool,
}

/// STUN/TURN server handed to clients in the join response.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct IceServer {
    /// `stun:` or `turn:` URLs.
    pub urls: Vec<String>,
    /// TURN username, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// TURN credential, if any. Sent to authenticated clients only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

/// Resolved media settings.
#[derive(Debug, Clone)]
pub struct MediaConfig {
    /// UDP port shared by all media sockets.
    pub udp_port: u16,
    /// Sockets to open. Never empty after [`load`].
    pub listen: Vec<MediaListen>,
    /// Extra ICE servers for clients.
    pub ice_servers: Vec<IceServer>,
    /// With an empty `listen`, move the media socket when the machine's
    /// primary address changes (new Wi-Fi). Default `true`.
    pub follow_primary: bool,
    /// Per-join TURN credentials, when configured.
    pub turn: Option<TurnConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileStorageConfig {
    path: PathBuf,
    master_key_env: String,
    #[serde(default = "default_retention_days")]
    retention_days: u64,
}
fn default_retention_days() -> u64 {
    30
}

/// Validated private archive storage; the master key never appears in Debug output.
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// Persistent volume directory.
    pub path: PathBuf,
    /// Base64 encoded AES-256 wrapping key resolved from environment.
    pub master_key: Secret,
    /// Retention in seconds.
    pub retention_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRecordingConfig {
    #[serde(default = "default_ffmpeg")]
    ffmpeg: PathBuf,
    temp_path: Option<PathBuf>,
    #[serde(default = "default_recording_duration")]
    max_duration_secs: u64,
    #[serde(default = "default_recording_bytes")]
    max_bytes: u64,
}
fn default_ffmpeg() -> PathBuf {
    PathBuf::from("ffmpeg")
}
fn default_recording_duration() -> u64 {
    4 * 3600
}
fn default_recording_bytes() -> u64 {
    4 * 1024 * 1024 * 1024
}
/// Optional bounded FFmpeg recording worker configuration.
#[derive(Debug, Clone)]
pub struct RecordingConfig {
    /// FFmpeg executable; invoked directly without a shell.
    pub ffmpeg: PathBuf,
    /// Private transient plaintext spool, removed on completion or failure.
    pub temp_path: PathBuf,
    /// Maximum recording duration before automatic finalization.
    pub max_duration_secs: u64,
    /// Maximum encoded input bytes before explicit recording failure.
    pub max_bytes: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileProvidersConfig {
    stt: Option<FileSttConfig>,
    summary: Option<FileSummaryConfig>,
    jira: Option<FileJiraConfig>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSttConfig {
    endpoint: String,
    api_key_env: String,
    model: String,
    #[serde(default = "default_response_format")]
    response_format: String,
    language: Option<String>,
}
fn default_response_format() -> String {
    "json".to_owned()
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSummaryConfig {
    endpoint: String,
    api_key_env: String,
    model: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileJiraConfig {
    base_url: String,
    email: String,
    api_token_env: String,
    project_keys: Vec<String>,
    issue_type_id: String,
    #[serde(default = "default_issue_name")]
    issue_type_name: String,
    #[serde(default)]
    identity_account_ids: std::collections::BTreeMap<String, String>,
}
fn default_issue_name() -> String {
    "Task".to_owned()
}
fn provider_secret(env_name: &str, env: &dyn Fn(&str) -> Option<String>) -> anyhow::Result<Secret> {
    let secret = env(env_name).context("provider credential environment variable is missing")?;
    let secret = secret.trim();
    anyhow::ensure!(
        !secret.is_empty() && !secret.chars().any(char::is_control),
        "invalid provider credential"
    );
    Ok(Secret::new(secret))
}
fn provider_url(value: &str, production: bool) -> anyhow::Result<String> {
    let url = reqwest::Url::parse(value).context("invalid provider endpoint")?;
    anyhow::ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.host_str().is_some(),
        "provider endpoint must not contain credentials, query or fragment"
    );
    let local = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host.parse::<std::net::IpAddr>().is_ok_and(|ip| {
                ip.is_loopback()
                    || match ip {
                        std::net::IpAddr::V4(ip) => ip.is_private(),
                        std::net::IpAddr::V6(ip) => ip.is_unique_local(),
                    }
            })
    });
    anyhow::ensure!(
        url.scheme() == "https" || (!production && url.scheme() == "http" && local),
        "provider HTTPS required except local development"
    );
    Ok(url.to_string())
}
fn valid_provider_identity(id: &str) -> bool {
    crate::auth::valid_id(id)
}
fn provider_model(model: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        !model.trim().is_empty() && model.len() <= 128 && !model.chars().any(char::is_control),
        "invalid provider model"
    );
    Ok(model.to_owned())
}

/// Fully validated server configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Development or production rules.
    pub mode: Mode,
    /// HTTP listener.
    pub http: HttpConfig,
    /// Token and admin authentication.
    pub auth: AuthConfig,
    /// Room limits.
    pub rooms: RoomsConfig,
    /// Media sockets.
    pub media: MediaConfig,
    /// Optional encrypted archive storage.
    pub storage: Option<StorageConfig>,
    /// Optional background recorder; requires archive storage.
    pub recording: Option<RecordingConfig>,
    /// Explicitly configured transcription, summary and Jira adapters.
    pub providers: crate::providers::ProvidersConfig,
}

impl Config {
    /// `true` when the listener itself serves TLS.
    pub fn serves_tls(&self) -> bool {
        self.http.tls_cert.is_some() && self.http.tls_key.is_some()
    }
}

/// Reads and validates a configuration file.
///
/// Relative TLS paths are resolved against the directory of the file so that
/// the server can be started from any working directory.
pub fn load(path: &Path) -> anyhow::Result<Config> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read config {}: {e}", path.display()))?;
    let base = path.parent().unwrap_or(Path::new("."));
    parse(&text, base, &|name| std::env::var(name).ok())
}

/// Parses configuration text. `env` looks up environment variables; it is a
/// parameter so tests do not have to mutate the process environment.
pub fn parse(
    text: &str,
    base: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> anyhow::Result<Config> {
    let file: FileConfig = toml::from_str(text)?;
    let production = file.mode == Mode::Production;

    let token_secret = resolve_secret(
        "auth.token_secret",
        file.auth.token_secret_env.as_deref(),
        file.auth.token_secret.as_deref(),
        production,
        env,
    )?;
    let admin_key = resolve_secret(
        "auth.admin_key",
        file.auth.admin_key_env.as_deref(),
        file.auth.admin_key.as_deref(),
        production,
        env,
    )?;
    anyhow::ensure!(
        token_secret.expose().len() >= 32,
        "auth.token_secret must be at least 32 bytes"
    );
    anyhow::ensure!(
        admin_key.expose().len() >= 16,
        "auth.admin_key must be at least 16 bytes"
    );
    anyhow::ensure!(
        !file.auth.issuer.is_empty(),
        "auth.issuer must not be empty"
    );

    let mut http = file.http;
    http.tls_cert = http.tls_cert.map(|p| base.join(p));
    http.tls_key = http.tls_key.map(|p| base.join(p));
    anyhow::ensure!(
        http.tls_cert.is_some() == http.tls_key.is_some(),
        "http.tls_cert and http.tls_key must be set together"
    );
    if production {
        anyhow::ensure!(
            http.tls_cert.is_some() || http.behind_tls_proxy,
            "production mode requires http.tls_cert/tls_key or http.behind_tls_proxy = true"
        );
    }

    let rooms = file.rooms;
    anyhow::ensure!(
        rooms.default_max_participants >= 1
            && rooms.default_max_participants <= rooms.max_participants_limit,
        "rooms.default_max_participants must be between 1 and rooms.max_participants_limit"
    );
    anyhow::ensure!(
        rooms.max_video_subscriptions >= 1,
        "rooms.max_video_subscriptions must be at least 1"
    );
    anyhow::ensure!(
        (1..=8).contains(&rooms.max_forwarded_audio),
        "rooms.max_forwarded_audio must be between 1 and 8"
    );

    let mut listen = file.media.listen;
    if listen.is_empty() {
        listen = default_media_listen(production);
    }
    anyhow::ensure!(
        !listen.is_empty(),
        "no media address could be determined; set media.listen"
    );

    let turn = match file.media.turn {
        None => None,
        Some(t) => {
            anyhow::ensure!(!t.urls.is_empty(), "media.turn.urls must not be empty");
            for url in &t.urls {
                anyhow::ensure!(
                    url.starts_with("turn:") || url.starts_with("turns:"),
                    "media.turn.urls entries must start with turn: or turns:"
                );
            }
            anyhow::ensure!(t.ttl_secs >= 60, "media.turn.ttl_secs must be at least 60");
            // A secret read from a file into the environment often keeps its
            // trailing newline, which would silently break every TURN login.
            let trimmed_env = |name: &str| env(name).map(|v| v.trim().to_owned());
            let secret = resolve_secret(
                "media.turn.secret",
                t.secret_env.as_deref(),
                t.secret.as_deref(),
                production,
                &trimmed_env,
            )?;
            anyhow::ensure!(
                !secret.expose().is_empty(),
                "media.turn.secret must not be empty"
            );
            Some(TurnConfig {
                urls: t.urls,
                secret,
                ttl_secs: t.ttl_secs,
            })
        }
    };

    let storage = match file.storage {
        None => None,
        Some(storage) => {
            use base64::Engine;
            let key = env(&storage.master_key_env)
                .context("storage master key environment variable is missing")?;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(key.trim())
                .context("storage master key must be base64")?;
            anyhow::ensure!(
                decoded.len() == 32,
                "storage master key must decode to 32 bytes"
            );
            anyhow::ensure!(
                (1..=3650).contains(&storage.retention_days),
                "storage retention_days must be 1..3650"
            );
            Some(StorageConfig {
                path: base.join(storage.path),
                master_key: Secret::new(key.trim()),
                retention_secs: storage.retention_days * 86400,
            })
        }
    };
    let recording = match file.recording {
        None => None,
        Some(recording) => {
            let store = storage
                .as_ref()
                .context("recording requires configured encrypted storage")?;
            anyhow::ensure!(
                (1..=86400).contains(&recording.max_duration_secs),
                "recording max_duration_secs must be 1..86400"
            );
            anyhow::ensure!(
                (1024 * 1024..=64u64 * 1024 * 1024 * 1024).contains(&recording.max_bytes),
                "recording max_bytes out of bounds"
            );
            Some(RecordingConfig {
                ffmpeg: recording.ffmpeg,
                temp_path: recording
                    .temp_path
                    .map_or_else(|| store.path.join("work"), |p| base.join(p)),
                max_duration_secs: recording.max_duration_secs,
                max_bytes: recording.max_bytes,
            })
        }
    };
    let stt = match file.providers.stt {
        None => None,
        Some(p) => {
            anyhow::ensure!(
                recording.is_some(),
                "STT requires recording/FFmpeg configuration"
            );
            anyhow::ensure!(
                ["json", "verbose_json"].contains(&p.response_format.as_str()),
                "STT response_format must be json or verbose_json"
            );
            anyhow::ensure!(
                p.language.as_ref().is_none_or(|v| !v.is_empty()
                    && v.len() <= 16
                    && v.bytes().all(|c| c.is_ascii_alphabetic() || c == b'-')),
                "invalid STT language"
            );
            Some(crate::providers::SttConfig {
                endpoint: provider_url(&p.endpoint, production)?,
                api_key: provider_secret(&p.api_key_env, env)?,
                model: provider_model(&p.model)?,
                response_format: p.response_format,
                language: p.language,
            })
        }
    };
    let summary = match file.providers.summary {
        None => None,
        Some(p) => Some(crate::providers::SummaryConfig {
            endpoint: provider_url(&p.endpoint, production)?,
            api_key: provider_secret(&p.api_key_env, env)?,
            model: provider_model(&p.model)?,
        }),
    };
    let jira = match file.providers.jira {
        None => None,
        Some(p) => {
            let base_url = provider_url(&p.base_url, production)?;
            let url = reqwest::Url::parse(&base_url)?;
            anyhow::ensure!(
                url.path() == "/",
                "Jira base_url must be an origin without a path"
            );
            anyhow::ensure!(
                !p.email.is_empty()
                    && p.email.len() <= 254
                    && !p.email.chars().any(char::is_control),
                "invalid Jira email"
            );
            anyhow::ensure!(
                !p.project_keys.is_empty()
                    && p.project_keys.len() <= 100
                    && p.project_keys.iter().all(|key| key.len() <= 32
                        && key.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
                        && key
                            .bytes()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')),
                "Jira project allowlist required"
            );
            anyhow::ensure!(
                !p.issue_type_id.is_empty()
                    && p.issue_type_id.len() <= 64
                    && p.issue_type_id.bytes().all(|c| c.is_ascii_digit()),
                "invalid Jira issue type ID"
            );
            anyhow::ensure!(
                p.identity_account_ids.len() <= 1000
                    && p.identity_account_ids
                        .iter()
                        .all(|(id, account)| valid_provider_identity(id)
                            && !account.is_empty()
                            && account.len() <= 128
                            && !account.chars().any(char::is_control)),
                "invalid Jira identity mapping"
            );
            Some(crate::providers::JiraConfig {
                base_url,
                email: p.email,
                api_token: provider_secret(&p.api_token_env, env)?,
                project_keys: p.project_keys,
                issue_type_id: p.issue_type_id,
                issue_type_name: p.issue_type_name,
                identity_account_ids: p.identity_account_ids,
            })
        }
    };
    anyhow::ensure!(
        storage.is_some() || (stt.is_none() && summary.is_none() && jira.is_none()),
        "providers require encrypted archive storage"
    );
    let providers = crate::providers::ProvidersConfig { stt, summary, jira };
    Ok(Config {
        providers,
        storage,
        recording,
        mode: file.mode,
        http,
        auth: AuthConfig {
            issuer: file.auth.issuer,
            token_secret,
            admin_key,
        },
        rooms,
        media: MediaConfig {
            udp_port: file.media.udp_port,
            listen,
            ice_servers: file.media.ice_servers,
            follow_primary: file.media.follow_primary,
            turn,
        },
    })
}

fn resolve_secret(
    name: &str,
    env_name: Option<&str>,
    inline: Option<&str>,
    production: bool,
    env: &dyn Fn(&str) -> Option<String>,
) -> anyhow::Result<Secret> {
    if let Some(var) = env_name {
        if let Some(value) = env(var).filter(|v| !v.is_empty()) {
            return Ok(Secret::new(value));
        }
    }
    match inline {
        Some(_) if production => {
            anyhow::bail!(
                "{name}: inline secrets are not allowed in production; use the *_env field"
            )
        }
        Some(value) => Ok(Secret::new(value)),
        None => anyhow::bail!(
            "{name}: environment variable {} is not set",
            env_name.unwrap_or("<not configured>")
        ),
    }
}

/// Picks the machine's primary LAN address (and loopback in development).
fn default_media_listen(production: bool) -> Vec<MediaListen> {
    let mut listen = Vec::new();
    if let Some(ip) = primary_lan_ip() {
        listen.push(MediaListen {
            bind_ip: ip,
            advertise_ip: None,
            auto: true,
        });
    }
    if !production {
        listen.push(MediaListen {
            bind_ip: IpAddr::from([127, 0, 0, 1]),
            advertise_ip: None,
            auto: false,
        });
    }
    listen
}

/// The address of the interface that carries the default route, if any.
///
/// Found by "connecting" a UDP socket to a public address; no packet is
/// sent, the OS only selects the outgoing interface.
pub fn primary_lan_ip() -> Option<IpAddr> {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.connect("8.8.8.8:80").map(|_| s))
        .and_then(|s| s.local_addr())
        .map(|a| a.ip())
        .ok()
        .filter(|ip| !ip.is_unspecified() && !ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV: &str = r#"
mode = "development"
[http]
bind = "127.0.0.1:7880"
[auth]
issuer = "ecochain"
token_secret = "dev-secret-dev-secret-dev-secret-0001"
admin_key = "dev-admin-key-0001"
[media]
udp_port = 7882
listen = [{ bind_ip = "127.0.0.1" }]
"#;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn development_accepts_inline_secrets() {
        let config = parse(DEV, Path::new("."), &no_env).unwrap();
        assert_eq!(config.mode, Mode::Development);
        assert_eq!(config.rooms.default_max_participants, 25);
        assert_eq!(config.rooms.max_participants_limit, 150);
    }

    #[test]
    fn max_forwarded_audio_defaults_to_three() {
        let config = parse(DEV, Path::new("."), &no_env).unwrap();
        assert_eq!(config.rooms.max_forwarded_audio, 3);
        let text = DEV.replace("[media]", "[rooms]\nmax_forwarded_audio = 8\n[media]");
        let config = parse(&text, Path::new("."), &no_env).unwrap();
        assert_eq!(config.rooms.max_forwarded_audio, 8);
    }

    #[test]
    fn max_forwarded_audio_out_of_range_is_rejected() {
        for value in [0, 9] {
            let text = DEV.replace(
                "[media]",
                &format!("[rooms]\nmax_forwarded_audio = {value}\n[media]"),
            );
            let error = parse(&text, Path::new("."), &no_env)
                .unwrap_err()
                .to_string();
            assert!(error.contains("max_forwarded_audio"), "{value}: {error}");
        }
    }

    #[test]
    fn production_rejects_inline_secrets_and_plain_http() {
        let prod = DEV.replace("\"development\"", "\"production\"");
        let error = parse(&prod, Path::new("."), &no_env)
            .unwrap_err()
            .to_string();
        assert!(error.contains("inline secrets"), "{error}");

        let prod_env = prod
            .replace("token_secret = ", "token_secret_env = \"T\"\n#")
            .replace("admin_key = ", "admin_key_env = \"A\"\n#");
        let env = |name: &str| match name {
            "T" => Some("x".repeat(40)),
            "A" => Some("y".repeat(20)),
            _ => None,
        };
        let error = parse(&prod_env, Path::new("."), &env)
            .unwrap_err()
            .to_string();
        assert!(error.contains("requires http.tls_cert"), "{error}");
    }

    #[test]
    fn secrets_are_redacted() {
        let config = parse(DEV, Path::new("."), &no_env).unwrap();
        let printed = format!("{config:?}");
        assert!(!printed.contains("dev-secret"));
        assert!(!printed.contains("dev-admin-key"));
    }

    #[test]
    fn turn_config_is_validated() {
        let with = |turn: &str| format!("{DEV}\n[media.turn]\n{turn}");
        let ok = with("urls = [\"turns:t.example.com:443?transport=tcp\"]\nsecret = \"s\"");
        let turn = parse(&ok, Path::new("."), &no_env).unwrap().media.turn;
        let turn = turn.unwrap();
        assert_eq!(turn.ttl_secs, 43_200);
        assert!(!format!("{turn:?}").contains("\"s\""));

        for (bad, needle) in [
            ("urls = [\"stun:x:3478\"]\nsecret = \"s\"", "turn:"),
            (
                "urls = [\"turn:x\"]\nsecret = \"s\"\nttl_secs = 59",
                "ttl_secs",
            ),
            ("urls = []\nsecret = \"s\"", "urls"),
            ("urls = [\"turn:x\"]", "media.turn.secret"),
        ] {
            let error = parse(&with(bad), Path::new("."), &no_env)
                .unwrap_err()
                .to_string();
            assert!(error.contains(needle), "{bad}: {error}");
        }

        let prod = with("urls = [\"turn:x\"]\nsecret = \"s\"")
            .replace("\"development\"", "\"production\"");
        let error = parse(&prod, Path::new("."), &no_env)
            .unwrap_err()
            .to_string();
        assert!(error.contains("inline secrets"), "{error}");
    }

    #[test]
    fn turn_secret_from_env_is_trimmed() {
        let config = format!("{DEV}\n[media.turn]\nurls = [\"turn:x\"]\nsecret_env = \"TURN\"");
        let env = |name: &str| (name == "TURN").then(|| "  s3cret\n".to_owned());
        let turn = parse(&config, Path::new("."), &env)
            .unwrap()
            .media
            .turn
            .unwrap();
        assert_eq!(turn.secret.expose(), "s3cret");

        let blank = |name: &str| (name == "TURN").then(|| " \n".to_owned());
        let error = parse(&config, Path::new("."), &blank)
            .unwrap_err()
            .to_string();
        assert!(error.contains("media.turn.secret"), "{error}");
    }
}
