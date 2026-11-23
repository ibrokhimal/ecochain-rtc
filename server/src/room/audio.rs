//! Loudest-N audio selection.
//!
//! In large rooms only a few audio tracks are forwarded. Each track's level
//! (RFC 6464, dBov) is smoothed in linear amplitude with an asymmetric
//! exponential filter; thresholds and the swap margin are compared in dB.
//!
//! - A free slot takes any non-silent track (level at or above
//!   `SILENCE_FLOOR_DBOV`, or the voice activity flag set), so soft talkers
//!   and phrase starts are not lost while the room has room for them.
//! - A track that was at or above `SPEAKING_LEVEL` within the last `HOLD` is
//!   a speaker: only speakers are reported, and a newcomer takes a speaker's
//!   slot only when it is `SWAP_MARGIN_DB` louder.
//! - A member that is not a speaker gives its slot to any newcomer that is
//!   a speaker, without the margin.
//! - A member is released once it sent only silence for longer than `HOLD`
//!   (or when it is evicted, muted, removed or stalls).

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// Linear audio level (0..1) above which a track counts as speaking.
/// 0.03 is about -30.46 dBov, quieter than normal speech, louder than room noise.
pub(super) const SPEAKING_LEVEL: f32 = 0.03;

/// Packets below this level (dBov) without the voice activity flag are
/// silence: they never take a free slot and do not keep one.
const SILENCE_FLOOR_DBOV: i8 = -70;

/// A newcomer must be this much louder (dB) than the quietest speaker to take
/// its slot.
const SWAP_MARGIN_DB: f32 = 6.0;

/// A speaker stays a speaker this long after its smoothed level drops below
/// `SPEAKING_LEVEL`; a member keeps its slot this long after its last
/// non-silent packet.
const HOLD: Duration = Duration::from_millis(500);

/// Smoothing time constant while the level rises.
const ATTACK: Duration = Duration::from_millis(20);

/// Smoothing time constant while the level falls.
const RELEASE: Duration = Duration::from_millis(300);

struct Track {
    /// Smoothed linear amplitude.
    level: f32,
    last_seen: Instant,
    /// Last time the smoothed level was at or above `SPEAKING_LEVEL`.
    last_active: Option<Instant>,
    /// Last non-silent packet.
    last_audible: Option<Instant>,
    selected: bool,
    /// Selected and speaking (as of the last packet); reported as a speaker.
    speaking: bool,
}

impl Track {
    fn db(&self) -> f32 {
        to_db(self.level)
    }

    fn is_speaker_at(&self, now: Instant) -> bool {
        self.last_active
            .is_some_and(|at| now.saturating_duration_since(at) <= HOLD)
    }
}

fn to_db(linear: f32) -> f32 {
    20.0 * linear.max(1e-9).log10()
}

pub struct AudioSelector {
    max: usize,
    tracks: HashMap<String, Track>,
    /// Bumped whenever the set of reported speakers changes.
    generation: u64,
}

impl AudioSelector {
    pub fn new(max: usize) -> Self {
        Self {
            max,
            tracks: HashMap::new(),
            generation: 0,
        }
    }

