//! Video forwarding gate, independent of SDP negotiation state.

use crate::sfu::layers::Allocation;
use str0m::media::Rid;

#[derive(Debug, Clone)]
pub(super) struct VideoGate {
    pub allocation: Allocation,
    pub target_rid: Option<Rid>,
    pub current_rid: Option<Rid>,
    pub waiting: bool,
    allow_old: bool,
    forwarded: bool,
}

impl Default for VideoGate {
    fn default() -> Self {
        Self {
            allocation: Allocation::Paused,
            target_rid: None,
            current_rid: None,
            waiting: true,
            allow_old: false,
            forwarded: false,
        }
    }
}

impl VideoGate {
    /// Return whether a new selected encoding needs a keyframe request.
    pub fn configure(&mut self, allocation: Allocation, target_rid: Option<Rid>) -> bool {
        if self.allocation == allocation && self.target_rid == target_rid {
            return false;
        }
        self.allow_old = matches!((self.allocation, allocation),
            (Allocation::Layer(old), Allocation::Layer(new)) if new > old)
            && self.forwarded;
        self.allocation = allocation;
        self.target_rid = target_rid;
        self.waiting = true;
        allocation != Allocation::Paused
    }

    /// Some(rebase) permits a frame; None drops it without consuming RTP time.
    pub fn accept(&mut self, rid: Option<Rid>, keyframe: bool) -> Option<bool> {
        if self.allocation == Allocation::Paused {
            return None;
        }
        if self.waiting {
            if rid == self.target_rid {
                if !keyframe {
                    return None;
                }
                let rebase = self.forwarded;
                self.current_rid = rid;
                self.waiting = false;
                self.allow_old = false;
                self.forwarded = true;
                return Some(rebase);
            }
            return (self.allow_old && rid == self.current_rid).then_some(false);
        }
        (rid == self.current_rid).then_some(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sfu::layers::Allocation::{Layer, Paused};
    use str0m::media::Rid;

    #[test]
    fn pause_and_same_rid_resume_require_a_new_keyframe() {
        let rid = Some(Rid::from("q"));
        let mut gate = VideoGate::default();
        assert!(gate.configure(Layer(0), rid));
        assert_eq!(gate.accept(rid, false), None);
        assert_eq!(gate.accept(rid, true), Some(false));
        gate.configure(Paused, None);
        assert_eq!(gate.accept(rid, true), None);
        assert!(gate.configure(Layer(0), rid));
        assert_eq!(gate.accept(rid, false), None);
        assert_eq!(gate.accept(rid, true), Some(true));
        assert_eq!(gate.accept(rid, false), Some(false));
    }

    #[test]
    fn single_encoding_resume_is_also_keyframe_gated() {
        let mut gate = VideoGate::default();
        gate.configure(Layer(0), None);
        assert_eq!(gate.accept(None, false), None);
        assert_eq!(gate.accept(None, true), Some(false));
        gate.configure(Paused, None);
        gate.configure(Layer(0), None);
        assert_eq!(gate.accept(None, false), None);
        assert_eq!(gate.accept(None, true), Some(true));
    }

    #[test]
    fn downgrade_stops_old_layer_before_target_keyframe() {
        let q = Some(Rid::from("q"));
        let f = Some(Rid::from("f"));
        let mut gate = VideoGate::default();
        gate.configure(Layer(2), f);
        gate.accept(f, true);
        gate.configure(Layer(0), q);
        assert_eq!(gate.accept(f, false), None);
        assert_eq!(gate.accept(q, false), None);
        assert_eq!(gate.accept(q, true), Some(true));
        assert_eq!(gate.accept(f, true), None);
    }

    #[test]
    fn upgrade_continues_old_layer_until_new_keyframe() {
        let q = Some(Rid::from("q"));
        let f = Some(Rid::from("f"));
        let mut gate = VideoGate::default();
        gate.configure(Layer(0), q);
        gate.accept(q, true);
        gate.configure(Layer(2), f);
        assert_eq!(gate.accept(q, false), Some(false));
        assert_eq!(gate.accept(f, false), None);
        assert_eq!(gate.accept(f, true), Some(true));
        assert_eq!(gate.accept(q, true), None);
        assert!(!gate.configure(Layer(2), f));
        assert_eq!(gate.accept(f, false), Some(false));
    }
}
