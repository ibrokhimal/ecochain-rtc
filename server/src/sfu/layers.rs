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
/// its encodings with the same numbers (`maxBitrate`), see
/// `packages/ecochain_rtc/lib/src/media/publish_options.dart`.
pub const LAYER_KBPS: [u64; 3] = [150, 500, 1500];

/// Estimated cost of a track without simulcast.
const SINGLE_LAYER_KBPS: u64 = 800;

/// One video subscription that needs a layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoDemand {
    /// Number of simulcast layers the publisher sends (0 = no simulcast).
    pub layers: usize,
    /// Quality the receiver asked for.
    pub quality: Quality,
    /// Screen shares are served first.
    pub screen: bool,
}

impl VideoDemand {
    /// Layer index that matches the requested quality.
    pub fn requested_layer(&self) -> usize {
        if self.layers == 0 {
            return 0;
        }
        let top = self.layers - 1;
        match self.quality {
            Quality::Low => 0,
            Quality::Medium => top.min(1),
            Quality::High => top,
        }
    }
}

/// Chooses a layer index for every demand.
///
/// Every simulcast track gets at least layer 0 so that no tile goes black
/// when bandwidth is short. The remaining budget upgrades tracks in
/// priority order: screen shares, then higher requested quality.
/// Without an estimate (`budget_kbps = None`) everybody gets what they asked.
pub fn allocate(demands: &[VideoDemand], budget_kbps: Option<u64>) -> Vec<usize> {
    let mut chosen = vec![0usize; demands.len()];
    let Some(budget) = budget_kbps else {
        return demands.iter().map(VideoDemand::requested_layer).collect();
    };

    let mut used: u64 = demands
        .iter()
        .map(|d| {
            if d.layers == 0 {
                SINGLE_LAYER_KBPS
            } else {
                LAYER_KBPS[0]
            }
        })
        .sum();

    let mut order: Vec<usize> = (0..demands.len()).collect();
    order.sort_by_key(|&i| (!demands[i].screen, std::cmp::Reverse(demands[i].quality)));

    // Strict priority: a screen share or a pinned (high quality) tile is
    // upgraded fully before lower priority tiles get anything above layer 0.
    for &i in &order {
        let demand = demands[i];
        while demand.layers > 0 && chosen[i] < demand.requested_layer() {
            let extra = LAYER_KBPS[chosen[i] + 1] - LAYER_KBPS[chosen[i]];
            if used + extra > budget {
                break;
            }
            used += extra;
            chosen[i] += 1;
        }
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
        }
    }

    #[test]
    fn unlimited_budget_gives_requested_layers() {
        let demands = [demand(Quality::High, false), demand(Quality::Low, false)];
        assert_eq!(allocate(&demands, None), vec![2, 0]);
    }

    #[test]
    fn short_budget_prefers_screen_share() {
        let demands = [demand(Quality::High, false), demand(Quality::High, true)];
        // 150 + 150 base, then 350 + 1000 for the screen share fits 1700;
        // the camera upgrade (+350) does not fit any more.
        assert_eq!(allocate(&demands, Some(1700)), vec![0, 2]);
    }

    #[test]
    fn every_track_keeps_the_lowest_layer() {
        let demands = [demand(Quality::High, false); 4];
        assert_eq!(allocate(&demands, Some(100)), vec![0, 0, 0, 0]);
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
