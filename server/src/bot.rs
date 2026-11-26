//! Headless test participant.
//!
//! The bot speaks the real signaling protocol and runs real WebRTC peers
//! (str0m as the client side), so integration tests and the load generator
//! exercise exactly the path a Flutter client uses: WebSocket, ICE, DTLS-SRTP
//! and SFU forwarding. It sends synthetic Opus and VP8 frames; the SFU does
//! not decode media, so the payload content does not matter, only its size
//! and keyframe markers.

use crate::protocol::{ErrorCode, ProtocolError};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use str0m::{
    change::{SdpAnswer, SdpOffer, SdpPendingOffer},
    format::Codec,
    media::{Direction, MediaKind, MediaTime, Mid, Rid, Simulcast, SimulcastLayer},
    net::{Protocol, Receive},
    Candidate, Event, Input, Output, Rtc,
};
use tokio::{
    net::{TcpStream, UdpSocket},
    sync::mpsc,
};
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

/// Error of a bot or signaling operation.
#[derive(Debug, thiserror::Error)]
pub enum BotError {
    /// WebSocket transport failed or closed.
    #[error("signaling closed: {0}")]
    Closed(String),
    /// The server answered with an error.
    #[error("server error {:?}: {}", .0.code, .0.message)]
    Server(ProtocolError),
    /// Nothing arrived in time.
    #[error("timeout waiting for {0}")]
    Timeout(&'static str),
    /// Local WebRTC failure.
    #[error("webrtc: {0}")]
    Rtc(String),
}

impl BotError {
    /// Server error code, if this is a server error.
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            BotError::Server(e) => Some(e.code),
            _ => None,
        }
    }
}

/// Minimal signaling client: request/response matching plus an event queue.
pub struct SignalClient {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    events: VecDeque<Value>,
    /// Close code received from the server, if any.
    pub close_code: Option<u16>,
}

impl SignalClient {
    /// Opens a WebSocket to `url` (for example `ws://127.0.0.1:7880/v1/rtc`).
    pub async fn connect(url: &str) -> Result<Self, BotError> {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| BotError::Closed(e.to_string()))?;
        Ok(Self {
            ws,
            next_id: 1,
            events: VecDeque::new(),
            close_code: None,
        })
    }

    /// Sends a raw text frame (used by tests for malformed input).
    pub async fn send_raw(&mut self, text: &str) -> Result<(), BotError> {
        self.ws
            .send(Message::Text(text.to_owned().into()))
            .await
            .map_err(|e| BotError::Closed(e.to_string()))
    }

    /// Sends a request with an explicit id and waits for its response.
    pub async fn request_with_id(
        &mut self,
        id: &str,
        kind: &str,
        data: Value,
    ) -> Result<Value, BotError> {
        let message = json!({"v": 1, "type": kind, "id": id, "data": data});
        self.send_raw(&message.to_string()).await?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let frame = self.read(deadline, "response").await?;
            if frame["type"] == "response" && frame["id"] == id {
                if frame["ok"] == true {
                    return Ok(frame["data"].clone());
                }
                let error: ProtocolError = serde_json::from_value(frame["error"].clone())
                    .map_err(|e| BotError::Closed(format!("bad error payload: {e}")))?;
                return Err(BotError::Server(error));
            }
            self.events.push_back(frame);
        }
    }

    /// Sends a `ping` without waiting; its response is skipped later.
    pub async fn send_ping(&mut self) -> Result<(), BotError> {
        let id = format!("ping-{}", self.next_id);
        self.next_id += 1;
        self.send_raw(&json!({"v": 1, "type": "ping", "id": id}).to_string())
            .await
    }

    /// Sends a request with a fresh id and waits for its response.
    pub async fn request(&mut self, kind: &str, data: Value) -> Result<Value, BotError> {
        let id = format!("bot-{}", self.next_id);
        self.next_id += 1;
        self.request_with_id(&id, kind, data).await
    }

    /// Next server event of type `kind`, skipping others (they stay queued).
    pub async fn wait_event(&mut self, kind: &str, timeout: Duration) -> Result<Value, BotError> {
        if let Some(pos) = self.events.iter().position(|e| e["type"] == kind) {
            return Ok(self.events.remove(pos).expect("position exists"));
        }
        let deadline = Instant::now() + timeout;
        loop {
            let frame = self.read(deadline, "event").await?;
            if frame["type"] == kind {
                return Ok(frame);
            }
            self.events.push_back(frame);
        }
    }

    /// Next frame of any kind (queued events first).
    pub async fn next_frame(&mut self, timeout: Duration) -> Result<Value, BotError> {
        if let Some(frame) = self.events.pop_front() {
            return Ok(frame);
        }
        self.read(Instant::now() + timeout, "frame").await
    }

    /// Waits until the server closes the socket and returns the close code.
    pub async fn wait_closed(&mut self, timeout: Duration) -> Option<u16> {
        let deadline = Instant::now() + timeout;
        while self.close_code.is_none() {
            match self.read(deadline, "close").await {
                Ok(frame) => self.events.push_back(frame),
                Err(_) => break,
            }
        }
        self.close_code
    }

    async fn read(&mut self, deadline: Instant, what: &'static str) -> Result<Value, BotError> {
        loop {
            let next = tokio::time::timeout_at(deadline.into(), self.ws.next())
                .await
                .map_err(|_| BotError::Timeout(what))?;
            match next {
                Some(Ok(Message::Text(text))) => {
                    return serde_json::from_str(&text).map_err(|e| BotError::Closed(e.to_string()))
                }
                Some(Ok(Message::Close(frame))) => {
                    self.close_code = Some(frame.map_or(1005, |f| f.code.into()));
                    return Err(BotError::Closed("closed by server".into()));
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(BotError::Closed(e.to_string())),
                None => return Err(BotError::Closed("stream ended".into())),
            }
        }
    }

    /// Closes the socket abruptly (no `leave`), like a lost connection.
    pub async fn drop_connection(mut self) {
        let _ = self.ws.close(None).await;
    }
}

