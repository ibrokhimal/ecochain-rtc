//! Room actor.
//!
//! Each room runs as one tokio task that owns all of its state: participants,
//! their str0m peers and the forwarding tables. Signaling connections and
//! the UDP dispatcher talk to it through channels only. Because a single task
//! processes every command in order, concurrent joins, duplicate requests and
//! a leave racing a reconnect are resolved deterministically without locks.

mod media;
mod participant;

pub use participant::Connection;

use crate::{
    auth::{Claims, Role},
    config::Config,
    metrics::Metrics,
    protocol::{close_code, ErrorCode, Outbound, ProtocolError, SessionRequest},
    rooms::Rooms,
    sfu::udp::{Datagram, MediaSockets, RoomPackets},
};
use participant::Participant;
use rand::{distributions::Alphanumeric, Rng};
use serde_json::{json, Value};
use std::{
    sync::{atomic::Ordering, Arc, Weak},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

/// Rooms created through the admin API wait this long for the first join.
const UNUSED_ROOM_TTL: Duration = Duration::from_secs(15 * 60);

/// Housekeeping interval: grace expiry, offer timeouts, layer allocation.
const TICK: Duration = Duration::from_millis(500);

/// Result of a join, reported back to the signaling task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinOutcome {
    /// The socket is attached to a session. The response was already sent.
    Joined,
    /// Rejected. The error response was already sent.
    Rejected(ErrorCode),
}

/// Messages a room accepts.
pub enum RoomCommand {
    /// Attach a socket to a new or resumed session.
    Join {
        /// Verified token claims.
        claims: Claims,
        /// Request id of the join message.
        request_id: String,
        /// Session to resume, if the client has one.
        resume: Option<String>,
        /// The socket.
        conn: Connection,
        /// Outcome for the signaling task.
        reply: oneshot::Sender<JoinOutcome>,
    },
    /// A request from a joined socket. The room sends the response.
    Request {
        /// Socket the request came from.
        conn: Connection,
        /// Request id.
        request_id: String,
        /// The request.
        request: SessionRequest,
    },
    /// The socket closed or timed out.
    Disconnected {
        /// Connection id.
        conn_id: u64,
    },
    /// End the room (admin API).
    End {
        /// Short reason sent to clients.
        reason: &'static str,
    },
    /// Summary for the admin API.
    Summary {
        /// Where to send the summary.
        reply: oneshot::Sender<Value>,
    },
}

/// Cheap, cloneable reference to a running room.
#[derive(Clone)]
pub struct RoomHandle {
    /// Room id.
    pub id: String,
    commands: mpsc::Sender<RoomCommand>,
}

impl RoomHandle {
    /// Sends a command. Returns `false` when the room already stopped.
    pub async fn send(&self, command: RoomCommand) -> bool {
        self.commands.send(command).await.is_ok()
    }

    /// Whether the room task stopped.
    pub fn is_closed(&self) -> bool {
        self.commands.is_closed()
    }

    /// Whether both handles point at the same room task.
    pub fn same_room(&self, other: &RoomHandle) -> bool {
        self.commands.same_channel(&other.commands)
    }
}

/// Shared services a room needs.
#[derive(Clone)]
pub struct RoomServices {
    /// Configuration.
    pub config: Arc<Config>,
    /// Counters.
    pub metrics: Arc<Metrics>,
    /// UDP sockets.
    pub sockets: MediaSockets,
    /// Registry, to deregister on shutdown.
    pub rooms: Weak<Rooms>,
}

/// Starts a room task.
pub fn spawn(
    id: String,
    max_participants: usize,
    from_api: bool,
    services: RoomServices,
) -> RoomHandle {
    let (commands_tx, commands_rx) = mpsc::channel(256);
    let (packets_tx, packets_rx) = mpsc::channel(8192);
    let handle = RoomHandle {
        id: id.clone(),
        commands: commands_tx,
    };
    services
        .metrics
        .rooms_active
        .fetch_add(1, Ordering::Relaxed);
    let room = Room {
        id,
        max_participants,
        services,
        packets_tx,
        participants: Vec::new(),
        empty_since: Some(Instant::now()),
        empty_ttl_override: from_api.then_some(UNUSED_ROOM_TTL),
        speakers: Vec::new(),
        last_speaker_check: Instant::now(),
        ended: false,
    };
    tokio::spawn(room.run(commands_rx, packets_rx, handle.clone()));
    handle
}

