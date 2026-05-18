//! Wire format for the st-sync beat-window protocol.
//!
//! Each message is a *length-prefixed window of absolute beat frames*:
//!
//! ```text
//!     u8  length         number of beat-frame entries that follow
//!     u64 frame_0        little-endian, oldest published beat
//!     u64 frame_1
//!     ...
//!     u64 frame_(length-1)   most-recently-committed future beat
//! ```
//!
//! Frame values are sample positions in JACK's transport, which is the
//! sole source of truth for absolute musical position. Clients that need
//! bar/beat/tick (meter-aware musical time) ask JACK directly via
//! `jack_transport::query_transport`; st-sync's job is only to publish
//! sample-accurate beat-boundary timing, derived once by the conductor
//! and shared so every client agrees.
//!
//! All `u64` values are encoded little-endian on the wire (portability hygiene;
//! cost is negligible on x86).
//!
//! The protocol guarantees, stated here for reference and enforced by the
//! controller/client implementations:
//!
//! 1. **Immutability.** Once a beat frame appears in any window, it is fixed
//!    for the lifetime of the session. The controller may never republish a
//!    different value for the same beat index.
//! 2. **Monotonicity.** Beat frames within a window are strictly increasing.
//!    Across successive windows, beats that overlap match exactly, and the
//!    new window's high end is strictly greater than the previous window's
//!    high end (or equal, if no new beat has been committed yet).
//! 3. **Forward extension only.** Successive windows may add new beats at the
//!    high end and drop old beats at the low end (sliding forward), but
//!    never rewrite middle entries.
//!
//! The wire format itself enforces none of this — these are properties the
//! controller must uphold and clients may rely on. See `window.rs` for the
//! consumer-side data structure.

use std::io;

/// Maximum window length the wire format can carry (constrained by the `u8`
/// length prefix). Far larger than any practical `beats_per_bar + 1`.
pub const MAX_WINDOW_LEN: usize = u8::MAX as usize;

/// Encode a slice of beat frames into the wire format.
///
/// Returns `Err` if `frames.len() > MAX_WINDOW_LEN`.
pub fn encode(frames: &[u64]) -> io::Result<Vec<u8>> {
    if frames.len() > MAX_WINDOW_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("window length {} exceeds max {}", frames.len(), MAX_WINDOW_LEN),
        ));
    }
    let mut out = Vec::with_capacity(1 + frames.len() * 8);
    out.push(frames.len() as u8);
    for f in frames {
        out.extend_from_slice(&f.to_le_bytes());
    }
    Ok(out)
}

/// Try to decode a single message from the front of `buf`.
///
/// Returns:
///   - `Ok(Some((window, consumed)))` — successfully decoded a window of
///     `window.len()` entries, having consumed `consumed` bytes from `buf`.
///   - `Ok(None)` — `buf` does not yet contain a complete message; the caller
///     should read more bytes from the transport and try again.
///   - `Err(_)` — the bytes in `buf` cannot represent a valid message
///     (currently no failure case beyond the trivial empty buffer; reserved
///     for future versioning).
pub fn decode(buf: &[u8]) -> io::Result<Option<(Vec<u64>, usize)>> {
    if buf.is_empty() {
        return Ok(None);
    }
    let len = buf[0] as usize;
    let needed = 1 + len * 8;
    if buf.len() < needed {
        return Ok(None);
    }
    let mut frames = Vec::with_capacity(len);
    for i in 0..len {
        let start = 1 + i * 8;
        let mut chunk = [0u8; 8];
        chunk.copy_from_slice(&buf[start..start + 8]);
        frames.push(u64::from_le_bytes(chunk));
    }
    Ok(Some((frames, needed)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_empty_window() {
        let bytes = encode(&[]).unwrap();
        assert_eq!(bytes, vec![0u8]);
    }

    #[test]
    fn encode_single_frame() {
        let bytes = encode(&[0x0102_0304_0506_0708]).unwrap();
        // len=1, then 8 little-endian bytes.
        assert_eq!(
            bytes,
            vec![1, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
    }

    #[test]
    fn encode_multiple_frames_uses_little_endian() {
        let bytes = encode(&[1u64, 256u64]).unwrap();
        assert_eq!(
            bytes,
            vec![
                2,
                // 1
                0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                // 256
                0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ]
        );
    }

    #[test]
    fn roundtrip_typical_window() {
        let frames = vec![48_000u64, 72_000, 96_000, 120_000, 144_000];
        let encoded = encode(&frames).unwrap();
        let (decoded, consumed) = decode(&encoded).unwrap().unwrap();
        assert_eq!(decoded, frames);
        assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn roundtrip_max_size_window() {
        let frames: Vec<u64> = (0..MAX_WINDOW_LEN as u64).map(|i| i * 1000).collect();
        let encoded = encode(&frames).unwrap();
        let (decoded, consumed) = decode(&encoded).unwrap().unwrap();
        assert_eq!(decoded, frames);
        assert_eq!(consumed, 1 + MAX_WINDOW_LEN * 8);
    }

    #[test]
    fn roundtrip_large_frame_values() {
        // Long-running session: late-connect client receives a window
        // whose frame values are in the tens of millions.
        let frames = vec![48_000_000u64, 48_024_000, 48_048_000];
        let encoded = encode(&frames).unwrap();
        let (decoded, _) = decode(&encoded).unwrap().unwrap();
        assert_eq!(decoded, frames);
    }

    #[test]
    fn encode_rejects_oversize_window() {
        let frames: Vec<u64> = (0..(MAX_WINDOW_LEN + 1) as u64).collect();
        let err = encode(&frames).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn decode_empty_buffer_returns_none() {
        assert!(decode(&[]).unwrap().is_none());
    }

    #[test]
    fn decode_truncated_header_returns_none() {
        // Says length=3 but only one frame's worth of bytes follows.
        let buf = vec![3u8, 1, 0, 0, 0, 0, 0, 0, 0];
        assert!(decode(&buf).unwrap().is_none());
    }

    #[test]
    fn decode_handles_concatenated_messages() {
        // Two windows back-to-back in the same buffer; decode should
        // consume only the first and leave the second for the next call.
        let a = encode(&[100u64, 200, 300]).unwrap();
        let b = encode(&[400u64, 500]).unwrap();
        let mut combined = a.clone();
        combined.extend_from_slice(&b);

        let (first, consumed) = decode(&combined).unwrap().unwrap();
        assert_eq!(first, vec![100u64, 200, 300]);
        assert_eq!(consumed, a.len());

        let (second, consumed2) = decode(&combined[consumed..]).unwrap().unwrap();
        assert_eq!(second, vec![400u64, 500]);
        assert_eq!(consumed2, b.len());
    }
}
