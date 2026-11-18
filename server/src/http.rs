//! HTTP routes: health, metrics, admin API and the development token helper.

use crate::{
    auth::{self, Claims, Role},
    config::Mode,
    room::RoomCommand,
    signaling, App,
};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::oneshot;

/// Longest token lifetime the token endpoints issue.
const MAX_TOKEN_TTL_SECS: u64 = 24 * 3600;

/// Builds the router.
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/metrics", get(metrics))
        .route("/v1/rtc", get(signaling::upgrade))
        .route("/v1/rooms", post(create_room).get(list_rooms))
        .route("/v1/rooms/{room_id}", delete(end_room))
        .route("/v1/tokens", post(issue_token))
        .route("/v1/dev/token", post(dev_token))
        .with_state(app)
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

/// Checks `Authorization: Bearer <admin key>` in constant time.
fn is_admin(app: &App, headers: &HeaderMap) -> bool {
    let Some(given) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return false;
    };
    constant_time_eq(
        given.as_bytes(),
        app.config.auth.admin_key.expose().as_bytes(),
    )
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn metrics(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if app.config.mode == Mode::Production && !is_admin(&app, &headers) {
        return error(StatusCode::UNAUTHORIZED, "admin key required");
    }
    app.metrics.render().into_response()
}

#[derive(Deserialize)]
struct CreateRoom {
    room_id: Option<String>,
    max_participants: Option<usize>,
}

async fn create_room(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(body): Json<CreateRoom>,
) -> Response {
    if !is_admin(&app, &headers) {
        return error(StatusCode::UNAUTHORIZED, "admin key required");
    }
    let room_id = body
        .room_id
        .unwrap_or_else(|| crate::room::random_id("RM_"));
    if !auth::valid_id(&room_id) {
        return error(StatusCode::BAD_REQUEST, "invalid room_id");
    }
    let max = body
        .max_participants
        .unwrap_or(app.config.rooms.default_max_participants);
    match app.rooms.create(&room_id, max) {
        Ok(_) => (
            StatusCode::CREATED,
            Json(json!({"room_id": room_id, "max_participants": max})),
        )
            .into_response(),
        Err(message) => error(StatusCode::CONFLICT, message),
    }
}

async fn list_rooms(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if !is_admin(&app, &headers) {
        return error(StatusCode::UNAUTHORIZED, "admin key required");
    }
    let mut rooms = Vec::new();
    for room in app.rooms.list() {
        let (reply, summary) = oneshot::channel();
        if room.send(RoomCommand::Summary { reply }).await {
            if let Ok(summary) = summary.await {
                rooms.push(summary);
            }
        }
    }
    Json(json!({"rooms": rooms})).into_response()
}

async fn end_room(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> Response {
    if !is_admin(&app, &headers) {
        return error(StatusCode::UNAUTHORIZED, "admin key required");
    }
    match app.rooms.get(&room_id) {
        Some(room) => {
            room.send(RoomCommand::End {
                reason: "ended_by_admin",
            })
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        None => error(StatusCode::NOT_FOUND, "room not running"),
    }
}

#[derive(Deserialize)]
struct TokenRequest {
    room: String,
    identity: String,
    name: Option<String>,
    #[serde(default = "participant")]
    role: Role,
    #[serde(default = "yes")]
    can_publish: bool,
    ttl_secs: Option<u64>,
}

fn participant() -> Role {
    Role::Participant
}

fn yes() -> bool {
    true
}

fn sign(app: &App, request: TokenRequest) -> Result<Value, &'static str> {
    if !auth::valid_id(&request.room) || !auth::valid_id(&request.identity) {
        return Err("invalid room or identity");
    }
    let now = auth::unix_now();
    let ttl = request
        .ttl_secs
        .unwrap_or(3600)
        .clamp(60, MAX_TOKEN_TTL_SECS);
    let claims = Claims {
        iss: app.config.auth.issuer.clone(),
        sub: request.identity,
        room: request.room,
        name: request.name,
        role: request.role,
        can_publish: request.can_publish,
        exp: now + ttl,
        iat: now,
        nbf: None,
    };
    Ok(
        json!({"token": auth::issue(&claims, &app.config.auth.token_secret), "expires_at": claims.exp}),
    )
}

/// Admin token issuing, for back-office tools. Product backends normally
/// sign tokens themselves with the shared secret.
async fn issue_token(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(body): Json<TokenRequest>,
) -> Response {
    if !is_admin(&app, &headers) {
        return error(StatusCode::UNAUTHORIZED, "admin key required");
    }
    match sign(&app, body) {
        Ok(value) => Json(value).into_response(),
        Err(message) => error(StatusCode::BAD_REQUEST, message),
    }
}

/// Unauthenticated token helper. Exists only in development mode so the
/// demo apps can join without a backend.
async fn dev_token(State(app): State<Arc<App>>, Json(body): Json<TokenRequest>) -> Response {
    if app.config.mode != Mode::Development {
        return error(StatusCode::NOT_FOUND, "not found");
    }
    tracing::info!(room = %body.room, identity = %body.identity, "issued development token");
    match sign(&app, body) {
        Ok(value) => Json(value).into_response(),
        Err(message) => error(StatusCode::BAD_REQUEST, message),
    }
}
