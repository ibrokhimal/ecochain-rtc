//! Pending sockets are kept outside the participant/media collection.
use super::{participant::Connection, response, JoinOutcome, Room};
use crate::{
    auth::{Claims, Role},
    protocol::{close_code, ErrorCode, Outbound, ProtocolError, SessionRequest},
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct Lobby {
    pending: Vec<PendingGuest>,
}
#[derive(Clone)]
struct PendingGuest {
    id: String,
    claims: Claims,
    conn: Connection,
    at: Instant,
}
impl Room {
    pub(super) fn gate_join(
        &mut self,
        claims: &Claims,
        resume: Option<&str>,
        conn: &Connection,
        id: &str,
    ) -> Option<JoinOutcome> {
        let valid_resume = self
            .participants
            .iter()
            .any(|p| p.identity == claims.sub && resume == Some(p.session_id.as_str()));
        if claims.role == Role::Host || valid_resume {
            return None;
        }
        if self.collaboration.state["room_locked"] == true {
            conn.send(response(
                id,
                Err(ProtocolError::new(
                    ErrorCode::Forbidden,
                    "meeting is locked",
                )),
            ));
            return Some(JoinOutcome::Rejected(ErrorCode::Forbidden));
        }
        if self.collaboration.state["lobby_enabled"] != true {
            return None;
        }
        if self
            .services
            .rooms
            .upgrade()
            .is_some_and(|rooms| rooms.is_removed(&self.id, &claims.sub))
        {
            conn.send(response(
                id,
                Err(ProtocolError::new(
                    ErrorCode::Forbidden,
                    "removed from meeting",
                )),
            ));
            return Some(JoinOutcome::Rejected(ErrorCode::Forbidden));
        }
        if self.lobby.pending.len() >= 150 {
            conn.send(response(
                id,
                Err(ProtocolError::new(
                    ErrorCode::LimitExceeded,
                    "waiting room is full",
                )),
            ));
            return Some(JoinOutcome::Rejected(ErrorCode::LimitExceeded));
        }
        if let Some(pos) = self
            .lobby
            .pending
            .iter()
            .position(|p| p.claims.sub == claims.sub)
        {
            let old = self.lobby.pending.remove(pos);
            old.conn.send(Outbound::Close {
                code: close_code::SESSION_REPLACED,
                reason: "waiting session replaced",
            });
        }
        let pending = PendingGuest {
            id: super::random_id("LO_"),
            claims: claims.clone(),
            conn: conn.clone(),
            at: Instant::now(),
        };
        conn.send(response(
            id,
            Ok(json!({"waiting":true,"waiting_id":pending.id,"heartbeat_interval_ms":2000})),
        ));
        self.lobby.pending.push(pending);
        self.broadcast_lobby();
        Some(JoinOutcome::Joined)
    }
    pub(super) fn lobby_snapshot(&self) -> Value {
        json!({"waiting":self.lobby.pending.iter().map(|p|json!({"id":p.id,"identity":p.claims.sub,"name":p.claims.name.as_deref().unwrap_or(&p.claims.sub)})).collect::<Vec<_>>()})
    }
    fn broadcast_lobby(&self) {
        let event = Outbound::event("lobby.changed", self.lobby_snapshot());
        for p in &self.participants {
            if p.role == Role::Host {
                p.send(event.clone());
            }
        }
    }
    pub(super) fn lobby_request(
        &mut self,
        index: usize,
        action: &Value,
    ) -> Option<Result<Value, ProtocolError>> {
        let kind = action["type"].as_str()?;
        if !["lobby.list", "lobby.admit", "lobby.reject"].contains(&kind) {
            return None;
        }
        Some((|| {
            self.require_host(index)?;
            if kind == "lobby.list" {
                return Ok(self.lobby_snapshot());
            }
            let id = action["id"]
                .as_str()
                .ok_or_else(|| ProtocolError::new(ErrorCode::BadRequest, "waiting id required"))?;
            let pos = self
                .lobby
                .pending
                .iter()
                .position(|p| p.id == id)
                .ok_or_else(|| {
                    ProtocolError::new(ErrorCode::InvalidState, "guest is no longer waiting")
                })?;
            let guest = self.lobby.pending[pos].clone();
            if kind == "lobby.reject" {
                self.lobby.pending.remove(pos);
                guest.conn.send(Outbound::event(
                    "lobby.rejected",
                    json!({"reason":"rejected_by_host"}),
                ));
                guest.conn.send(Outbound::Close {
                    code: close_code::REMOVED,
                    reason: "admission rejected",
                });
            } else {
                if guest.claims.exp < crate::auth::unix_now() {
                    return Err(ProtocolError::new(
                        ErrorCode::TokenExpired,
                        "waiting token expired",
                    ));
                }
                let (guest_index, data) = self.join(&guest.claims, None, &guest.conn)?;
                self.lobby.pending.remove(pos);
                self.members.insert(guest.claims.sub);
                self.persist_members();
                guest.conn.send(Outbound::event("lobby.admitted", data));
                self.after_join(guest_index);
            }
            self.broadcast_lobby();
            Ok(json!({"id":id,"admitted":kind=="lobby.admit"}))
        })())
    }
    pub(super) fn pending_request(
        &mut self,
        conn: &Connection,
        id: &str,
        request: &SessionRequest,
    ) -> bool {
        let Some(pos) = self.lobby.pending.iter().position(|p| p.conn.id == conn.id) else {
            return false;
        };
        if matches!(request, SessionRequest::Leave {}) {
            self.lobby.pending.remove(pos);
            conn.send(response(id, Ok(json!({}))));
            conn.send(Outbound::Close {
                code: close_code::NORMAL,
                reason: "left waiting room",
            });
            self.broadcast_lobby();
        } else {
            conn.send(response(
                id,
                Err(ProtocolError::new(
                    ErrorCode::NotJoined,
                    "waiting for host admission",
                )),
            ));
        }
        true
    }
    pub(super) fn lobby_disconnected(&mut self, conn_id: u64) {
        let count = self.lobby.pending.len();
        self.lobby.pending.retain(|p| p.conn.id != conn_id);
        if count != self.lobby.pending.len() {
            self.broadcast_lobby();
        }
    }
    pub(super) fn expire_lobby(&mut self) {
        let before = self.lobby.pending.len();
        self.lobby.pending.retain(|p| {
            if p.claims.exp < crate::auth::unix_now() || p.at.elapsed() > Duration::from_secs(600) {
                p.conn.send(Outbound::Close {
                    code: close_code::UNAUTHORIZED,
                    reason: "waiting admission expired",
                });
                false
            } else {
                true
            }
        });
        if before != self.lobby.pending.len() {
            self.broadcast_lobby();
        }
    }
    pub(super) fn end_lobby(&mut self) {
        for guest in self.lobby.pending.drain(..) {
            guest.conn.send(Outbound::Close {
                code: close_code::ROOM_ENDED,
                reason: "meeting ended",
            });
        }
    }
}

/// Ephemeral host commands bypass the durable workspace reducer.
pub(super) fn is_control(action: &Value) -> bool {
    matches!(
        action["type"].as_str(),
        Some(
            "lobby.list"
                | "lobby.admit"
                | "lobby.reject"
                | "participant.mute"
                | "reaction.send"
                | "recording.start"
                | "recording.stop"
                | "recording.status"
                | "captions.start"
                | "captions.stop"
                | "captions.status"
        )
    )
}
