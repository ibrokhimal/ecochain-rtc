//! Signaling protocol types (version 1).
//!
//! Every WebSocket text frame is one JSON object. The full description lives
//! in `docs/PROTOCOL.md`; this module is the machine-checked version of it.
//!
//! * Client requests: `{"v":1,"type":"<request>","id":"<request id>","data":{...}}`
//! * Responses: `{"v":1,"type":"response","id":"<request id>","ok":true,"data":{...}}`
//!   or `{"v":1,"type":"response","id":"...","ok":false,"error":{"code":"room_full",...}}`
//! * Server events: `{"v":1,"type":"<event>","data":{...}}` (no `id`).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The only protocol version this server speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// Longest accepted request id.
pub const MAX_REQUEST_ID_LEN: usize = 64;

/// Largest accepted text frame. SDP with many m-lines is the biggest payload.
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// Machine readable error codes. Clients branch on these, never on `message`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The message could not be parsed or has invalid fields.
    BadRequest,
    /// `v` is not [`PROTOCOL_VERSION`].
    UnsupportedVersion,
    /// The token is missing, malformed or has a bad signature.
    Unauthorized,
    /// The token expired. Fetch a new token and join again.
    TokenExpired,
    /// The token is valid but does not allow this action.
    Forbidden,
    /// The room does not exist and auto creation is disabled.
    RoomNotFound,
    /// The room reached `max_participants`.
    RoomFull,
    /// The host ended the meeting.
    RoomEnded,
    /// The request needs a joined session.
    NotJoined,
    /// The request is not valid in the current state, for example an answer
    /// for an outdated offer.
    InvalidState,
    /// Too many video subscriptions or similar limit.
    LimitExceeded,
    /// The server is overloaded or shutting down. Retry later.
    Unavailable,
    /// Unexpected server error.
    Internal,
}

impl ErrorCode {
    /// Whether retrying the same request later can succeed.
    pub fn retryable(self) -> bool {
        matches!(self, ErrorCode::Unavailable | ErrorCode::Internal)
    }

    /// Stable string form used in metrics labels.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::UnsupportedVersion => "unsupported_version",
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::TokenExpired => "token_expired",
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::RoomNotFound => "room_not_found",
            ErrorCode::RoomFull => "room_full",
            ErrorCode::RoomEnded => "room_ended",
            ErrorCode::NotJoined => "not_joined",
            ErrorCode::InvalidState => "invalid_state",
            ErrorCode::LimitExceeded => "limit_exceeded",
            ErrorCode::Unavailable => "unavailable",
            ErrorCode::Internal => "internal",
        }
    }
}

/// Error payload of a failed response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    /// Machine readable code.
    pub code: ErrorCode,
    /// Human readable English text for logs. Never contains secrets.
    pub message: String,
    /// Copy of [`ErrorCode::retryable`] so clients need no lookup table.
    pub retryable: bool,
}

impl ProtocolError {
    /// Creates an error with a message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: code.retryable(),
        }
    }
}

/// WebSocket close codes used by the server. Clients use them to decide
/// whether to reconnect (see `docs/PROTOCOL.md`).
pub mod close_code {
    /// Normal close after `leave`.
    pub const NORMAL: u16 = 1000;
    /// Malformed traffic or unsupported version. Do not reconnect blindly.
    pub const PROTOCOL_ERROR: u16 = 4000;
    /// A newer connection of the same identity took over this session.
    pub const SESSION_REPLACED: u16 = 4001;
    /// Authentication or authorization failed.
    pub const UNAUTHORIZED: u16 = 4003;
    /// The meeting was ended by the host.
    pub const ROOM_ENDED: u16 = 4004;
    /// The participant was removed by the host.
    pub const REMOVED: u16 = 4005;
    /// No `join` arrived in time or the heartbeat stopped.
    pub const TIMEOUT: u16 = 4008;
    /// The server is shutting down. Reconnect with backoff.
    pub const GOING_AWAY: u16 = 1001;
}

