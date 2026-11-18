//! Simple operational counters exposed in Prometheus text format.
//!
//! Plain atomics are enough for a handful of process-wide numbers and avoid a
//! metrics framework dependency. Labels are limited to fixed error codes so
//! that user input (room ids, identities, tokens) never reaches the output.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Mutex;

/// Process-wide counters and gauges.
#[derive(Debug, Default)]
pub struct Metrics {
    /// Rooms whose actor is running.
    pub rooms_active: AtomicI64,
    /// Participants holding a seat (including those inside the reconnect grace period).
    pub participants_active: AtomicI64,
    /// Open signaling WebSockets.
    pub connections_active: AtomicI64,
    /// Successful joins (new sessions).
    pub joins_total: AtomicU64,
    /// Successful resumes of an existing session.
    pub resumes_total: AtomicU64,
    /// Participants removed because the reconnect grace period ran out.
    pub grace_expired_total: AtomicU64,
    /// Peer connections that failed (ICE/DTLS errors or ICE timeout).
    pub peer_failures_total: AtomicU64,
    /// RTP media units received from publishers.
    pub media_in_total: AtomicU64,
    /// RTP media units written to subscribers.
    pub media_out_total: AtomicU64,
    /// UDP datagrams dropped because a room queue was full or no peer matched.
    pub udp_dropped_total: AtomicU64,
    join_failures: Mutex<BTreeMap<&'static str, u64>>,
}

impl Metrics {
    /// Counts a rejected join by error code.
    pub fn join_failed(&self, code: crate::protocol::ErrorCode) {
        let mut map = self.join_failures.lock().expect("metrics lock");
        *map.entry(code.as_str()).or_default() += 1;
    }

    /// Renders all metrics in the Prometheus text exposition format.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let gauge = |out: &mut String, name: &str, help: &str, value: i64| {
            let _ = writeln!(
                out,
                "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}"
            );
        };
        let counter = |out: &mut String, name: &str, help: &str, value: u64| {
            let _ = writeln!(
                out,
                "# HELP {name} {help}\n# TYPE {name} counter\n{name} {value}"
            );
        };
        let load = |v: &AtomicU64| v.load(Ordering::Relaxed);
        gauge(
            &mut out,
            "ecochain_rtc_rooms_active",
            "Rooms currently running.",
            self.rooms_active.load(Ordering::Relaxed),
        );
        gauge(
            &mut out,
            "ecochain_rtc_participants_active",
            "Participants holding a seat.",
            self.participants_active.load(Ordering::Relaxed),
        );
        gauge(
            &mut out,
            "ecochain_rtc_connections_active",
            "Open signaling connections.",
            self.connections_active.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "ecochain_rtc_joins_total",
            "New sessions.",
            load(&self.joins_total),
        );
        counter(
            &mut out,
            "ecochain_rtc_resumes_total",
            "Resumed sessions.",
            load(&self.resumes_total),
        );
        counter(
            &mut out,
            "ecochain_rtc_grace_expired_total",
            "Participants dropped after the reconnect grace period.",
            load(&self.grace_expired_total),
        );
        counter(
            &mut out,
            "ecochain_rtc_peer_failures_total",
            "Failed peer connections.",
            load(&self.peer_failures_total),
        );
        counter(
            &mut out,
            "ecochain_rtc_media_in_total",
            "Media units received from publishers.",
            load(&self.media_in_total),
        );
        counter(
            &mut out,
            "ecochain_rtc_media_out_total",
            "Media units forwarded to subscribers.",
            load(&self.media_out_total),
        );
        counter(
            &mut out,
            "ecochain_rtc_udp_dropped_total",
            "Dropped UDP datagrams.",
            load(&self.udp_dropped_total),
        );
        let _ = writeln!(
            out,
            "# HELP ecochain_rtc_join_failures_total Rejected joins by error code.\n# TYPE ecochain_rtc_join_failures_total counter"
        );
        for (code, value) in self.join_failures.lock().expect("metrics lock").iter() {
            let _ = writeln!(
                out,
                "ecochain_rtc_join_failures_total{{code=\"{code}\"}} {value}"
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_labels_from_fixed_codes() {
        let metrics = Metrics::default();
        metrics.join_failed(crate::protocol::ErrorCode::RoomFull);
        metrics.join_failed(crate::protocol::ErrorCode::RoomFull);
        let text = metrics.render();
        assert!(text.contains("ecochain_rtc_join_failures_total{code=\"room_full\"} 2"));
        assert!(text.contains("ecochain_rtc_rooms_active 0"));
    }
}
