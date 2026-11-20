//! Media side of the room: peer lifecycle, SDP negotiation and forwarding.
//!
//! Forwarding works on whole media units (`Event::MediaData`, str0m "frame
//! mode"). str0m depacketizes incoming RTP, handles NACK/RTX towards the
//! publisher, and re-packetizes for every subscriber with its own sequence
//! numbers. That makes simulcast layer switching safe: the receiver never
//! sees sequence number gaps, only a keyframe from the new layer.

use super::{
    participant::{OutState, OutTrack, PendingOffer, PublishedTrack, Subscriber},
    random_id, Room,
};
use crate::{
    protocol::{
        ErrorCode, OfferedTrack, Outbound, PeerTarget, ProtocolError, Quality, SubscriptionChange,
        TrackSource,
    },
    sfu::{
        layers::{self, VideoDemand},
        peer::{parse_candidate, Peer, PeerKind},
        udp::Datagram,
    },
};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
use str0m::{
    bwe::{Bitrate, BweKind},
    change::{SdpAnswer, SdpOffer},
    media::{Direction, KeyframeRequestKind, MediaData, MediaKind, Mid, Rid},
    Event, IceConnectionState,
};

/// A subscriber that does not answer an offer within this time gets the
/// very same offer again.
///
/// The offer is never replaced by a different one while unanswered: the
/// client may already have applied it (its answer can be lost with a dying
/// socket), and WebRTC rejects a later offer whose m-lines differ from the
/// applied one ("order of m-lines ... doesn't match"). Changes made in the
/// meantime go into the next offer, after the answer arrives.
const OFFER_TIMEOUT: Duration = Duration::from_secs(10);

/// Minimum spacing of `speakers.changed` events. A change inside this
/// window is sent when it ends.
const SPEAKERS_THROTTLE: Duration = Duration::from_millis(250);

/// Upper bound of drive iterations per wakeup, to keep one room from
/// starving others if events keep producing events.
const MAX_DRIVE_ROUNDS: usize = 16;

impl Room {
    /// Earliest timer of any peer, but never later than the next tick.
    pub(super) fn next_timeout(&self) -> Instant {
        let mut next = Instant::now() + super::TICK;
        if let Some(due) = self.speakers_due {
            next = next.min(due);
        }
        if let Some(due) = self.keyframes_due {
            next = next.min(due);
        }
        for participant in &self.participants {
            if let Some(peer) = &participant.publisher {
                next = next.min(peer.next_timeout());
            }
            if let Some(sub) = &participant.subscriber {
                next = next.min(sub.peer.next_timeout());
            }
        }
        next
    }

    pub(super) fn handle_datagram(&mut self, datagram: Datagram) {
        // Fast path: the source address was seen before. The cached peer is
        // always verified with `accepts`, so a stale entry only costs a scan.
        let cached = self.routes.get(&datagram.source).cloned();
        if let Some((participant_id, kind)) = cached {
            if let Some(index) = self.index_of(&participant_id) {
                if self.deliver(index, kind, &datagram) {
                    return;
                }
            }
        }
        for index in 0..self.participants.len() {
            for kind in [PeerKind::Publisher, PeerKind::Subscriber] {
                if self.deliver(index, kind, &datagram) {
                    let id = self.participants[index].id.clone();
                    insert_route(&mut self.routes, datagram.source, id, kind);
                    return;
                }
            }
        }
        self.routes.remove(&datagram.source);
        self.services
            .metrics
            .udp_dropped(crate::metrics::UdpDrop::NoPeer);
    }

    /// Hands the datagram to the peer if it accepts it. Returns whether it did.
    fn deliver(&mut self, index: usize, kind: PeerKind, datagram: &Datagram) -> bool {
        let Some(peer) = self.peer_mut(index, kind) else {
            return false;
        };
        if !peer.accepts(datagram) {
            return false;
        }
        if let Err(e) = peer.handle_datagram(datagram) {
            tracing::debug!(error = ?e, "peer rejected datagram");
            self.fail_peer(index, kind);
        }
        true
    }

    /// Drops cached routes to a participant's peers (`None` = both). Called
    /// wherever a peer is closed or replaced, and on participant removal, which
    /// also keeps the cache bounded.
    pub(super) fn forget_routes(&mut self, participant_id: &str, kind: Option<PeerKind>) {
        self.routes
            .retain(|_, (id, k)| !(id == participant_id && kind.is_none_or(|kind| kind == *k)));
    }

    /// Advances all peers, then announces a speaker change if the audio
    /// selection changed or a throttled change is due.
    pub(super) fn drive(&mut self) {
        self.flush_keyframes(Instant::now());
        self.drive_peers();
        let now = Instant::now();
        if self.audio.generation() != self.audio_generation
            || self.speakers_due.is_some_and(|due| due <= now)
        {
            self.update_speakers(now);
        }
    }

