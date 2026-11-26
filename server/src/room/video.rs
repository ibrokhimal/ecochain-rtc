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
        self.allow_old = (!self.waiting || self.allow_old)
            && matches!((self.allocation, allocation),
            (Allocation::Layer(old), Allocation::Layer(new)) if new > old)
            && self.forwarded;
        self.allocation = allocation;
        self.target_rid = target_rid;
        self.waiting = true;
        allocation != Allocation::Paused
    }

    /// Commit decoder state only after the subscriber accepted the frame.
    pub fn forward<E>(
        &mut self,
        rid: Option<Rid>,
        keyframe: bool,
        write: impl FnOnce(bool) -> Result<(), E>,
    ) -> Result<bool, E> {
        let mut staged = self.clone();
        let Some(rebase) = staged.accept(rid, keyframe) else {
            return Ok(false);
        };
        write(rebase)?;
        *self = staged;
        Ok(true)
    }

    /// Some(rebase) permits a frame; None drops it without consuming RTP time.
    fn accept(&mut self, rid: Option<Rid>, keyframe: bool) -> Option<bool> {
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
    fn downgrade_then_upgrade_cannot_reenable_an_over_budget_old_encoding() {
        let q = Some(Rid::from("q"));
        let h = Some(Rid::from("h"));
        let f = Some(Rid::from("f"));
        let mut gate = VideoGate::default();
        gate.configure(Layer(2), f);
        gate.accept(f, true);
        gate.configure(Layer(0), q);
        gate.configure(Layer(1), h);
        assert_eq!(gate.accept(f, false), None);
        assert_eq!(gate.accept(h, false), None);
        assert_eq!(gate.accept(h, true), Some(true));
    }

    #[test]
    fn pending_resume_upgrade_cannot_reenable_old_delta_frames() {
        let q = Some(Rid::from("q"));
        let h = Some(Rid::from("h"));
        let f = Some(Rid::from("f"));
        let mut gate = VideoGate::default();
        gate.configure(Layer(0), q);
        gate.accept(q, true);
        gate.configure(Paused, None);
        gate.configure(Layer(1), h);
        gate.configure(Layer(2), f);
        assert_eq!(gate.accept(q, false), None);
        assert_eq!(gate.accept(f, false), None);
        assert_eq!(gate.accept(f, true), Some(true));
    }

    #[test]
    fn failed_keyframe_enqueue_does_not_open_delta_gate() {
        let q = Some(Rid::from("q"));
        let h = Some(Rid::from("h"));
        let mut gate = VideoGate::default();
        gate.configure(Layer(0), q);
        gate.accept(q, true);
        gate.configure(Paused, None);
        gate.configure(Layer(1), h);
        assert_eq!(
            gate.forward(h, true, |_| Err::<(), _>("queue full")),
            Err("queue full")
        );
        assert_eq!(gate.forward(h, false, |_| Ok::<(), &str>(())), Ok(false));
        assert_eq!(gate.forward(h, true, |_| Ok::<(), &str>(())), Ok(true));
        assert_eq!(gate.forward(h, false, |_| Ok::<(), &str>(())), Ok(true));
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