#[cfg(test)]
pub(crate) mod test_transport;

/// What the bot publishes and receives.
#[derive(Debug, Clone)]
pub struct BotOptions {
    /// Signaling URL.
    pub url: String,
    /// Room token.
    pub token: String,
    /// Local IP for the UDP socket.
    pub bind_ip: IpAddr,
    /// Publish a synthetic microphone track.
    pub audio: bool,
    /// Publish a synthetic camera track at this bitrate (kbps), if set.
    pub video_kbps: Option<u64>,
    /// Publish the camera with three simulcast layers (`q`, `h`, `f`).
    pub simulcast: bool,
    /// Subscribe to at most this many remote video tracks.
    pub max_video: usize,
    /// Audio level (dBov, negative) written into the RTP audio-level header.
    pub audio_level: i8,
    /// Negotiate the microphone but send no packets and tell the server the
    /// track is muted, like the SDK does after `setMicrophoneEnabled(false)`.
    pub audio_muted: bool,
    /// The first this many video subscriptions use `"high"`, the rest
    /// `"low"`. `usize::MAX` subscribes everything `"high"`.
    pub high_video: usize,
}

/// Counters the bot updates while it runs.
#[derive(Debug, Default, Clone)]
pub struct BotStats {
    /// Participant id assigned by the server.
    pub participant_id: Option<String>,
    /// Audio frames received from the SFU.
    pub audio_frames: u64,
    /// Video frames received from the SFU.
    pub video_frames: u64,
    /// Video keyframes received.
    pub video_keyframes: u64,
    /// Recent video frame ordinals and keyframe flags for loss-recovery tests.
    #[cfg(test)]
    pub video_frame_events: VecDeque<(u64, bool)>,
    /// Acknowledges that the test bot is actually dropping network packets.
    #[cfg(test)]
    pub outage_active: bool,
    /// Media bytes received.
    pub bytes: u64,
    /// When the last audio frame arrived.
    pub last_audio: Option<Instant>,
    /// When the last video frame arrived.
    pub last_video: Option<Instant>,
    /// Signaling reconnects that resumed the session.
    pub resumes: u64,
    /// Events seen, by type (for assertions).
    pub events: Vec<String>,
    /// Remote participants by id with their connected flag.
    pub remote_participants: Vec<(String, bool)>,
    /// Audio frames received, by the participant that published them.
    pub audio_frames_by_participant: HashMap<String, u64>,
    /// Video frames received, by the participant that published them.
    pub video_frames_by_participant: HashMap<String, u64>,
    /// Synthetic video frames by (publisher, layer), for SFU diagnostics.
    pub video_frames_by_layer: HashMap<(String, usize), u64>,
    /// Tracks of remote participants: `(participant, track, kind)`.
    pub remote_tracks: Vec<(String, String, String)>,
    /// Video track ids this bot is currently subscribed to.
    pub subscribed_video: Vec<String>,
    /// `KeyframeRequest` events received by the publisher peer.
    pub keyframe_requests: u64,
    /// Set when the bot stopped, with the reason.
    pub stopped: Option<String>,
}

enum Control {
    Leave,
    Outage(Duration),
    SwitchNetwork,
    DropSignaling,
    SubscribeVideo(String, &'static str),
    UnsubscribeVideo(String),
    SetAudioMuted(bool),
    EnableAudio,
}

/// Handle to a running bot.
pub struct Bot {
    control: mpsc::UnboundedSender<Control>,
    stats: Arc<Mutex<BotStats>>,
    task: tokio::task::JoinHandle<()>,
}

impl Bot {
    /// Joins the room and starts publishing. Returns after the join response.
    pub async fn start(options: BotOptions) -> Result<Bot, BotError> {
        let stats = Arc::new(Mutex::new(BotStats::default()));
        let (control, control_rx) = mpsc::unbounded_channel();
        let mut runner = Runner::connect(options, stats.clone()).await?;
        let task = tokio::spawn(async move {
            let reason = match runner.run(control_rx).await {
                Ok(reason) => reason,
                Err(e) => e.to_string(),
            };
            runner.stats.lock().expect("stats").stopped = Some(reason);
        });
        Ok(Bot {
            control,
            stats,
            task,
        })
    }