    /// Advances all peers, sends their datagrams and processes their events
    /// until nothing new happens.
    fn drive_peers(&mut self) {
        for _ in 0..MAX_DRIVE_ROUNDS {
            let now = Instant::now();
            let mut produced = Vec::new();
            for index in 0..self.participants.len() {
                for kind in [PeerKind::Publisher, PeerKind::Subscriber] {
                    let sockets = self.services.sockets.clone();
                    let Some(peer) = self.peer_mut(index, kind) else {
                        continue;
                    };
                    // Peers whose timer is not due and that got no input
                    // since the last poll have nothing to do. Skipping them
                    // matters with 50 peers and thousands of packets/s.
                    if !peer.is_due(now) {
                        continue;
                    }
                    let mut events = Vec::new();
                    let result = peer
                        .handle_timeout(now)
                        .and_then(|_| peer.poll(&sockets, &mut events));
                    let alive = result.is_ok() && peer.rtc.is_alive();
                    if !events.is_empty() {
                        produced.push((self.participants[index].id.clone(), kind, events));
                    }
                    if !alive {
                        self.fail_peer(index, kind);
                    }
                }
            }
            if produced.is_empty() {
                return;
            }
            for (participant_id, kind, events) in produced {
                for event in events {
                    self.handle_event(&participant_id, kind, event);
                }
            }
        }
    }

    fn peer_mut(&mut self, index: usize, kind: PeerKind) -> Option<&mut Peer> {
        let participant = self.participants.get_mut(index)?;
        match kind {
            PeerKind::Publisher => participant.publisher.as_mut(),
            PeerKind::Subscriber => participant.subscriber.as_mut().map(|s| &mut s.peer),
        }
    }

    fn index_of(&self, participant_id: &str) -> Option<usize> {
        self.participants
            .iter()
            .position(|p| p.id == participant_id)
    }

    /// A peer hit a fatal error or ICE timed out for good. The client is told
    /// so it can rebuild that peer (`publisher.offer` with `reset` or
    /// `subscriber.reset`).
    fn fail_peer(&mut self, index: usize, kind: PeerKind) {
        self.forwarding_dirty = true;
        self.services
            .metrics
            .peer_failures_total
            .fetch_add(1, Ordering::Relaxed);
        let sockets = self.services.sockets.clone();
        let participant_id = self.participants[index].id.clone();
        self.forget_routes(&participant_id, Some(kind));
        let target = match kind {
            PeerKind::Publisher => {
                if let Some(mut peer) = self.participants[index].publisher.take() {
                    peer.close(&sockets);
                }
                self.unpublish(index, None);
                "publisher"
            }
            PeerKind::Subscriber => {
                if let Some(mut sub) = self.participants[index].subscriber.take() {
                    sub.peer.close(&sockets);
                }
                "subscriber"
            }
        };
        tracing::warn!(room = %self.id, participant = %self.participants[index].id, target, "peer failed");
        self.participants[index].send(Outbound::event("peer.failed", json!({"target": target})));
    }

    fn handle_event(&mut self, participant_id: &str, kind: PeerKind, event: Event) {
        let Some(index) = self.index_of(participant_id) else {
            return;
        };
        match (kind, event) {
            (PeerKind::Publisher, Event::MediaData(data)) => self.forward(index, data),
            (PeerKind::Subscriber, Event::KeyframeRequest(request)) => {
                let Some(sub) = &self.participants[index].subscriber else {
                    return;
                };
                let Some(out) = sub.out.iter().find(|o| o.state.mid() == Some(request.mid)) else {
                    return;
                };
                let (track_id, publisher_id, rid) = (
                    out.track_id.clone(),
                    out.publisher_id.clone(),
                    out.current_rid.or(out.target_rid),
                );
                self.request_keyframe(&publisher_id, &track_id, rid, request.kind, false);
            }
            (
                PeerKind::Subscriber,
                Event::EgressBitrateEstimate(BweKind::Twcc { estimate, .. }),
            ) => {
                if let Some(sub) = self.participants[index].subscriber.as_mut() {
                    sub.estimate_kbps = Some(estimate.as_u64() / 1000);
                    tracing::debug!(
                        participant = participant_id,
                        kbps = estimate.as_u64() / 1000,
                        "bandwidth estimate"
                    );
                }
            }
            (_, Event::IceConnectionStateChange(state)) => {
                tracing::debug!(room = %self.id, participant = participant_id, ?kind, ?state, "ice state");
                if state == IceConnectionState::Disconnected {
                    // Not fatal: the client will try an ICE restart. If that
                    // never happens str0m eventually gives up and the peer
                    // fails, which `drive` reports to the client.
                }
            }
            _ => {}
        }
    }

    // ----------------------------------------------------------------------
    // Publisher
    // ----------------------------------------------------------------------

