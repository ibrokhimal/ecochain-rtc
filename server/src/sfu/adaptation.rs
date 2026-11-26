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

#[cfg(test)]
mod tests {
    use super::*;

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