    /// Same bot over a test-only bidirectional relay with a shaped downlink.
    #[cfg(test)]
    pub(crate) async fn start_shaped(
        options: BotOptions,
        link: Arc<test_transport::Link>,
    ) -> Result<Bot, BotError> {
        let stats = Arc::new(Mutex::new(BotStats::default()));
        let (control, control_rx) = mpsc::unbounded_channel();
        let mut runner = Runner::connect(options, stats.clone()).await?;
        runner.sub_local = link.attach(runner.sub_local);
        runner.test_link = Some(link);
        let task = tokio::spawn(async move {
            let reason = match runner.run(control_rx).await {
                Ok(reason) => reason,
                Err(e) => e.to_string(),
            };
            runner.stats.lock().expect("stats").stopped = Some(reason);
        });
        Ok(Bot {
            control,
            stats,
            task,
        })
    }

    /// Snapshot of the counters.
    pub fn stats(&self) -> BotStats {
        self.stats.lock().expect("stats").clone()
    }

    /// Simulates a total network outage of `duration`: every UDP datagram
    /// and WebSocket frame in both directions is lost.
    pub fn outage(&self, duration: Duration) {
        let _ = self.control.send(Control::Outage(duration));
    }

    /// Simulates moving to another network: new local address, ICE restart
    /// on both peers.
    pub fn switch_network(&self) {
        let _ = self.control.send(Control::SwitchNetwork);
    }

    /// Drops the WebSocket without `leave`; the bot reconnects and resumes.
    pub fn drop_signaling(&self) {
        let _ = self.control.send(Control::DropSignaling);
    }

    /// Subscribes to a remote video track with the given quality
    /// (`"low" | "medium" | "high"`).
    pub fn subscribe_video(&self, track_id: String, quality: &'static str) {
        let _ = self
            .control
            .send(Control::SubscribeVideo(track_id, quality));
    }

    /// Drops the subscription to a remote video track.
    pub fn unsubscribe_video(&self, track_id: String) {
        let _ = self.control.send(Control::UnsubscribeVideo(track_id));
    }

    /// Mutes or unmutes the microphone: stops or resumes sending packets and
    /// sends `track.mute`.
    pub fn set_audio_muted(&self, muted: bool) {
        let _ = self.control.send(Control::SetAudioMuted(muted));
    }

    /// Adds the microphone track to a bot that started without one and
    /// renegotiates the publisher (the "first unmute").
    pub fn enable_audio(&self) {
        let _ = self.control.send(Control::EnableAudio);
    }

    /// Leaves the room and waits for the task to finish.
    pub async fn leave(self) {
        let _ = self.control.send(Control::Leave);
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }

    /// Waits until `predicate` holds for the stats or `timeout` passes.
    pub async fn wait_for(&self, timeout: Duration, predicate: impl Fn(&BotStats) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate(&self.stats()) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        predicate(&self.stats())
    }
}

struct Publisher {
    rtc: Rtc,
    socket: UdpSocket,
    local: SocketAddr,
    audio: Option<Mid>,
    video: Option<Mid>,
    pending: Option<SdpPendingOffer>,
    audio_time: u64,
    video_time: [u64; 3],
    keyframe_needed: bool,
}

struct Runner {
    #[cfg(test)]
    test_link: Option<Arc<test_transport::Link>>,
    options: BotOptions,
    stats: Arc<Mutex<BotStats>>,
    signal: SignalClient,
    session_id: String,
    publisher: Publisher,
    /// Like libwebrtc, each peer connection has its own UDP socket. The
    /// server tells peers apart by remote address after ICE.
    subscriber: Option<Rtc>,
    sub_socket: UdpSocket,
    sub_local: SocketAddr,
    outage_until: Option<Instant>,
    subscribed_video: Vec<String>,
    audio_muted: bool,
    /// Subscriber mid to the participant that publishes it.
    mid_participant: HashMap<Mid, String>,
}

const LAYERS: [&str; 3] = ["q", "h", "f"];

impl Runner {
    async fn connect(options: BotOptions, stats: Arc<Mutex<BotStats>>) -> Result<Self, BotError> {
        let mut signal = SignalClient::connect(&options.url).await?;
        let joined = signal
            .request(
                "join",
                json!({"token": options.token, "client": {"sdk": "bot", "platform": "test"}}),
            )
            .await?;
        let session_id = joined["session_id"].as_str().unwrap_or_default().to_owned();
        stats.lock().expect("stats").participant_id = joined["participant"]["participant_id"]
            .as_str()
            .map(str::to_owned);
        let (sub_socket, sub_local) = bind(options.bind_ip).await?;
        let audio_muted = options.audio_muted;
        let mut runner = Self {
            #[cfg(test)]
            test_link: None,
            publisher: new_publisher(options.bind_ip).await?,
            options,
            stats,
            signal,
            session_id,
            sub_socket,
            sub_local,
            subscriber: None,
            outage_until: None,
            subscribed_video: Vec::new(),
            audio_muted,
            mid_participant: HashMap::new(),
        };
        runner.record_participants(&joined["participants"]);
        runner.publish(false, false).await?;
        for participant in joined["participants"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            runner.subscribe_videos(&participant).await?;
        }
        Ok(runner)
    }