    pub(super) fn publisher_offer(
        &mut self,
        index: usize,
        sdp: &str,
        tracks: &[OfferedTrack],
        reset: bool,
    ) -> Result<String, ProtocolError> {
        if !tracks.is_empty() && !self.participants[index].can_publish {
            return Err(ProtocolError::new(
                ErrorCode::Forbidden,
                "token does not allow publishing",
            ));
        }
        validate_tracks(tracks)?;
        let offer = SdpOffer::from_sdp_string(sdp).map_err(|e| {
            ProtocolError::new(ErrorCode::BadRequest, format!("invalid offer: {e}"))
        })?;

        let sockets = self.services.sockets.clone();
        if reset {
            let participant_id = self.participants[index].id.clone();
            self.forget_routes(&participant_id, Some(PeerKind::Publisher));
            if let Some(mut old) = self.participants[index].publisher.take() {
                old.close(&sockets);
            }
            self.unpublish(index, None);
        }
        let packets = self.packets_tx.clone();
        let participant = &mut self.participants[index];
        let peer = participant.publisher.get_or_insert_with(|| {
            Peer::new(PeerKind::Publisher, &sockets, &packets, Instant::now())
        });
        let answer = peer.rtc.sdp_api().accept_offer(offer).map_err(|e| {
            ProtocolError::new(ErrorCode::InvalidState, format!("offer rejected: {e}"))
        })?;
        peer.refresh_ufrag(&sockets, &packets);
        peer.wake(Instant::now());

        // The offered track list is the source of truth: new mids become
        // tracks, missing ones are unpublished.
        let mut added = Vec::new();
        let mut kept = HashSet::new();
        for offered in tracks {
            let mid = Mid::from(offered.mid.as_str());
            if peer.rtc.media(mid).is_none() {
                return Err(ProtocolError::new(
                    ErrorCode::BadRequest,
                    format!("mid {} not in offer", offered.mid),
                ));
            }
            let layers: Vec<Rid> = offered
                .layers
                .iter()
                .map(|r| Rid::from(r.as_str()))
                .collect();
            if let Some(track) = participant
                .tracks
                .iter_mut()
                .find(|t| t.mid == mid && t.source == offered.source)
            {
                track.muted = offered.muted;
                track.layers = layers;
                kept.insert(track.id.clone());
                if track.muted {
                    // replaceTrack(null) sends nothing more that could
                    // release the slot.
                    self.audio.remove(&track.id);
                }
            } else {
                let track = PublishedTrack {
                    id: random_id("TR_"),
                    mid,
                    kind: if offered.source.is_video() {
                        MediaKind::Video
                    } else {
                        MediaKind::Audio
                    },
                    source: offered.source,
                    muted: offered.muted,
                    layers,
                    keyframes: HashMap::new(),
                    orientation: None,
                };
                kept.insert(track.id.clone());
                added.push(track.id.clone());
                participant.tracks.push(track);
            }
        }
        self.unpublish(index, Some(&kept));
        let answer = answer.to_sdp_string();

        let info = self.participants[index].info();
        self.broadcast_except(
            index,
            Outbound::event("participant.updated", json!({"participant": info})),
        );
        Ok(answer)
    }

    /// Removes published tracks of `index` that are not in `keep` (all of
    /// them when `keep` is `None`) and stops forwarding them.
    fn unpublish(&mut self, index: usize, keep: Option<&HashSet<String>>) {
        self.forwarding_dirty = true;
        let participant = &mut self.participants[index];
        let audio = &mut self.audio;
        let before = participant.tracks.len();
        participant.tracks.retain(|t| {
            let kept = keep.is_some_and(|keep| keep.contains(&t.id));
            if !kept {
                audio.remove(&t.id);
            }
            kept
        });
        if participant.tracks.len() == before {
            return;
        }
        if keep.is_some() {
            let info = participant.info();
            self.broadcast_except(
                index,
                Outbound::event("participant.updated", json!({"participant": info})),
            );
        }
        // Stop forwarding right away; the m-lines are removed by the next
        // `negotiate_all`, which every caller runs.
        for other in 0..self.participants.len() {
            if other != index {
                self.reconcile(other);
            }
        }
    }

    pub(super) fn mute(
        &mut self,
        index: usize,
        source: TrackSource,
        muted: bool,
    ) -> Result<(), ProtocolError> {
        let participant = &mut self.participants[index];
        let track = participant
            .tracks
            .iter_mut()
            .find(|t| t.source == source)
            .ok_or_else(|| {
                ProtocolError::new(ErrorCode::InvalidState, "source is not published")
            })?;
        if muted && track.kind == MediaKind::Audio {
            // replaceTrack(null) sends nothing more that could release the slot.
            self.audio.remove(&track.id);
        }
        if track.muted != muted {
            track.muted = muted;
            let info = participant.info();
            self.broadcast_except(
                index,
                Outbound::event("participant.updated", json!({"participant": info})),
            );
        }
        Ok(())
    }

    // ----------------------------------------------------------------------
    // Subscriber
    // ----------------------------------------------------------------------

    /// The server's media sockets changed (new network): update the host
    /// candidates of every peer. Clients whose path broke restart ICE
    /// (SDK ICE monitor) and get the new address in that negotiation.
    pub(super) fn media_addresses_changed(&mut self) {
        let sockets = &self.services.sockets;
        let mut peers = 0;
        for participant in &mut self.participants {
            for peer in [
                participant.publisher.as_mut(),
                participant.subscriber.as_mut().map(|s| &mut s.peer),
            ]
            .into_iter()
            .flatten()
            {
                if peer.sync_local_candidates(sockets) {
                    peers += 1;
                }
            }
        }
        tracing::info!(room = %self.id, peers, addresses = ?sockets.advertised(), "media addresses updated");
    }

    pub(super) fn create_subscriber(&mut self, index: usize) {
        self.forwarding_dirty = true;
        let peer = Peer::new(
            PeerKind::Subscriber,
            &self.services.sockets,
            &self.packets_tx,
            Instant::now(),
        );
        self.participants[index].subscriber = Some(Subscriber::new(peer));
        self.negotiate(index);
    }

    pub(super) fn reset_subscriber(&mut self, index: usize) {
        self.forwarding_dirty = true;
        let participant_id = self.participants[index].id.clone();
        self.forget_routes(&participant_id, Some(PeerKind::Subscriber));
        if let Some(mut old) = self.participants[index].subscriber.take() {
            old.peer.close(&self.services.sockets);
        }
        self.create_subscriber(index);
    }

