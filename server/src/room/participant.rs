//! Per-participant state owned by the room task.

use crate::{
    auth::{Claims, Role},
    protocol::{
        Outbound, ParticipantInfo, ProtocolError, Quality, SubscriberTrackMapping, TrackInfo,
        TrackSource,
    },
    sfu::{layers::Timeline, peer::Peer},
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::Instant,
};
use str0m::{
    change::SdpPendingOffer,
    media::{MediaKind, Mid, Rid},
};
use tokio::sync::mpsc;

/// How many recent responses are kept for duplicate request detection.
const RESPONSE_CACHE: usize = 64;

/// The signaling socket currently attached to a session.
#[derive(Debug, Clone)]
pub struct Connection {
    /// Process-unique connection id.
    pub id: u64,
    /// Queue drained by the socket writer task.
    pub tx: mpsc::Sender<Outbound>,
}

impl Connection {
    /// Queues a message. A full queue means the client stopped reading; the
    /// socket is then treated as dead instead of buffering without bound.
    pub fn send(&self, message: Outbound) -> bool {
        self.tx.try_send(message).is_ok()
    }
}

/// A track a participant publishes.
#[derive(Debug, Clone)]
pub struct PublishedTrack {
    /// Room-unique id.
    pub id: String,
    /// mid on the publisher peer.
    pub mid: Mid,
    /// Audio or video.
    pub kind: MediaKind,
    /// What it carries.
    pub source: TrackSource,
    /// Mute state reported by the client.
    pub muted: bool,
    /// Simulcast rids, lowest quality first. Empty without simulcast.
    pub layers: Vec<Rid>,
    /// Last keyframe request sent to the publisher, for throttling.
    pub last_keyframe_request: Option<Instant>,
}

impl PublishedTrack {
    /// Public description.
    pub fn info(&self) -> TrackInfo {
        TrackInfo {
            track_id: self.id.clone(),
            kind: if self.kind == MediaKind::Audio {
                "audio"
            } else {
                "video"
            },
            source: self.source,
            muted: self.muted,
            simulcast: !self.layers.is_empty(),
        }
    }
}

/// Negotiation state of one forwarded track on a subscriber peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutState {
    /// Needs an m-line in the next offer.
    ToOpen,
    /// Offered, waiting for the answer.
    Negotiating(Mid),
    /// Negotiated; media flows.
    Open(Mid),
    /// Must be stopped in the next offer.
    ToStop(Mid),
    /// Stop offered, waiting for the answer.
    NegotiatingStop(Mid),
}

impl OutState {
    /// mid when one is assigned.
    pub fn mid(self) -> Option<Mid> {
        match self {
            OutState::ToOpen => None,
            OutState::Negotiating(m)
            | OutState::Open(m)
            | OutState::ToStop(m)
            | OutState::NegotiatingStop(m) => Some(m),
        }
    }

    /// Whether the track is (or is becoming) delivered.
    pub fn is_wanted(self) -> bool {
        matches!(
            self,
            OutState::ToOpen | OutState::Negotiating(_) | OutState::Open(_)
        )
    }
}

/// A track forwarded to one subscriber.
#[derive(Debug, Clone)]
pub struct OutTrack {
    /// Forwarded track.
    pub track_id: String,
    /// Publisher of the track.
    pub publisher_id: String,
    /// Audio or video.
    pub kind: MediaKind,
    /// Source of the track.
    pub source: TrackSource,
    /// Negotiation state.
    pub state: OutState,
    /// Requested quality.
    pub quality: Quality,
    /// Simulcast layer we want to forward.
    pub target_rid: Option<Rid>,
    /// Simulcast layer currently forwarded (switches happen on keyframes).
    pub current_rid: Option<Rid>,
    /// Keeps RTP time continuous across layer switches.
    pub timeline: Timeline,
}

/// An offer sent to the client and not yet answered.
pub struct PendingOffer {
    /// str0m state needed to apply the answer.
    pub pending: SdpPendingOffer,
    /// Revision echoed by the answer.
    pub revision: u32,
    /// The event as sent, so it can be re-sent after a resume.
    pub event: Value,
    /// When it was sent.
    pub sent_at: Instant,
}

/// Server → client media peer and what it forwards.
pub struct Subscriber {
    /// The peer connection.
    pub peer: Peer,
    /// Forwarded tracks.
    pub out: Vec<OutTrack>,
    /// Offer waiting for an answer.
    pub pending: Option<PendingOffer>,
    /// Revision of the last offer.
    pub revision: u32,
    /// Next offer must restart ICE.
    pub ice_restart: bool,
    /// The data channel was added (it keeps the peer connected while no
    /// track is forwarded).
    pub has_channel: bool,
    /// Latest TWCC estimate in kbps.
    pub estimate_kbps: Option<u64>,
}