    /// Sends a publisher offer with the configured tracks.
    async fn publish(&mut self, ice_restart: bool, reset: bool) -> Result<(), BotError> {
        let mut change = self.publisher.rtc.sdp_api();
        if self.publisher.audio.is_none() && self.publisher.video.is_none() && !ice_restart {
            change.add_channel("ecochain-pub".into());
            if self.options.audio {
                self.publisher.audio = Some(change.add_media(
                    MediaKind::Audio,
                    Direction::SendOnly,
                    Some("bot".into()),
                    None,
                    None,
                ));
            }
            if self.options.video_kbps.is_some() {
                let simulcast = self.options.simulcast.then(|| {
                    let mut s = Simulcast::new();
                    s.send = LAYERS.iter().map(|r| SimulcastLayer::new(r)).collect();
                    s
                });
                self.publisher.video = Some(change.add_media(
                    MediaKind::Video,
                    Direction::SendOnly,
                    Some("bot".into()),
                    None,
                    simulcast,
                ));
            }
        }
        if ice_restart {
            // Keep candidates: switch_network already swapped them.
            change.ice_restart(true);
        }
        let applied = change.apply();
        self.send_publisher_offer(applied, reset).await?;
        if !ice_restart && self.audio_muted && self.publisher.audio.is_some() {
            self.send_mute(true).await?;
        }
        Ok(())
    }

    /// Adds the microphone to a bot that has none and renegotiates.
    async fn enable_audio(&mut self) -> Result<(), BotError> {
        if self.publisher.audio.is_some() {
            return Ok(());
        }
        self.options.audio = true;
        self.audio_muted = false;
        let mut change = self.publisher.rtc.sdp_api();
        self.publisher.audio = Some(change.add_media(
            MediaKind::Audio,
            Direction::SendOnly,
            Some("bot".into()),
            None,
            None,
        ));
        let applied = change.apply();
        self.send_publisher_offer(applied, false).await
    }

    /// The request the SDK sends from `_sendMute`.
    async fn send_mute(&mut self, muted: bool) -> Result<(), BotError> {
        self.signal
            .request(
                "track.mute",
                json!({"source": "microphone", "muted": muted}),
            )
            .await
            .map(|_| ())
    }

    async fn set_audio_muted(&mut self, muted: bool) -> Result<(), BotError> {
        self.audio_muted = muted;
        if self.publisher.audio.is_some() {
            self.send_mute(muted).await?;
        }
        Ok(())
    }

    /// Applies `change`, sends the publisher offer with all current tracks
    /// and accepts the answer.
    async fn send_publisher_offer(
        &mut self,
        applied: Option<(SdpOffer, SdpPendingOffer)>,
        reset: bool,
    ) -> Result<(), BotError> {
        let Some((offer, pending)) = applied else {
            return Ok(());
        };
        self.publisher.pending = Some(pending);
        let mut tracks = Vec::new();
        if let Some(mid) = self.publisher.audio {
            tracks.push(
                json!({"mid": mid.to_string(), "source": "microphone", "muted": self.audio_muted}),
            );
        }
        if let Some(mid) = self.publisher.video {
            let layers: Vec<&str> = if self.options.simulcast {
                LAYERS.to_vec()
            } else {
                vec![]
            };
            tracks.push(json!({"mid": mid.to_string(), "source": "camera", "layers": layers}));
        }
        let answer = self
            .signal
            .request(
                "publisher.offer",
                json!({"sdp": offer.to_sdp_string(), "tracks": tracks, "reset": reset}),
            )
            .await?;
        let answer = SdpAnswer::from_sdp_string(answer["sdp"].as_str().unwrap_or_default())
            .map_err(|e| BotError::Rtc(e.to_string()))?;
        let pending = self.publisher.pending.take().expect("set above");
        self.publisher
            .rtc
            .sdp_api()
            .accept_answer(pending, answer)
            .map_err(|e| BotError::Rtc(e.to_string()))?;
        self.publisher.keyframe_needed = true;
        Ok(())
    }

