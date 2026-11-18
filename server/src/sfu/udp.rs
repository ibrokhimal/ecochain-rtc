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

use crate::metrics::{Metrics, UdpDrop};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    sync::{mpsc, watch},
};

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

/// One bound media socket.
struct SocketEntry {
    /// Address announced to clients (bind address unless 1:1 NAT).
    advertised: SocketAddr,
    /// Queue of the socket's sender thread. Dropping it ends the thread.
    queue: SendQueue,
    /// Follows the machine's primary address (see [`MediaSockets::follow_primary`]).
    auto: bool,
    /// Receive task, aborted when the socket is replaced.
    receiver: tokio::task::AbortHandle,
}

/// Shared set of media sockets.
///
/// Sending happens on one dedicated OS thread per socket. Profiling a 25
/// person room showed ~40% of the room task in the `sendto` syscall; moving
/// it off the room task lets forwarding and sending run on separate cores.
///
/// The set can change at runtime: when the machine moves to another network,
/// the automatically chosen socket is replaced by one on the new address and
/// [`MediaSockets::subscribe`] tells the rooms to update their candidates.
#[derive(Clone)]
pub struct MediaSockets {
    sockets: Arc<RwLock<Vec<SocketEntry>>>,
    routes: Arc<Mutex<Routes>>,
    metrics: Arc<Metrics>,
    changes: Arc<watch::Sender<u64>>,
}

/// How often the primary address is checked when it is followed.
pub const PRIMARY_CHECK_INTERVAL: Duration = Duration::from_secs(3);

impl MediaSockets {
    /// Binds one socket per address and starts receive loops.
    ///
    /// Each entry is the local bind address, an optional public IP to
    /// announce instead of the bind IP (1:1 NAT) and whether the address was
    /// chosen automatically. Port 0 picks a free port, which tests use.
    pub async fn bind(
        addresses: &[(SocketAddr, Option<IpAddr>, bool)],
        metrics: Arc<Metrics>,
    ) -> anyhow::Result<Self> {
        let this = Self {
            sockets: Arc::default(),
            routes: Arc::default(),
            metrics,
            changes: Arc::new(watch::channel(0).0),
        };
        for (bind, advertise_ip, auto) in addresses {
            let entry = this.open(*bind, *advertise_ip, *auto)?;
            this.sockets.write().expect("sockets lock").push(entry);
        }
        tokio::spawn(this.clone().prune_loop());
        Ok(this)
    }

    fn open(
        &self,
        bind: SocketAddr,
        advertise_ip: Option<IpAddr>,
        auto: bool,
    ) -> anyhow::Result<SocketEntry> {
        let std_socket = std::net::UdpSocket::bind(bind)
            .map_err(|e| anyhow::anyhow!("cannot bind media socket {bind}: {e}"))?;
        std_socket.set_nonblocking(true)?;
        // Wi-Fi links stall for tens of milliseconds (power save,
        // retries). The default ~200 KB send buffer then fills and every
        // further datagram fails with EAGAIN; a larger one rides it out.
        // The kernel caps the value at net.core.{w,r}mem_max.
        let sock = socket2::SockRef::from(&std_socket);
        let _ = sock.set_send_buffer_size(SOCKET_BUFFER);
        let _ = sock.set_recv_buffer_size(SOCKET_BUFFER);
        let local = std_socket.local_addr()?;
        let advertised = SocketAddr::new(advertise_ip.unwrap_or(local.ip()), local.port());
        tracing::info!(
            %local,
            %advertised,
            auto,
            send_buffer = sock.send_buffer_size().unwrap_or(0),
            recv_buffer = sock.recv_buffer_size().unwrap_or(0),
            "media socket bound"
        );
        let (queue, outgoing) = std::sync::mpsc::sync_channel(SEND_QUEUE);
        let sender = std_socket.try_clone()?;
        let send_metrics = self.metrics.clone();
        std::thread::Builder::new()
            .name(format!("udp-send-{}", local.port()))
            .spawn(move || send_loop(sender, outgoing, send_metrics))?;
        let socket = UdpSocket::from_std(std_socket)?;
        let receiver = tokio::spawn(self.clone().receive_loop(advertised, socket)).abort_handle();
        Ok(SocketEntry {
            advertised,
            queue,
            auto,
            receiver,
        })
    }

