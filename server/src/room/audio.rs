//! Loudest-N audio selection.
//!
//! In large rooms only the few loudest audio tracks are forwarded. Each
//! track's level (RFC 6464, dBov) is smoothed in linear amplitude with an
//! asymmetric exponential filter; thresholds and the swap margin are compared
//! in dB. A selected track keeps its slot for a hold time after it falls
//! silent, and a newcomer only takes a slot from a clearly quieter track.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// Linear audio level (0..1) above which a track counts as speaking.
/// 0.03 is about -30.46 dBov, quieter than normal speech, louder than room noise.
pub(super) const SPEAKING_LEVEL: f32 = 0.03;

/// A newcomer must be this much louder (dB) than the quietest selected track
/// to take its slot.
const SWAP_MARGIN_DB: f32 = 6.0;

/// A selected track keeps its slot this long after its smoothed level drops
/// below the threshold.
const HOLD: Duration = Duration::from_millis(500);

/// Smoothing time constant while the level rises.
const ATTACK: Duration = Duration::from_millis(20);

/// Smoothing time constant while the level falls.
const RELEASE: Duration = Duration::from_millis(300);

struct Track {
    /// Smoothed linear amplitude.
    level: f32,
    last_seen: Instant,
    /// Last time the smoothed level was at or above the threshold.
    last_active: Instant,
    selected: bool,
}

impl Track {
    fn db(&self) -> f32 {
        to_db(self.level)
    }
}

fn to_db(linear: f32) -> f32 {
    20.0 * linear.max(1e-9).log10()
}

pub struct AudioSelector {
    max: usize,
    tracks: HashMap<String, Track>,
}

impl AudioSelector {
    pub fn new(max: usize) -> Self {
        Self {
            max,
            tracks: HashMap::new(),
        }
    }

    /// Decides whether a packet of `track_id` is forwarded. `level_dbov` is the
    /// RFC 6464 level (0 loudest, -127 silence); `None` means the extension is
    /// missing, in which case the packet is always forwarded.
    pub fn observe(
        &mut self,
        track_id: &str,
        level_dbov: Option<i8>,
        muted: bool,
        now: Instant,
    ) -> bool {
        if muted {
            self.tracks.remove(track_id);
            return false;
        }
        let Some(dbov) = level_dbov else {
            return true;
        };
        let sample = 10f32.powf(f32::from(dbov.clamp(-127, 0)) / 20.0);

        let track = self
            .tracks
            .entry(track_id.to_owned())
            .and_modify(|track| {
                let dt = now.saturating_duration_since(track.last_seen);
                let tau = if sample > track.level {
                    ATTACK
                } else {
                    RELEASE
                };
                let alpha = 1.0 - (-dt.as_secs_f32() / tau.as_secs_f32()).exp();
                track.level += (sample - track.level) * alpha;
                track.last_seen = track.last_seen.max(now);
            })
            .or_insert(Track {
                level: sample,
                last_seen: now,
                last_active: now,
                selected: false,
            });
        let active = track.level >= SPEAKING_LEVEL;
        if active {
            track.last_active = track.last_active.max(now);
        }
        let selected = track.selected;
        let last_active = track.last_active;
        let db = track.db();

        if selected {
            if active || now.saturating_duration_since(last_active) <= HOLD {
                return true;
            }
            self.tracks.remove(track_id);
            return false;
        }
        if !active {
            self.tracks.remove(track_id);
            return false;
        }

        // Selected tracks that stopped sending (DTX, loss, replaced track)
        // must not hold a slot or be compared at their frozen level.
        self.tracks
            .retain(|_, t| !t.selected || now.saturating_duration_since(t.last_seen) <= HOLD);

        let taken = self.tracks.values().filter(|t| t.selected).count();
        if taken < self.max {
            self.select(track_id);
            return true;
        }
        let quietest = self
            .tracks
            .iter()
            .filter(|(_, t)| t.selected)
            .min_by(|a, b| a.1.level.total_cmp(&b.1.level))
            .map(|(id, t)| (id.clone(), t.db()));
        match quietest {
            Some((id, quiet_db)) if db >= quiet_db + SWAP_MARGIN_DB => {
                self.tracks.remove(&id);
                self.select(track_id);
                true
            }
            _ => false,
        }
    }

    fn select(&mut self, track_id: &str) {
        if let Some(track) = self.tracks.get_mut(track_id) {
            track.selected = true;
        }
    }

