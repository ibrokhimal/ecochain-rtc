//! `ecochain-rtc-server` command line.

use clap::{Parser, Subcommand};
use ecochain_rtc_server::{
    auth::{self, Claims, Role},
    config,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Ecochain signaling server and WebRTC SFU")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server.
    Serve {
        /// Configuration file.
        #[arg(
            long,
            env = "ECOCHAIN_RTC_CONFIG",
            default_value = "config/development.toml"
        )]
        config: PathBuf,
    },
    /// Print a room token signed with the configured secret.
    Token {
        /// Configuration file (for issuer and secret).
        #[arg(
            long,
            env = "ECOCHAIN_RTC_CONFIG",
            default_value = "config/development.toml"
        )]
        config: PathBuf,
        /// Room id.
        #[arg(long)]
        room: String,
        /// Participant identity.
        #[arg(long)]
        identity: String,
        /// Display name.
        #[arg(long)]
        name: Option<String>,
        /// Grant the host role.
        #[arg(long)]
        host: bool,
        /// Lifetime in seconds.
        #[arg(long, default_value_t = 3600)]
        ttl: u64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,str0m=warn,dimpl=warn".into()),
        )
        .init();

    match Cli::parse().command {
        Command::Serve { config } => {
            let config = config::load(&config)?;
            tracing::info!(mode = ?config.mode, "starting");
            let server = ecochain_rtc_server::start(config).await?;
            tracing::info!(http = %server.http_addr, media = ?server.media_addrs, "ready");
            tokio::select! {
                result = server.task => result??,
                _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
            }
        }
        Command::Token {
            config,
            room,
            identity,
            name,
            host,
            ttl,
        } => {
            let config = config::load(&config)?;
            anyhow::ensure!(
                auth::valid_id(&room) && auth::valid_id(&identity),
                "invalid room or identity"
            );
            let now = auth::unix_now();
            let claims = Claims {
                iss: config.auth.issuer.clone(),
                sub: identity,
                room,
                name,
                role: if host { Role::Host } else { Role::Participant },
                can_publish: true,
                exp: now + ttl,
                iat: now,
                nbf: None,
            };
            println!("{}", auth::issue(&claims, &config.auth.token_secret));
        }
    }
    Ok(())
}
