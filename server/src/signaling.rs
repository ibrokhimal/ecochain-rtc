//! WebSocket signaling endpoint (`GET /v1/rtc`).
//!
//! One task per socket. The task authenticates the first `join`, forwards
//! every later request to the room task and writes whatever the room sends
//! back. Requests of one socket are handled strictly in order, so a client
//! never sees responses or events reordered.

use crate::{
    auth::{self, AuthError},
    protocol::{self, close_code, ErrorCode, Inbound, Outbound, ProtocolError, SessionRequest},
    room::{Connection, JoinOutcome, RoomCommand, RoomHandle},
    App,
};
use axum::{
    extract::{
        ws::{CloseFrame, Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

/// Clients send `ping` this often (told to them in the join response).
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// A socket that sends nothing for this long is considered dead. Three
/// missed heartbeats tolerate short radio stalls on mobile networks.
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(15);

/// Time allowed between connect and the `join` request.
const JOIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Upgrades the HTTP request to a signaling WebSocket.
pub async fn upgrade(State(app): State<Arc<App>>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(protocol::MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| run(app, socket))
}

/// Keeps `connections_active` correct even if the task panics.
struct ConnectionGauge(Arc<App>);

impl ConnectionGauge {
    fn new(app: Arc<App>) -> Self {
        app.metrics
            .connections_active
            .fetch_add(1, Ordering::Relaxed);
        Self(app)
    }
}

impl Drop for ConnectionGauge {
    fn drop(&mut self) {
        self.0
            .metrics
            .connections_active
            .fetch_sub(1, Ordering::Relaxed);
    }
}

async fn run(app: Arc<App>, socket: WebSocket) {
    let _gauge = ConnectionGauge::new(app.clone());
    let conn_id = app.next_connection_id.fetch_add(1, Ordering::Relaxed);
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<Outbound>(512);
    let conn = Connection { id: conn_id, tx };

    // Writer: drains the queue until a Close message or until every sender
    // (this task and the room) dropped its end.
    let mut writer = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let frame = match &message {
                Outbound::Close { code, reason } => Message::Close(Some(CloseFrame {
                    code: *code,
                    reason: (*reason).into(),
                })),
                other => Message::Text(other.to_json().unwrap_or_default().into()),
            };
            let closing = matches!(message, Outbound::Close { .. });
            if sink.send(frame).await.is_err() || closing {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let room = tokio::select! {
        room = join(&app, &conn, &mut stream) => room,
        _ = &mut writer => None,
    };

    if let Some(room) = room {
        session(&room, &conn, &mut stream, &mut writer).await;
        room.send(RoomCommand::Disconnected { conn_id }).await;
    }
    drop(conn);
    // The writer may already have been awaited to completion by a select
    // above; polling a finished JoinHandle again panics.
    if !writer.is_finished() {
        let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
    }
}

/// Waits for `join`, verifies the token and attaches the socket to a room.
async fn join(
    app: &App,
    conn: &Connection,
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
) -> Option<RoomHandle> {
    let reject = |id: Option<&str>, error: ProtocolError, close: u16| {
        app.metrics.join_failed(error.code);
        if let Some(id) = id {
            conn.send(Outbound::Response {
                id: id.to_owned(),
                result: Err(error),
            });
        }
        conn.send(Outbound::Close {
            code: close,
            reason: "join failed",
        });
        None
    };

    let text = match tokio::time::timeout(JOIN_TIMEOUT, next_text(stream)).await {
        Ok(Some(text)) => text,
        Ok(None) => return None,
        Err(_) => {
            conn.send(Outbound::Close {
                code: close_code::TIMEOUT,
                reason: "join timeout",
            });
            return None;
        }
    };
    let (id, request) = match protocol::parse(&text) {
        Ok(Inbound::Join(id, request)) => (id, request),
        Ok(Inbound::Session(id, _)) => {
            return reject(
                Some(&id),
                ProtocolError::new(ErrorCode::NotJoined, "the first message must be join"),
                close_code::PROTOCOL_ERROR,
            )
        }
        Err((id, error)) => return reject(id.as_deref(), error, close_code::PROTOCOL_ERROR),
    };

    let config = &app.config;
    let claims = match auth::verify(
        &request.token,
        &config.auth.token_secret,
        &config.auth.issuer,
        auth::unix_now(),
    ) {
        Ok(claims) => claims,
        Err(error) => {
            let code = if error == AuthError::Expired {
                ErrorCode::TokenExpired
            } else {
                ErrorCode::Unauthorized
            };
            return reject(
                Some(&id),
                ProtocolError::new(code, error.to_string()),
                close_code::UNAUTHORIZED,
            );
        }
    };
    if request
        .room
        .as_ref()
        .is_some_and(|room| *room != claims.room)
    {
        return reject(
            Some(&id),
            ProtocolError::new(ErrorCode::Forbidden, "token is for a different room"),
            close_code::UNAUTHORIZED,
        );
    }

    let room = match app.rooms.for_join(&claims.room) {
        Ok(room) => room,
        Err(code) => {
            let close = if code == ErrorCode::RoomEnded {
                close_code::ROOM_ENDED
            } else {
                close_code::PROTOCOL_ERROR
            };
            return reject(
                Some(&id),
                ProtocolError::new(code, "cannot join this room"),
                close,
            );
        }
    };
    tracing::info!(
        room = %claims.room,
        identity = %claims.sub,
        platform = %request.client.platform,
        resume = request.resume_session_id.is_some(),
        "join"
    );
    let (reply, outcome) = oneshot::channel();
    let sent = room
        .send(RoomCommand::Join {
            claims,
            request_id: id.clone(),
            resume: request.resume_session_id,
            conn: conn.clone(),
            reply,
        })
        .await;
    if !sent {
        return reject(
            Some(&id),
            ProtocolError::new(ErrorCode::Unavailable, "room is closing, retry"),
            close_code::GOING_AWAY,
        );
    }
    match outcome.await {
        Ok(JoinOutcome::Joined) => Some(room),
        Ok(JoinOutcome::Rejected(code)) => {
            let close = match code {
                ErrorCode::RoomEnded => close_code::ROOM_ENDED,
                ErrorCode::Unavailable => close_code::GOING_AWAY,
                _ => close_code::PROTOCOL_ERROR,
            };
            conn.send(Outbound::Close {
                code: close,
                reason: "join rejected",
            });
            None
        }
        Err(_) => None,
    }
}

/// Forwards requests of a joined socket until it closes, times out or the
/// room closes it.
async fn session(
    room: &RoomHandle,
    conn: &Connection,
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    writer: &mut tokio::task::JoinHandle<()>,
) {
    loop {
        let text = tokio::select! {
            next = tokio::time::timeout(HEARTBEAT_TIMEOUT, next_text(stream)) => match next {
                Ok(Some(text)) => text,
                Ok(None) => return,
                Err(_) => {
                    conn.send(Outbound::Close { code: close_code::TIMEOUT, reason: "heartbeat timeout" });
                    return;
                }
            },
            // The room closed the socket (leave, end, replaced, removed).
            _ = &mut *writer => return,
        };
        match protocol::parse(&text) {
            // Keepalive is answered here, not by the room task, so a busy
            // room can never make healthy clients look dead.
            Ok(Inbound::Session(id, SessionRequest::Ping {})) => {
                conn.send(Outbound::Response {
                    id,
                    result: Ok(serde_json::json!({"server_time_ms": unix_millis()})),
                });
            }
            Ok(Inbound::Session(request_id, request)) => {
                let command = RoomCommand::Request {
                    conn: conn.clone(),
                    request_id,
                    request,
                };
                if !room.send(command).await {
                    conn.send(Outbound::Close {
                        code: close_code::GOING_AWAY,
                        reason: "room closed",
                    });
                    return;
                }
            }
            Ok(Inbound::Join(id, _)) => {
                conn.send(Outbound::Response {
                    id,
                    result: Err(ProtocolError::new(
                        ErrorCode::InvalidState,
                        "already joined",
                    )),
                });
            }
            Err((Some(id), error)) => {
                conn.send(Outbound::Response {
                    id,
                    result: Err(error),
                });
            }
            Err((None, _)) => {
                conn.send(Outbound::Close {
                    code: close_code::PROTOCOL_ERROR,
                    reason: "malformed message",
                });
                return;
            }
        }
    }
}

/// Next text frame. Pings, pongs and binary frames are skipped; `None` on
/// close or error.
async fn next_text(stream: &mut futures_util::stream::SplitStream<WebSocket>) -> Option<String> {
    loop {
        match stream.next().await? {
            Ok(Message::Text(text)) => return Some(text.to_string()),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => continue,
        }
    }
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
