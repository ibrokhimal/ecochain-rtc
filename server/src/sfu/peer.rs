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
    remote_candidates: usize,
}

/// Upper bound of trickled candidates per peer. A real client sends a
/// handful (one per interface and transport); the cap stops a client from
/// making the server probe an unbounded list of addresses.
const MAX_REMOTE_CANDIDATES: usize = 32;

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
            remote_candidates: 0,
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

    /// Adds a trickled client candidate if it is safe to send STUN checks
    /// to it. Returns whether it was added.
    ///
    /// The server sends connectivity checks to every remote candidate, so
    /// an unchecked candidate would let a client point the server at any
    /// address (internal services, third parties). Peer-reflexive addresses
    /// learned from the client's own STUN requests are unaffected, so
    /// dropping a candidate never breaks a genuine connection.
    pub fn add_remote_candidate(&mut self, candidate: Candidate, allow_private: bool) -> bool {
        if self.remote_candidates >= MAX_REMOTE_CANDIDATES
            || !is_allowed_remote(&candidate, allow_private)
        {
            return false;
        }
        self.remote_candidates += 1;
        self.rtc.add_remote_candidate(candidate);
        self.wake(Instant::now());
        true
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

/// Whether STUN checks may be sent to `candidate`.
///
/// Always rejected: non-UDP (the SFU only listens on UDP), port 0,
/// unspecified, multicast and broadcast addresses. Loopback, private,
/// link-local and IPv6 unique-local addresses are only accepted when
/// `allow_private` is set (development, where clients share the LAN).
pub fn is_allowed_remote(candidate: &Candidate, allow_private: bool) -> bool {
    use std::net::IpAddr;
    let addr = candidate.addr();
    if candidate.proto() != Protocol::Udp || addr.port() == 0 {
        return false;
    }
    let ip = addr.ip();
    if ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    let internal = match ip {
        IpAddr::V4(v4) => {
            if v4.is_broadcast() {
                return false;
            }
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.octets()[0] == 0
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                || (first & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (first & 0xffc0) == 0xfe80 // link local fe80::/10
                || v6.to_ipv4_mapped().is_some_and(|v4| {
                    v4.is_loopback() || v4.is_private() || v4.is_link_local()
                })
        }
    };
    allow_private || !internal
}

/// Parses a trickled candidate. Accepts the line with or without the
/// `candidate:` prefix and with an `a=` prefix.
pub fn parse_candidate(line: &str) -> Option<Candidate> {
    let line = line.trim().trim_start_matches("a=");
    Candidate::from_sdp_string(line)
        .or_else(|_| Candidate::from_sdp_string(line.trim_start_matches("candidate:")))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(line: &str) -> Candidate {
        parse_candidate(line).expect("valid candidate")
    }

    #[test]
    fn rejects_unsafe_candidates() {
        let public = candidate("candidate:1 1 udp 2122260223 203.0.113.7 50000 typ host");
        let private = candidate("candidate:1 1 udp 2122260223 192.168.1.20 50000 typ host");
        let loopback = candidate("candidate:1 1 udp 2122260223 127.0.0.1 50000 typ host");
        let metadata = candidate("candidate:1 1 udp 2122260223 169.254.169.254 80 typ host");
        let unique_local = candidate("candidate:1 1 udp 2122260223 fd00::1 50000 typ host");
        let tcp = candidate("candidate:1 1 tcp 1518280447 203.0.113.7 9 typ host tcptype active");

        assert!(is_allowed_remote(&public, false));
        for internal in [&private, &loopback, &metadata, &unique_local] {
            assert!(!is_allowed_remote(internal, false), "{internal:?}");
            assert!(is_allowed_remote(internal, true), "dev allows {internal:?}");
        }
        assert!(!is_allowed_remote(&tcp, true));
    }
}
