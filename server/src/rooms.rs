//! Registry of running rooms.

use crate::{
    config::Config,
    metrics::Metrics,
    protocol::ErrorCode,
    room::{self, RoomHandle, RoomServices},
    sfu::udp::MediaSockets,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};

/// Finds, creates and forgets rooms.
///
/// The registry only maps ids to handles. Everything else about a room lives
/// in its task, so the lock here is held for map operations only.
pub struct Rooms {
    config: Arc<Config>,
    metrics: Arc<Metrics>,
    sockets: MediaSockets,
    me: Weak<Rooms>,
    running: Mutex<HashMap<String, RoomHandle>>,
    ended: Mutex<HashMap<String, Instant>>,
}

impl Rooms {
    /// Creates an empty registry.
    pub fn new(config: Arc<Config>, metrics: Arc<Metrics>, sockets: MediaSockets) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            config,
            metrics,
            sockets,
            me: me.clone(),
            running: Mutex::default(),
            ended: Mutex::default(),
        })
    }

    fn services(&self) -> RoomServices {
        RoomServices {
            config: self.config.clone(),
            metrics: self.metrics.clone(),
            sockets: self.sockets.clone(),
            rooms: self.me.clone(),
        }
    }

    /// Returns the room a join should go to.
    ///
    /// * An ended room answers `room_ended`, so clients retrying after the
    ///   host ended the meeting do not bring it back to life.
    /// * A missing room is created only when `rooms.auto_create` is on.
    pub fn for_join(&self, room_id: &str) -> Result<RoomHandle, ErrorCode> {
        if self.is_ended(room_id) {
            return Err(ErrorCode::RoomEnded);
        }
        let mut running = self.running.lock().expect("rooms lock");
        if let Some(handle) = running.get(room_id).filter(|h| !h.is_closed()) {
            return Ok(handle.clone());
        }
        if !self.config.rooms.auto_create {
            return Err(ErrorCode::RoomNotFound);
        }
        let handle = room::spawn(
            room_id.to_owned(),
            self.config.rooms.default_max_participants,
            false,
            self.services(),
        );
        running.insert(room_id.to_owned(), handle.clone());
        Ok(handle)
    }

    /// Creates a room through the admin API. Fails if it already runs.
    pub fn create(
        &self,
        room_id: &str,
        max_participants: usize,
    ) -> Result<RoomHandle, &'static str> {
        if max_participants == 0 || max_participants > self.config.rooms.max_participants_limit {
            return Err("max_participants out of range");
        }
        let mut running = self.running.lock().expect("rooms lock");
        if running.get(room_id).is_some_and(|h| !h.is_closed()) {
            return Err("room already exists");
        }
        // Creating a room explicitly re-opens an id that was ended before.
        self.ended.lock().expect("rooms lock").remove(room_id);
        let handle = room::spawn(room_id.to_owned(), max_participants, true, self.services());
        running.insert(room_id.to_owned(), handle.clone());
        Ok(handle)
    }

    /// Running room by id.
    pub fn get(&self, room_id: &str) -> Option<RoomHandle> {
        self.running
            .lock()
            .expect("rooms lock")
            .get(room_id)
            .filter(|h| !h.is_closed())
            .cloned()
    }

    /// All running rooms.
    pub fn list(&self) -> Vec<RoomHandle> {
        self.running
            .lock()
            .expect("rooms lock")
            .values()
            .cloned()
            .collect()
    }

    /// Called by a room task right before it stops.
    pub fn remove(&self, handle: &RoomHandle) {
        let mut running = self.running.lock().expect("rooms lock");
        // Only remove our own entry; a new room with the same id may already
        // have replaced it.
        if running.get(&handle.id).is_some_and(|h| h.same_room(handle)) {
            running.remove(&handle.id);
        }
    }

    /// Remembers that the host ended `room_id`.
    pub fn mark_ended(&self, room_id: &str) {
        let keep = Duration::from_secs(self.config.rooms.ended_room_memory_secs);
        let mut ended = self.ended.lock().expect("rooms lock");
        let now = Instant::now();
        ended.retain(|_, at| now - *at < keep);
        ended.insert(room_id.to_owned(), now);
    }

    /// Whether `room_id` was ended recently.
    pub fn is_ended(&self, room_id: &str) -> bool {
        let keep = Duration::from_secs(self.config.rooms.ended_room_memory_secs);
        self.ended
            .lock()
            .expect("rooms lock")
            .get(room_id)
            .is_some_and(|at| at.elapsed() < keep)
    }
}
