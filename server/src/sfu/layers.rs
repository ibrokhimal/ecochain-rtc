//! Simulcast layer selection and RTP timeline continuity.
//!
//! Publishers send up to three simulcast layers of a camera or screen track
//! (lowest first). For each subscriber the server forwards exactly one layer.
//! The choice combines what the receiver asked for (tile size) with what its
//! downlink can carry (str0m TWCC bandwidth estimate).

use crate::protocol::Quality;
use std::time::Instant;
use str0m::media::MediaTime;

/// Nominal bitrate of layer 0, 1 and 2 in kbps. The Flutter SDK configures
/// its encodings with the same numbers (`maxBitrate`) in `session.dart`.
pub const LAYER_KBPS: [u64; 3] = [150, 500, 1500];

/// Estimated cost of a track without simulcast.
const SINGLE_LAYER_KBPS: u64 = 800;

/// A video can be suspended without tearing down its negotiated receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allocation {
    /// No video fits the available budget.
    Paused,
    /// Index into the publisher RID list, or zero for a single encoding.
    Layer(usize),
}

/// One video subscription that needs a layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoDemand {
    /// Number of simulcast layers the publisher sends (0 = no simulcast).
    pub layers: usize,
    /// Quality the receiver asked for.
    pub quality: Quality,
    /// Screen shares are served first.
    pub screen: bool,
    /// The publisher is currently speaking.
    pub speaker: bool,
}

impl VideoDemand {
    /// Layer index that matches the requested quality.
    pub fn requested_layer(&self) -> usize {
        if self.layers == 0 {
            return 0;
        }
        let top = self.layers.min(3) - 1;
        match self.quality {
            Quality::Low => 0,
            Quality::Medium => top.min(1),
            Quality::High => top,
        }
    }
}

impl VideoDemand {
    /// Conservative payload cost of an allocation, in kilobits per second.
    pub fn cost_kbps(&self, allocation: Allocation) -> u64 {
        match allocation {
            Allocation::Paused => 0,
            Allocation::Layer(_) if self.layers == 0 => {
                if self.screen {
                    1500
                } else {
                    SINGLE_LAYER_KBPS
                }
            }
            Allocation::Layer(i) => LAYER_KBPS[i.min(2)],
        }
    }
}

/// Admit base layers by stable priority, then spend remaining budget on upgrades.
pub fn allocate(demands: &[VideoDemand], budget_kbps: u64) -> Vec<Allocation> {
    let mut chosen = vec![Allocation::Paused; demands.len()];
    let mut remaining = budget_kbps;
    let mut order: Vec<usize> = (0..demands.len()).collect();
    order.sort_by_key(|&i| {
        let d = demands[i];
        (!d.screen, d.quality != Quality::High, !d.speaker)
    });
    for &i in &order {
        let base = demands[i].cost_kbps(Allocation::Layer(0));
        if base <= remaining {
            chosen[i] = Allocation::Layer(0);
            remaining -= base;
        }
    }
    for i in order {
        let Allocation::Layer(mut layer) = chosen[i] else {
            continue;
        };
        while layer < demands[i].requested_layer() {
            let extra = LAYER_KBPS[layer + 1] - LAYER_KBPS[layer];
            if extra > remaining {
                break;
            }
            remaining -= extra;
            layer += 1;
        }
        chosen[i] = Allocation::Layer(layer);
    }
    chosen
}

/// Rewrites RTP time so a receiver sees one continuous timeline even when
/// the server switches between simulcast layers, which use independent
/// random RTP timestamp offsets.
#[derive(Debug, Default, Clone)]
pub struct Timeline {
    offset: i64,
    last: Option<(u64, Instant)>,
}

