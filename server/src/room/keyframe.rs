//! Coalescing of keyframe requests per published track and layer.
//!
//! A request inside the throttle window is not dropped but deferred: the gate
//! remembers when the window ends and the room sends one request then, however
//! many callers asked meanwhile.

use std::time::{Duration, Instant};

/// Minimum spacing of keyframe requests for the same layer.
pub const KEYFRAME_THROTTLE: Duration = Duration::from_millis(500);
/// Spacing for urgent requests (new subscription, layer switch).
pub const KEYFRAME_THROTTLE_URGENT: Duration = Duration::from_millis(150);

/// Whether `rid` names a layer of a track (`None` only for non-simulcast).
pub fn is_known_layer<R: PartialEq>(layers: &[R], rid: Option<R>) -> bool {
    match rid {
        None => layers.is_empty(),
        Some(r) => layers.contains(&r),
    }
}

#[derive(Debug, Clone, Default)]
pub struct KeyframeGate {
    last: Option<Instant>,
    due: Option<Instant>,
    /// When the short urgent window was last used. It is available at most
    /// once per `KEYFRAME_THROTTLE`, so churning subscriptions cannot drive
    /// a request every 150 ms.
    urgent_used: Option<Instant>,
}

impl KeyframeGate {
    /// `true` = send now (and it is recorded as sent); `false` = a request was
    /// scheduled for when the throttle window ends.
    pub fn request(&mut self, now: Instant, urgent: bool) -> bool {
        let fast = urgent
            && self
                .urgent_used
                .is_none_or(|at| now.saturating_duration_since(at) >= KEYFRAME_THROTTLE);
        if fast {
            self.urgent_used = Some(now);
        }
        let window = if fast {
            KEYFRAME_THROTTLE_URGENT
        } else {
            KEYFRAME_THROTTLE
        };
        match self.last {
            Some(last) if now.saturating_duration_since(last) < window => {
                let at = last + window;
                self.due = Some(self.due.map_or(at, |due| due.min(at)));
                false
            }
            _ => {
                self.last = Some(now);
                self.due = None;
                true
            }
        }
    }

    pub fn due(&self) -> Option<Instant> {
        self.due
    }

    /// A keyframe arrived: a deferred request is no longer needed.
    pub fn keyframe_received(&mut self) {
        self.due = None;
    }

    /// `true` if a deferred request is due; it is then recorded as sent.
    pub fn take_due(&mut self, now: Instant) -> bool {
        if self.due.is_some_and(|due| due <= now) {
            self.due = None;
            self.last = Some(now);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn first_request_is_sent() {
        let mut gate = KeyframeGate::default();
        assert!(gate.request(Instant::now(), false));
        assert_eq!(gate.due(), None);
    }

    #[test]
    fn request_within_throttle_is_deferred() {
        let (mut gate, t0) = (KeyframeGate::default(), Instant::now());
        assert!(gate.request(t0, false));
        assert!(!gate.request(t0 + ms(100), false));
        assert_eq!(gate.due(), Some(t0 + ms(500)));
    }

    #[test]
    fn urgent_uses_short_throttle() {
        let (mut gate, t0) = (KeyframeGate::default(), Instant::now());
        assert!(gate.request(t0, false));
        assert!(!gate.request(t0 + ms(100), true));
        assert_eq!(gate.due(), Some(t0 + ms(150)));
    }

    #[test]
    fn urgent_after_short_throttle_is_sent_now() {
        let (mut gate, t0) = (KeyframeGate::default(), Instant::now());
        assert!(gate.request(t0, false));
        assert!(gate.request(t0 + ms(160), true));
    }

    #[test]
    fn urgent_fast_path_is_used_once_per_window() {
        let (mut gate, t0) = (KeyframeGate::default(), Instant::now());
        assert!(gate.request(t0, true));
        assert!(!gate.request(t0 + ms(200), true));
        assert_eq!(gate.due(), Some(t0 + ms(500)));
    }

    #[test]
    fn keyframe_cancels_deferred_request() {
        let (mut gate, t0) = (KeyframeGate::default(), Instant::now());
        assert!(gate.request(t0, true));
        assert!(!gate.request(t0 + ms(10), false));
        gate.keyframe_received();
        assert_eq!(gate.due(), None);
        assert!(!gate.take_due(t0 + ms(600)));
    }

    #[test]
    fn unknown_rid_is_not_a_layer() {
        assert!(is_known_layer::<u8>(&[], None));
        assert!(!is_known_layer::<u8>(&[], Some(1)));
        assert!(is_known_layer(&[1, 2, 3], Some(2)));
        assert!(!is_known_layer(&[1, 2, 3], Some(9)));
        assert!(!is_known_layer(&[1, 2, 3], None));
    }

    #[test]
    fn deferred_requests_coalesce() {
        let (mut gate, t0) = (KeyframeGate::default(), Instant::now());
        assert!(gate.request(t0, false));
        for i in 1..=50 {
            assert!(!gate.request(t0 + ms(i), false));
        }
        let due = gate.due().unwrap();
        assert!(!gate.take_due(due - ms(1)));
        assert!(gate.take_due(due));
        assert!(!gate.take_due(due));
        assert_eq!(gate.due(), None);
    }
}
