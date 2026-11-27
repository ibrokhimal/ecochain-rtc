//! Nonblocking recording controls and encoded-media tap ownership.
use super::Room;
use crate::{
    protocol::{ErrorCode, Outbound, ProtocolError},
    recording::RecordingHandle,
};
use serde_json::{json, Value};
impl Room {
    pub(super) fn recording_action(
        &mut self,
        index: usize,
        action: &Value,
    ) -> Option<Result<Value, ProtocolError>> {
        match action["type"].as_str()? {
            "recording.status" => Some(Ok(self.recording_state.clone())),
            "recording.start" => Some((|| {
                self.require_host(index)?;
                if self.recording.is_some() {
                    return Err(ProtocolError::new(
                        ErrorCode::InvalidState,
                        "recording is active or finalizing",
                    ));
                }
                let config = self.services.config.recording.clone().ok_or_else(|| {
                    ProtocolError::new(ErrorCode::Unavailable, "recording worker not configured")
                })?;
                let vault = self.services.vault.clone().ok_or_else(|| {
                    ProtocolError::new(ErrorCode::Unavailable, "archive storage not configured")
                })?;
                let instance = self.collaboration.state["instance_id"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let handle = RecordingHandle::start(
                    config,
                    vault,
                    instance,
                    self.collaboration.state.clone(),
                    self.commands.clone(),
                )
                .map_err(|_| {
                    ProtocolError::new(ErrorCode::Unavailable, "recording worker could not start")
                })?;
                self.recording_state = json!({"id":handle.id,"kind":"recording","status":"starting","started_at":crate::auth::unix_now()});
                self.recording_layers.clear();
                self.recording = Some(handle);
                self.broadcast(Outbound::event(
                    "recording.changed",
                    self.recording_state.clone(),
                ));
                // A recording must start on independently decodable video frames.
                let videos: Vec<_> = self
                    .participants
                    .iter()
                    .flat_map(|p| {
                        p.tracks
                            .iter()
                            .filter(|t| t.source.is_video() && !t.muted)
                            .flat_map(|t| {
                                let rids: Vec<_> = if t.layers.is_empty() {
                                    vec![None]
                                } else {
                                    t.layers.iter().copied().map(Some).collect()
                                };
                                rids.into_iter()
                                    .map(|rid| (p.id.clone(), t.id.clone(), rid))
                                    .collect::<Vec<_>>()
                            })
                    })
                    .collect();
                for (publisher, track, rid) in videos {
                    self.request_keyframe(
                        &publisher,
                        &track,
                        rid,
                        str0m::media::KeyframeRequestKind::Pli,
                        true,
                    );
                }
                Ok(self.recording_state.clone())
            })()),
            "recording.stop" => Some((|| {
                self.require_host(index)?;
                if let Some(handle) = &self.recording {
                    handle.stop();
                    self.recording_state["status"] = json!("processing");
                    self.broadcast(Outbound::event(
                        "recording.changed",
                        self.recording_state.clone(),
                    ));
                }
                Ok(self.recording_state.clone())
            })()),
            _ => None,
        }
    }
}
