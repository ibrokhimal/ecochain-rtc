//! Host media controls are enforced in the SFU as well as on cooperative clients.
use super::Room;
use crate::protocol::{ErrorCode, Outbound, ProtocolError, TrackSource};
use serde_json::{json, Value};
impl Room {
    pub(super) fn moderation_action(
        &mut self,
        index: usize,
        action: &Value,
    ) -> Option<Result<Value, ProtocolError>> {
        match action["type"].as_str()? {
            "participant.mute" => Some((|| {
                self.require_host(index)?;
                let target = action["participant_id"].as_str().ok_or_else(|| {
                    ProtocolError::new(ErrorCode::BadRequest, "participant id required")
                })?;
                let targets: Vec<_> = self
                    .participants
                    .iter()
                    .enumerate()
                    .filter(|(i, p)| *i != index && (target == "all" || p.id == target))
                    .map(|(i, _)| i)
                    .collect();
                if targets.is_empty() && target != "all" {
                    return Err(ProtocolError::new(
                        ErrorCode::BadRequest,
                        "unknown participant",
                    ));
                }
                for target in targets {
                    self.participants[target].host_muted = true;
                    if self.participants[target]
                        .tracks
                        .iter()
                        .any(|t| t.source == TrackSource::Microphone)
                    {
                        self.mute(target, TrackSource::Microphone, true)?;
                    }
                    self.participants[target].send(Outbound::event(
                        "participant.mute",
                        json!({"participant_id":self.participants[target].id}),
                    ));
                }
                Ok(json!({"muted":true}))
            })()),
            "reaction.send" => Some((|| {
                let p = &self.participants[index];
                let event = self.collaboration.reaction(&p.id, action)?;
                self.broadcast(Outbound::event("meeting.reaction", event));
                Ok(json!({}))
            })()),
            _ => None,
        }
    }
}
