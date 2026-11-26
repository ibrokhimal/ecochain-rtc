//! Per-subscriber bandwidth budgets and adaptation timing.

/// Shared startup estimate before the first transport feedback arrives.
pub const INITIAL_SUBSCRIBER_KBPS: u64 = 800;

/// Reserve transport overhead and eligible audio before allocating video.
pub fn video_budget(
    estimate_kbps: Option<u64>,
    microphone_count: usize,
    screen_audio_count: usize,
    max_forwarded_audio: usize,
) -> u64 {
    // Use u128 so even hostile/extreme inputs cannot overflow the reserve.
    let usable = u128::from(estimate_kbps.unwrap_or(INITIAL_SUBSCRIBER_KBPS)) * 85 / 100;
    let audio =
        (microphone_count.min(max_forwarded_audio) as u128 + screen_audio_count as u128) * 64;
    usable.saturating_sub(audio) as u64
}

use super::layers::{allocate, Allocation, VideoDemand};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// Hysteresis state scoped to one subscriber peer and keyed by track identity.
#[derive(Debug, Default)]
pub struct AdaptationState {
    current: HashMap<String, Allocation>,
    pending: Option<(Vec<(String, Allocation)>, Instant)>,
    previous_budget: Option<u64>,
}

impl AdaptationState {
    /// Degrade immediately, but recover only with sustained spare capacity.
    pub fn update(
        &mut self,
        requests: &[(&str, VideoDemand)],
        budget_kbps: u64,
        now: Instant,
    ) -> Vec<Allocation> {
        let demands: Vec<_> = requests.iter().map(|(_, demand)| *demand).collect();
        let desired = allocate(&demands, budget_kbps);
        let first = self.previous_budget.is_none();
        let fell = self
            .previous_budget
            .is_some_and(|previous| budget_kbps < previous);
        self.previous_budget = Some(budget_kbps);
        self.current
            .retain(|id, _| requests.iter().any(|(track, _)| *track == id));
        for ((id, _), &want) in requests.iter().zip(&desired) {
            let current = self
                .current
                .entry((*id).to_owned())
                .or_insert(Allocation::Paused);
            *current = if first {
                want
            } else {
                match (*current, want) {
                    (_, Allocation::Paused) | (Allocation::Paused, _) => Allocation::Paused,
                    (Allocation::Layer(a), Allocation::Layer(b)) => Allocation::Layer(a.min(b)),
                }
            };
        }
        let cost: u128 = demands
            .iter()
            .zip(&desired)
            .map(|(d, &a)| u128::from(d.cost_kbps(a)))
            .sum();
        let upgrade = requests
            .iter()
            .zip(&desired)
            .any(|((id, _), want)| self.current.get(*id) != Some(want));
        if !upgrade || cost * 120 > u128::from(budget_kbps) * 100 {
            self.pending = None;
        } else {
            let mut key: Vec<_> = requests
                .iter()
                .zip(&desired)
                .map(|((id, _), &a)| ((*id).to_owned(), a))
                .collect();
            key.sort_by(|a, b| a.0.cmp(&b.0));
            let same = !fell
                && self
                    .pending
                    .as_ref()
                    .is_some_and(|(previous, _)| *previous == key);
            if !same {
                self.pending = Some((key, now));
            }
            if self.pending.as_ref().is_some_and(|(_, since)| {
                now.saturating_duration_since(*since) >= Duration::from_secs(2)
            }) {
                for ((id, _), &want) in requests.iter().zip(&desired) {
                    self.current.insert((*id).to_owned(), want);
                }
                self.pending = None;
            }
        }
        requests.iter().map(|(id, _)| self.current[*id]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{
        protocol::Quality,
        sfu::layers::{Allocation, VideoDemand},
    };
    use std::time::{Duration, Instant};

    fn camera() -> VideoDemand {
        VideoDemand {
            layers: 3,
            quality: Quality::High,
            screen: false,
            speaker: false,
        }
    }

    #[test]
    fn upgrades_only_after_headroom_and_two_stable_seconds() {
        let t = Instant::now();
        let mut c = AdaptationState::default();
        let req = [("a", camera())];
        assert_eq!(c.update(&req, 150, t), vec![Allocation::Layer(0)]);
        assert_eq!(c.update(&req, 599, t), vec![Allocation::Layer(0)]);
        assert_eq!(
            c.update(&req, 600, t + Duration::from_secs(10)),
            vec![Allocation::Layer(0)]
        );
        assert_eq!(
            c.update(&req, 600, t + Duration::from_millis(11999)),
            vec![Allocation::Layer(0)]
        );
        assert_eq!(
            c.update(&req, 600, t + Duration::from_secs(12)),
            vec![Allocation::Layer(1)]
        );
        assert_eq!(
            c.update(&req, 0, t + Duration::from_secs(13)),
            vec![Allocation::Paused]
        );
        assert_eq!(
            c.update(&req, 180, t + Duration::from_secs(14)),
            vec![Allocation::Paused]
        );
        assert_eq!(
            c.update(&req, 180, t + Duration::from_secs(16)),
            vec![Allocation::Layer(0)]
        );
    }

    #[test]
    fn lower_budget_resets_pending_upgrade() {
        let t = Instant::now();
        let req = [("a", camera())];
        let mut c = AdaptationState::default();
        c.update(&req, 150, t);
        c.update(&req, 700, t);
        assert_eq!(
            c.update(&req, 650, t + Duration::from_secs(1)),
            vec![Allocation::Layer(0)]
        );
        assert_eq!(
            c.update(&req, 650, t + Duration::from_secs(2)),
            vec![Allocation::Layer(0)]
        );
        assert_eq!(
            c.update(&req, 650, t + Duration::from_secs(3)),
            vec![Allocation::Layer(1)]
        );
    }

    #[test]
    fn identities_survive_reordering_but_new_tracks_do_not_inherit_timers() {
        let t = Instant::now();
        let mut c = AdaptationState::default();
        c.update(&[("a", camera()), ("b", camera())], 300, t);
        c.update(&[("a", camera()), ("b", camera())], 1200, t);
        assert_eq!(
            c.update(
                &[("b", camera()), ("a", camera())],
                1200,
                t + Duration::from_secs(2)
            ),
            vec![Allocation::Layer(1); 2]
        );
        c.update(
            &[("b", camera()), ("c", camera())],
            3600,
            t + Duration::from_secs(3),
        );
        assert_eq!(
            c.update(&[("d", camera())], 1800, t + Duration::from_secs(4)),
            vec![Allocation::Paused]
        );
        assert_eq!(
            c.update(&[("d", camera())], 1800, t + Duration::from_secs(6)),
            vec![Allocation::Layer(2)]
        );
        assert_eq!(c.update(&[], 0, t + Duration::from_secs(7)), vec![]);
    }

    #[test]
    fn lower_requested_quality_applies_immediately_and_reset_is_fresh() {
        let t = Instant::now();
        let mut c = AdaptationState::default();
        c.update(&[("a", camera())], 1800, t);
        let low = VideoDemand {
            quality: Quality::Low,
            ..camera()
        };
        assert_eq!(c.update(&[("a", low)], 1800, t), vec![Allocation::Layer(0)]);
        let mut fresh = AdaptationState::default();
        assert_eq!(
            fresh.update(&[("a", camera())], 0, t),
            vec![Allocation::Paused]
        );
    }

    #[test]
    fn reserves_audio_and_transport_before_video() {
        assert_eq!(video_budget(None, 1, 0, 3), 616);
        assert_eq!(video_budget(Some(1000), 8, 1, 3), 594);
        assert_eq!(video_budget(Some(0), 1, 0, 3), 0);
        assert_eq!(video_budget(Some(100), 3, 1, 3), 0);
        assert_eq!(video_budget(Some(1000), 0, 0, 3), 850);
    }

    #[test]
    fn extreme_inputs_do_not_overflow() {
        assert_eq!(
            video_budget(Some(u64::MAX), usize::MAX, usize::MAX, usize::MAX),
            0
        );
        assert!(video_budget(Some(u64::MAX), 0, 0, 3) > u64::MAX / 2);
    }
}