    pub(super) fn subscriber_answer(
        &mut self,
        index: usize,
        sdp: &str,
        revision: u32,
    ) -> Result<(), ProtocolError> {
        let sub = self.participants[index]
            .subscriber
            .as_mut()
            .ok_or_else(|| ProtocolError::new(ErrorCode::InvalidState, "no subscriber peer"))?;
        if sub.pending.as_ref().map(|p| p.revision) != Some(revision) {
            tracing::debug!(revision, pending = ?sub.pending.as_ref().map(|p| p.revision), "outdated subscriber answer");
            return Err(ProtocolError::new(
                ErrorCode::InvalidState,
                "answer is for an outdated offer",
            ));
        }
        let answer = SdpAnswer::from_sdp_string(sdp).map_err(|e| {
            ProtocolError::new(ErrorCode::BadRequest, format!("invalid answer: {e}"))
        })?;
        let pending = sub.pending.take().expect("checked above");
        sub.peer
            .rtc
            .sdp_api()
            .accept_answer(pending.pending, answer)
            .map_err(|e| {
                ProtocolError::new(ErrorCode::InvalidState, format!("answer rejected: {e}"))
            })?;
        // After an ICE restart offer the new credentials take effect only
        // now; without re-registering, the client's checks with the new
        // ufrag are unroutable until some unrelated packet maps its address.
        sub.peer
            .refresh_ufrag(&self.services.sockets, &self.packets_tx);
        sub.peer.wake(Instant::now());

        self.forwarding_dirty = true;
        let mut opened = Vec::new();
        for out in &mut sub.out {
            if let OutState::Negotiating(mid) = out.state {
                out.state = OutState::Open(mid);
                if out.kind == MediaKind::Video {
                    opened.push((
                        out.publisher_id.clone(),
                        out.track_id.clone(),
                        out.target_rid,
                    ));
                }
            }
        }
        sub.out
            .retain(|o| !matches!(o.state, OutState::NegotiatingStop(_)));
        // A new video receiver can only start decoding at a keyframe.
        for (publisher_id, track_id, rid) in opened {
            self.request_keyframe(
                &publisher_id,
                &track_id,
                rid,
                KeyframeRequestKind::Pli,
                true,
            );
        }
        Ok(())
    }

    pub(super) fn add_candidate(
        &mut self,
        index: usize,
        target: PeerTarget,
        line: &str,
    ) -> Result<bool, ProtocolError> {
        let kind = match target {
            PeerTarget::Publisher => PeerKind::Publisher,
            PeerTarget::Subscriber => PeerKind::Subscriber,
        };
        let allow_private = self.services.config.mode == crate::config::Mode::Development;
        let peer = self
            .peer_mut(index, kind)
            .ok_or_else(|| ProtocolError::new(ErrorCode::InvalidState, "peer does not exist"))?;
        // Unparseable (for example mDNS `.local`), internal-network and
        // excess candidates are ignored: the server still learns the
        // client's real address from its STUN requests (peer-reflexive).
        Ok(parse_candidate(line).is_some_and(|c| peer.add_remote_candidate(c, allow_private)))
    }

    pub(super) fn ice_restart(
        &mut self,
        index: usize,
        target: PeerTarget,
    ) -> Result<(), ProtocolError> {
        if target == PeerTarget::Publisher {
            return Err(ProtocolError::new(
                ErrorCode::BadRequest,
                "restart the publisher by sending publisher.offer with new ICE credentials",
            ));
        }
        let sub = self.participants[index]
            .subscriber
            .as_mut()
            .ok_or_else(|| ProtocolError::new(ErrorCode::InvalidState, "no subscriber peer"))?;
        // Applied with the next offer. An outstanding offer is kept (see
        // OFFER_TIMEOUT); the restart follows once it is answered.
        sub.ice_restart = true;
        Ok(())
    }

