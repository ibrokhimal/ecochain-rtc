//! One server side WebRTC peer connection.
//!
//! Every participant has up to two peers, following the same split LiveKit
//! uses: a *publisher* peer where the client is the offerer and only sends,
//! and a *subscriber* peer where the server is the offerer and only sends.
//! Keeping the directions apart means offers never collide ("glare"), and
//! either side can be rebuilt without touching the other.

use super::udp::{Datagram, MediaSockets, RoomPackets};
use std::{net::SocketAddr, time::Instant};
use str0m::{
    bwe::Bitrate,
    format::Codec,
    net::{Protocol, Receive},
    Candidate, Event, IceCreds, Input, Output, Rtc, RtcConfig, RtcError,
};

/// Initial bandwidth estimate for subscriber peers, before TWCC feedback
/// arrives. Low enough to start with the medium layer on a phone.
const INITIAL_SUBSCRIBER_BITRATE: Bitrate =
    Bitrate::kbps(super::adaptation::INITIAL_SUBSCRIBER_KBPS);

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
    /// Media socket addresses added as host candidates.
    local: Vec<SocketAddr>,
}

/// Upper bound of trickled candidates per peer. A real client sends a
/// handful (one per interface and transport); the cap stops a client from
/// making the server probe an unbounded list of addresses.
const MAX_REMOTE_CANDIDATES: usize = 32;

fn rtc_config(kind: PeerKind, creds: IceCreds) -> RtcConfig {
    let mut config = Rtc::builder().set_local_ice_credentials(creds);
    if kind == PeerKind::Subscriber {
        // Only subscriber transports need bandwidth estimation.
        config = config.enable_bwe(Some(INITIAL_SUBSCRIBER_BITRATE));
    } else {
        // str0m answers in its own codec order, not the offered order.
        // Prefer native-friendly H264 but retain VP8 and every RTX mapping.
        // This only negotiates the publisher codec; no media is transcoded.
        config.codec_config().sort_by_key(|p| {
            let spec = p.spec();
            match spec.codec {
                codec if codec.is_audio() => 0,
                Codec::H264 if spec.format.packetization_mode == Some(1) => {
                    if spec.format.profile_level_id.map(|id| id >> 8) == Some(0x42e0) {
                        1
                    } else {
                        2
                    }
                }
                Codec::Vp8 => 3,
                _ => 4,
            }
        });
    }
    config
}

impl Peer {
    /// Creates a peer with host candidates for every media socket and
    /// registers its ICE ufrag so that STUN traffic reaches `room`.
    pub fn new(kind: PeerKind, sockets: &MediaSockets, room: &RoomPackets, now: Instant) -> Self {
        let creds = IceCreds::new();
        let rtc = rtc_config(kind, creds.clone()).build(now);
        sockets.register_ufrag(&creds.ufrag, room.clone());
        let mut peer = Self {
            rtc,
            kind,
            ufrag: creds.ufrag,
            next_timeout: now,
            remote_candidates: 0,
            local: Vec::new(),
        };
        peer.sync_local_candidates(sockets);
        peer
    }