    async fn run(
        &mut self,
        mut control: mpsc::UnboundedReceiver<Control>,
    ) -> Result<String, BotError> {
        let mut media_tick = tokio::time::interval(Duration::from_millis(20));
        let mut ping_tick = tokio::time::interval(Duration::from_secs(5));
        let mut buf = vec![0u8; 2048];
        let mut sub_buf = vec![0u8; 2048];
        let mut frame_count: u64 = 0;
        loop {
            let deadline = self.next_timeout();
            let in_outage = self
                .outage_until
                .is_some_and(|until| Instant::now() < until);
            #[cfg(test)]
            {
                self.stats.lock().expect("stats").outage_active = in_outage;
            }
            if !in_outage && self.outage_until.take().is_some() {
                self.recover_after_outage().await?;
            }
            tokio::select! {
                command = control.recv() => match command {
                    Some(Control::Leave) | None => {
                        let _ = self.signal.request("leave", json!({})).await;
                        return Ok("left".into());
                    }
                    Some(Control::Outage(duration)) => self.outage_until = Some(Instant::now() + duration),
                    Some(Control::SwitchNetwork) => self.switch_network().await?,
                    Some(Control::DropSignaling) => self.reconnect_signaling().await?,
                    Some(Control::SubscribeVideo(track, quality)) => self.subscribe_track(track, quality).await?,
                    Some(Control::UnsubscribeVideo(track)) => self.unsubscribe_track(track).await?,
                    Some(Control::SetAudioMuted(muted)) => self.set_audio_muted(muted).await?,
                    Some(Control::EnableAudio) => self.enable_audio().await?,
                },
                received = self.publisher.socket.recv_from(&mut buf) => {
                    let mut next = received.ok();
                    // Drain what is queued, then poll once (cheaper than a
                    // full poll per datagram at thousands of packets/s).
                    for _ in 0..64 {
                        let Some((len, source)) = next else { break };
                        if !in_outage {
                            let input = receive(source, self.publisher.local, &buf[..len]);
                            if let Some(input) = input.filter(|i| self.publisher.rtc.accepts(i)) {
                                let _ = self.publisher.rtc.handle_input(input);
                            }
                        }
                        next = self.publisher.socket.try_recv_from(&mut buf).ok();
                    }
                }
                received = self.sub_socket.recv_from(&mut sub_buf) => {
                    let mut next = received.ok();
                    for _ in 0..64 {
                        let Some((len, source)) = next else { break };
                        #[cfg(test)]
                        let source = self.test_link.as_ref().map_or(source, |link| link.source(source));
                        if let (false, Some(sub)) = (in_outage, self.subscriber.as_mut()) {
                            if let Some(input) = receive(source, self.sub_local, &sub_buf[..len]).filter(|i| sub.accepts(i)) {
                                let _ = sub.handle_input(input);
                            }
                        }
                        next = self.sub_socket.try_recv_from(&mut sub_buf).ok();
                    }
                }
                frame = self.signal.next_frame(Duration::from_secs(3600)), if !in_outage => {
                    match frame {
                        Ok(frame) => {
                            if let Some(reason) = self.handle_event(frame).await? {
                                return Ok(reason);
                            }
                        }
                        Err(_) => match self.signal.close_code {
                            Some(code) if code >= 4000 => return Ok(format!("closed {code}")),
                            _ => self.reconnect_signaling().await?,
                        },
                    }
                }
                _ = media_tick.tick() => {
                    frame_count += 1;
                    self.send_media(frame_count);
                }
                _ = ping_tick.tick(), if !in_outage => {
                    // Fire and forget: waiting here would stall media.
                    if self.signal.send_ping().await.is_err() {
                        self.reconnect_signaling().await?;
                    }
                }
                _ = tokio::time::sleep_until(deadline.into()) => {}
            }
            self.drive(in_outage)?;
        }
    }

    fn next_timeout(&self) -> Instant {
        // str0m reports its timeout from poll_output; polling every 10 ms is
        // simpler here and precise enough for a test client.
        Instant::now() + Duration::from_millis(10)
    }

