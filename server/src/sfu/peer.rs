//! One server side WebRTC peer connection.
//!
//! Every participant has up to two peers, following the same split LiveKit
//! uses: a *publisher* peer where the client is the offerer and only sends,
//! and a *subscriber* peer where the server is the offerer and only sends.
//! Keeping the directions apart means offers never collide ("glare"), and
//! either side can be rebuilt without touching the other.

use super::udp::{Datagram, MediaSockets, RoomPackets};
use std::time::Instant;
use str0m::{
    bwe::Bitrate,
    net::{Protocol, Receive},
    Candidate, Event, IceCreds, Input, Output, Rtc, RtcError,
};

/// Initial bandwidth estimate for subscriber peers, before TWCC feedback
/// arrives. Low enough to start with the medium layer on a phone.
const INITIAL_SUBSCRIBER_BITRATE: Bitrate = Bitrate::kbps(800);

/// Which side offers on this peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerKind {
    /// Client offers, server receives media.
    Publisher,
    /// Server offers, server sends media.
    Subscriber,
}

/// A `str0m::Rtc` plus the bookkeeping needed to drive it from a room task.
pub struct Peer {
    /// The sans-IO WebRTC state machine.
    pub rtc: Rtc,
    /// Publisher or subscriber.
    pub kind: PeerKind,
    ufrag: String,
    next_timeout: Instant,
}

impl Peer {
    /// Creates a peer with host candidates for every media socket and
    /// registers its ICE ufrag so that STUN traffic reaches `room`.
    pub fn new(kind: PeerKind, sockets: &MediaSockets, room: &RoomPackets, now: Instant) -> Self {
        let creds = IceCreds::new();
        let mut config = Rtc::builder().set_local_ice_credentials(creds.clone());
        if kind == PeerKind::Subscriber {
            // Bandwidth estimation drives simulcast layer selection per
            // receiver. Publishers do not need it: the server never sends
            // media on them.
            config = config.enable_bwe(Some(INITIAL_SUBSCRIBER_BITRATE));
        }
        let mut rtc = config.build(now);
        for addr in sockets.advertised() {
            match Candidate::host(addr, "udp") {
                Ok(candidate) => {
                    rtc.add_local_candidate(candidate);
                }
                Err(e) => tracing::warn!(%addr, error = ?e, "invalid host candidate"),
            }
        }
        sockets.register_ufrag(&creds.ufrag, room.clone());
        Self {
            rtc,
            kind,
            ufrag: creds.ufrag,
            next_timeout: now,
        }
    }

    /// Updates the STUN route after ICE credentials changed (ICE restart).
    pub fn refresh_ufrag(&mut self, sockets: &MediaSockets, room: &RoomPackets) {
        let current = self.rtc.direct_api().local_ice_credentials().ufrag;
        if current != self.ufrag {
            sockets.unregister_ufrag(&self.ufrag);
            sockets.register_ufrag(&current, room.clone());
            self.ufrag = current;
        }
    }

    /// Removes the STUN route. Call before dropping the peer.
    pub fn close(&mut self, sockets: &MediaSockets) {
        sockets.unregister_ufrag(&self.ufrag);
        self.rtc.disconnect();
    }

    /// Whether this peer owns the datagram (ICE credentials and addresses match).
    pub fn accepts(&self, datagram: &Datagram) -> bool {
        match receive(datagram) {
            Some(receive) => self
                .rtc
                .accepts(&Input::Receive(datagram.received, receive)),
            None => false,
        }
    }

    /// Feeds a datagram into the state machine.
    pub fn handle_datagram(&mut self, datagram: &Datagram) -> Result<(), RtcError> {
        let Some(receive) = receive(datagram) else {
            return Ok(());
        };
        self.rtc
            .handle_input(Input::Receive(datagram.received, receive))?;
        // Force a poll right away: incoming data usually produces output.
        self.next_timeout = datagram.received;
        Ok(())
    }

    /// Whether the peer needs `handle_timeout` and `poll` now: its timer
    /// expired, or input or an API call ([`Peer::wake`]) changed its state.
    pub fn is_due(&self, now: Instant) -> bool {
        now >= self.next_timeout
    }

    /// Advances timers.
    pub fn handle_timeout(&mut self, now: Instant) -> Result<(), RtcError> {
        self.rtc.handle_input(Input::Timeout(now))
    }

    /// Marks the peer for polling, for example after writing media to it.
    pub fn wake(&mut self, now: Instant) {
        self.next_timeout = self.next_timeout.min(now);
    }

    /// Sends pending datagrams and collects events until str0m reports its
    /// next timeout. Returns that timeout.
    pub fn poll(
        &mut self,
        sockets: &MediaSockets,
        events: &mut Vec<Event>,
    ) -> Result<Instant, RtcError> {
        loop {
            match self.rtc.poll_output()? {
                Output::Transmit(transmit) => {
                    sockets.send(transmit.source, transmit.destination, &transmit.contents);
                }
                Output::Event(event) => events.push(event),
                Output::Timeout(at) => {
                    self.next_timeout = at;
                    return Ok(at);
                }
            }
        }
    }

    /// Next instant at which [`Peer::handle_timeout`] must run.
    pub fn next_timeout(&self) -> Instant {
        self.next_timeout
    }
}

fn receive(datagram: &Datagram) -> Option<Receive<'_>> {
    let contents = datagram.data.as_slice().try_into().ok()?;
    Some(Receive {
        proto: Protocol::Udp,
        source: datagram.source,
        destination: datagram.destination,
        contents,
    })
}

/// Parses a trickled candidate. Accepts the line with or without the
/// `candidate:` prefix and with an `a=` prefix.
pub fn parse_candidate(line: &str) -> Option<Candidate> {
    let line = line.trim().trim_start_matches("a=");
    Candidate::from_sdp_string(line)
        .or_else(|_| Candidate::from_sdp_string(line.trim_start_matches("candidate:")))
        .ok()
}