    pub(super) fn update_subscriptions(
        &mut self,
        index: usize,
        changes: &[SubscriptionChange],
    ) -> Result<(), ProtocolError> {
        let mut video = self.participants[index].video_subscriptions.clone();
        let mut muted_audio = self.participants[index].muted_audio.clone();
        for change in changes {
            let Some(track) = self
                .participants
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != index)
                .flat_map(|(_, p)| p.tracks.iter())
                .find(|t| t.id == change.track_id)
            else {
                return Err(ProtocolError::new(
                    ErrorCode::BadRequest,
                    format!("unknown track {}", change.track_id),
                ));
            };
            match (track.kind, change.subscribed) {
                (MediaKind::Video, true) => {
                    video.insert(track.id.clone(), change.quality);
                }
                (MediaKind::Video, false) => {
                    video.remove(&track.id);
                }
                (MediaKind::Audio, true) => {
                    muted_audio.remove(&track.id);
                }
                (MediaKind::Audio, false) => {
                    muted_audio.insert(track.id.clone());
                }
            }
        }
        let limit = self.services.config.rooms.max_video_subscriptions;
        if video.len() > limit {
            return Err(ProtocolError::new(
                ErrorCode::LimitExceeded,
                format!("at most {limit} video subscriptions"),
            ));
        }
        let participant = &mut self.participants[index];
        participant.video_subscriptions = video;
        participant.muted_audio = muted_audio;
        // Quality changes apply right away; m-line changes in `negotiate`.
        self.reconcile(index);
        Ok(())
    }

    /// Tracks participant `index` should receive: every other participant's
    /// audio (unless it unsubscribed) plus the video tracks it subscribed to.
    fn desired_tracks(&self, index: usize) -> Vec<Desired> {
        let me = &self.participants[index];
        let mut desired = Vec::new();
        for other in self.participants.iter().filter(|p| p.id != me.id) {
            for track in &other.tracks {
                let quality = match track.kind {
                    MediaKind::Audio if me.muted_audio.contains(&track.id) => continue,
                    MediaKind::Audio => Quality::default(),
                    MediaKind::Video => match me.video_subscriptions.get(&track.id) {
                        Some(quality) => *quality,
                        None => continue,
                    },
                };
                desired.push(Desired {
                    track_id: track.id.clone(),
                    publisher_id: other.id.clone(),
                    kind: track.kind,
                    source: track.source,
                    quality,
                    lowest_rid: track.layers.first().copied(),
                });
            }
        }
        desired
    }

    /// Brings the subscriber's forwarded tracks in line with
    /// [`Room::desired_tracks`].
    ///
    /// m-lines are only added or stopped while no offer is outstanding, so
    /// at that point every entry is `ToOpen`, `Open` or `ToStop`. Quality
    /// changes apply immediately.
    fn reconcile(&mut self, index: usize) {
        self.forwarding_dirty = true;
        let desired = self.desired_tracks(index);
        let participant = &mut self.participants[index];
        let wanted: HashSet<&str> = desired.iter().map(|d| d.track_id.as_str()).collect();
        participant
            .video_subscriptions
            .retain(|id, _| wanted.contains(id.as_str()));

        let Some(sub) = participant.subscriber.as_mut() else {
            return;
        };
        let settled = sub.pending.is_none();
        for out in &mut sub.out {
            match desired.iter().find(|d| d.track_id == out.track_id) {
                Some(d) if out.state.is_wanted() => out.quality = d.quality,
                None if settled => {
                    if let OutState::Open(mid) = out.state {
                        out.state = OutState::ToStop(mid);
                    }
                }
                _ => {}
            }
        }
        if settled {
            sub.out
                .retain(|o| o.state != OutState::ToOpen || wanted.contains(o.track_id.as_str()));
            for d in &desired {
                if sub
                    .out
                    .iter()
                    .any(|o| o.track_id == d.track_id && o.state.is_wanted())
                {
                    continue;
                }
                sub.out.push(OutTrack {
                    track_id: d.track_id.clone(),
                    publisher_id: d.publisher_id.clone(),
                    kind: d.kind,
                    source: d.source,
                    state: OutState::ToOpen,
                    quality: d.quality,
                    target_rid: d.lowest_rid,
                    current_rid: None,
                    timeline: Default::default(),
                });
            }
        }
        self.allocate_layers(index);
    }

    pub(super) fn negotiate_all(&mut self) {
        for index in 0..self.participants.len() {
            self.negotiate(index);
        }
    }

    /// Sends a subscriber offer when forwarded tracks changed and no offer
    /// is outstanding. Changes made while an offer is pending are collected
    /// and sent once the answer arrives.
    fn negotiate(&mut self, index: usize) {
        if self.participants[index]
            .subscriber
            .as_ref()
            .is_none_or(|s| s.pending.is_some())
        {
            return;
        }
        self.reconcile(index);
        let sockets = self.services.sockets.clone();
        let packets = self.packets_tx.clone();
        let participant = &mut self.participants[index];
        let Some(sub) = participant.subscriber.as_mut() else {
            return;
        };
        let Subscriber {
            peer,
            out,
            ice_restart,
            has_channel,
            ..
        } = sub;
        let mut change = peer.rtc.sdp_api();
        if !*has_channel {
            change.add_channel("ecochain".into());
        }
        if *ice_restart {
            change.ice_restart(true);
        }
        self.forwarding_dirty = true;
        for track in out.iter_mut() {
            match track.state {
                OutState::ToOpen => {
                    let mid = change.add_media(
                        track.kind,
                        Direction::SendOnly,
                        Some(track.publisher_id.clone()),
                        Some(track.track_id.clone()),
                        None,
                    );
                    track.state = OutState::Negotiating(mid);
                }
                OutState::ToStop(mid) => {
                    change.stop_media(mid);
                    track.state = OutState::NegotiatingStop(mid);
                }
                _ => {}
            }
        }
        if !change.has_changes() {
            return;
        }
        let Some((offer, pending)) = change.apply() else {
            return;
        };
        *has_channel = true;
        *ice_restart = false;
        peer.refresh_ufrag(&sockets, &packets);
        peer.wake(Instant::now());
        sub.revision += 1;
        let sdp = offer.to_sdp_string();
        tracing::debug!(
            participant = %participant.id,
            revision = sub.revision,
            m_lines = sdp.lines().filter(|l| l.starts_with("m=")).count(),
            "subscriber offer"
        );
        let event = json!({
            "sdp": sdp,
            "revision": sub.revision,
            "tracks": sub.mappings(),
        });
        sub.pending = Some(PendingOffer {
            pending,
            revision: sub.revision,
            event: event.clone(),
            sent_at: Instant::now(),
        });
        participant.send(Outbound::event("subscriber.offer", event));
    }

    // ----------------------------------------------------------------------
    // Forwarding
    // ----------------------------------------------------------------------

    fn forward(&mut self, index: usize, data: MediaData) {
        self.services
            .metrics
            .media_in_total
            .fetch_add(1, Ordering::Relaxed);
        let publisher = &mut self.participants[index];
        let Some(track) = publisher.tracks.iter_mut().find(|t| t.mid == data.mid) else {
            return;
        };
        if data.is_keyframe() {
            // The deferred request, if any, is satisfied. Never inserts: the
            // rid comes from the network.
            if let Some(gate) = track.keyframes.get_mut(&data.rid) {
                gate.keyframe_received();
            }
        }
        if data.ext_vals.video_orientation.is_some() {
            track.orientation = data.ext_vals.video_orientation;
        }
        let (track_id, kind, orientation, muted) =
            (track.id.clone(), track.kind, track.orientation, track.muted);
        let publisher_id = publisher.id.clone();
        let now = Instant::now();

        let mut audio_level = None;
        if kind == MediaKind::Audio {
            // RFC 6464: level is -dBov, 0 is loudest, -127 silence.
            let level = data.ext_vals.audio_level;
            // Only the loudest few tracks reach subscribers; the m-lines of
            // the others stay negotiated and simply carry no packets.
            if !self.audio.observe(&track_id, level, muted, now) {
                return;
            }
            audio_level = level.map(|l| (l, data.ext_vals.voice_activity.unwrap_or(false)));
        } else if !data.contiguous {
            // Packets were lost beyond what NACK recovered; the decoder
            // needs a fresh keyframe.
            self.request_keyframe(
                &publisher_id,
                &track_id,
                data.rid,
                KeyframeRequestKind::Pli,
                false,
            );
        }

        if self.forwarding_dirty {
            self.rebuild_forwarding();
        }
        let mut written = 0u64;
        let mut wanted_keyframe: Vec<Option<Rid>> = Vec::new();
        let subscribers = self.forwarding.get(&track_id).map(Vec::as_slice);
        for subscriber_id in subscribers.unwrap_or_default() {
            let Some(&slot) = self.slots.get(subscriber_id) else {
                continue;
            };
            let Some(other) = self.participants.get_mut(slot) else {
                continue;
            };
            if slot == index || other.id != *subscriber_id {
                continue;
            }
            let Some(sub) = other.subscriber.as_mut() else {
                continue;
            };
            // The index is only a hint; never write to a mid that is not open.
            let Some((out, mid)) = sub.out.iter_mut().find_map(|o| match o.state {
                OutState::Open(mid) if o.track_id == track_id => Some((o, mid)),
                _ => None,
            }) else {
                continue;
            };

            let mut switched = false;
            if data.rid.is_some() && data.rid != out.current_rid {
                if data.rid != out.target_rid {
                    continue;
                }
                if !data.is_keyframe() {
                    if !wanted_keyframe.contains(&out.target_rid) {
                        wanted_keyframe.push(out.target_rid);
                    }
                    continue;
                }
                switched = out.current_rid.is_some();
                out.current_rid = data.rid;
            }
            let time = out.timeline.map(data.time, data.network_time, switched);
            let Some(mut writer) = sub.peer.rtc.writer(mid) else {
                continue;
            };
            let Some(pt) = writer.match_params(data.params) else {
                continue;
            };
            if let Some((level, voice)) = audio_level {
                writer = writer.audio_level(level, voice);
            }
            if let Some(orientation) = orientation {
                writer = writer.video_orientation(orientation);
            }
            match writer.write(pt, data.network_time, time, data.data.clone()) {
                Ok(()) => {
                    written += 1;
                    sub.peer.wake(now);
                }
                Err(e) => tracing::debug!(error = ?e, "forward write failed"),
            }
        }
        self.services
            .metrics
            .media_out_total
            .fetch_add(written, Ordering::Relaxed);
        for rid in wanted_keyframe {
            self.request_keyframe(
                &publisher_id,
                &track_id,
                rid,
                KeyframeRequestKind::Pli,
                false,
            );
        }
    }

    /// Rebuilds `forwarding` from the `Open` entries of every subscriber.
    pub(super) fn rebuild_forwarding(&mut self) {
        self.forwarding.clear();
        self.slots.clear();
        for (slot, participant) in self.participants.iter().enumerate() {
            self.slots.insert(participant.id.clone(), slot);
            let Some(sub) = participant.subscriber.as_ref() else {
                continue;
            };
            for out in &sub.out {
                if matches!(out.state, OutState::Open(_)) {
                    self.forwarding
                        .entry(out.track_id.clone())
                        .or_default()
                        .push(participant.id.clone());
                }
            }
        }
        self.forwarding_dirty = false;
    }

    /// Asks the publisher for a keyframe of one layer. Requests inside the
    /// layer's throttle window are deferred and sent by `flush_keyframes`.
    /// `urgent` (new subscription, layer switch) uses a shorter window.
    fn request_keyframe(
        &mut self,
        publisher_id: &str,
        track_id: &str,
        rid: Option<Rid>,
        kind: KeyframeRequestKind,
        urgent: bool,
    ) {
        let Some(index) = self.index_of(publisher_id) else {
            return;
        };
        let participant = &mut self.participants[index];
        let Some(track) = participant.tracks.iter_mut().find(|t| t.id == track_id) else {
            return;
        };
        // Only layers the track really has: the rid may come from the network.
        if !super::keyframe::is_known_layer(&track.layers, rid) {
            return;
        }
        let now = Instant::now();
        let gate = track.keyframes.entry(rid).or_default();
        if !gate.request(now, urgent) {
            if let Some(due) = gate.due() {
                self.keyframes_due = Some(self.keyframes_due.map_or(due, |d| d.min(due)));
            }
            return;
        }
        let mid = track.mid;
        Self::send_keyframe_request(participant, mid, rid, kind, now);
    }

    fn send_keyframe_request(
        participant: &mut super::participant::Participant,
        mid: Mid,
        rid: Option<Rid>,
        kind: KeyframeRequestKind,
        now: Instant,
    ) {
        let Some(peer) = participant.publisher.as_mut() else {
            return;
        };
        let Some(mut writer) = peer.rtc.writer(mid) else {
            return;
        };
        if writer.is_request_keyframe_possible(kind) && writer.request_keyframe(rid, kind).is_ok() {
            peer.wake(now);
        }
    }

    /// Sends the keyframe requests whose deferral has ended.
    pub(super) fn flush_keyframes(&mut self, now: Instant) {
        if self.keyframes_due.is_none_or(|due| due > now) {
            return;
        }
        let mut next: Option<Instant> = None;
        for participant in &mut self.participants {
            let mut due = Vec::new();
            for track in &mut participant.tracks {
                for (rid, gate) in &mut track.keyframes {
                    if gate.take_due(now) {
                        due.push((track.mid, *rid));
                    }
                    if let Some(at) = gate.due() {
                        next = Some(next.map_or(at, |n| n.min(at)));
                    }
                }
            }
            for (mid, rid) in due {
                Self::send_keyframe_request(participant, mid, rid, KeyframeRequestKind::Pli, now);
            }
        }
        self.keyframes_due = next;
    }

    /// Picks the simulcast layer per forwarded video track from requested
    /// quality and the subscriber's bandwidth estimate.
    fn allocate_layers(&mut self, index: usize) {
        let layer_counts: Vec<(String, Vec<Rid>)> = self
            .participants
            .iter()
            .flat_map(|p| p.tracks.iter())
            .filter(|t| t.kind == MediaKind::Video)
            .map(|t| (t.id.clone(), t.layers.clone()))
            .collect();
        let participant = &mut self.participants[index];
        let Some(sub) = participant.subscriber.as_mut() else {
            return;
        };
        let videos: Vec<usize> = (0..sub.out.len())
            .filter(|&i| sub.out[i].kind == MediaKind::Video && sub.out[i].state.is_wanted())
            .collect();
        let rids_of = |track_id: &str| {
            layer_counts
                .iter()
                .find(|(id, _)| id == track_id)
                .map(|(_, rids)| rids.clone())
                .unwrap_or_default()
        };
        let demands: Vec<VideoDemand> = videos
            .iter()
            .map(|&i| VideoDemand {
                layers: rids_of(&sub.out[i].track_id).len(),
                quality: sub.out[i].quality,
                screen: sub.out[i].source == TrackSource::Screen,
            })
            .collect();
        let chosen = layers::allocate(&demands, sub.estimate_kbps);
        let mut desired_kbps = 0;
        let mut changed = Vec::new();
        for (slot, &i) in videos.iter().enumerate() {
            let rids = rids_of(&sub.out[i].track_id);
            let target = rids.get(chosen[slot]).copied();
            desired_kbps += layers::LAYER_KBPS[demands[slot].requested_layer().min(2)];
            let out = &mut sub.out[i];
            if out.target_rid != target {
                out.target_rid = target;
                // An out that is not open yet gets its urgent request when
                // the answer opens it; asking now would burn the throttle
                // window before the m-line can receive the keyframe.
                if matches!(out.state, OutState::Open(_)) {
                    changed.push((out.publisher_id.clone(), out.track_id.clone(), target));
                }
            }
        }
        // Let the estimator probe up to what the receiver asked for.
        if desired_kbps > 0 {
            sub.peer
                .rtc
                .bwe()
                .set_bitrate(Bitrate::kbps(300), Bitrate::kbps(desired_kbps + 100));
            sub.peer.wake(Instant::now());
        }
        for (publisher_id, track_id, rid) in changed {
            self.request_keyframe(
                &publisher_id,
                &track_id,
                rid,
                KeyframeRequestKind::Pli,
                true,
            );
        }
    }

    /// Tears down both peers of a participant and unpublishes its tracks.
    pub(super) fn close_media(&mut self, index: usize) {
        self.forwarding_dirty = true;
        let participant_id = self.participants[index].id.clone();
        self.forget_routes(&participant_id, None);
        let sockets = self.services.sockets.clone();
        let participant = &mut self.participants[index];
        if let Some(mut peer) = participant.publisher.take() {
            peer.close(&sockets);
        }
        if let Some(mut sub) = participant.subscriber.take() {
            sub.peer.close(&sockets);
        }
        self.unpublish(index, None);
    }

    pub(super) fn media_housekeeping(&mut self, now: Instant) {
        // Re-send unanswered offers.
        for index in 0..self.participants.len() {
            let participant = &mut self.participants[index];
            let Some(pending) = participant
                .subscriber
                .as_mut()
                .and_then(|s| s.pending.as_mut())
                .filter(|p| now - p.sent_at >= OFFER_TIMEOUT)
            else {
                continue;
            };
            pending.sent_at = now;
            tracing::debug!(participant = %participant.id, revision = pending.revision, "resending unanswered subscriber offer");
            let event = pending.event.clone();
            participant.send(Outbound::event("subscriber.offer", event));
        }

        // Safety net: picks up any change whose caller did not negotiate.
        self.negotiate_all();
        for index in 0..self.participants.len() {
            self.allocate_layers(index);
        }

        // Expires stalled speakers and picks up order changes.
        self.update_speakers(now);
    }

    /// Current speakers by participant, loudest first, with the smoothed
    /// linear level. Stalled tracks are expired first.
    pub(super) fn current_speakers(&mut self, now: Instant) -> Vec<(String, f32)> {
        self.audio.expire(now);
        let mut speakers: Vec<(String, f32)> = Vec::new();
        for (track_id, level) in self.audio.selected_levels() {
            let Some(publisher) = self
                .participants
                .iter()
                .find(|p| p.tracks.iter().any(|t| t.id == track_id))
            else {
                continue;
            };
            // Loudest first: a second track of the same participant is quieter.
            if !speakers.iter().any(|(id, _)| *id == publisher.id) {
                speakers.push((publisher.id.clone(), level));
            }
        }
        speakers
    }

    /// Sends `speakers.changed` when the speaker list changed, at most once
    /// per `SPEAKERS_THROTTLE`; a held back change is sent via `speakers_due`.
    fn update_speakers(&mut self, now: Instant) {
        let levels = self.current_speakers(now);
        self.audio_generation = self.audio.generation();
        let ids: Vec<String> = levels.iter().map(|(id, _)| id.clone()).collect();
        if ids == self.speakers {
            self.speakers_due = None;
            return;
        }
        if let Some(last) = self.last_speakers_sent {
            if now.saturating_duration_since(last) < SPEAKERS_THROTTLE {
                self.speakers_due = Some(last + SPEAKERS_THROTTLE);
                return;
            }
        }
        self.speakers = ids;
        self.last_speakers_sent = Some(now);
        self.speakers_due = None;
        let speakers: Vec<_> = levels
            .iter()
            .map(|(id, level)| json!({"participant_id": id, "level": (level * 100.0).round() / 100.0}))
            .collect();
        let event = Outbound::event("speakers.changed", json!({"speakers": speakers}));
        for participant in &self.participants {
            participant.send(event.clone());
        }
    }
}