    /// Polls both peers. During an outage transmits are dropped, which is
    /// what a dead network does.
    fn drive(&mut self, in_outage: bool) -> Result<(), BotError> {
        let now = Instant::now();
        let mut rtcs: Vec<(&mut Rtc, &UdpSocket)> =
            vec![(&mut self.publisher.rtc, &self.publisher.socket)];
        if let Some(sub) = self.subscriber.as_mut() {
            rtcs.push((sub, &self.sub_socket));
        }
        let mid_participant = &self.mid_participant;
        let mut stats = self.stats.lock().expect("stats");
        let mut keyframe = false;
        for (i, (rtc, socket)) in rtcs.into_iter().enumerate() {
            rtc.handle_input(Input::Timeout(now))
                .map_err(|e| BotError::Rtc(e.to_string()))?;
            loop {
                match rtc
                    .poll_output()
                    .map_err(|e| BotError::Rtc(e.to_string()))?
                {
                    Output::Timeout(_) => break,
                    Output::Transmit(t) => {
                        if !in_outage {
                            #[cfg(test)]
                            let destination = if i == 1 {
                                self.test_link
                                    .as_ref()
                                    .map_or(t.destination, |link| link.destination(t.destination))
                            } else {
                                t.destination
                            };
                            #[cfg(not(test))]
                            let destination = t.destination;
                            let _ = socket.try_send_to(&t.contents, destination);
                        }
                    }
                    Output::Event(Event::MediaData(data)) if i == 1 => {
                        stats.bytes += data.data.len() as u64;
                        let participant = mid_participant.get(&data.mid);
                        if data.params.spec().codec.is_audio() {
                            stats.audio_frames += 1;
                            stats.last_audio = Some(now);
                            if let Some(p) = participant {
                                *stats
                                    .audio_frames_by_participant
                                    .entry(p.clone())
                                    .or_default() += 1;
                            }
                        } else {
                            stats.video_frames += 1;
                            #[cfg(test)]
                            {
                                let ordinal = stats.video_frames;
                                stats
                                    .video_frame_events
                                    .push_back((ordinal, data.is_keyframe()));
                                if stats.video_frame_events.len() > 128 {
                                    stats.video_frame_events.pop_front();
                                }
                            }
                            stats.last_video = Some(now);
                            if data.is_keyframe() {
                                stats.video_keyframes += 1;
                            }
                            if let Some(p) = participant {
                                if let Some(layer) = synthetic_layer(&data.data) {
                                    *stats
                                        .video_frames_by_layer
                                        .entry((p.clone(), layer))
                                        .or_default() += 1;
                                }
                                *stats
                                    .video_frames_by_participant
                                    .entry(p.clone())
                                    .or_default() += 1;
                            }
                        }
                    }
                    Output::Event(Event::KeyframeRequest(_)) if i == 0 => {
                        keyframe = true;
                        stats.keyframe_requests += 1;
                    }
                    Output::Event(_) => {}
                }
            }
        }
        if keyframe {
            self.publisher.keyframe_needed = true;
        }
        Ok(())
    }

    /// Writes one 20 ms Opus frame and, every other tick, a VP8 frame
    /// (25 fps) per simulcast layer.
    fn send_media(&mut self, tick: u64) {
        let now = Instant::now();
        let publisher = &mut self.publisher;
        if let Some(mid) = publisher.audio.filter(|_| !self.audio_muted) {
            if let Some(writer) = publisher.rtc.writer(mid) {
                if let Some(pt) = find_pt(&writer, Codec::Opus) {
                    publisher.audio_time += 960;
                    let time = MediaTime::new(
                        publisher.audio_time,
                        str0m::media::Frequency::FORTY_EIGHT_KHZ,
                    );
                    let _ = writer.audio_level(self.options.audio_level, true).write(
                        pt,
                        now,
                        time,
                        vec![0xFCu8; 80],
                    );
                }
            }
        }
        let (Some(mid), Some(kbps)) = (publisher.video, self.options.video_kbps) else {
            return;
        };
        if !tick.is_multiple_of(2) {
            return;
        }
        let keyframe = publisher.keyframe_needed || tick.is_multiple_of(150);
        publisher.keyframe_needed = false;
        let layers: Vec<Option<(usize, &str)>> = if self.options.simulcast {
            LAYERS
                .iter()
                .enumerate()
                .map(|(i, r)| Some((i, *r)))
                .collect()
        } else {
            vec![None]
        };
        for layer in layers {
            let Some(writer) = publisher.rtc.writer(mid) else {
                return;
            };
            let Some(pt) = find_pt(&writer, Codec::Vp8) else {
                return;
            };
            let (index, layer_kbps) = match layer {
                Some((i, _)) => (i, crate::sfu::layers::LAYER_KBPS[i]),
                None => (0, kbps),
            };
            // Independent random-looking offsets per layer, like libwebrtc.
            publisher.video_time[index] += 3600;
            let base = [1_000_000u64, 50_000_000, 900_000_000][index];
            let time = MediaTime::new(
                base + publisher.video_time[index],
                str0m::media::Frequency::NINETY_KHZ,
            );
            let size = (layer_kbps * 1000 / 8 / 25).max(20) as usize * if keyframe { 3 } else { 1 };
            let writer = match layer {
                Some((_, rid)) => writer.rid(Rid::from(rid)),
                None => writer,
            };
            let _ = writer.write(pt, now, time, vp8_frame(keyframe, size, index));
        }
    }