impl Subscriber {
    /// Creates a subscriber around a fresh peer.
    pub fn new(peer: Peer) -> Self {
        Self {
            peer,
            out: Vec::new(),
            pending: None,
            revision: 0,
            ice_restart: false,
            has_channel: false,
            estimate_kbps: None,
        }
    }

    /// mid → track mapping for every track that has a mid.
    pub fn mappings(&self) -> Vec<SubscriberTrackMapping> {
        self.out
            .iter()
            .filter(|o| matches!(o.state, OutState::Negotiating(_) | OutState::Open(_)))
            .filter_map(|o| {
                Some(SubscriberTrackMapping {
                    mid: o.state.mid()?.to_string(),
                    track_id: o.track_id.clone(),
                    participant_id: o.publisher_id.clone(),
                    source: o.source,
                })
            })
            .collect()
    }
}

/// Answers to recent requests, keyed by request id.
#[derive(Debug, Default)]
pub struct ResponseCache {
    entries: VecDeque<(String, Result<Value, ProtocolError>)>,
}

impl ResponseCache {
    /// Cached result for `id`, if the request was already executed.
    pub fn get(&self, id: &str) -> Option<&Result<Value, ProtocolError>> {
        self.entries.iter().find(|(k, _)| k == id).map(|(_, v)| v)
    }

    /// Remembers a result, evicting the oldest entry when full.
    pub fn put(&mut self, id: String, result: Result<Value, ProtocolError>) {
        if self.entries.len() == RESPONSE_CACHE {
            self.entries.pop_front();
        }
        self.entries.push_back((id, result));
    }
}

/// Everything the room knows about one participant.
pub struct Participant {
    /// Room-unique id, stable while the identity stays in the room.
    pub id: String,
    /// Token identity (`sub`).
    pub identity: String,
    /// Display name.
    pub name: String,
    /// Role.
    pub role: Role,
    /// May publish media.
    pub can_publish: bool,
    /// Latest `exp` of the tokens this session joined with. A host removal
    /// must outlive it, or the token could simply be reused.
    pub token_exp: u64,
    /// Secret id the client presents to resume this session.
    pub session_id: String,
    /// Attached socket. `None` while waiting for a resume.
    pub conn: Option<Connection>,
    /// When the socket dropped.
    pub disconnected_at: Option<Instant>,
    /// Client → server peer.
    pub publisher: Option<Peer>,
    /// Server → client peer.
    pub subscriber: Option<Subscriber>,
    /// Published tracks.
    pub tracks: Vec<PublishedTrack>,
    /// Requested video subscriptions with quality.
    pub video_subscriptions: HashMap<String, Quality>,
    /// Audio tracks this participant explicitly unsubscribed from.
    pub muted_audio: HashSet<String>,
    /// Duplicate request detection.
    pub responses: ResponseCache,
    /// Sum of linear audio levels in the current speaker window.
    pub level_sum: f32,
    /// Number of samples in `level_sum`.
    pub level_count: u32,
}

impl Participant {
    /// Creates a participant from verified token claims.
    pub fn new(id: String, claims: &Claims, session_id: String, conn: Connection) -> Self {
        Self {
            id,
            identity: claims.sub.clone(),
            name: claims.name.clone().unwrap_or_else(|| claims.sub.clone()),
            role: claims.role,
            can_publish: claims.can_publish,
            token_exp: claims.exp,
            session_id,
            conn: Some(conn),
            disconnected_at: None,
            publisher: None,
            subscriber: None,
            tracks: Vec::new(),
            video_subscriptions: HashMap::new(),
            muted_audio: HashSet::new(),
            responses: ResponseCache::default(),
            level_sum: 0.0,
            level_count: 0,
        }
    }

    /// Public description.
    pub fn info(&self) -> ParticipantInfo {
        ParticipantInfo {
            participant_id: self.id.clone(),
            identity: self.identity.clone(),
            name: self.name.clone(),
            role: self.role,
            connected: self.conn.is_some(),
            tracks: self.tracks.iter().map(PublishedTrack::info).collect(),
        }
    }

    /// Sends a message if a socket is attached.
    pub fn send(&self, message: Outbound) {
        if let Some(conn) = &self.conn {
            conn.send(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ErrorCode;

    #[test]
    fn response_cache_evicts_oldest() {
        let mut cache = ResponseCache::default();
        for i in 0..=RESPONSE_CACHE {
            cache.put(i.to_string(), Ok(Value::from(i)));
        }
        assert!(cache.get("0").is_none());
        assert_eq!(cache.get("64"), Some(&Ok(Value::from(64))));
        cache.put(
            "e".into(),
            Err(ProtocolError::new(ErrorCode::Forbidden, "no")),
        );
        assert!(matches!(cache.get("e"), Some(Err(_))));
    }
}
