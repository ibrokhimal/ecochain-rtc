//! Ephemeral TURN credentials (coturn `use-auth-secret`, TURN REST API).
//!
//! The username is `<expiry unix time>:<identity>` and the credential is
//! `base64(HMAC-SHA1(secret, username))`. coturn recomputes the HMAC with the
//! same `static-auth-secret` and rejects the username once the expiry passed.
//! Neither the secret nor the generated credential is ever logged.

use crate::config::{IceServer, TurnConfig};
use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use sha1::Sha1;

/// Builds the TURN [`IceServer`] entry for one participant.
pub fn credentials(config: &TurnConfig, identity: &str, now_unix: u64) -> IceServer {
    let username = format!("{}:{}", now_unix + config.ttl_secs, identity);
    let mut mac = Hmac::<Sha1>::new_from_slice(config.secret.expose().as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(username.as_bytes());
    let credential = STANDARD.encode(mac.finalize().into_bytes());
    IceServer {
        urls: config.urls.clone(),
        username: Some(username),
        credential: Some(credential),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Secret;

    #[test]
    fn credential_matches_coturn_formula() {
        let config = TurnConfig {
            urls: vec!["turn:turn.example.com:3478".into()],
            secret: Secret::new("s3cret"),
            ttl_secs: 3600,
        };
        let server = credentials(&config, "alice", 1_700_000_000);
        assert_eq!(server.urls, config.urls);
        assert_eq!(server.username.as_deref(), Some("1700003600:alice"));

        let mut mac = Hmac::<Sha1>::new_from_slice(b"s3cret").unwrap();
        mac.update(b"1700003600:alice");
        let expected = STANDARD.encode(mac.finalize().into_bytes());
        assert_eq!(server.credential, Some(expected));
    }
}