    /// Returns `Some(reason)` when the session ended.
    async fn handle_event(&mut self, frame: Value) -> Result<Option<String>, BotError> {
        let kind = frame["type"].as_str().unwrap_or_default().to_owned();
        self.stats.lock().expect("stats").events.push(kind.clone());
        let data = &frame["data"];
        match kind.as_str() {
            "subscriber.offer" => {
                let offer = SdpOffer::from_sdp_string(data["sdp"].as_str().unwrap_or_default())
                    .map_err(|e| BotError::Rtc(e.to_string()))?;
                let local = self.sub_local;
                let sub = self.subscriber.get_or_insert_with(|| {
                    let mut rtc = Rtc::builder().build(Instant::now());
                    rtc.add_local_candidate(Candidate::host(local, "udp").expect("host candidate"));
                    rtc
                });
                let answer = sub
                    .sdp_api()
                    .accept_offer(offer)
                    .map_err(|e| BotError::Rtc(e.to_string()))?;
                self.mid_participant = data["tracks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|t| {
                        Some((
                            Mid::from(t["mid"].as_str()?),
                            t["participant_id"].as_str()?.to_owned(),
                        ))
                    })
                    .collect();
                let revision = data["revision"].clone();
                let result = self
                    .signal
                    .request(
                        "subscriber.answer",
                        json!({"sdp": answer.to_sdp_string(), "revision": revision}),
                    )
                    .await;
                // An outdated revision is fine: a newer offer is on its way.
                if let Err(e) = result {
                    if e.code() != Some(ErrorCode::InvalidState) {
                        return Err(e);
                    }
                }
            }
            "participant.joined" | "participant.updated" => {
                self.record_participant(&data["participant"]);
                self.subscribe_videos(&data["participant"]).await?;
            }
            "participant.left" => {
                let id = data["participant_id"].as_str().unwrap_or_default();
                self.stats
                    .lock()
                    .expect("stats")
                    .remote_participants
                    .retain(|(p, _)| p != id);
                self.stats
                    .lock()
                    .expect("stats")
                    .remote_tracks
                    .retain(|(p, _, _)| p != id);
            }
            "peer.failed" => {
                if data["target"] == "subscriber" {
                    self.subscriber = None;
                    self.signal.request("subscriber.reset", json!({})).await?;
                } else {
                    self.publisher = new_publisher(self.options.bind_ip).await?;
                    self.publish(false, true).await?;
                }
            }
            "room.ended" => return Ok(Some("room_ended".into())),
            "removed" => return Ok(Some("removed".into())),
            "session.replaced" => return Ok(Some("replaced".into())),
            _ => {}
        }
        Ok(None)
    }

    fn record_participants(&mut self, list: &Value) {
        for participant in list.as_array().into_iter().flatten() {
            self.record_participant(participant);
        }
    }

    fn record_participant(&mut self, participant: &Value) {
        let id = participant["participant_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let connected = participant["connected"].as_bool().unwrap_or(true);
        let mut stats = self.stats.lock().expect("stats");
        stats.remote_participants.retain(|(p, _)| *p != id);
        stats.remote_participants.push((id.clone(), connected));
        stats.remote_tracks.retain(|(p, _, _)| *p != id);
        for track in participant["tracks"].as_array().into_iter().flatten() {
            stats.remote_tracks.push((
                id.clone(),
                track["track_id"].as_str().unwrap_or_default().to_owned(),
                track["kind"].as_str().unwrap_or_default().to_owned(),
            ));
        }
    }

    async fn subscribe_videos(&mut self, participant: &Value) -> Result<(), BotError> {
        let mut changes = Vec::new();
        for track in participant["tracks"].as_array().into_iter().flatten() {
            let id = track["track_id"].as_str().unwrap_or_default().to_owned();
            if track["kind"] == "video"
                && !self.subscribed_video.contains(&id)
                && self.subscribed_video.len() < self.options.max_video
            {
                let quality = if self.subscribed_video.len() < self.options.high_video {
                    "high"
                } else {
                    "low"
                };
                self.subscribed_video.push(id.clone());
                changes.push(json!({"track_id": id, "subscribed": true, "quality": quality}));
            }
        }
        if !changes.is_empty() {
            self.signal
                .request("subscription.update", json!({"changes": changes}))
                .await?;
            self.publish_subscribed();
        }
        Ok(())
    }

    /// Subscribes to one more video track, beyond `max_video` if asked to.
    async fn subscribe_track(
        &mut self,
        track_id: String,
        quality: &'static str,
    ) -> Result<(), BotError> {
        if !self.subscribed_video.contains(&track_id) {
            self.subscribed_video.push(track_id.clone());
        }
        self.signal
            .request(
                "subscription.update",
                json!({"changes": [{"track_id": track_id, "subscribed": true, "quality": quality}]}),
            )
            .await?;
        self.publish_subscribed();
        Ok(())
    }

    async fn unsubscribe_track(&mut self, track_id: String) -> Result<(), BotError> {
        self.subscribed_video.retain(|t| *t != track_id);
        self.signal
            .request(
                "subscription.update",
                json!({"changes": [{"track_id": track_id, "subscribed": false}]}),
            )
            .await?;
        self.publish_subscribed();
        Ok(())
    }

    /// Mirrors the subscription list into the stats.
    fn publish_subscribed(&self) {
        self.stats.lock().expect("stats").subscribed_video = self.subscribed_video.clone();
    }

    /// After an outage: make sure signaling still works, then restart ICE if
    /// media did not come back by itself.
    async fn recover_after_outage(&mut self) -> Result<(), BotError> {
        if self.signal.request("ping", json!({})).await.is_err() {
            self.reconnect_signaling().await?;
        }
        if !self.publisher.rtc.is_connected() {
            self.publish(true, false).await?;
        }
        Ok(())
    }

    /// Reconnects signaling with exponential backoff and resumes the session.
    async fn reconnect_signaling(&mut self) -> Result<(), BotError> {
        let mut delay = Duration::from_millis(250);
        for _ in 0..10 {
            let attempt = async {
                let mut signal = SignalClient::connect(&self.options.url).await?;
                let joined = signal
                    .request(
                        "join",
                        json!({"token": self.options.token, "resume_session_id": self.session_id}),
                    )
                    .await?;
                Ok::<_, BotError>((signal, joined))
            };
            match attempt.await {
                Ok((signal, joined)) => {
                    self.signal = signal;
                    self.session_id = joined["session_id"].as_str().unwrap_or_default().to_owned();
                    self.record_participants(&joined["participants"]);
                    if joined["resumed"] == true {
                        self.stats.lock().expect("stats").resumes += 1;
                    } else {
                        // Session lost: rebuild both peers.
                        self.subscriber = None;
                        self.publisher = new_publisher(self.options.bind_ip).await?;
                        self.subscribed_video.clear();
                        self.publish(false, true).await?;
                    }
                    return Ok(());
                }
                Err(BotError::Server(e)) if !e.retryable => return Err(BotError::Server(e)),
                Err(_) => {
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(4));
                }
            }
        }
        Err(BotError::Timeout("signaling reconnect"))
    }