    /// Addresses announced to clients as ICE host candidates.
    pub fn advertised(&self) -> Vec<SocketAddr> {
        self.sockets
            .read()
            .expect("sockets lock")
            .iter()
            .map(|e| e.advertised)
            .collect()
    }

    /// Notified whenever [`MediaSockets::advertised`] changes.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// Moves the automatically chosen socket to `ip`, keeping its port.
    ///
    /// Returns whether anything changed. Explicitly configured sockets are
    /// never touched. The old socket is closed: its address is gone with
    /// the old network, and clients reach the new one after an ICE restart.
    pub fn move_primary(&self, ip: IpAddr) -> anyhow::Result<bool> {
        let (old, port) = {
            let sockets = self.sockets.read().expect("sockets lock");
            let Some(current) = sockets.iter().find(|e| e.auto) else {
                return Ok(false);
            };
            if current.advertised.ip() == ip || sockets.iter().any(|e| e.advertised.ip() == ip) {
                return Ok(false);
            }
            (current.advertised, current.advertised.port())
        };
        let entry = self.open(SocketAddr::new(ip, port), None, true)?;
        let new = entry.advertised;
        {
            let mut sockets = self.sockets.write().expect("sockets lock");
            sockets.retain(|e| {
                if e.auto {
                    e.receiver.abort();
                }
                !e.auto
            });
            sockets.insert(0, entry);
        }
        tracing::info!(%old, %new, "primary network address changed, media socket moved");
        self.changes.send_modify(|generation| *generation += 1);
        Ok(true)
    }

    /// Checks the primary address every [`PRIMARY_CHECK_INTERVAL`] and
    /// moves the automatic socket when it changes. `detect` is
    /// [`crate::config::primary_lan_ip`] outside tests. Without a network
    /// (`None`) the old socket is kept, so a short Wi-Fi drop to the same
    /// network needs no renegotiation.
    pub fn follow_primary(&self, detect: impl Fn() -> Option<IpAddr> + Send + 'static) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(PRIMARY_CHECK_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let Some(ip) = detect() else { continue };
                if let Err(e) = this.move_primary(ip) {
                    tracing::warn!(%ip, error = %e, "cannot move media socket to the new address");
                }
            }
        });
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
        // ICE pairs every local candidate with every remote one. A loopback
        // socket can never reach another host (EINVAL); skip those checks.
        if source.ip().is_loopback() && !destination.ip().is_loopback() {
            return;
        }
        let sockets = self.sockets.read().expect("sockets lock");
        // A socket that was just replaced: the peer learns the new address
        // with the next candidate update.
        let Some(entry) = sockets.iter().find(|e| e.advertised == source) else {
            return;
        };
        if entry.queue.try_send((destination, data.to_vec())).is_err() {
            self.metrics.udp_dropped(UdpDrop::SendQueueFull);
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
                self.metrics.udp_dropped(UdpDrop::Unrouted);
                continue;
            };
            let datagram = Datagram {
                source,
                destination: advertised,
                data: data.to_vec(),
                received: Instant::now(),
            };
            if room.try_send(datagram).is_err() {
                self.metrics.udp_dropped(UdpDrop::RoomQueueFull);
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

/// Requested kernel send and receive buffer of each media socket (4 MiB).
const SOCKET_BUFFER: usize = 4 << 20;

/// Sender thread body. The socket is non-blocking (shared with the tokio
/// receiver); on a full kernel buffer it waits briefly and retries once.
fn send_loop(
    socket: std::net::UdpSocket,
    outgoing: std::sync::mpsc::Receiver<(SocketAddr, Vec<u8>)>,
    metrics: Arc<Metrics>,
) {
    let local = socket.local_addr().ok();
    let mut failures: u64 = 0;
    while let Ok((destination, data)) = outgoing.recv() {
        let mut sent = socket.send_to(&data, destination);
        if matches!(&sent, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock) {
            std::thread::sleep(Duration::from_micros(200));
            sent = socket.send_to(&data, destination);
        }
        if let Err(e) = sent {
            metrics.udp_dropped(UdpDrop::SendError);
            // The first failure and then every 10 000th: enough to see the
            // cause without flooding the log. Addresses are not secrets.
            if failures.is_multiple_of(10_000) {
                tracing::warn!(error = %e, ?local, %destination, len = data.len(), failures, "udp send failed");
            }
            failures += 1;
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