    pub fn remove(&mut self, track_id: &str) {
        self.tracks.remove(track_id);
    }

    /// Selected tracks, loudest first.
    pub fn selected(&self) -> Vec<String> {
        let mut selected: Vec<(&String, f32)> = self
            .tracks
            .iter()
            .filter(|(_, t)| t.selected)
            .map(|(id, t)| (id, t.level))
            .collect();
        selected.sort_by(|a, b| b.1.total_cmp(&a.1));
        selected.into_iter().map(|(id, _)| id.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_millis(20);

    /// Feeds `(track, level)` pairs every 20 ms for `ms` milliseconds starting
    /// at `from_ms`; returns the last decision for each track.
    fn feed(
        sel: &mut AudioSelector,
        t0: Instant,
        from_ms: u64,
        ms: u64,
        tracks: &[(&str, i8)],
    ) -> Vec<bool> {
        let mut last = vec![false; tracks.len()];
        let mut at = from_ms;
        while at < from_ms + ms {
            for (i, (id, level)) in tracks.iter().enumerate() {
                last[i] = sel.observe(id, Some(*level), false, t0 + Duration::from_millis(at));
            }
            at += FRAME.as_millis() as u64;
        }
        last
    }

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    #[test]
    fn admits_immediately_when_slot_free() {
        let mut sel = AudioSelector::new(3);
        assert!(sel.observe("a", Some(-25), false, Instant::now()));
    }

    #[test]
    fn silence_is_not_forwarded() {
        let mut sel = AudioSelector::new(3);
        assert!(!sel.observe("b", Some(-127), false, Instant::now()));
    }

    #[test]
    fn muted_is_never_forwarded() {
        let mut sel = AudioSelector::new(3);
        assert!(!sel.observe("a", Some(-10), true, Instant::now()));
        assert!(sel.selected().is_empty());
    }

    #[test]
    fn missing_level_is_always_forwarded() {
        let mut sel = AudioSelector::new(3);
        assert!(sel.observe("x", None, false, Instant::now()));
        assert!(sel.selected().is_empty());
    }

    #[test]
    fn noise_below_margin_cannot_evict() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        feed(&mut sel, t0, 0, 200, &[("a", -25), ("b", -25), ("c", -25)]);
        let last = feed(
            &mut sel,
            t0,
            200,
            500,
            &[("a", -25), ("b", -25), ("c", -25), ("d", -22)],
        );
        assert!(!last[3]);
        assert_eq!(sorted(sel.selected()), ["a", "b", "c"]);
    }

    #[test]
    fn louder_newcomer_evicts_quietest() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        feed(&mut sel, t0, 0, 200, &[("a", -25), ("b", -25), ("c", -28)]);
        let last = feed(
            &mut sel,
            t0,
            200,
            100,
            &[("a", -25), ("b", -25), ("c", -28), ("d", -15)],
        );
        assert!(last[3]);
        assert!(!sel.selected().contains(&"c".to_string()));
        assert!(sel.selected().contains(&"d".to_string()));
    }

    #[test]
    fn holds_through_short_pause() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        feed(&mut sel, t0, 0, 200, &[("a", -25)]);
        // Silence starts at 200 ms.
        let mut decisions = HashMap::new();
        for ms in (200..=1200).step_by(20) {
            let r = sel.observe("a", Some(-127), false, t0 + Duration::from_millis(ms));
            decisions.insert(ms, r);
        }
        assert!(decisions[&600], "still held 400 ms after silence starts");
        assert!(!decisions[&1200], "released 1000 ms after silence starts");
    }

    #[test]
    fn stalled_tracks_release_slots() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        feed(&mut sel, t0, 0, 200, &[("a", -25), ("b", -25), ("c", -25)]);
        // a, b, c stop sending entirely.
        assert!(sel.observe("d", Some(-25), false, t0 + Duration::from_millis(900)));
        assert_eq!(sel.selected(), ["d"]);
    }

    #[test]
    fn remove_frees_slot() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        feed(&mut sel, t0, 0, 200, &[("a", -25), ("b", -25), ("c", -25)]);
        sel.remove("a");
        assert!(sel.observe("d", Some(-30), false, t0 + Duration::from_millis(220)));
    }

    #[test]
    fn selected_is_loudest_first() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        feed(&mut sel, t0, 0, 200, &[("a", -30), ("b", -10), ("c", -20)]);
        assert_eq!(sel.selected(), ["b", "c", "a"]);
    }
}