pub(crate) struct Room {
    id: String,
    max_participants: usize,
    services: RoomServices,
    packets_tx: RoomPackets,
    participants: Vec<Participant>,
    empty_since: Option<Instant>,
    empty_ttl_override: Option<Duration>,
    speakers: Vec<String>,
    last_speaker_check: Instant,
    ended: bool,
}

impl Room {
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<RoomCommand>,
        mut packets: mpsc::Receiver<Datagram>,
        handle: RoomHandle,
    ) {
        tracing::info!(room = %self.id, "room started");
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        while !self.ended {
            let deadline = self.next_timeout();
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => self.handle_command(command),
                    None => break,
                },
                Some(datagram) = packets.recv() => {
                    self.handle_datagram(datagram);
                    // Drain what is already queued before polling peers, so a
                    // burst of packets is handled in one pass.
                    while let Ok(datagram) = packets.try_recv() {
                        self.handle_datagram(datagram);
                    }
                }
                _ = tokio::time::sleep_until(deadline.into()) => {}
                _ = tick.tick() => self.housekeeping(),
            }
            self.drive();
        }

        // Deregister first so new joins create a fresh room instead of
        // queueing behind a room that is about to disappear.
        if let Some(rooms) = self.services.rooms.upgrade() {
            rooms.remove(&handle);
        }
        commands.close();
        while let Ok(command) = commands.try_recv() {
            if let RoomCommand::Join {
                conn,
                request_id,
                reply,
                ..
            } = command
            {
                let code = if self.ended_by_host() {
                    ErrorCode::RoomEnded
                } else {
                    ErrorCode::Unavailable
                };
                conn.send(response(
                    &request_id,
                    Err(ProtocolError::new(code, "room closed")),
                ));
                let _ = reply.send(JoinOutcome::Rejected(code));
            }
        }
        for index in (0..self.participants.len()).rev() {
            self.remove_participant(index, None);
        }
        self.services
            .metrics
            .rooms_active
            .fetch_sub(1, Ordering::Relaxed);
        tracing::info!(room = %self.id, "room stopped");
    }

    fn ended_by_host(&self) -> bool {
        self.services
            .rooms
            .upgrade()
            .is_some_and(|rooms| rooms.is_ended(&self.id))
    }

    fn handle_command(&mut self, command: RoomCommand) {
        match command {
            RoomCommand::Join {
                claims,
                request_id,
                resume,
                conn,
                reply,
            } => {
                let result = self.join(&claims, resume.as_deref(), &conn);
                let outcome = match &result {
                    Ok(_) => JoinOutcome::Joined,
                    Err(e) => JoinOutcome::Rejected(e.code),
                };
                match result {
                    Ok((index, data)) => {
                        conn.send(response(&request_id, Ok(data)));
                        // Events (subscriber offer) only after the response.
                        self.after_join(index);
                    }
                    Err(error) => {
                        self.services.metrics.join_failed(error.code);
                        conn.send(response(&request_id, Err(error)));
                    }
                }
                let _ = reply.send(outcome);
            }
            RoomCommand::Request {
                conn,
                request_id,
                request,
            } => {
                self.request(conn, request_id, request);
            }
            RoomCommand::Disconnected { conn_id } => self.disconnected(conn_id),
            RoomCommand::End { reason } => self.end(reason),
            RoomCommand::Summary { reply } => {
                let _ = reply.send(json!({
                    "room_id": self.id,
                    "max_participants": self.max_participants,
                    "participants": self.participants.iter().map(|p| json!({
                        "participant_id": p.id,
                        "identity": p.identity,
                        "connected": p.conn.is_some(),
                        "tracks": p.tracks.len(),
                    })).collect::<Vec<_>>(),
                }));
            }
        }
    }

    /// Attaches `conn` to a session. Rules (see docs/PROTOCOL.md, "Sessions"):
    ///
    /// * One seat per identity. A second connection of the same identity
    ///   never creates a second participant.
    /// * With the right `resume` session id the new socket takes over the
    ///   existing session; media keeps flowing.
    /// * Without it, the new connection *replaces* the old session: the old
    ///   socket gets `session.replaced` and its media is torn down.
    fn join(
        &mut self,
        claims: &Claims,
        resume: Option<&str>,
        conn: &Connection,
    ) -> Result<(usize, Value), ProtocolError> {
        if self.ended {
            return Err(ProtocolError::new(
                ErrorCode::RoomEnded,
                "the meeting has ended",
            ));
        }
        // Removed participants keep valid tokens until `exp`; the registry
        // remembers the removal, even across room re-creation.
        if self
            .services
            .rooms
            .upgrade()
            .is_some_and(|rooms| rooms.is_removed(&self.id, &claims.sub))
        {
            return Err(ProtocolError::new(
                ErrorCode::Forbidden,
                "removed from this meeting by the host",
            ));
        }
        let existing = self
            .participants
            .iter()
            .position(|p| p.identity == claims.sub);
        let (index, resumed) = match existing {
            Some(index) if resume == Some(self.participants[index].session_id.as_str()) => {
                let participant = &mut self.participants[index];
                if let Some(old) = participant.conn.replace(conn.clone()) {
                    old.send(Outbound::event(
                        "session.replaced",
                        json!({"reason": "resumed_elsewhere"}),
                    ));
                    old.send(Outbound::Close {
                        code: close_code::SESSION_REPLACED,
                        reason: "session resumed",
                    });
                }
                participant.disconnected_at = None;
                participant.name = claims.name.clone().unwrap_or_else(|| claims.sub.clone());
                participant.role = claims.role;
                participant.can_publish = claims.can_publish;
                self.services
                    .metrics
                    .resumes_total
                    .fetch_add(1, Ordering::Relaxed);
                (index, true)
            }
            Some(index) => {
                self.replace_session(index, claims, conn);
                self.services
                    .metrics
                    .joins_total
                    .fetch_add(1, Ordering::Relaxed);
                (index, false)
            }
            None => {
                if self.participants.len() >= self.max_participants {
                    return Err(ProtocolError::new(
                        ErrorCode::RoomFull,
                        format!("room allows {} participants", self.max_participants),
                    ));
                }
                let participant =
                    Participant::new(random_id("PA_"), claims, random_id("SE_"), conn.clone());
                self.participants.push(participant);
                self.empty_since = None;
                self.empty_ttl_override = None;
                self.services
                    .metrics
                    .joins_total
                    .fetch_add(1, Ordering::Relaxed);
                self.services
                    .metrics
                    .participants_active
                    .fetch_add(1, Ordering::Relaxed);
                let index = self.participants.len() - 1;
                let info = self.participants[index].info();
                self.broadcast_except(
                    index,
                    Outbound::event("participant.joined", json!({"participant": info})),
                );
                (index, false)
            }
        };
        if resumed {
            let info = self.participants[index].info();
            self.broadcast_except(
                index,
                Outbound::event("participant.updated", json!({"participant": info})),
            );
        }

        let participant = &self.participants[index];
        let config = &self.services.config;
        let data = json!({
            "protocol": crate::protocol::PROTOCOL_VERSION,
            "session_id": participant.session_id,
            "resumed": resumed,
            "participant": participant.info(),
            "participants": self.participants.iter()
                .filter(|p| p.id != participant.id)
                .map(Participant::info)
                .collect::<Vec<_>>(),
            "room": {
                "room_id": self.id,
                "max_participants": self.max_participants,
                "max_video_subscriptions": config.rooms.max_video_subscriptions,
            },
            "speakers": self.speakers,
            "ice_servers": config.media.ice_servers,
            "reconnect_grace_ms": config.rooms.reconnect_grace().as_millis() as u64,
            "heartbeat_interval_ms": crate::signaling::HEARTBEAT_INTERVAL.as_millis() as u64,
        });
        Ok((index, data))
    }

    /// Tears down the old session of an identity that joined again without
    /// a valid resume id. The participant id is kept so other clients never
    /// see the same person twice.
    fn replace_session(&mut self, index: usize, claims: &Claims, conn: &Connection) {
        self.close_media(index);
        let participant = &mut self.participants[index];
        if let Some(old) = participant.conn.replace(conn.clone()) {
            old.send(Outbound::event(
                "session.replaced",
                json!({"reason": "joined_elsewhere"}),
            ));
            old.send(Outbound::Close {
                code: close_code::SESSION_REPLACED,
                reason: "session replaced",
            });
        }
        let id = participant.id.clone();
        *participant = Participant::new(id, claims, random_id("SE_"), conn.clone());
        let info = participant.info();
        self.broadcast_except(
            index,
            Outbound::event("participant.updated", json!({"participant": info})),
        );
    }

    fn after_join(&mut self, index: usize) {
        let participant = &self.participants[index];
        if participant.subscriber.is_none() {
            self.create_subscriber(index);
        } else if let Some(pending) = participant
            .subscriber
            .as_ref()
            .and_then(|s| s.pending.as_ref())
        {
            // The previous socket may have died before the offer arrived.
            participant.send(Outbound::event("subscriber.offer", pending.event.clone()));
        }
        // A replaced session unpublished its tracks; update the others.
        self.negotiate_all();
    }

    fn request(&mut self, conn: Connection, request_id: String, request: SessionRequest) {
        let Some(index) = self
            .participants
            .iter()
            .position(|p| p.conn.as_ref().is_some_and(|c| c.id == conn.id))
        else {
            conn.send(response(
                &request_id,
                Err(ProtocolError::new(
                    ErrorCode::NotJoined,
                    "socket is not attached to a session",
                )),
            ));
            return;
        };

        if request.needs_dedup() {
            if let Some(cached) = self.participants[index].responses.get(&request_id) {
                conn.send(response(&request_id, cached.clone()));
                return;
            }
        }

        let dedup = request.needs_dedup();
        let result = self.execute(index, request);
        let (result, after) = match result {
            Ok((data, after)) => (Ok(data), after),
            Err(e) => (Err(e), After::Nothing),
        };
        if dedup {
            self.participants[index]
                .responses
                .put(request_id.clone(), result.clone());
        }
        conn.send(response(&request_id, result));

        match after {
            After::Nothing => {}
            After::Negotiate => self.negotiate_all(),
            After::Leave => {
                if let Some(conn) = &self.participants[index].conn {
                    conn.send(Outbound::Close {
                        code: close_code::NORMAL,
                        reason: "left",
                    });
                }
                self.remove_participant(index, Some("left"));
            }
            After::EndRoom => self.end("ended_by_host"),
            After::Remove(target) => {
                if let Some(target) = self.participants.iter().position(|p| p.id == target) {
                    if let Some(rooms) = self.services.rooms.upgrade() {
                        rooms.mark_removed(&self.id, &self.participants[target].identity);
                    }
                    let participant = &self.participants[target];
                    participant.send(Outbound::event(
                        "removed",
                        json!({"reason": "removed_by_host"}),
                    ));
                    participant.send(Outbound::Close {
                        code: close_code::REMOVED,
                        reason: "removed",
                    });
                    self.remove_participant(target, Some("removed"));
                }
            }
        }
    }

    fn execute(
        &mut self,
        index: usize,
        request: SessionRequest,
    ) -> Result<(Value, After), ProtocolError> {
        match request {
            // Normally answered by the signaling task; kept for completeness.
            SessionRequest::Ping {} => Ok((json!({}), After::Nothing)),
            SessionRequest::Leave {} => Ok((json!({}), After::Leave)),
            SessionRequest::RoomEnd {} => {
                self.require_host(index)?;
                Ok((json!({}), After::EndRoom))
            }
            SessionRequest::ParticipantRemove { participant_id } => {
                self.require_host(index)?;
                if participant_id == self.participants[index].id {
                    return Err(ProtocolError::new(
                        ErrorCode::BadRequest,
                        "use leave to remove yourself",
                    ));
                }
                if !self.participants.iter().any(|p| p.id == participant_id) {
                    return Err(ProtocolError::new(
                        ErrorCode::BadRequest,
                        "unknown participant",
                    ));
                }
                Ok((json!({}), After::Remove(participant_id)))
            }
            SessionRequest::PublisherOffer { sdp, tracks, reset } => {
                let answer = self.publisher_offer(index, &sdp, &tracks, reset)?;
                Ok((json!({"sdp": answer}), After::Negotiate))
            }
            SessionRequest::SubscriberAnswer { sdp, revision } => {
                self.subscriber_answer(index, &sdp, revision)?;
                Ok((json!({}), After::Negotiate))
            }
            SessionRequest::IceCandidate { target, candidate } => {
                let added = self.add_candidate(index, target, &candidate)?;
                Ok((json!({"added": added}), After::Nothing))
            }
            SessionRequest::IceRestart { target } => {
                self.ice_restart(index, target)?;
                Ok((json!({}), After::Negotiate))
            }
            SessionRequest::SubscriberReset {} => {
                self.reset_subscriber(index);
                Ok((json!({}), After::Negotiate))
            }
            SessionRequest::TrackMute { source, muted } => {
                self.mute(index, source, muted)?;
                Ok((json!({}), After::Nothing))
            }
            SessionRequest::SubscriptionUpdate { changes } => {
                self.update_subscriptions(index, &changes)?;
                Ok((json!({}), After::Negotiate))
            }
        }
    }

    fn require_host(&self, index: usize) -> Result<(), ProtocolError> {
        if self.participants[index].role == Role::Host {
            Ok(())
        } else {
            Err(ProtocolError::new(
                ErrorCode::Forbidden,
                "only the host can do this",
            ))
        }
    }

    fn disconnected(&mut self, conn_id: u64) {
        let Some(index) = self
            .participants
            .iter()
            .position(|p| p.conn.as_ref().is_some_and(|c| c.id == conn_id))
        else {
            // Already replaced, resumed elsewhere or removed.
            return;
        };
        let participant = &mut self.participants[index];
        participant.conn = None;
        participant.disconnected_at = Some(Instant::now());
        tracing::info!(room = %self.id, participant = %participant.id, "signaling lost, waiting for resume");
        let info = participant.info();
        self.broadcast_except(
            index,
            Outbound::event("participant.updated", json!({"participant": info})),
        );
    }

    fn end(&mut self, reason: &'static str) {
        if let Some(rooms) = self.services.rooms.upgrade() {
            rooms.mark_ended(&self.id);
        }
        for participant in &self.participants {
            participant.send(Outbound::event("room.ended", json!({"reason": reason})));
            participant.send(Outbound::Close {
                code: close_code::ROOM_ENDED,
                reason: "room ended",
            });
        }
        tracing::info!(room = %self.id, reason, "room ended");
        self.ended = true;
    }

    /// Removes a participant, its peers and every forwarding entry that
    /// points at it. `reason` is sent to the others; `None` skips the event
    /// (room shutdown).
    fn remove_participant(&mut self, index: usize, reason: Option<&str>) {
        self.close_media(index);
        let participant = self.participants.remove(index);
        self.services
            .metrics
            .participants_active
            .fetch_sub(1, Ordering::Relaxed);
        tracing::info!(room = %self.id, participant = %participant.id, reason = reason.unwrap_or("shutdown"), "participant left");
        if let Some(reason) = reason {
            let event = Outbound::event(
                "participant.left",
                json!({"participant_id": participant.id, "reason": reason}),
            );
            for other in &self.participants {
                other.send(event.clone());
            }
        }
        if self.participants.is_empty() {
            self.empty_since = Some(Instant::now());
        }
        self.negotiate_all();
    }

    fn housekeeping(&mut self) {
        let now = Instant::now();
        let grace = self.services.config.rooms.reconnect_grace();
        while let Some(index) = self
            .participants
            .iter()
            .position(|p| p.disconnected_at.is_some_and(|at| now - at >= grace))
        {
            self.services
                .metrics
                .grace_expired_total
                .fetch_add(1, Ordering::Relaxed);
            self.remove_participant(index, Some("timeout"));
        }

        let ttl = self
            .empty_ttl_override
            .unwrap_or_else(|| self.services.config.rooms.empty_room_ttl());
        if self.empty_since.is_some_and(|since| now - since >= ttl) {
            tracing::info!(room = %self.id, "closing empty room");
            self.ended = true;
            return;
        }

        self.media_housekeeping(now);
    }

    fn broadcast_except(&self, index: usize, message: Outbound) {
        for (i, participant) in self.participants.iter().enumerate() {
            if i != index {
                participant.send(message.clone());
            }
        }
    }
}

/// Follow-up work after a response was sent.
enum After {
    Nothing,
    Negotiate,
    Leave,
    EndRoom,
    Remove(String),
}

fn response(id: &str, result: Result<Value, ProtocolError>) -> Outbound {
    Outbound::Response {
        id: id.to_owned(),
        result,
    }
}

/// Random id with a readable prefix, for example `PA_3kq9...`.
pub(crate) fn random_id(prefix: &str) -> String {
    let suffix: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(16)
        .map(char::from)
        .collect();
    format!("{prefix}{suffix}")
}