/// Envelope fields read before the body so that version and id errors can
/// be reported even when the body is invalid.
#[derive(Debug, Deserialize)]
pub struct Header {
    /// Protocol version.
    pub v: u32,
    /// Message type.
    #[serde(rename = "type")]
    pub kind: String,
    /// Request id, echoed in the response.
    pub id: Option<String>,
}

/// Source of a published track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackSource {
    /// Microphone audio.
    Microphone,
    /// Camera video.
    Camera,
    /// Screen share video.
    Screen,
    /// Audio captured together with a screen share.
    ScreenAudio,
}

impl TrackSource {
    /// Whether the source carries video.
    pub fn is_video(self) -> bool {
        matches!(self, TrackSource::Camera | TrackSource::Screen)
    }
}

/// Requested receive quality for a video subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    /// Thumbnail size.
    Low,
    /// Gallery tile.
    #[default]
    Medium,
    /// Spotlight, pinned or screen share.
    High,
}

/// Which peer connection a message refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerTarget {
    /// Client → server media (client offers).
    Publisher,
    /// Server → client media (server offers).
    Subscriber,
}

/// Track description sent with a publisher offer.
#[derive(Debug, Clone, Deserialize)]
pub struct OfferedTrack {
    /// Transceiver mid from the client's local description.
    pub mid: String,
    /// What the track carries.
    pub source: TrackSource,
    /// Whether the track starts muted.
    #[serde(default)]
    pub muted: bool,
    /// Simulcast rids from lowest to highest quality, empty if none.
    #[serde(default)]
    pub layers: Vec<String>,
}

/// One entry of `subscription.update`.
#[derive(Debug, Clone, Deserialize)]
pub struct SubscriptionChange {
    /// Track to change.
    pub track_id: String,
    /// `true` to receive the track, `false` to stop.
    pub subscribed: bool,
    /// Desired quality; ignored for audio.
    #[serde(default)]
    pub quality: Quality,
}

/// Optional client description, used only for logs and metrics.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClientInfo {
    /// SDK name and version, for example `ecochain_rtc/0.1.0`.
    #[serde(default)]
    pub sdk: String,
    /// `android`, `ios`, `macos`, `windows`, ...
    #[serde(default)]
    pub platform: String,
}

/// Data of the `join` request.
#[derive(Debug, Clone, Deserialize)]
pub struct JoinRequest {
    /// Room token.
    pub token: String,
    /// Session id from a previous join response. When present and still
    /// known, the server re-attaches this socket to the existing session.
    #[serde(default)]
    pub resume_session_id: Option<String>,
    /// Optional room id the client expects. Must match the token.
    #[serde(default)]
    pub room: Option<String>,
    /// Optional client details.
    #[serde(default)]
    pub client: ClientInfo,
}

/// Requests a joined session may send.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum SessionRequest {
    /// Leave the room and release all resources.
    #[serde(rename = "leave")]
    Leave {},
    /// Keepalive. Answered with server time.
    #[serde(rename = "ping")]
    Ping {},
    /// New or updated publisher offer.
    #[serde(rename = "publisher.offer")]
    PublisherOffer {
        /// Offer SDP.
        sdp: String,
        /// Tracks in the offer. Tracks missing from the list are unpublished.
        #[serde(default)]
        tracks: Vec<OfferedTrack>,
        /// Discard the server side peer and start a new one (full media
        /// reconnect, for example after the client recreated its peer).
        #[serde(default)]
        reset: bool,
    },
    /// Answer to a `subscriber.offer`.
    #[serde(rename = "subscriber.answer")]
    SubscriberAnswer {
        /// Answer SDP.
        sdp: String,
        /// Revision of the offer this answers.
        revision: u32,
    },
    /// Trickled client ICE candidate.
    #[serde(rename = "ice.candidate")]
    IceCandidate {
        /// Peer the candidate belongs to.
        target: PeerTarget,
        /// Candidate line, with or without the `candidate:` prefix.
        candidate: String,
    },
    /// Ask the server for a subscriber offer with new ICE credentials.
    #[serde(rename = "ice.restart")]
    IceRestart {
        /// Only `subscriber` is valid; the client restarts its publisher by
        /// sending a new `publisher.offer`.
        target: PeerTarget,
    },
    /// Discard the server side subscriber peer and offer a fresh one.
    #[serde(rename = "subscriber.reset")]
    SubscriberReset {},
    /// Mute state of a published source.
    #[serde(rename = "track.mute")]
    TrackMute {
        /// Source to change.
        source: TrackSource,
        /// New state.
        muted: bool,
    },
    /// Subscribe to or unsubscribe from tracks.
    #[serde(rename = "subscription.update")]
    SubscriptionUpdate {
        /// Changes to apply.
        changes: Vec<SubscriptionChange>,
    },
    /// Host only: end the meeting for everybody.
    #[serde(rename = "room.end")]
    RoomEnd {},
    /// Host only: remove a participant.
    #[serde(rename = "participant.remove")]
    ParticipantRemove {
        /// Participant to remove.
        participant_id: String,
    },
}

