//! A bounded, bandwidth-shaped UDP path used only by real transport tests.
use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{net::UdpSocket, task::JoinHandle};

#[derive(Default)]
struct Routes {
    client: Option<SocketAddr>,
    server: Option<SocketAddr>,
}

#[derive(Debug)]
pub(crate) struct Link {
    addr: SocketAddr,
    capacity: Arc<AtomicU64>,
    routes: Arc<Mutex<Routes>>,
    task: JoinHandle<()>,
}

impl std::fmt::Debug for Routes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Routes").finish_non_exhaustive()
    }
}

impl Link {
    pub async fn new(bits_per_second: u64) -> Arc<Self> {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let capacity = Arc::new(AtomicU64::new(bits_per_second));
        let routes = Arc::new(Mutex::new(Routes::default()));
        let rate = capacity.clone();
        let peers = routes.clone();
        let task = tokio::spawn(async move {
            let mut buf = vec![0; 65536];
            let mut queue: VecDeque<(Instant, Vec<u8>, SocketAddr)> = VecDeque::new();
            let mut next = Instant::now();
            loop {
                let deadline = queue
                    .front()
                    .map_or_else(|| Instant::now() + Duration::from_secs(3600), |p| p.0);
                tokio::select! {
                    received = socket.recv_from(&mut buf) => {
                        let Ok((len, source)) = received else { break };
                        let (client, server) = { let r = peers.lock().unwrap(); (r.client, r.server) };
                        let (Some(client), Some(server)) = (client, server) else { continue };
                        if source == client {
                            let _ = socket.send_to(&buf[..len], server).await;
                        } else if source == server {
                            // ICE, DTLS and RTCP bypass the media bottleneck. Shape all
                            // SRTP, including empty padding/probes, using actual wire bytes.
                            let rtp = len >= 12 && buf[0] & 0xc0 == 0x80 && !(192..=223).contains(&buf[1]);
                            if !rtp { let _ = socket.send_to(&buf[..len], client).await; continue; }
                            let bps = rate.load(Ordering::Relaxed);
                            if bps == 0 { continue; }
                            let now = Instant::now();
                            let due = next.max(now) + Duration::from_secs_f64((len as f64 * 8.0) / bps as f64);
                            // Drop when a 150 ms bottleneck queue is full. Do not
                            // extend the serialization clock for a dropped packet.
                            if due <= now + Duration::from_millis(150) {
                                next = due;
                                queue.push_back((due, buf[..len].to_vec(), client));
                            }
                        }
                    }
                    _ = tokio::time::sleep_until(deadline.into()) => {
                        if let Some((_, data, client)) = queue.pop_front() { let _ = socket.send_to(&data, client).await; }
                    }
                }
            }
        });
        Arc::new(Self {
            addr,
            capacity,
            routes,
            task,
        })
    }

    pub fn set_capacity(&self, bits_per_second: u64) {
        self.capacity.store(bits_per_second, Ordering::Relaxed);
    }
    pub fn attach(&self, client: SocketAddr) -> SocketAddr {
        self.routes.lock().unwrap().client = Some(client);
        self.addr
    }
    pub fn destination(&self, server: SocketAddr) -> SocketAddr {
        self.routes.lock().unwrap().server = Some(server);
        self.addr
    }
    pub fn source(&self, source: SocketAddr) -> SocketAddr {
        if source == self.addr {
            self.routes.lock().unwrap().server.unwrap_or(source)
        } else {
            source
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.task.abort();
    }
}