/// A track a subscriber should receive.
struct Desired {
    track_id: String,
    publisher_id: String,
    kind: MediaKind,
    source: TrackSource,
    quality: Quality,
    lowest_rid: Option<Rid>,
}

fn validate_tracks(tracks: &[OfferedTrack]) -> Result<(), ProtocolError> {
    let bad = |m: &str| Err(ProtocolError::new(ErrorCode::BadRequest, m.to_owned()));
    if tracks.len() > 4 {
        return bad("at most 4 published tracks");
    }
    let mut sources = HashSet::new();
    let mut mids = HashSet::new();
    for track in tracks {
        if !sources.insert(track.source) || !mids.insert(&track.mid) {
            return bad("duplicate source or mid");
        }
        if track.mid.is_empty() || track.mid.len() > 16 {
            return bad("mid must be 1..=16 characters");
        }
        if track.layers.len() > 3 || (!track.source.is_video() && !track.layers.is_empty()) {
            return bad("only video tracks may have up to 3 simulcast layers");
        }
        if track
            .layers
            .iter()
            .any(|r| r.is_empty() || r.len() > 8 || !r.bytes().all(|b| b.is_ascii_alphanumeric()))
        {
            return bad("rid must be 1..=8 alphanumeric characters");
        }
    }
    Ok(())
}

