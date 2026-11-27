//! Instance-scoped archive access. Live-room tokens cannot read old occurrences.
use crate::{
    auth::{self, Claims, Role},
    config::{Config, Secret},
    room::RoomCommand,
    App,
};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::Sha256;
use std::sync::Arc;
use tokio::sync::oneshot;

const MAX_IMAGE: usize = 5 * 1024 * 1024;
fn secret(config: &Config) -> Secret {
    let mut mac = Hmac::<Sha256>::new_from_slice(config.auth.token_secret.expose().as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(b"ecochain/archive_read/v1");
    Secret::new(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}
/// Issues a read-only token for one admitted identity and one meeting instance.
pub fn issue(config: &Config, instance: &str, identity: &str, ttl: u64) -> String {
    let now = auth::unix_now();
    auth::issue(
        &Claims {
            iss: format!("{}:archive_read", config.auth.issuer),
            sub: identity.to_owned(),
            room: instance.to_owned(),
            name: None,
            role: Role::Participant,
            can_publish: false,
            exp: now + ttl.clamp(60, 86400),
            iat: now,
            nbf: None,
        },
        &secret(config),
    )
}
/// Checks the archive-only key/issuer and instance binding, including token expiry.
pub fn verify(config: &Config, token: &str, instance: &str) -> Result<Claims, auth::AuthError> {
    let claims = auth::verify(
        token,
        &secret(config),
        &format!("{}:archive_read", config.auth.issuer),
        auth::unix_now(),
    )?;
    if claims.room != instance || uuid::Uuid::parse_str(instance).is_err() {
        return Err(auth::AuthError::InvalidClaims);
    }
    Ok(claims)
}
/// Issues an archive-management credential only to a verified meeting host.
pub fn issue_manage(config: &Config, instance: &str, identity: &str, ttl: u64) -> String {
    let now = auth::unix_now();
    auth::issue(
        &Claims {
            iss: format!("{}:archive_manage", config.auth.issuer),
            sub: identity.to_owned(),
            room: instance.to_owned(),
            name: None,
            role: Role::Host,
            can_publish: false,
            exp: now + ttl.clamp(60, 86400),
            iat: now,
            nbf: None,
        },
        &manage_secret(config),
    )
}
fn manage_secret(config: &Config) -> Secret {
    let mut mac = Hmac::<Sha256>::new_from_slice(config.auth.token_secret.expose().as_bytes())
        .expect("HMAC key");
    mac.update(b"ecochain/archive_manage/v1");
    Secret::new(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}
/// Checks management scope, instance binding and host role; read tokens fail this check.
pub fn verify_manage(
    config: &Config,
    token: &str,
    instance: &str,
) -> Result<Claims, auth::AuthError> {
    let claims = auth::verify(
        token,
        &manage_secret(config),
        &format!("{}:archive_manage", config.auth.issuer),
        auth::unix_now(),
    )?;
    if claims.room != instance
        || claims.role != Role::Host
        || uuid::Uuid::parse_str(instance).is_err()
    {
        return Err(auth::AuthError::InvalidClaims);
    }
    Ok(claims)
}
/// Authorizes a host management API without treating read access as write authority.
pub(crate) async fn authorized_manage(
    app: &App,
    headers: &HeaderMap,
    instance: &str,
) -> Result<Claims, Response> {
    let claims =
        verify_manage(&app.config, bearer(headers).unwrap_or(""), instance).map_err(|_| {
            error(
                StatusCode::UNAUTHORIZED,
                "archive management token required",
            )
        })?;
    let vault = app.vault.clone().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "archive storage not configured",
        )
    })?;
    let id = instance.to_owned();
    let identity = claims.sub.clone();
    if !matches!(
        tokio::task::spawn_blocking(move || vault.is_member(&id, &identity)).await,
        Ok(Ok(true))
    ) {
        return Err(error(StatusCode::FORBIDDEN, "not an admitted manager"));
    }
    Ok(claims)
}

