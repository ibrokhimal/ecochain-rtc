//! Minimal timestamped single-track Matroska spool. FFmpeg finalizes playback MP4/AAC.
use super::RecordingCodec;
use anyhow::{ensure, Context, Result};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};

/// One encoded track spool; every frame carries an explicit millisecond timestamp.
pub struct MatroskaWriter {
    file: BufWriter<File>,
    codec: RecordingCodec,
    last_ms: u64,
}
impl MatroskaWriter {
    /// Starts a codec-specific stream at the first decodable frame.
    pub fn new(path: &Path, codec: RecordingCodec, first: &[u8]) -> Result<Self> {
        let mut file = BufWriter::new(File::create(path)?);
        let header = join(&[
            uint(0x4286, 1),
            uint(0x42f7, 1),
            uint(0x42f2, 4),
            uint(0x42f3, 8),
            element(0x4282, b"matroska"),
            uint(0x4287, 4),
            uint(0x4285, 2),
        ]);
        file.write_all(&element(0x1a45dfa3, &header))?;
        file.write_all(&[
            0x18, 0x53, 0x80, 0x67, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ])?;
        file.write_all(&element(
            0x1549a966,
            &join(&[
                uint(0x2ad7b1, 1_000_000),
                element(0x4d80, b"Ecochain"),
                element(0x5741, b"Ecochain"),
            ]),
        ))?;
        let (id, private, width, height) = match codec {
            RecordingCodec::Opus => {
                let mut private = b"OpusHead".to_vec();
                private.extend_from_slice(&[1, 2]);
                private.extend_from_slice(&312u16.to_le_bytes());
                private.extend_from_slice(&48000u32.to_le_bytes());
                private.extend_from_slice(&[0, 0, 0]);
                ("A_OPUS", private, 0, 0)
            }
            RecordingCodec::Vp8 => {
                ensure!(
                    first.len() >= 10 && first[3..6] == [0x9d, 0x01, 0x2a],
                    "VP8 keyframe header missing"
                );
                let width = (u16::from_le_bytes([first[6], first[7]]) & 0x3fff) as u64;
                let height = (u16::from_le_bytes([first[8], first[9]]) & 0x3fff) as u64;
                ensure!(width > 0 && height > 0, "VP8 dimensions missing");
                ("V_VP8", vec![], width, height)
            }
            RecordingCodec::H264 => {
                let nals = nal_units(first)?;
                let sps = nals
                    .iter()
                    .find(|n| n[0] & 31 == 7)
                    .context("H264 SPS missing")?;
                let pps = nals
                    .iter()
                    .find(|n| n[0] & 31 == 8)
                    .context("H264 PPS missing")?;
                ensure!(
                    sps.len() >= 4 && sps.len() <= 65535 && pps.len() <= 65535,
                    "invalid H264 parameters"
                );
                let mut private = vec![1, sps[1], sps[2], sps[3], 0xff, 0xe1];
                private.extend_from_slice(&(sps.len() as u16).to_be_bytes());
                private.extend_from_slice(sps);
                private.push(1);
                private.extend_from_slice(&(pps.len() as u16).to_be_bytes());
                private.extend_from_slice(pps);
                // Decoder reads the exact dimensions from SPS; the spool container supplies a hint.
                ("V_MPEG4/ISO/AVC", private, 1920, 1080)
            }
        };
        let mut entry = join(&[
            uint(0xd7, 1),
            uint(0x73c5, 1),
            uint(0x83, if codec == RecordingCodec::Opus { 2 } else { 1 }),
            uint(0x9c, 0),
            element(0x86, id.as_bytes()),
        ]);
        if !private.is_empty() {
            entry.extend(element(0x63a2, &private));
        }
        if codec == RecordingCodec::Opus {
            entry.extend(uint(0x56aa, 6_500_000));
            entry.extend(uint(0x56bb, 80_000_000));
            entry.extend(element(
                0xe1,
                &join(&[element(0xb5, &48000f64.to_be_bytes()), uint(0x9f, 2)]),
            ));
        } else {
            entry.extend(element(
                0xe0,
                &join(&[uint(0xb0, width), uint(0xba, height)]),
            ));
        }
        file.write_all(&element(0x1654ae6b, &element(0xae, &entry)))?;
        Ok(Self {
            file,
            codec,
            last_ms: 0,
        })
    }
    /// Appends a complete encoded access unit, preserving its presentation time.
    pub fn frame(&mut self, ms: u64, bytes: &[u8], keyframe: bool) -> Result<()> {
        ensure!(ms >= self.last_ms, "recording timestamps regressed");
        self.last_ms = ms;
        let payload = if self.codec == RecordingCodec::H264 {
            let mut avc = Vec::new();
            for nal in nal_units(bytes)? {
                avc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                avc.extend_from_slice(nal);
            }
            avc
        } else {
            bytes.to_vec()
        };
        let mut block = vec![
            0x81,
            0,
            0,
            if keyframe || self.codec == RecordingCodec::Opus {
                0x80
            } else {
                0
            },
        ];
        block.extend(payload);
        self.file.write_all(&element(
            0x1f43b675,
            &join(&[uint(0xe7, ms), element(0xa3, &block)]),
        ))?;
        Ok(())
    }
    /// Flushes and fsyncs the spool before finalization.
    pub fn finish(mut self) -> Result<()> {
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        Ok(())
    }
}
fn element(id: u32, data: &[u8]) -> Vec<u8> {
    let raw = id.to_be_bytes();
    let start = raw.iter().position(|v| *v != 0).unwrap_or(3);
    let mut result = raw[start..].to_vec();
    result.extend(size(data.len() as u64));
    result.extend_from_slice(data);
    result
}
fn size(value: u64) -> Vec<u8> {
    for width in 1..=8 {
        if value < (1u64 << (7 * width)) - 1 {
            let encoded = value | (1u64 << (7 * width));
            return encoded.to_be_bytes()[8 - width..].to_vec();
        }
    }
    unreachable!("bounded media element")
}
fn uint(id: u32, value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let start = bytes.iter().position(|v| *v != 0).unwrap_or(7);
    element(id, &bytes[start..])
}
fn join(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}
fn nal_units(bytes: &[u8]) -> Result<Vec<&[u8]>> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let len = if bytes[i..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[i..].starts_with(&[0, 0, 1]) {
            3
        } else {
            0
        };
        if len > 0 {
            starts.push((i, i + len));
            i += len;
        } else {
            i += 1;
        }
    }
    if starts.is_empty() {
        let mut result = Vec::new();
        let mut pos = 0;
        while pos + 4 <= bytes.len() {
            let len = u32::from_be_bytes(bytes[pos..pos + 4].try_into()?) as usize;
            pos += 4;
            ensure!(
                len > 0 && pos + len <= bytes.len(),
                "invalid AVC NAL length"
            );
            result.push(&bytes[pos..pos + len]);
            pos += len;
        }
        ensure!(
            pos == bytes.len() && !result.is_empty(),
            "H264 access unit missing NAL units"
        );
        return Ok(result);
    }
    let mut result = Vec::new();
    for (index, (_, start)) in starts.iter().enumerate() {
        let end = starts.get(index + 1).map_or(bytes.len(), |s| s.0);
        if *start < end {
            result.push(&bytes[*start..end]);
        }
    }
    ensure!(!result.is_empty(), "empty H264 access unit");
    Ok(result)
}

/// Caches SPS/PPS across pause- and loss-induced recording segments.
pub(super) fn parameter_sets(bytes: &[u8]) -> Option<Vec<u8>> {
    let nals = nal_units(bytes).ok()?;
    let mut result = Vec::new();
    let mut sps = false;
    let mut pps = false;
    for nal in nals {
        match nal[0] & 31 {
            7 => sps = true,
            8 => pps = true,
            _ => continue,
        };
        result.extend_from_slice(&[0, 0, 0, 1]);
        result.extend_from_slice(nal);
    }
    if sps && pps {
        Some(result)
    } else {
        None
    }
}
