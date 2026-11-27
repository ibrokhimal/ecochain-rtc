//! Host-controlled live speech capture is independent of recording capture.
use super::Room;
use crate::{
    protocol::{ErrorCode, Outbound, ProtocolError},
    providers::captions::CaptionHandle,
};
use serde_json::{json, Value};
impl Room {
    pub(super) fn captions_action(
        &mut self,
        index: usize,
        action: &Value,
    ) -> Option<Result<Value, ProtocolError>> {
        match action["type"].as_str()? {
            "captions.status" => Some(Ok(self.captions_state.clone())),
            "captions.start" => Some((|| {
                self.require_host(index)?;
                if self.captions.is_some() {
                    return Err(ProtocolError::new(
                        ErrorCode::InvalidState,
                        "captions are active",
                    ));
                }
                let stt = self.services.config.providers.stt.clone().ok_or_else(|| {
                    ProtocolError::new(ErrorCode::Unavailable, "STT provider not configured")
                })?;
                let config = self.services.config.recording.clone().ok_or_else(|| {
                    ProtocolError::new(ErrorCode::Unavailable, "audio worker not configured")
                })?;
                let vault = self.services.vault.clone().ok_or_else(|| {
                    ProtocolError::new(ErrorCode::Unavailable, "archive not configured")
                })?;
                let handle = CaptionHandle::start(
                    config,
                    stt,
                    vault,
                    self.collaboration.state["instance_id"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    self.commands.clone(),
                    self.services.provider_slots.clone(),
                )
                .map_err(|_| {
                    ProtocolError::new(ErrorCode::Unavailable, "caption worker could not start")
                })?;
                self.captions_state = json!({"id":handle.id,"status":"starting"});
                self.captions = Some(handle);
                self.broadcast(Outbound::event(
                    "captions.changed",
                    self.captions_state.clone(),
                ));
                Ok(self.captions_state.clone())
            })()),
            "captions.stop" => Some((|| {
                self.require_host(index)?;
                if let Some(handle) = &self.captions {
                    handle.stop();
                    self.captions_state["status"] = json!("stopping");
                    self.broadcast(Outbound::event(
                        "captions.changed",
                        self.captions_state.clone(),
                    ));
                }
                Ok(self.captions_state.clone())
            })()),
            _ => None,
        }
    }
}