/// Routes composed into the existing authenticated server.
pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/v1/archive-tokens", post(token))
        .route("/v1/archives/{instance}", get(read))
        .route("/v1/archives/{instance}/files/{id}", get(file))
        .route("/v1/archives/{instance}/playback-link", post(playback_link))
        .route(
            "/v1/archives/{instance}/playback-session",
            post(playback_session),
        )
        .route("/v1/archives/{instance}/player", get(player))
        .route("/v1/recording-player.js", get(player_js))
        .route("/v1/recording-player.css", get(player_css))
        .route(
            "/v1/archives/{instance}/images",
            post(image).layer(DefaultBodyLimit::max(MAX_IMAGE)),
        )
}
fn error(code: StatusCode, message: &str) -> Response {
    (code, Json(json!({"error":message}))).into_response()
}
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}
async fn authorized_member(app: &App, headers: &HeaderMap, instance: &str) -> Result<(), Response> {
    let claims = verify(
        &app.config,
        request_token(headers, instance).unwrap_or(""),
        instance,
    )
    .map_err(|_| error(StatusCode::UNAUTHORIZED, "archive token required"))?;
    let vault = app.vault.clone().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "archive storage not configured",
        )
    })?;
    let instance = instance.to_owned();
    match tokio::task::spawn_blocking(move || vault.is_member(&instance, &claims.sub)).await {
        Ok(Ok(true)) => Ok(()),
        Ok(Ok(false)) => Err(error(
            StatusCode::FORBIDDEN,
            "not an admitted archive member",
        )),
        _ => Err(error(StatusCode::NOT_FOUND, "archive unavailable")),
    }
}
async fn authorized(
    app: &App,
    headers: &HeaderMap,
    instance: &str,
) -> Result<crate::storage::Archive, Response> {
    let claims = verify(
        &app.config,
        request_token(headers, instance).unwrap_or(""),
        instance,
    )
    .map_err(|_| error(StatusCode::UNAUTHORIZED, "archive token required"))?;
    let vault = app.vault.clone().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "archive storage not configured",
        )
    })?;
    let id = instance.to_owned();
    let archive = tokio::task::spawn_blocking(move || vault.read(&id))
        .await
        .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "archive unavailable"))?
        .map_err(|_| error(StatusCode::NOT_FOUND, "archive not found"))?;
    if !archive.members.contains(&claims.sub) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "not an admitted archive member",
        ));
    }
    Ok(archive)
}
async fn read(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(instance): Path<String>,
) -> Response {
    match authorized(&app, &headers, &instance).await {
        Ok(mut archive) => {
            let vault = app.vault.clone().unwrap();
            let id = instance.clone();
            match tokio::task::spawn_blocking(move || vault.jobs(&id)).await {
                Ok(Ok(jobs)) => {
                    archive.state["members"] = json!(archive.members);
                    archive.state["provider_capabilities"] = json!({"stt":app.config.providers.stt.is_some(),"summary":app.config.providers.summary.is_some(),"jira":app.config.providers.jira.is_some()});
                    archive.state["recordings"] = json!(jobs
                        .iter()
                        .filter(|j| j["kind"] == "recording")
                        .collect::<Vec<_>>());
                    archive.state["jobs"] = json!(jobs);
                    Json(archive.state).into_response()
                }
                _ => error(StatusCode::SERVICE_UNAVAILABLE, "archive jobs unavailable"),
            }
        }
        Err(e) => e,
    }
}
async fn file(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path((instance, id)): Path<(String, String)>,
) -> Response {
    if let Err(e) = authorized_member(&app, &headers, &instance).await {
        return e;
    }
    let vault = app.vault.clone().unwrap();
    let v = vault.clone();
    let room = instance.clone();
    let file_id = id.clone();
    let metadata = tokio::task::spawn_blocking(move || v.stream_metadata(&room, &file_id)).await;
    if let Ok(Ok((length, mime))) = metadata {
        let range = headers.get("range").and_then(|v| v.to_str().ok());
        let (start, end, status) = match parse_range(range, length) {
            Ok(v) => v,
            Err(_) => {
                return (
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [("content-range", format!("bytes */{length}"))],
                )
                    .into_response()
            }
        };
        let stream = futures_util::stream::unfold(
            (vault, instance, id, start, end),
            |(vault, instance, id, offset, end)| async move {
                if offset >= end {
                    return None;
                }
                let count = (end - offset).min(1024 * 1024) as usize;
                let v = vault.clone();
                let room = instance.clone();
                let file = id.clone();
                let result =
                    tokio::task::spawn_blocking(move || v.read_range(&room, &file, offset, count))
                        .await;
                match result {
                    Ok(Ok(bytes)) => Some((
                        Ok::<Bytes, std::io::Error>(Bytes::from(bytes)),
                        (vault, instance, id, offset + count as u64, end),
                    )),
                    _ => Some((
                        Err(std::io::Error::other("recording read failed")),
                        (vault, instance, id, end, end),
                    )),
                }
            },
        );
        let mut response = Response::new(axum::body::Body::from_stream(stream));
        *response.status_mut() = status;
        let h = response.headers_mut();
        h.insert("content-type", mime.parse().unwrap());
        h.insert("content-length", (end - start).to_string().parse().unwrap());
        h.insert("accept-ranges", "bytes".parse().unwrap());
        h.insert("cache-control", "private, no-store".parse().unwrap());
        h.insert("x-content-type-options", "nosniff".parse().unwrap());
        if status == StatusCode::PARTIAL_CONTENT {
            h.insert(
                "content-range",
                format!("bytes {start}-{}/{length}", end - 1)
                    .parse()
                    .unwrap(),
            );
        }
        return response;
    }
    match tokio::task::spawn_blocking(move || vault.get(&instance, &id)).await {
        Ok(Ok(bytes)) => match image_dimensions(&bytes) {
            Ok((mime, _, _)) => (
                [
                    ("content-type", mime),
                    ("cache-control", "private, no-store"),
                    ("x-content-type-options", "nosniff"),
                ],
                bytes,
            )
                .into_response(),
            Err(_) => error(StatusCode::NOT_FOUND, "image not found"),
        },
        _ => error(StatusCode::NOT_FOUND, "file not found"),
    }
}
/// Parses a single HTTP byte range; the end offset returned is exclusive.
pub fn parse_range(
    range: Option<&str>,
    length: u64,
) -> Result<(u64, u64, StatusCode), &'static str> {
    let Some(range) = range else {
        return Ok((0, length, StatusCode::OK));
    };
    let range = range.strip_prefix("bytes=").ok_or("invalid range")?;
    let (start, end) = range.split_once('-').ok_or("invalid range")?;
    if length == 0 {
        return Err("empty file");
    }
    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| "invalid suffix")?;
        if suffix == 0 {
            return Err("empty range");
        }
        return Ok((
            length.saturating_sub(suffix),
            length,
            StatusCode::PARTIAL_CONTENT,
        ));
    }
    let start = start.parse::<u64>().map_err(|_| "invalid start")?;
    let end = if end.is_empty() {
        length
    } else {
        end.parse::<u64>()
            .map_err(|_| "invalid end")?
            .checked_add(1)
            .ok_or("invalid end")?
            .min(length)
    };
    if start >= length || end <= start {
        return Err("range outside file");
    }
    Ok((start, end, StatusCode::PARTIAL_CONTENT))
}
#[derive(Deserialize)]
struct TokenRequest {
    instance_id: String,
    identity: String,
    ttl_secs: Option<u64>,
}
async fn token(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(request): Json<TokenRequest>,
) -> Response {
    if !crate::http::is_admin(&app, &headers) {
        return error(StatusCode::UNAUTHORIZED, "admin key required");
    }
    if !auth::valid_id(&request.identity) {
        return error(StatusCode::BAD_REQUEST, "invalid identity");
    }
    let Some(vault) = app.vault.clone() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "archive storage not configured",
        );
    };
    let instance = request.instance_id.clone();
    let archive = match tokio::task::spawn_blocking(move || vault.read(&instance)).await {
        Ok(Ok(a)) => a,
        _ => return error(StatusCode::NOT_FOUND, "archive not found"),
    };
    if !archive.members.contains(&request.identity) {
        return error(StatusCode::FORBIDDEN, "identity was not admitted");
    }
    Json(json!({"token":issue(&app.config,&request.instance_id,&request.identity,request.ttl_secs.unwrap_or(3600))})).into_response()
}
async fn image(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(instance): Path<String>,
    bytes: Bytes,
) -> Response {
    let claims = match auth::verify(
        bearer(&headers).unwrap_or(""),
        &app.config.auth.token_secret,
        &app.config.auth.issuer,
        auth::unix_now(),
    ) {
        Ok(c) => c,
        Err(_) => return error(StatusCode::UNAUTHORIZED, "live meeting token required"),
    };
    let Some(vault) = app.vault.clone() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "archive storage not configured",
        );
    };
    let Some(room) = app.rooms.get(&claims.room) else {
        return error(StatusCode::FORBIDDEN, "meeting is not active");
    };
    let (reply, result) = oneshot::channel();
    if !room
        .send(RoomCommand::AuthorizeImage {
            instance: instance.clone(),
            identity: claims.sub,
            reply,
        })
        .await
    {
        return error(StatusCode::SERVICE_UNAVAILABLE, "meeting unavailable");
    }
    if !matches!(
        tokio::time::timeout(std::time::Duration::from_secs(3), result).await,
        Ok(Ok(true))
    ) {
        return error(StatusCode::FORBIDDEN, "drawing permission required");
    }
    let (mime, width, height) = match image_dimensions(&bytes) {
        Ok(v) => v,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "valid PNG or JPEG up to 8192 pixels required",
            )
        }
    };
    let saved = tokio::task::spawn_blocking(move || vault.put(&instance, &bytes)).await;
    match saved {
        Ok(Ok(id)) => {
            Json(json!({"file_id":id,"mime":mime,"width":width,"height":height})).into_response()
        }
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "image could not be saved"),
    }
}
/// Reads image dimensions before decoding to limit memory used by attachments.
pub fn image_dimensions(bytes: &[u8]) -> Result<(&'static str, u32, u32), &'static str> {
    if bytes.len() > MAX_IMAGE {
        return Err("image too large");
    }
    let dimensions =
        if bytes.len() >= 33 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" && &bytes[12..16] == b"IHDR" {
            (
                "image/png",
                u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
                u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
            )
        } else if bytes.starts_with(&[0xff, 0xd8]) {
            let mut pos = 2;
            let mut found = None;
            while pos + 4 <= bytes.len() {
                if bytes[pos] != 0xff {
                    return Err("invalid JPEG");
                }
                while pos < bytes.len() && bytes[pos] == 0xff {
                    pos += 1;
                }
                if pos >= bytes.len() {
                    break;
                }
                let marker = bytes[pos];
                pos += 1;
                if marker == 0xda || marker == 0xd9 {
                    break;
                }
                if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
                    continue;
                }
                if pos + 2 > bytes.len() {
                    break;
                }
                let len = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as usize;
                if len < 2 || pos + len > bytes.len() {
                    return Err("truncated JPEG");
                }
                if [
                    0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf,
                ]
                .contains(&marker)
                    && len >= 8
                {
                    found = Some((
                        "image/jpeg",
                        u16::from_be_bytes([bytes[pos + 5], bytes[pos + 6]]) as u32,
                        u16::from_be_bytes([bytes[pos + 3], bytes[pos + 4]]) as u32,
                    ));
                    break;
                }
                pos += len;
            }
            found.ok_or("JPEG dimensions missing")?
        } else {
            return Err("unsupported image");
        };
    if dimensions.1 == 0 || dimensions.2 == 0 || dimensions.1 > 8192 || dimensions.2 > 8192 {
        return Err("image dimensions out of bounds");
    }
    Ok(dimensions)
}