impl SessionRequest {
    /// Whether the request changes state and therefore needs duplicate
    /// detection by request id. Pings are cheap and idempotent anyway.
    pub fn needs_dedup(&self) -> bool {
        !matches!(self, SessionRequest::Ping {})
    }
}

/// Published track as seen by other participants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrackInfo {
    /// Server assigned id, unique within the room.
    pub track_id: String,
    /// `audio` or `video`.
    pub kind: &'static str,
    /// Track source.
    pub source: TrackSource,
    /// Mute state.
    pub muted: bool,
    /// Whether the publisher sends simulcast layers.
    pub simulcast: bool,
}

/// Participant as seen by other participants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParticipantInfo {
    /// Server assigned id, stable for the identity while it stays in the room.
    pub participant_id: String,
    /// Identity from the token `sub` claim.
    pub identity: String,
    /// Display name.
    pub name: String,
    /// Role.
    pub role: crate::auth::Role,
    /// `false` while the participant's socket is down inside the grace period.
    pub connected: bool,
    /// Published tracks.
    pub tracks: Vec<TrackInfo>,
}

/// Mapping of a subscriber m-line to the track it carries.
#[derive(Debug, Clone, Serialize)]
pub struct SubscriberTrackMapping {
    /// mid in the subscriber offer.
    pub mid: String,
    /// Track id.
    pub track_id: String,
    /// Publisher of the track.
    pub participant_id: String,
    /// Track source.
    pub source: TrackSource,
}

/// Something the server sends to a client.
#[derive(Debug, Clone)]
pub enum Outbound {
    /// Response to a request.
    Response {
        /// Request id.
        id: String,
        /// Result.
        result: Result<Value, ProtocolError>,
    },
    /// Server push event.
    Event {
        /// Event type, for example `participant.joined`.
        kind: &'static str,
        /// Event data.
        data: Value,
    },
    /// Close the socket with this code after flushing earlier messages.
    Close {
        /// WebSocket close code from [`close_code`].
        code: u16,
        /// Short reason.
        reason: &'static str,
    },
}

impl Outbound {
    /// Creates an event.
    pub fn event(kind: &'static str, data: Value) -> Self {
        Outbound::Event { kind, data }
    }

    /// Serializes the message to its JSON text. Returns `None` for `Close`.
    pub fn to_json(&self) -> Option<String> {
        let value = match self {
            Outbound::Response {
                id,
                result: Ok(data),
            } => {
                json!({"v": PROTOCOL_VERSION, "type": "response", "id": id, "ok": true, "data": data})
            }
            Outbound::Response {
                id,
                result: Err(error),
            } => {
                json!({"v": PROTOCOL_VERSION, "type": "response", "id": id, "ok": false, "error": error})
            }
            Outbound::Event { kind, data } => {
                json!({"v": PROTOCOL_VERSION, "type": kind, "data": data})
            }
            Outbound::Close { .. } => return None,
        };
        Some(value.to_string())
    }
}

/// Parsed client message.
#[derive(Debug)]
pub enum Inbound {
    /// The first message of every connection.
    Join(String, JoinRequest),
    /// Any other request.
    Session(String, SessionRequest),
}

