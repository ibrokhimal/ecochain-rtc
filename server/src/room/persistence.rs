//! Deferred durable commits. The media actor never waits for fsync.
use super::{collaboration::Collaboration, participant::Connection, response, Room, RoomCommand};
use crate::{
    auth::Role,
    protocol::{ErrorCode, Outbound, ProtocolError, SessionRequest},
    storage::Archive,
};
use serde_json::json;
use std::collections::{BTreeSet, VecDeque};

#[derive(Default)]
pub(super) struct Persistence {
    pending: Option<Pending>,
    queued: VecDeque<(Connection, String, SessionRequest)>,
}
struct Pending {
    candidate: Collaboration,
    base_revision: u64,
    members: BTreeSet<String>,
    request: Option<(Connection, String, u64, String, String)>,
    request_error: Option<ProtocolError>,
    external_reply: Option<tokio::sync::oneshot::Sender<Result<serde_json::Value, ProtocolError>>>,
}
impl Room {
    pub(super) fn persist_workspace(
        &mut self,
        conn: Connection,
        id: String,
        request: SessionRequest,
        fingerprint: u64,
    ) {
        if self.persistence.pending.is_some() {
            if self.persistence.queued.len() >= 128 {
                conn.send(response(&id, Err(unavailable())));
            } else {
                self.persistence.queued.push_back((conn, id, request));
            }
            return;
        }
        let Some(index) = self
            .participants
            .iter()
            .position(|p| p.conn.as_ref().is_some_and(|c| c.id == conn.id))
        else {
            conn.send(response(&id, Err(unavailable())));
            return;
        };
        let SessionRequest::MeetingAction { action } = request else {
            return;
        };
        let result = self.validate_meeting_action(index, &action).and_then(|()| {
            let p = &self.participants[index];
            self.collaboration
                .prepare(&p.id, &p.identity, p.role == Role::Host, action)
        });
        match result {
            Err(error) => {
                self.participants[index]
                    .responses
                    .put(id.clone(), fingerprint, Err(error.clone()));
                conn.send(response(&id, Err(error)));
            }
            Ok(candidate) => {
                let p = &self.participants[index];
                let pending = Pending {
                    candidate,
                    base_revision: self.workspace_revision(),
                    members: self.members.clone(),
                    request: Some((conn, id, fingerprint, p.id.clone(), p.session_id.clone())),
                    request_error: None,
                    external_reply: None,
                };
                self.start_persist(pending);
            }
        }
    }
    pub(super) fn persist_members(&mut self) {
        if self.services.vault.is_none() || self.persistence.pending.is_some() {
            return;
        }
        self.start_persist(Pending {
            candidate: self.collaboration.clone(),
            base_revision: self.workspace_revision(),
            members: self.members.clone(),
            request: None,
            request_error: None,
            external_reply: None,
        });
    }
    pub(super) fn persist_summary(
        &mut self,
        identity: String,
        confirmation: crate::providers::SummaryConfirmation,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, ProtocolError>>,
    ) {
        if self.persistence.pending.is_some() {
            let _ = reply.send(Err(unavailable()));
            return;
        }
        if !self.members.contains(&identity) {
            let _ = reply.send(Err(ProtocolError::new(
                ErrorCode::Forbidden,
                "not an admitted manager",
            )));
            return;
        }
        let author = self
            .participants
            .iter()
            .find(|p| p.identity == identity)
            .map_or_else(|| format!("archive:{identity}"), |p| p.id.clone());
        let state = match crate::providers::confirmed_snapshot(
            &self.collaboration.state,
            &self.members,
            &author,
            &confirmation,
        ) {
            Ok(state) => state,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let mut candidate = self.collaboration.clone();
        candidate.state = state;
        self.start_persist(Pending {
            candidate,
            base_revision: self.workspace_revision(),
            members: self.members.clone(),
            request: None,
            request_error: None,
            external_reply: Some(reply),
        });
    }
    fn start_persist(&mut self, pending: Pending) {
        let Some(vault) = self.services.vault.clone() else {
            return;
        };
        let archive = Archive {
            state: pending.candidate.state.clone(),
            members: pending.members.clone(),
            updated_at: crate::auth::unix_now(),
        };
        let commands = self.commands.clone();
        self.persistence.pending = Some(pending);
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || vault.append(&archive)).await;
            let success = matches!(result, Ok(Ok(())));
            if !success {
                tracing::warn!("meeting persistence failed");
            }
            let _ = commands
                .send(RoomCommand::WorkspacePersisted { success })
                .await;
        });
    }
    pub(super) fn workspace_persisted(&mut self, success: bool) {
        let Some(mut pending) = self.persistence.pending.take() else {
            return;
        };
        if success && self.workspace_revision() != pending.base_revision {
            // A leave/removal changed the workspace while disk I/O was running.
            // Persist that canonical state before releasing the queued actions.
            pending.candidate = self.collaboration.clone();
            pending.base_revision = self.workspace_revision();
            pending.members = self.members.clone();
            pending.request_error = Some(ProtocolError::new(
                ErrorCode::InvalidState,
                "meeting changed during save; refresh",
            ));
            self.start_persist(pending);
            return;
        }
        let mut change = None;
        if success && pending.request_error.is_none() {
            change = Some(super::collaboration::delta(
                &self.collaboration.state,
                &pending.candidate.state,
            ));
            let workspace_changed = self.collaboration.state != pending.candidate.state;
            self.collaboration.state = pending.candidate.state;
            for receiver in 0..self.participants.len() {
                self.allocate_layers(receiver);
            }
            if workspace_changed && (pending.request.is_some() || pending.external_reply.is_some())
            {
                self.broadcast(Outbound::event("meeting.changed", change.clone().unwrap()));
            }
        }
        if let Some(reply) = pending.external_reply.take() {
            let result = if !success {
                Err(unavailable())
            } else if let Some(error) = pending.request_error.clone() {
                Err(error)
            } else {
                Ok(change.clone().unwrap())
            };
            let _ = reply.send(result);
        }
        if let Some((conn, id, fingerprint, participant_id, session_id)) = pending.request {
            let result = if !success {
                Err(unavailable())
            } else if let Some(error) = pending.request_error {
                Err(error)
            } else {
                Ok(change.unwrap())
            };
            if let Some(p) = self
                .participants
                .iter_mut()
                .find(|p| p.id == participant_id && p.session_id == session_id)
            {
                p.responses.put(id.clone(), fingerprint, result.clone());
            }
            conn.send(response(&id, result));
        } else if !success {
            self.broadcast(Outbound::event(
                "meeting.storage_error",
                json!({"message":"workspace storage unavailable"}),
            ));
        }
        // Membership joins are independent of workspace revision.
        if success && pending.members != self.members {
            self.persist_members();
        }
        while self.persistence.pending.is_none() {
            let Some((conn, id, request)) = self.persistence.queued.pop_front() else {
                break;
            };
            self.request(conn, id, request);
        }
    }
    // Media is already stopped; finish disk acknowledgements before dropping the actor.
    pub(super) async fn finish_persistence(
        &mut self,
        commands: &mut tokio::sync::mpsc::Receiver<RoomCommand>,
    ) {
        for (conn, id, _) in self.persistence.queued.drain(..) {
            conn.send(response(&id, Err(unavailable())));
        }
        self.persist_members();
        while self.persistence.pending.is_some() {
            match commands.recv().await {
                Some(RoomCommand::WorkspacePersisted { success }) => {
                    self.workspace_persisted(success)
                }
                Some(RoomCommand::Join {
                    conn,
                    request_id,
                    reply,
                    ..
                }) => {
                    conn.send(response(&request_id, Err(unavailable())));
                    let _ = reply.send(super::JoinOutcome::Rejected(ErrorCode::Unavailable));
                }
                Some(RoomCommand::ConfirmSummary { reply, .. }) => {
                    let _ = reply.send(Err(unavailable()));
                }
                Some(_) => {}
                None => break,
            }
        }
    }
    fn workspace_revision(&self) -> u64 {
        self.collaboration.state["revision"].as_u64().unwrap_or(0)
    }
}
fn unavailable() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::Unavailable,
        "workspace could not be durably saved; retry",
    )
}
