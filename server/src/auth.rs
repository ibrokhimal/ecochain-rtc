//! Room access tokens.
//!
//! Tokens are standard JWTs signed with HMAC-SHA256 (`alg: HS256`). A backend
//! (for example the Messenger backend) that already knows who the user is
//! signs a token for exactly one room and one participant identity. The media
//! server only verifies tokens; it never decides who a user is.
//!
//! HS256 was chosen over asymmetric keys for the first stage because every
//! issuing backend is operated by Ecochain itself. Moving to EdDSA later only
//! changes [`verify`] and [`issue`].

use crate::config::Secret;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Longest token accepted. Protects the parser from oversized input.
pub const MAX_TOKEN_LEN: usize = 4096;

/// Maximum clock difference tolerated between the issuer and this server.
const CLOCK_SKEW_SECS: u64 = 30;

/// Participant role inside a room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// May end the meeting and remove participants.
    Host,
    /// Regular participant.
    Participant,
}

/// Claims carried by a room token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// Issuer; must match `auth.issuer`.
    pub iss: String,
    /// Participant identity, stable for a user (for example a Messenger user id).
    pub sub: String,
    /// The only room this token grants access to.
    pub room: String,
    /// Display name shown to other participants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Role inside the room.
    pub role: Role,
    /// Whether the participant may publish microphone, camera and screen.
    #[serde(default = "default_true")]
    pub can_publish: bool,
    /// Expiry, seconds since the Unix epoch.
    pub exp: u64,
    /// Issue time, seconds since the Unix epoch.
    pub iat: u64,
    /// Not-before time, seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nbf: Option<u64>,
}

fn default_true() -> bool {
    true
}

/// Why a token was rejected. The variants are intentionally coarse: clients
/// only need to know whether fetching a fresh token can help.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// Not a well-formed HS256 JWT.
    #[error("malformed token")]
    Malformed,
    /// Signature does not match.
    #[error("invalid token signature")]
    BadSignature,
    /// `exp` is in the past. A fresh token may help.
    #[error("token expired")]
    Expired,
    /// `nbf` is in the future.
    #[error("token not valid yet")]
    NotYetValid,
    /// Issuer, identity or room claim is invalid.
    #[error("invalid token claims")]
    InvalidClaims,
}

#[derive(Serialize, Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    typ: Option<String>,
}

/// Signs `claims` into a compact JWT.
pub fn issue(claims: &Claims, secret: &Secret) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("claims serialize"));
    let signing_input = format!("{header}.{payload}");
    let mut mac = HmacSha256::new_from_slice(secret.expose().as_bytes()).expect("any key length");
    mac.update(signing_input.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    format!("{signing_input}.{signature}")
}

/// Verifies a token and returns its claims.
///
/// `now` is seconds since the Unix epoch; it is a parameter so expiry can be
/// tested deterministically.
pub fn verify(token: &str, secret: &Secret, issuer: &str, now: u64) -> Result<Claims, AuthError> {
    if token.len() > MAX_TOKEN_LEN {
        return Err(AuthError::Malformed);
    }
    let mut parts = token.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(AuthError::Malformed);
    };

    let header: Header = decode_json(header)?;
    // Pinning the algorithm prevents "alg: none" and algorithm confusion.
    if header.alg != "HS256" {
        return Err(AuthError::Malformed);
    }

    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| AuthError::Malformed)?;
    let mut mac = HmacSha256::new_from_slice(secret.expose().as_bytes()).expect("any key length");
    mac.update(&token.as_bytes()[..token.len() - signature_len(token)]);
    // `verify_slice` compares in constant time.
    mac.verify_slice(&signature)
        .map_err(|_| AuthError::BadSignature)?;

    let claims: Claims = decode_json(payload)?;
    if claims.iss != issuer || !valid_id(&claims.sub) || !valid_id(&claims.room) {
        return Err(AuthError::InvalidClaims);
    }
    if claims.exp + CLOCK_SKEW_SECS < now {
        return Err(AuthError::Expired);
    }
    if claims.nbf.is_some_and(|nbf| nbf > now + CLOCK_SKEW_SECS) {
        return Err(AuthError::NotYetValid);
    }
    Ok(claims)
}

/// Length of the signature segment including the leading dot.
fn signature_len(token: &str) -> usize {
    token.rsplit('.').next().map_or(0, |s| s.len() + 1)
}

fn decode_json<T: serde::de::DeserializeOwned>(segment: &str) -> Result<T, AuthError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| AuthError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| AuthError::Malformed)
}

/// Room ids and identities are 1..=128 characters of `[A-Za-z0-9_.:@-]`.
///
/// The restricted alphabet keeps ids safe to use in logs and metrics labels.
pub fn valid_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':' | b'@'))
}

/// Seconds since the Unix epoch.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> Secret {
        Secret::new("test-secret-test-secret-test-secret")
    }

    fn claims(now: u64) -> Claims {
        Claims {
            iss: "ecochain".into(),
            sub: "alice".into(),
            room: "team-1".into(),
            name: Some("Alice".into()),
            role: Role::Host,
            can_publish: true,
            exp: now + 60,
            iat: now,
            nbf: None,
        }
    }

    #[test]
    fn round_trip() {
        let now = 1_700_000_000;
        let token = issue(&claims(now), &secret());
        assert_eq!(verify(&token, &secret(), "ecochain", now), Ok(claims(now)));
    }

    #[test]
    fn rejects_wrong_secret_issuer_and_expiry() {
        let now = 1_700_000_000;
        let token = issue(&claims(now), &secret());
        let other = Secret::new("other-secret-other-secret-other-secret");
        assert_eq!(
            verify(&token, &other, "ecochain", now),
            Err(AuthError::BadSignature)
        );
        assert_eq!(
            verify(&token, &secret(), "someone-else", now),
            Err(AuthError::InvalidClaims)
        );
        assert_eq!(
            verify(&token, &secret(), "ecochain", now + 3600),
            Err(AuthError::Expired)
        );
    }

    #[test]
    fn rejects_tampered_payload_and_alg_none() {
        let now = 1_700_000_000;
        let token = issue(&claims(now), &secret());
        let mut forged = claims(now);
        forged.room = "other-room".into();
        let forged_payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap());
        let parts: Vec<&str> = token.split('.').collect();
        let tampered = format!("{}.{}.{}", parts[0], forged_payload, parts[2]);
        assert_eq!(
            verify(&tampered, &secret(), "ecochain", now),
            Err(AuthError::BadSignature)
        );

        let none_header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let unsigned = format!("{none_header}.{}.", parts[1]);
        assert_eq!(
            verify(&unsigned, &secret(), "ecochain", now),
            Err(AuthError::Malformed)
        );
    }

    #[test]
    fn identity_alphabet_is_restricted() {
        assert!(valid_id("user-42@ecochain"));
        assert!(!valid_id(""));
        assert!(!valid_id("has space"));
        assert!(!valid_id("new\nline"));
    }
}