/// Parses one text frame.
///
/// On failure returns the request id (when it could be read) and the error,
/// so the caller can still answer the right request.
pub fn parse(text: &str) -> Result<Inbound, (Option<String>, ProtocolError)> {
    if text.len() > MAX_MESSAGE_BYTES {
        return Err((
            None,
            ProtocolError::new(ErrorCode::BadRequest, "message too large"),
        ));
    }
    let value: Value = serde_json::from_str(text).map_err(|_| {
        (
            None,
            ProtocolError::new(ErrorCode::BadRequest, "invalid JSON"),
        )
    })?;
    let header: Header = serde_json::from_value(value.clone()).map_err(|_| {
        (
            None,
            ProtocolError::new(ErrorCode::BadRequest, "missing v, type or id"),
        )
    })?;
    if header.v != PROTOCOL_VERSION {
        return Err((
            header.id,
            ProtocolError::new(
                ErrorCode::UnsupportedVersion,
                format!("server speaks protocol version {PROTOCOL_VERSION}"),
            ),
        ));
    }
    let id = match header.id {
        Some(id) if !id.is_empty() && id.len() <= MAX_REQUEST_ID_LEN => id,
        _ => {
            return Err((
                None,
                ProtocolError::new(ErrorCode::BadRequest, "id must be 1..=64 characters"),
            ))
        }
    };
    let bad = |e: serde_json::Error| {
        (
            Some(id.clone()),
            ProtocolError::new(
                ErrorCode::BadRequest,
                format!("invalid {}: {e}", header.kind),
            ),
        )
    };
    if header.kind == "join" {
        let data = value.get("data").cloned().unwrap_or(Value::Null);
        let join: JoinRequest = serde_json::from_value(data).map_err(bad)?;
        return Ok(Inbound::Join(id, join));
    }
    let body =
        json!({"type": header.kind, "data": value.get("data").cloned().unwrap_or(json!({}))});
    let request: SessionRequest = serde_json::from_value(body).map_err(bad)?;
    Ok(Inbound::Session(id, request))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_join_and_session_requests() {
        let join = parse(r#"{"v":1,"type":"join","id":"1","data":{"token":"t"}}"#).unwrap();
        assert!(matches!(join, Inbound::Join(id, j) if id == "1" && j.token == "t"));

        let mute = parse(
            r#"{"v":1,"type":"track.mute","id":"2","data":{"source":"microphone","muted":true}}"#,
        )
        .unwrap();
        assert!(matches!(
            mute,
            Inbound::Session(
                _,
                SessionRequest::TrackMute {
                    source: TrackSource::Microphone,
                    muted: true
                }
            )
        ));

        let leave = parse(r#"{"v":1,"type":"leave","id":"3"}"#).unwrap();
        assert!(matches!(
            leave,
            Inbound::Session(_, SessionRequest::Leave {})
        ));
    }

    #[test]
    fn reports_version_and_id_errors() {
        let (id, error) = parse(r#"{"v":2,"type":"ping","id":"9"}"#).unwrap_err();
        assert_eq!(id.as_deref(), Some("9"));
        assert_eq!(error.code, ErrorCode::UnsupportedVersion);

        let (_, error) = parse(r#"{"v":1,"type":"ping"}"#).unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);

        let (id, error) = parse(r#"{"v":1,"type":"nope","id":"5"}"#).unwrap_err();
        assert_eq!(id.as_deref(), Some("5"));
        assert_eq!(error.code, ErrorCode::BadRequest);
    }

    #[test]
    fn serializes_responses() {
        let ok = Outbound::Response {
            id: "1".into(),
            result: Ok(json!({"a": 1})),
        };
        assert_eq!(
            ok.to_json().unwrap(),
            r#"{"data":{"a":1},"id":"1","ok":true,"type":"response","v":1}"#
        );
        let err = Outbound::Response {
            id: "2".into(),
            result: Err(ProtocolError::new(ErrorCode::RoomFull, "full")),
        };
        assert!(err.to_json().unwrap().contains(r#""code":"room_full""#));
    }
}
