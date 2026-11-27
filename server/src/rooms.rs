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
    vault: Option<Arc<crate::storage::Vault>>,
    provider_slots: Arc<tokio::sync::Semaphore>,
    metrics: Arc<Metrics>,
    sockets: MediaSockets,
    me: Weak<Rooms>,
    running: Mutex<HashMap<String, RoomHandle>>,
    ended: Mutex<HashMap<String, Instant>>,
    /// `(room id, identity)` pairs removed by a host. Kept here rather than
    /// in the room task so the ban survives the room closing when empty and
    /// being created again on the next join.
    /// Value: unix time until which the ban holds.
    removed: Mutex<HashMap<(String, String), u64>>,
}

impl Rooms {
    /// Creates an empty registry.
    pub fn new(
        config: Arc<Config>,
        metrics: Arc<Metrics>,
        sockets: MediaSockets,
        vault: Option<Arc<crate::storage::Vault>>,
        provider_slots: Arc<tokio::sync::Semaphore>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            config,
            vault,
            provider_slots,
            metrics,
            sockets,
            me: me.clone(),
            running: Mutex::default(),
            ended: Mutex::default(),
            removed: Mutex::default(),
        })
    }

    fn services(&self) -> RoomServices {
        RoomServices {
            vault: self.vault.clone(),
            provider_slots: self.provider_slots.clone(),
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

    /// Remembers that the host removed `identity` from `room_id`.
    ///
    /// `token_exp` is the latest `exp` of the removed session's tokens; see
    /// [`ban_until`] for how long the ban lasts.
    pub fn mark_removed(&self, room_id: &str, identity: &str, token_exp: u64) {
        let now = crate::auth::unix_now();
        let until = ban_until(token_exp, now, self.config.rooms.removal_memory_secs);
        let mut removed = self.removed.lock().expect("rooms lock");
        removed.retain(|_, until| *until >= now);
        let entry = removed
            .entry((room_id.to_owned(), identity.to_owned()))
            .or_default();
        *entry = (*entry).max(until);
    }

    /// Whether `identity` is currently banned from `room_id`.
    pub fn is_removed(&self, room_id: &str, identity: &str) -> bool {
        self.removed
            .lock()
            .expect("rooms lock")
            .get(&(room_id.to_owned(), identity.to_owned()))
            .is_some_and(|until| *until >= crate::auth::unix_now())
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

/// Unix time until which a host removal holds: never before the removed
/// token stops verifying (`exp` plus allowed clock skew), and at least
/// `memory_secs` from now so a freshly issued token is refused as well.
pub fn ban_until(token_exp: u64, now: u64, memory_secs: u64) -> u64 {
    (token_exp + crate::auth::CLOCK_SKEW_SECS).max(now + memory_secs)
}

#[cfg(test)]
mod tests {
    use super::ban_until;

    #[test]
    fn ban_outlives_long_tokens() {
        let now = 1_000_000;
        let week = 7 * 24 * 3600;
        // A token valid for a week keeps the ban for that week, not 24 h.
        assert_eq!(ban_until(now + week, now, 24 * 3600), now + week + 30);
        // Short tokens still get the minimum memory.
        assert_eq!(ban_until(now + 60, now, 24 * 3600), now + 24 * 3600);
    }
}