    /// Makes the host candidates match the current media sockets: new
    /// addresses are added, vanished ones invalidated (their candidate
    /// pairs stop). Returns whether anything changed.
    ///
    /// The client learns new candidates from the next SDP of this peer,
    /// which follows the ICE restart it starts when the old path fails.
    pub fn sync_local_candidates(&mut self, sockets: &MediaSockets) -> bool {
        let current = sockets.advertised();
        let mut changed = false;
        for addr in self.local.iter().filter(|a| !current.contains(a)) {
            if let Ok(candidate) = Candidate::host(*addr, "udp") {
                self.rtc.direct_api().invalidate_candidate(&candidate);
                changed = true;
            }
        }
        for addr in current.iter().filter(|a| !self.local.contains(a)) {
            match Candidate::host(*addr, "udp") {
                Ok(candidate) => {
                    self.rtc.add_local_candidate(candidate);
                    changed = true;
                }
                Err(e) => tracing::warn!(%addr, error = ?e, "invalid host candidate"),
            }
        }
        self.local = current;
        if changed {
            self.wake(Instant::now());
        }
        changed
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
/// unspecified, multicast and broadcast addresses. Addresses that are not
/// globally routable (loopback, private, link-local, carrier-grade NAT,
/// benchmarking, reserved, IPv6 unique-local and site-local) are only
/// accepted when `allow_private` is set (development on a LAN).
///
/// IPv6 forms that embed an IPv4 address (mapped, compatible, NAT64
/// `64:ff9b::/96`, 6to4 `2002::/16`) are judged by the embedded address,
/// so they cannot be used to smuggle an internal IPv4 target.
pub fn is_allowed_remote(candidate: &Candidate, allow_private: bool) -> bool {
    let addr = candidate.addr();
    if candidate.proto() != Protocol::Udp || addr.port() == 0 {
        return false;
    }
    match classify(addr.ip()) {
        AddressClass::Never => false,
        AddressClass::Internal => allow_private,
        AddressClass::Global => true,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum AddressClass {
    /// Never a valid unicast peer.
    Never,
    /// Not globally routable.
    Internal,
    /// Globally routable unicast.
    Global,
}

fn classify(ip: std::net::IpAddr) -> AddressClass {
    use std::net::{IpAddr, Ipv4Addr};
    let v4 = match ip {
        IpAddr::V4(v4) => v4,
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            let octets = v6.octets();
            let embedded = if let Some(v4) = v6.to_ipv4_mapped() {
                Some(v4)
            } else if seg[..6] == [0; 6] && !v6.is_loopback() && !v6.is_unspecified() {
                // IPv4-compatible ::a.b.c.d (deprecated, still parsed).
                Some(Ipv4Addr::new(
                    octets[12], octets[13], octets[14], octets[15],
                ))
            } else if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                // NAT64 well-known prefix.
                Some(Ipv4Addr::new(
                    octets[12], octets[13], octets[14], octets[15],
                ))
            } else if seg[0] == 0x2002 {
                // 6to4: 2002:aabb:ccdd::/48.
                Some(Ipv4Addr::new(octets[2], octets[3], octets[4], octets[5]))
            } else {
                None
            };
            if let Some(v4) = embedded {
                v4
            } else {
                if v6.is_unspecified() || v6.is_multicast() {
                    return AddressClass::Never;
                }
                let internal = v6.is_loopback()
                    || (seg[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                    || (seg[0] & 0xffc0) == 0xfe80 // link local fe80::/10
                    || (seg[0] & 0xffc0) == 0xfec0 // site local fec0::/10 (deprecated)
                    || (seg[0] & 0xe000) != 0x2000; // outside global unicast 2000::/3
                return if internal {
                    AddressClass::Internal
                } else {
                    AddressClass::Global
                };
            }
        }
    };
    let [a, b, ..] = v4.octets();
    if v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast() {
        return AddressClass::Never;
    }
    let internal = v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || a == 0 // "this network" 0.0.0.0/8
        || (a == 100 && (64..128).contains(&b)) // carrier-grade NAT 100.64.0.0/10
        || (a == 192 && b == 0 && v4.octets()[2] == 0) // IETF protocol assignments 192.0.0.0/24
        || (a == 198 && (b == 18 || b == 19)) // benchmarking 198.18.0.0/15
        || a >= 240; // reserved 240.0.0.0/4
    if internal {
        AddressClass::Internal
    } else {
        AddressClass::Global
    }
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
    use str0m::media::{Direction, MediaKind};

    fn publisher_answer(vp8_only: bool) -> String {
        crate::install_crypto();
        let now = Instant::now();
        let mut client_config = Rtc::builder();
        if vp8_only {
            client_config.codec_config().clear();
            client_config.codec_config().enable_vp8(true);
        }
        let mut client = client_config.build(now);
        let mut change = client.sdp_api();
        change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        let (offer, _) = change.apply().unwrap();
        let mut server = rtc_config(PeerKind::Publisher, IceCreds::new()).build(now);
        server
            .sdp_api()
            .accept_offer(offer)
            .unwrap()
            .to_sdp_string()
    }

    fn first_video_codec(sdp: &str) -> &str {
        let line = sdp.lines().find(|l| l.starts_with("m=video ")).unwrap();
        let pt = line.split_whitespace().nth(3).unwrap();
        let prefix = format!("a=rtpmap:{pt} ");
        sdp.lines().find_map(|l| l.strip_prefix(&prefix)).unwrap()
    }

    #[test]
    fn publisher_answer_prefers_h264_and_keeps_vp8_and_rtx() {
        let answer = publisher_answer(false);
        assert_eq!(first_video_codec(&answer), "H264/90000");
        assert!(answer.contains(" VP8/90000"));
        let pt = answer
            .lines()
            .find(|l| l.starts_with("m=video "))
            .unwrap()
            .split_whitespace()
            .nth(3)
            .unwrap();
        let prefix = format!("a=fmtp:{pt} ");
        let fmtp = answer
            .lines()
            .find_map(|l| l.strip_prefix(&prefix))
            .unwrap();
        assert!(fmtp.contains("profile-level-id=42e01f"));
        assert!(fmtp.contains("packetization-mode=1"));
        let apt = format!("apt={pt}");
        let rtx_line = answer
            .lines()
            .find(|l| l.starts_with("a=fmtp:") && l.split_whitespace().skip(1).any(|p| p == apt))
            .unwrap();
        let rtx = rtx_line
            .strip_prefix("a=fmtp:")
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        assert!(answer.contains(&format!("a=rtpmap:{rtx} rtx/90000")));
    }

    #[test]
    fn publisher_answer_accepts_vp8_only_devices() {
        let answer = publisher_answer(true);
        assert_eq!(first_video_codec(&answer), "VP8/90000");
        assert!(!answer.contains(" H264/90000"));
    }

    fn candidate(line: &str) -> Candidate {
        parse_candidate(line).expect("valid candidate")
    }

    #[test]
    fn rejects_unsafe_candidates() {
        let public = candidate("candidate:1 1 udp 2122260223 1.1.1.1 50000 typ host");
        let private = candidate("candidate:1 1 udp 2122260223 192.168.1.20 50000 typ host");
        let loopback = candidate("candidate:1 1 udp 2122260223 127.0.0.1 50000 typ host");
        let metadata = candidate("candidate:1 1 udp 2122260223 169.254.169.254 80 typ host");
        let unique_local = candidate("candidate:1 1 udp 2122260223 fd00::1 50000 typ host");
        let tcp = candidate("candidate:1 1 tcp 1518280447 1.1.1.1 9 typ host tcptype active");

        assert!(is_allowed_remote(&public, false));
        for internal in [&private, &loopback, &metadata, &unique_local] {
            assert!(!is_allowed_remote(internal, false), "{internal:?}");
            assert!(is_allowed_remote(internal, true), "dev allows {internal:?}");
        }
        assert!(!is_allowed_remote(&tcp, true));
    }

    #[test]
    fn classifies_embedded_and_special_ranges() {
        use std::net::IpAddr;
        let class = |s: &str| classify(s.parse::<IpAddr>().unwrap());
        for internal in [
            "::ffff:127.0.0.1",   // mapped
            "::10.0.0.1",         // compatible
            "64:ff9b::a9fe:a9fe", // NAT64 of 169.254.169.254
            "2002:c0a8:0101::1",  // 6to4 of 192.168.1.1
            "fec0::1",            // site local
            "100.64.0.1",         // CGNAT
            "198.18.0.1",         // benchmarking
            "240.0.0.1",          // reserved
            "192.0.0.8",
        ] {
            assert_eq!(class(internal), AddressClass::Internal, "{internal}");
        }
        for never in [
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "::",
            "ff02::1",
            "::ffff:224.0.0.1",
        ] {
            assert_eq!(class(never), AddressClass::Never, "{never}");
        }
        for global in [
            "1.1.1.1",
            "2606:4700:4700::1111",
            "64:ff9b::808:808",
            "2002:0808:0808::1",
        ] {
            assert_eq!(class(global), AddressClass::Global, "{global}");
        }
    }
}