    /// Decides whether a packet of `track_id` is forwarded. `level_dbov` is the
    /// RFC 6464 level (0 loudest, -127 silence); `None` means the extension is
    /// missing, in which case the packet is always forwarded. `voice_activity`
    /// is the extension's V flag.
    pub fn observe(
        &mut self,
        track_id: &str,
        level_dbov: Option<i8>,
        voice_activity: bool,
        muted: bool,
        now: Instant,
    ) -> bool {
        if muted {
            self.remove(track_id);
            return false;
        }
        let Some(dbov) = level_dbov else {
            return true;
        };
        let dbov = dbov.clamp(-127, 0);
        let sample = 10f32.powf(f32::from(dbov) / 20.0);
        let audible = voice_activity || dbov >= SILENCE_FLOOR_DBOV;

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
                last_active: None,
                last_audible: None,
                selected: false,
                speaking: false,
            });
        if track.level >= SPEAKING_LEVEL {
            track.last_active = Some(track.last_active.map_or(now, |at| at.max(now)));
        }
        if audible {
            track.last_audible = Some(track.last_audible.map_or(now, |at| at.max(now)));
        }
        let speaker = track.is_speaker_at(now);
        let db = track.db();

        if track.selected {
            let kept = track
                .last_audible
                .is_some_and(|at| now.saturating_duration_since(at) <= HOLD);
            if !kept {
                self.remove(track_id);
                return false;
            }
            if track.speaking != speaker {
                track.speaking = speaker;
                self.generation += 1;
            }
            return true;
        }
        if !audible {
            if !speaker {
                self.tracks.remove(track_id);
            }
            return false;
        }

        self.expire(now);

        let taken = self.tracks.values().filter(|t| t.selected).count();
        if taken < self.max {
            self.select(track_id, speaker);
            return true;
        }
        if !speaker {
            return false;
        }
        // A speaker newcomer takes the slot of the quietest non-speaker
        // outright, else of the quietest speaker with the margin.
        let quietest = |want_speaker: bool| {
            self.tracks
                .iter()
                .filter(|(_, t)| t.selected && t.speaking == want_speaker)
                .min_by(|a, b| a.1.level.total_cmp(&b.1.level))
                .map(|(id, t)| (id.clone(), t.db()))
        };
        let evict = match quietest(false) {
            Some((id, _)) => Some(id),
            None => match quietest(true) {
                Some((id, quiet_db)) if db >= quiet_db + SWAP_MARGIN_DB => Some(id),
                _ => None,
            },
        };
        match evict {
            Some(id) => {
                self.remove(&id);
                self.select(track_id, speaker);
                true
            }
            None => false,
        }
    }

    fn select(&mut self, track_id: &str, speaking: bool) {
        if let Some(track) = self.tracks.get_mut(track_id) {
            track.selected = true;
            track.speaking = speaking;
            if speaking {
                self.generation += 1;
            }
        }
    }

    /// Forgets a track (muted, unpublished, publisher left), freeing its slot.
    pub fn remove(&mut self, track_id: &str) {
        if self.tracks.remove(track_id).is_some_and(|t| t.speaking) {
            self.generation += 1;
        }
    }

    /// Changes whenever the set of reported speakers changes, so callers can
    /// skip recomputing derived state when it did not. Admitting or releasing
    /// a member that is not a speaker does not change it.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Forgets tracks that sent nothing for longer than the hold time.
    /// Selected tracks that stopped sending (DTX, loss, replaced track) must
    /// not hold a slot, be compared at their frozen level or be reported as
    /// speakers; a stalled unselected track starts afresh when it returns.
    pub fn expire(&mut self, now: Instant) {
        let mut dropped_speaker = false;
        self.tracks.retain(|_, t| {
            let keep = now.saturating_duration_since(t.last_seen) <= HOLD;
            dropped_speaker |= !keep && t.speaking;
            keep
        });
        if dropped_speaker {
            self.generation += 1;
        }
    }

    /// Selected tracks, loudest first.
    #[cfg(test)]
    pub fn selected(&self) -> Vec<String> {
        let mut selected: Vec<(String, f32)> = self
            .tracks
            .iter()
            .filter(|(_, t)| t.selected)
            .map(|(id, t)| (id.clone(), t.level))
            .collect();
        selected.sort_by(|a, b| b.1.total_cmp(&a.1));
        selected.into_iter().map(|(id, _)| id).collect()
    }

    /// Reported speakers, loudest first.
    #[cfg(test)]
    pub fn speakers(&self) -> Vec<String> {
        self.speaker_levels()
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    /// Selected tracks that are speaking (at or above `SPEAKING_LEVEL`, with
    /// the hold), with their smoothed linear level (0..1), loudest first.
    /// Quiet members that are forwarded but not speaking are left out.
    pub fn speaker_levels(&self) -> Vec<(String, f32)> {
        let mut speakers: Vec<(String, f32)> = self
            .tracks
            .iter()
            .filter(|(_, t)| t.speaking)
            .map(|(id, t)| (id.clone(), t.level))
            .collect();
        speakers.sort_by(|a, b| b.1.total_cmp(&a.1));
        speakers
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
                last[i] = sel.observe(
                    id,
                    Some(*level),
                    false,
                    false,
                    t0 + Duration::from_millis(at),
                );
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
        assert!(sel.observe("a", Some(-25), false, false, Instant::now()));
    }

    #[test]
    fn silence_is_not_forwarded() {
        let mut sel = AudioSelector::new(3);
        assert!(!sel.observe("b", Some(-127), false, false, Instant::now()));
    }

    #[test]
    fn muted_is_never_forwarded() {
        let mut sel = AudioSelector::new(3);
        assert!(!sel.observe("a", Some(-10), false, true, Instant::now()));
        assert!(sel.selected().is_empty());
    }

    #[test]
    fn missing_level_is_always_forwarded() {
        let mut sel = AudioSelector::new(3);
        assert!(sel.observe("x", None, false, false, Instant::now()));
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
            let r = sel.observe(
                "a",
                Some(-127),
                false,
                false,
                t0 + Duration::from_millis(ms),
            );
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
        assert!(sel.observe(
            "d",
            Some(-25),
            false,
            false,
            t0 + Duration::from_millis(900)
        ));
        assert_eq!(sel.selected(), ["d"]);
    }

    #[test]
    fn remove_frees_slot() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        feed(&mut sel, t0, 0, 200, &[("a", -25), ("b", -25), ("c", -25)]);
        sel.remove("a");
        assert!(sel.observe(
            "d",
            Some(-30),
            false,
            false,
            t0 + Duration::from_millis(220)
        ));
    }

    #[test]
    fn expire_drops_stalled_selected_tracks() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        feed(&mut sel, t0, 0, 200, &[("a", -25), ("b", -25)]);
        feed(&mut sel, t0, 200, 600, &[("b", -25)]);
        // a stopped at 180 ms; b is still sending at 780 ms.
        sel.expire(t0 + Duration::from_millis(700));
        assert_eq!(sel.selected(), ["b"]);
    }

    #[test]
    fn expire_keeps_tracks_within_hold() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        feed(&mut sel, t0, 0, 200, &[("a", -25)]);
        sel.expire(t0 + Duration::from_millis(600));
        assert_eq!(sel.selected(), ["a"]);
    }

    #[test]
    fn speaker_levels_are_smoothed_linear_loudest_first() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        feed(&mut sel, t0, 0, 400, &[("a", -20), ("b", -6)]);
        let levels = sel.speaker_levels();
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].0, "b");
        assert!((levels[0].1 - 0.501).abs() < 0.01, "{levels:?}");
        assert!((levels[1].1 - 0.1).abs() < 0.01, "{levels:?}");
    }

    #[test]
    fn generation_changes_only_with_the_speakers() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        let g0 = sel.generation();
        feed(&mut sel, t0, 0, 200, &[("a", -25)]);
        let g1 = sel.generation();
        assert_ne!(g0, g1);
        feed(&mut sel, t0, 200, 200, &[("a", -25)]);
        sel.remove("never-seen");
        assert_eq!(sel.generation(), g1);
        // A quiet member is admitted and released without touching it.
        feed(&mut sel, t0, 400, 200, &[("a", -25), ("q", -40)]);
        assert!(sel.selected().contains(&"q".to_string()));
        sel.remove("q");
        assert_eq!(sel.generation(), g1);
        sel.remove("a");
        assert_ne!(sel.generation(), g1);
    }

    #[test]
    fn quiet_speaker_with_free_slot_is_forwarded() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        assert!(sel.observe("a", Some(-40), false, false, t0));
        let last = feed(&mut sel, t0, 20, 1000, &[("a", -40)]);
        assert!(last[0]);
    }

    #[test]
    fn silence_below_floor_is_not_forwarded() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        assert!(!sel.observe("a", Some(-80), false, false, t0));
        let last = feed(&mut sel, t0, 20, 500, &[("a", -80)]);
        assert!(!last[0]);
        assert!(sel.selected().is_empty());
    }

    #[test]
    fn voice_activity_flag_admits_below_floor() {
        let mut sel = AudioSelector::new(3);
        assert!(sel.observe("a", Some(-80), true, false, Instant::now()));
        assert!(sel.speakers().is_empty());
    }

    #[test]
    fn quiet_member_is_not_a_speaker() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        let last = feed(&mut sel, t0, 0, 1000, &[("a", -40), ("b", -20)]);
        assert!(last[0], "quiet member forwarded");
        assert_eq!(sorted(sel.selected()), ["a", "b"]);
        assert_eq!(sel.speakers(), ["b"]);
    }

    #[test]
    fn speaker_evicts_quiet_member_without_margin() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        let quiet = [("a", -40), ("b", -40), ("c", -40)];
        let last = feed(&mut sel, t0, 0, 400, &quiet);
        assert!(last.iter().all(|&f| f));
        // -28 is only 12 dB louder than -40 but would need no margin anyway.
        let d = sel.observe(
            "d",
            Some(-28),
            false,
            false,
            t0 + Duration::from_millis(400),
        );
        assert!(d, "speaker blocked by quiet members");
        let selected = sel.selected();
        assert_eq!(selected.len(), 3);
        assert!(selected.contains(&"d".to_string()));
        assert_eq!(selected.iter().filter(|id| *id != "d").count(), 2);
        assert_eq!(sel.speakers(), ["d"]);
    }

    #[test]
    fn quiet_newcomer_cannot_evict_quiet_member() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        feed(&mut sel, t0, 0, 400, &[("a", -40), ("b", -40), ("c", -40)]);
        let last = feed(
            &mut sel,
            t0,
            400,
            400,
            &[("a", -40), ("b", -40), ("c", -40), ("d", -35)],
        );
        assert!(!last[3]);
        assert_eq!(sorted(sel.selected()), ["a", "b", "c"]);
    }

    #[test]
    fn quiet_member_stays_while_audible() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        let g0 = sel.generation();
        let last = feed(&mut sel, t0, 0, 3000, &[("a", -45)]);
        assert!(last[0]);
        assert_eq!(sel.selected(), ["a"]);
        assert_eq!(sel.generation(), g0, "no speaker churn");
    }

    #[test]
    fn hold_releases_after_silence_upper_bound() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        feed(&mut sel, t0, 0, 200, &[("a", -25)]);
        // Silence starts at 200 ms.
        let mut released = None;
        for ms in (200..=1400).step_by(20) {
            let forwarded = sel.observe(
                "a",
                Some(-127),
                false,
                false,
                t0 + Duration::from_millis(ms),
            );
            if !forwarded && released.is_none() {
                released = Some(ms);
            }
        }
        let released = released.expect("released");
        assert!(released >= 700, "released before the hold: {released} ms");
        assert!(released <= 1200, "released late: {released} ms");
    }

    #[test]
    fn speaker_falling_quiet_stops_being_reported() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        feed(&mut sel, t0, 0, 400, &[("a", -20)]);
        assert_eq!(sel.speakers(), ["a"]);
        let last = feed(&mut sel, t0, 400, 2000, &[("a", -45)]);
        assert!(last[0], "still forwarded while audible");
        assert!(sel.speakers().is_empty());
    }

    #[test]
    fn selected_is_loudest_first() {
        let t0 = Instant::now();
        let mut sel = AudioSelector::new(3);
        feed(&mut sel, t0, 0, 200, &[("a", -30), ("b", -10), ("c", -20)]);
        assert_eq!(sel.selected(), ["b", "c", "a"]);
    }
}
