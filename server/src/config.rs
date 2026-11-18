//! Server configuration.
//!
//! Configuration is read from a TOML file. Secrets (token signing secret and
//! admin key) are normally read from environment variables so that they never
//! end up in a file that is committed or copied around. Inline secrets are only
//! accepted in [`Mode::Development`].

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
            max_participants_limit: 100,
            max_video_subscriptions: 9,
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

    let mut listen = file.media.listen;
    if listen.is_empty() {
        listen = default_media_listen(production);
    }
    anyhow::ensure!(
        !listen.is_empty(),
        "no media address could be determined; set media.listen"
    );

    Ok(Config {
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
}