    /// New local addresses plus ICE restart on both peers.
    async fn switch_network(&mut self) -> Result<(), BotError> {
        let (socket, local) = bind(self.options.bind_ip).await?;
        let old = std::mem::replace(&mut self.publisher.local, local);
        self.publisher.socket = socket;
        swap_candidate(&mut self.publisher.rtc, old, local);

        let (socket, local) = bind(self.options.bind_ip).await?;
        let old = std::mem::replace(&mut self.sub_local, local);
        self.sub_socket = socket;
        if let Some(sub) = self.subscriber.as_mut() {
            swap_candidate(sub, old, local);
        }
        self.publish(true, false).await?;
        self.signal
            .request("ice.restart", json!({"target": "subscriber"}))
            .await?;
        Ok(())
    }
}

fn swap_candidate(rtc: &mut Rtc, old: SocketAddr, new: SocketAddr) {
    rtc.add_local_candidate(Candidate::host(new, "udp").expect("host candidate"));
    rtc.direct_api()
        .invalidate_candidate(&Candidate::host(old, "udp").expect("host candidate"));
}

async fn bind(ip: IpAddr) -> Result<(UdpSocket, SocketAddr), BotError> {
    let socket = UdpSocket::bind((ip, 0))
        .await
        .map_err(|e| BotError::Rtc(e.to_string()))?;
    let local = socket
        .local_addr()
        .map_err(|e| BotError::Rtc(e.to_string()))?;
    Ok((socket, local))
}

fn receive(source: SocketAddr, destination: SocketAddr, data: &[u8]) -> Option<Input<'_>> {
    let contents = data.try_into().ok()?;
    Some(Input::Receive(
        Instant::now(),
        Receive {
            proto: Protocol::Udp,
            source,
            destination,
            contents,
        },
    ))
}

async fn new_publisher(ip: IpAddr) -> Result<Publisher, BotError> {
    let (socket, local) = bind(ip).await?;
    let mut rtc = Rtc::builder().build(Instant::now());
    rtc.add_local_candidate(Candidate::host(local, "udp").expect("host candidate"));
    Ok(Publisher {
        rtc,
        socket,
        local,
        audio: None,
        video: None,
        pending: None,
        audio_time: 0,
        video_time: [0; 3],
        keyframe_needed: true,
    })
}

fn find_pt(writer: &str0m::media::Writer<'_>, codec: Codec) -> Option<str0m::media::Pt> {
    writer
        .payload_params()
        .find(|p| p.spec().codec == codec)
        .map(|p| p.pt())
}

/// A VP8 frame with a valid frame tag. Keyframes carry the start code and
/// a 640x360 size so depacketizers recognize them.
fn vp8_frame(keyframe: bool, size: usize, layer: usize) -> Vec<u8> {
    let mut frame = if keyframe {
        vec![0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0x68, 0x01]
    } else {
        vec![0x11, 0x02, 0x00]
    };
    frame.resize(size.max(15), 0x55);
    frame[10..14].copy_from_slice(b"ECLY");
    frame[14] = layer as u8;
    frame
}

/// Recognize only this load generator's marker, never arbitrary video payloads.
fn synthetic_layer(frame: &[u8]) -> Option<usize> {
    (frame.get(10..14)? == b"ECLY").then_some(())?;
    let layer = *frame.get(14)?;
    (layer < 3).then_some(usize::from(layer))
}

#[cfg(test)]
mod synthetic_video_tests {
    use super::*;

    #[test]
    fn synthetic_layer_marker_roundtrips_and_rejects_truncation() {
        for keyframe in [false, true] {
            for layer in 0..3 {
                let frame = vp8_frame(keyframe, 200, layer);
                assert_eq!(synthetic_layer(&frame), Some(layer));
                for len in 0..15 {
                    assert_eq!(synthetic_layer(&frame[..len]), None);
                }
            }
        }
        assert_eq!(synthetic_layer(&[0x55; 20]), None);
    }
}