/// Most source addresses cached per peer. A client may legitimately use a few
/// (network switch, several candidate pairs); more would only let it grow the
/// cache.
const MAX_ROUTES_PER_PEER: usize = 4;

/// Caches `source` for a peer, evicting another address of the same peer when
/// it already has `MAX_ROUTES_PER_PEER`.
fn insert_route(
    routes: &mut HashMap<SocketAddr, (String, PeerKind)>,
    source: SocketAddr,
    participant_id: String,
    kind: PeerKind,
) {
    let mut own = routes
        .iter()
        .filter(|(addr, (id, k))| **addr != source && *id == participant_id && *k == kind)
        .map(|(addr, _)| *addr);
    let count = own.clone().count();
    if count >= MAX_ROUTES_PER_PEER {
        if let Some(victim) = own.next() {
            routes.remove(&victim);
        }
    }
    routes.insert(source, (participant_id, kind));
}

#[cfg(test)]
mod route_tests {
    use super::*;

    #[test]
    fn routes_per_peer_are_capped() {
        let mut routes = HashMap::new();
        for port in 1000..1100u16 {
            let addr: SocketAddr = ([127, 0, 0, 1], port).into();
            insert_route(&mut routes, addr, "a".into(), PeerKind::Publisher);
        }
        let other: SocketAddr = ([127, 0, 0, 2], 1).into();
        insert_route(&mut routes, other, "b".into(), PeerKind::Publisher);
        let sub: SocketAddr = ([127, 0, 0, 3], 1).into();
        insert_route(&mut routes, sub, "a".into(), PeerKind::Subscriber);
        let count = |routes: &HashMap<SocketAddr, (String, PeerKind)>, id: &str, kind| {
            routes
                .values()
                .filter(|(i, k)| i == id && *k == kind)
                .count()
        };
        assert_eq!(
            count(&routes, "a", PeerKind::Publisher),
            MAX_ROUTES_PER_PEER
        );
        assert_eq!(count(&routes, "b", PeerKind::Publisher), 1);
        assert_eq!(count(&routes, "a", PeerKind::Subscriber), 1);
        // The newest address is always kept; re-inserting a known one is free.
        let newest: SocketAddr = ([127, 0, 0, 1], 1099).into();
        assert!(routes.contains_key(&newest));
        insert_route(&mut routes, newest, "a".into(), PeerKind::Publisher);
        assert_eq!(
            count(&routes, "a", PeerKind::Publisher),
            MAX_ROUTES_PER_PEER
        );
    }
}