impl Timeline {
    /// Maps an incoming media time to the outgoing timeline.
    ///
    /// Call with `switched = true` for the first frame after a layer switch:
    /// the new layer is placed where the old one would have continued, based
    /// on the wall clock time that passed since the last frame.
    pub fn map(&mut self, time: MediaTime, wallclock: Instant, switched: bool) -> MediaTime {
        let rate = u64::from(time.denom());
        let incoming = time.numer() as i64;
        if switched {
            if let Some((last_numer, last_wall)) = self.last {
                let elapsed = wallclock.saturating_duration_since(last_wall).as_secs_f64();
                let expected = last_numer as i64 + (elapsed * rate as f64) as i64 + 1;
                self.offset = expected - incoming;
            }
        }
        let numer = (incoming + self.offset).max(0) as u64;
        self.last = Some((numer, wallclock));
        MediaTime::new(numer, time.frequency())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn demand(quality: Quality, screen: bool) -> VideoDemand {
        VideoDemand {
            layers: 3,
            quality,
            screen,
            speaker: false,
        }
    }

    #[test]
    fn finite_budget_pauses_tracks_that_do_not_fit() {
        let ds = [demand(Quality::High, false); 3];
        assert_eq!(
            allocate(&ds, 300),
            vec![
                Allocation::Layer(0),
                Allocation::Layer(0),
                Allocation::Paused
            ]
        );
        assert_eq!(allocate(&ds, 0), vec![Allocation::Paused; 3]);
    }

    #[test]
    fn selects_each_layer_and_respects_requested_ceiling() {
        for (budget, want) in [
            (149, Allocation::Paused),
            (150, Allocation::Layer(0)),
            (500, Allocation::Layer(1)),
            (1500, Allocation::Layer(2)),
        ] {
            assert_eq!(
                allocate(&[demand(Quality::High, false)], budget),
                vec![want]
            );
        }
        assert_eq!(
            allocate(&[demand(Quality::Low, false)], 5000),
            vec![Allocation::Layer(0)]
        );
        assert_eq!(
            allocate(&[demand(Quality::Medium, false)], 5000),
            vec![Allocation::Layer(1)]
        );
    }

    #[test]
    fn bases_are_admitted_before_upgrades() {
        assert_eq!(
            allocate(&[demand(Quality::High, false); 2], 1500),
            vec![Allocation::Layer(1), Allocation::Layer(1)]
        );
    }

    #[test]
    fn prioritizes_screen_high_then_speaker_stably() {
        let ordinary = demand(Quality::Medium, false);
        let mut speaker = ordinary;
        speaker.speaker = true;
        let ds = [
            ordinary,
            speaker,
            demand(Quality::High, false),
            demand(Quality::High, true),
        ];
        assert_eq!(
            allocate(&ds, 300),
            vec![
                Allocation::Paused,
                Allocation::Paused,
                Allocation::Layer(0),
                Allocation::Layer(0)
            ]
        );
        assert_eq!(
            allocate(&[ordinary, speaker, ordinary], 150),
            vec![Allocation::Paused, Allocation::Layer(0), Allocation::Paused]
        );
    }

    #[test]
    fn single_encoding_has_source_specific_cost() {
        let camera = VideoDemand {
            layers: 0,
            ..demand(Quality::High, false)
        };
        let screen = VideoDemand {
            layers: 0,
            ..demand(Quality::High, true)
        };
        assert_eq!(allocate(&[camera], 799), vec![Allocation::Paused]);
        assert_eq!(allocate(&[camera], 800), vec![Allocation::Layer(0)]);
        assert_eq!(allocate(&[screen], 1499), vec![Allocation::Paused]);
        assert_eq!(allocate(&[screen], 1500), vec![Allocation::Layer(0)]);
        assert_eq!(
            allocate(&[screen, demand(Quality::Low, false)], 150),
            vec![Allocation::Paused, Allocation::Layer(0)]
        );
    }

    #[test]
    fn allocations_never_exceed_budget_for_supported_layer_counts() {
        for layers in 0..=3 {
            let ds = [VideoDemand {
                layers,
                ..demand(Quality::High, false)
            }; 9];
            for budget in [0, 100, 150, 500, 1500, 5000, u64::MAX] {
                let chosen = allocate(&ds, budget);
                let cost: u64 = ds.iter().zip(chosen).map(|(d, a)| d.cost_kbps(a)).sum();
                assert!(cost <= budget);
            }
        }
        assert!(allocate(&[], u64::MAX).is_empty());
    }

    #[test]
    fn timeline_is_continuous_across_switches() {
        let start = Instant::now();
        let freq = MediaTime::from_90khz(0).frequency();
        let mut timeline = Timeline::default();
        let a = timeline.map(MediaTime::new(1_000_000, freq), start, false);
        assert_eq!(a.numer(), 1_000_000);
        // New layer starts at an unrelated timestamp 33 ms later.
        let b = timeline.map(
            MediaTime::new(5_000, freq),
            start + Duration::from_millis(33),
            true,
        );
        let delta = b.numer() as i64 - a.numer() as i64;
        assert!((2900..=3100).contains(&delta), "delta {delta}");
        let c = timeline.map(
            MediaTime::new(8_000, freq),
            start + Duration::from_millis(66),
            false,
        );
        assert_eq!(c.numer() - b.numer(), 3_000);
    }
}