fn cookie_name(instance: &str) -> String {
    format!("eco_archive_{}", instance.replace('-', ""))
}
fn request_token<'a>(headers: &'a HeaderMap, instance: &str) -> Option<&'a str> {
    bearer(headers).or_else(|| {
        let name = format!("{}=", cookie_name(instance));
        headers
            .get("cookie")?
            .to_str()
            .ok()?
            .split(';')
            .find_map(|c| c.trim().strip_prefix(&name))
    })
}
#[derive(Deserialize)]
struct PlaybackLinkRequest {
    recording_id: String,
}
async fn playback_link(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(instance): Path<String>,
    Json(body): Json<PlaybackLinkRequest>,
) -> Response {
    if bearer(&headers).is_none() {
        return error(StatusCode::UNAUTHORIZED, "archive bearer token required");
    }
    if let Err(e) = authorized_member(&app, &headers, &instance).await {
        return e;
    }
    if uuid::Uuid::parse_str(&body.recording_id).is_err() {
        return error(StatusCode::BAD_REQUEST, "invalid recording id");
    }
    let mut links = app.playback_links.lock().unwrap();
    links.retain(|_, (at, _, _)| at.elapsed() < std::time::Duration::from_secs(60));
    if links.len() >= 1024 {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "too many playback handoffs",
        );
    }
    let handoff = uuid::Uuid::new_v4().to_string();
    links.insert(
        handoff.clone(),
        (
            std::time::Instant::now(),
            instance.clone(),
            bearer(&headers).unwrap().to_owned(),
        ),
    );
    Json(json!({"url":format!("/v1/archives/{instance}/player?recording={}#handoff={handoff}",body.recording_id)})).into_response()
}
#[derive(Deserialize)]
struct PlaybackSessionRequest {
    handoff: String,
}
async fn playback_session(
    State(app): State<Arc<App>>,
    Path(instance): Path<String>,
    Json(body): Json<PlaybackSessionRequest>,
) -> Response {
    let link = app.playback_links.lock().unwrap().remove(&body.handoff);
    let Some((at, expected, token)) = link else {
        return error(
            StatusCode::UNAUTHORIZED,
            "playback handoff expired or consumed",
        );
    };
    if expected != instance || at.elapsed() > std::time::Duration::from_secs(60) {
        return error(StatusCode::UNAUTHORIZED, "playback handoff expired");
    }
    let mut headers = HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    if let Err(e) = authorized_member(&app, &headers, &instance).await {
        return e;
    }
    let max_age = verify(&app.config, &token, &instance)
        .map(|c| c.exp.saturating_sub(crate::auth::unix_now()).min(86400))
        .unwrap_or(0);
    let secure = if app.config.mode == crate::config::Mode::Production {
        "; Secure"
    } else {
        ""
    };
    ([("set-cookie",format!("{}={token}; Path=/v1/archives/{instance}; Max-Age={max_age}; HttpOnly; SameSite=Strict{secure}",cookie_name(&instance)) )],Json(json!({"ok":true}))).into_response()
}
async fn player() -> Response {
    ([("content-type","text/html; charset=utf-8"),("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self'; media-src 'self'; img-src 'self' blob: data:; connect-src 'self'; frame-ancestors 'none'"),("referrer-policy","no-referrer"),("cache-control","no-store")],include_str!("recording/player.html")).into_response()
}
async fn player_js() -> Response {
    (
        [
            ("content-type", "text/javascript; charset=utf-8"),
            ("x-content-type-options", "nosniff"),
        ],
        include_str!("recording/player.js"),
    )
        .into_response()
}
async fn player_css() -> Response {
    (
        [
            ("content-type", "text/css; charset=utf-8"),
            ("x-content-type-options", "nosniff"),
        ],
        include_str!("recording/player.css"),
    )
        .into_response()
}
