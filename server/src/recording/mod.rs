//! Bounded encoded-media recording workers; no encoder or disk waits in the SFU.
pub mod mux;
mod worker;
pub use worker::{RecordedFrame, RecordingHandle};

/// Negotiated codecs supported by the recording spool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingCodec {
    /// WebRTC Opus microphone audio.
    Opus,
    /// H.264 access units in Annex B or AVC form.
    H264,
    /// VP8 frame units.
    Vp8,
}
