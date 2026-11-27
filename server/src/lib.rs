//! Ecochain RTC server: WebSocket signaling, rooms and a str0m based SFU.
//!
//! Module map:
//!
//! * [`config`]: TOML configuration, development vs. production rules.
//! * [`auth`]: HS256 room tokens bound to one room and one identity.
//! * [`protocol`]: signaling message types (see `docs/PROTOCOL.md`).
//! * [`signaling`]: one task per WebSocket.
//! * [`rooms`] / [`room`]: room registry and the per-room actor that owns
//!   participants and media peers.
//! * [`sfu`]: UDP sockets, peer wrapper, simulcast layer selection.
//! * [`http`]: health, metrics, admin API.
//! * [`bot`]: a headless str0m client used by tests and the load generator.

pub mod archive;
pub mod auth;
pub mod bot;
pub mod config;
pub mod http;
pub mod metrics;
pub mod protocol;
pub mod providers;
pub mod recording;
pub mod room;
pub mod rooms;
pub mod sfu;
pub mod signaling;
pub mod storage;
pub mod turn;

use config::Config;
use metrics::Metrics;
use rooms::Rooms;
use sfu::udp::MediaSockets;
use std::{
    net::SocketAddr,
    sync::{atomic::AtomicU64, Arc},
};

/// Shared state of the running server.
pub struct App {
    /// Validated configuration.
    pub config: Arc<Config>,
    /// Counters.
    pub metrics: Arc<Metrics>,
    /// Encrypted archive storage, when configured.
    pub vault: Option<Arc<storage::Vault>>,
    /// Global bound for configured external-provider HTTP operations.
    pub provider_slots: Arc<tokio::sync::Semaphore>,
    /// Per-job weak locks serialize submissions without a permanent lock registry.
    pub job_locks: std::sync::Mutex<
        std::collections::HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>,
    >,
    /// One-use, short-lived native-to-browser playback handoffs.
    pub playback_links:
        std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, String, String)>>,
    /// Room registry.
    pub rooms: Arc<Rooms>,
    /// Source of connection ids.
    pub next_connection_id: AtomicU64,
}

/// A started server.
pub struct Server {
    /// Address the HTTP/WebSocket listener is bound to.
    pub http_addr: SocketAddr,
    /// Advertised media addresses at startup.
    pub media_addrs: Vec<SocketAddr>,
    /// Media sockets (their addresses can change, see
    /// [`MediaSockets::follow_primary`]).
    pub sockets: MediaSockets,
    /// Shared state, useful for tests.
    pub app: Arc<App>,
    /// Resolves when the HTTP server stops.
    pub task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

/// Installs the process-wide crypto provider for str0m (DTLS-SRTP) and
/// rustls (HTTPS/WSS). Safe to call more than once.
pub fn install_crypto() {
    str0m::crypto::from_feature_flags().install_process_default();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// Binds all sockets and starts serving. Returns once the listeners are bound.
pub async fn start(config: Config) -> anyhow::Result<Server> {
    install_crypto();
    let config = Arc::new(config);
    let metrics = Arc::new(Metrics::default());

    let media: Vec<(SocketAddr, Option<std::net::IpAddr>, bool)> = config
        .media
        .listen
        .iter()
        .map(|l| {
            (
                SocketAddr::new(l.bind_ip, config.media.udp_port),
                l.advertise_ip,
                l.auto,
            )
        })
        .collect();
    let sockets = MediaSockets::bind(&media, metrics.clone()).await?;
    if config.media.follow_primary && config.media.listen.iter().any(|l| l.auto) {
        // Laptops and Wi-Fi servers change networks; follow the new address
        // instead of announcing one that no longer exists.
        sockets.follow_primary(crate::config::primary_lan_ip);
    }
    let media_addrs = sockets.advertised();

    let vault = match &config.storage {
        None => None,
        Some(storage) => {
            use base64::Engine;
            let key: [u8; 32] = base64::engine::general_purpose::STANDARD
                .decode(storage.master_key.expose())?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid storage key"))?;
            let root = storage.path.clone();
            let retention = storage.retention_secs;
            Some(
                tokio::task::spawn_blocking(move || storage::Vault::open(root, key, retention))
                    .await??,
            )
        }
    };
    if let Some(vault) = vault.clone() {
        let recovery_vault = vault.clone();
        let temp_root = config.recording.as_ref().map(|r| r.temp_path.clone());
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            if let Some(root) = temp_root {
                storage::remove_abandoned_worker_dirs(&root)?;
            }
            recovery_vault.recover_interrupted_jobs()?;
            Ok(())
        })
        .await??;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                let vault = vault.clone();
                let _ = tokio::task::spawn_blocking(move || vault.purge(auth::unix_now())).await;
            }
        });
    }
    let provider_slots = Arc::new(tokio::sync::Semaphore::new(8));
    let app = Arc::new(App {
        rooms: Rooms::new(
            config.clone(),
            metrics.clone(),
            sockets.clone(),
            vault.clone(),
            provider_slots.clone(),
        ),
        vault,
        playback_links: Default::default(),
        provider_slots,
        job_locks: Default::default(),
        config: config.clone(),
        metrics,
        next_connection_id: AtomicU64::new(1),
    });
    let router = http::router(app.clone());
    let listener = std::net::TcpListener::bind(config.http.bind)?;
    listener.set_nonblocking(true)?;
    let http_addr = listener.local_addr()?;

    let task = if let (Some(cert), Some(key)) = (&config.http.tls_cert, &config.http.tls_key) {
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
            .await
            .map_err(|e| anyhow::anyhow!("cannot load TLS certificate: {e}"))?;
        tracing::info!(%http_addr, "serving https/wss");
        tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, tls)
                .serve(router.into_make_service())
                .await
                .map_err(Into::into)
        })
    } else {
        tracing::warn!(%http_addr, "serving plain http/ws (development or behind a TLS proxy)");
        let listener = tokio::net::TcpListener::from_std(listener)?;
        tokio::spawn(async move { axum::serve(listener, router).await.map_err(Into::into) })
    };

    Ok(Server {
        http_addr,
        media_addrs,
        sockets,
        app,
        task,
    })
}
