//! UDP media sockets and packet routing to rooms.
//!
//! All rooms share the same UDP port. Each room runs in its own task and owns
//! its `str0m::Rtc` instances, so incoming datagrams must be routed to the
//! right room before str0m can look at them:
//!
//! 1. A datagram from a known source address goes to the room that last
//!    accepted traffic from that address.
//! 2. Otherwise, if it is a STUN binding request, the `USERNAME` attribute is
//!    `<server ufrag>:<client ufrag>`. Rooms register the ufrag of every peer
//!    they create, so the ufrag identifies the room. The source address is
//!    remembered for the following DTLS/SRTP traffic.
//! 3. Anything else is dropped.
//!
//! Because a network switch (Wi-Fi to mobile) always starts with new STUN
//! checks from the new address, rule 2 also handles roaming.

use crate::metrics::Metrics;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{atomic::Ordering, Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{net::UdpSocket, sync::mpsc};

/// One received datagram, owned so it can cross task boundaries.
#[derive(Debug)]
pub struct Datagram {
    /// Sender address.
    pub source: SocketAddr,
    /// Local (advertised) address the datagram arrived on.
    pub destination: SocketAddr,
    /// Payload.
    pub data: Vec<u8>,
    /// Receive time.
    pub received: Instant,
}

/// Queue of datagrams for one room.
pub type RoomPackets = mpsc::Sender<Datagram>;

/// Source addresses are forgotten after this much silence.
const ROUTE_IDLE: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Routes {
    by_ufrag: HashMap<String, RoomPackets>,
    by_addr: HashMap<SocketAddr, (RoomPackets, Instant)>,
}

/// Outgoing datagrams for one socket's sender thread.
type SendQueue = std::sync::mpsc::SyncSender<(SocketAddr, Vec<u8>)>;

/// Datagrams waiting for the sender thread. When full, new datagrams are
/// dropped: the receivers recover through NACK and keyframe requests.
const SEND_QUEUE: usize = 16 * 1024;

/// Shared set of media sockets.
///
/// Sending happens on one dedicated OS thread per socket. Profiling a 25
/// person room showed ~40% of the room task in the `sendto` syscall; moving
/// it off the room task lets forwarding and sending run on separate cores.
#[derive(Clone)]
pub struct MediaSockets {
    sockets: Arc<Vec<(SocketAddr, SendQueue)>>,
    routes: Arc<Mutex<Routes>>,
    metrics: Arc<Metrics>,
}

impl MediaSockets {
    /// Binds one socket per address and starts receive loops.
    ///
    /// Each entry is the local bind address and an optional public IP to
    /// announce instead of the bind IP (1:1 NAT). Port 0 picks a free port,
    /// which tests use.
    pub async fn bind(
        addresses: &[(SocketAddr, Option<IpAddr>)],
        metrics: Arc<Metrics>,
    ) -> anyhow::Result<Self> {
        let mut sockets = Vec::new();
        let mut receivers = Vec::new();
        for (bind, advertise_ip) in addresses {
            let std_socket = std::net::UdpSocket::bind(bind)
                .map_err(|e| anyhow::anyhow!("cannot bind media socket {bind}: {e}"))?;
            std_socket.set_nonblocking(true)?;
            let local = std_socket.local_addr()?;
            let advertised = SocketAddr::new(advertise_ip.unwrap_or(local.ip()), local.port());
            tracing::info!(%local, %advertised, "media socket bound");
            let (queue, outgoing) = std::sync::mpsc::sync_channel(SEND_QUEUE);
            let sender = std_socket.try_clone()?;
            let send_metrics = metrics.clone();
            std::thread::Builder::new()
                .name(format!("udp-send-{}", local.port()))
                .spawn(move || send_loop(sender, outgoing, send_metrics))?;
            sockets.push((advertised, queue));
            receivers.push((advertised, UdpSocket::from_std(std_socket)?));
        }
        let this = Self {
            sockets: Arc::new(sockets),
            routes: Arc::default(),
            metrics,
        };
        for (advertised, socket) in receivers {
            tokio::spawn(this.clone().receive_loop(advertised, socket));
        }
        tokio::spawn(this.clone().prune_loop());
        Ok(this)
    }

    /// Addresses announced to clients as ICE host candidates.
    pub fn advertised(&self) -> Vec<SocketAddr> {
        self.sockets.iter().map(|(addr, _)| *addr).collect()
    }

    /// Routes STUN traffic for `ufrag` to `room`.
    pub fn register_ufrag(&self, ufrag: &str, room: RoomPackets) {
        self.routes
            .lock()
            .expect("routes lock")
            .by_ufrag
            .insert(ufrag.to_owned(), room);
    }

    /// Stops routing STUN traffic for `ufrag`.
    pub fn unregister_ufrag(&self, ufrag: &str) {
        self.routes
            .lock()
            .expect("routes lock")
            .by_ufrag
            .remove(ufrag);
    }

    /// Queues a datagram on the socket advertised as `source`.
    ///
    /// Never blocks the room task. If the queue or the kernel buffer is full
    /// the datagram is dropped, which the receiver treats like ordinary
    /// packet loss (NACK, keyframe request, Opus FEC).
    pub fn send(&self, source: SocketAddr, destination: SocketAddr, data: &[u8]) {
        let Some((_, queue)) = self.sockets.iter().find(|(addr, _)| *addr == source) else {
            return;
        };
        if queue.try_send((destination, data.to_vec())).is_err() {
            self.metrics
                .udp_dropped_total
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    async fn receive_loop(self, advertised: SocketAddr, socket: UdpSocket) {
        let mut buf = vec![0u8; 2048];
        loop {
            let (len, source) = match socket.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(e) => {
                    // ICMP "port unreachable" from a vanished client shows up
                    // here on some platforms. It is not fatal for the socket.
                    tracing::debug!(error = %e, "media socket receive error");
                    continue;
                }
            };
            let data = &buf[..len];
            let Some(room) = self.route(source, data) else {
                self.metrics
                    .udp_dropped_total
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let datagram = Datagram {
                source,
                destination: advertised,
                data: data.to_vec(),
                received: Instant::now(),
            };
            if room.try_send(datagram).is_err() {
                self.metrics
                    .udp_dropped_total
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn route(&self, source: SocketAddr, data: &[u8]) -> Option<RoomPackets> {
        let mut routes = self.routes.lock().expect("routes lock");
        let now = Instant::now();
        // STUN is checked first so that a client whose address was reused by
        // another peer (rare, but possible behind NAT) is re-routed correctly.
        if let Some(ufrag) = stun_local_ufrag(data) {
            if let Some(room) = routes.by_ufrag.get(ufrag).cloned() {
                routes.by_addr.insert(source, (room.clone(), now));
                return Some(room);
            }
        }
        let (room, last_seen) = routes.by_addr.get_mut(&source)?;
        *last_seen = now;
        Some(room.clone())
    }

    async fn prune_loop(self) {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            let now = Instant::now();
            let mut routes = self.routes.lock().expect("routes lock");
            routes
                .by_addr
                .retain(|_, (room, seen)| !room.is_closed() && now - *seen < ROUTE_IDLE);
            routes.by_ufrag.retain(|_, room| !room.is_closed());
        }
    }
}

/// Sender thread body. The socket is non-blocking (shared with the tokio
/// receiver); on a full kernel buffer it waits briefly and retries once.
fn send_loop(
    socket: std::net::UdpSocket,
    outgoing: std::sync::mpsc::Receiver<(SocketAddr, Vec<u8>)>,
    metrics: Arc<Metrics>,
) {
    while let Ok((destination, data)) = outgoing.recv() {
        let mut sent = socket.send_to(&data, destination);
        if matches!(&sent, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock) {
            std::thread::sleep(Duration::from_micros(200));
            sent = socket.send_to(&data, destination);
        }
        if sent.is_err() {
            metrics.udp_dropped_total.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Returns the server ufrag of a STUN binding request.
///
/// STUN header: type (2), length (2), magic cookie 0x2112A442 (4),
/// transaction id (12), then TLV attributes padded to 4 bytes.
/// `USERNAME` (0x0006) is `<receiver ufrag>:<sender ufrag>`.
pub fn stun_local_ufrag(data: &[u8]) -> Option<&str> {
    const MAGIC: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];
    if data.len() < 20 || data[0] & 0xC0 != 0 || data[4..8] != MAGIC {
        return None;
    }
    let mut pos = 20;
    while pos + 4 <= data.len() {
        let kind = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let len = usize::from(u16::from_be_bytes([data[pos + 2], data[pos + 3]]));
        let value = data.get(pos + 4..pos + 4 + len)?;
        if kind == 0x0006 {
            let username = std::str::from_utf8(value).ok()?;
            return username.split(':').next();
        }
        pos += 4 + len.div_ceil(4) * 4;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stun_with_username(username: &str) -> Vec<u8> {
        let mut attr = Vec::new();
        attr.extend_from_slice(&0x0006u16.to_be_bytes());
        attr.extend_from_slice(&(username.len() as u16).to_be_bytes());
        attr.extend_from_slice(username.as_bytes());
        while attr.len() % 4 != 0 {
            attr.push(0);
        }
        let mut msg = vec![0x00, 0x01];
        msg.extend_from_slice(&(attr.len() as u16).to_be_bytes());
        msg.extend_from_slice(&[0x21, 0x12, 0xA4, 0x42]);
        msg.extend_from_slice(&[7u8; 12]);
        msg.extend_from_slice(&attr);
        msg
    }

    #[test]
    fn extracts_server_ufrag() {
        assert_eq!(
            stun_local_ufrag(&stun_with_username("srv1:cli9")),
            Some("srv1")
        );
    }

    #[test]
    fn ignores_non_stun_and_truncated() {
        assert_eq!(stun_local_ufrag(&[0x80; 40]), None);
        let mut msg = stun_with_username("abc:def");
        msg.truncate(26);
        assert_eq!(stun_local_ufrag(&msg), None);
    }
}
