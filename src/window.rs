//! `BeatWindow` — the consumer-side data structure backing the st-sync
//! beat-window protocol (see `wire.rs` for the wire format and protocol
//! guarantees).
//!
//! A `BeatWindow` holds the most-recent window received from the
//! controller: a strictly increasing sequence of absolute JACK frame
//! positions, one per beat boundary.
//!
//! The window is updated by calling [`BeatWindow::update`] with a freshly
//! decoded window slice. The update enforces the protocol guarantees:
//! the incoming window must be strictly increasing (monotonicity), any
//! overlap with the existing window — detected by matching frame values
//! at one or both ends — must match exactly (immutability), and the
//! window's high end must not move backward (forward extension only).
//!
//! Consumers that need *musical* position (which bar, which beat-within-
//! bar) read it from JACK transport. This data structure deals only in
//! sample-accurate beat timing.

use std::fmt;

/// A sliding window of recently-published and near-future beat frames.
///
/// All entries are absolute JACK sample positions, strictly increasing.
#[derive(Debug, Clone)]
pub struct BeatWindow {
    frames: Vec<u64>,
}

/// Reasons a window update may be rejected.
#[derive(Debug, PartialEq, Eq)]
pub enum UpdateError {
    /// The incoming window's frames are not strictly increasing.
    NotMonotonic,
    /// An overlap region with the existing window doesn't match the
    /// previously published frames (immutability violation).
    ImmutabilityViolation { previous: u64, incoming: u64 },
    /// The incoming window's high end is below the current window's high
    /// end, or the windows don't overlap and there is a gap or rewind
    /// between them. Either case would force us to discard known facts.
    BackwardMove {
        current_high: u64,
        incoming_high: u64,
    },
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::NotMonotonic => write!(f, "window frames are not strictly increasing"),
            UpdateError::ImmutabilityViolation { previous, incoming } => write!(
                f,
                "overlap frame mismatch: previously {} but incoming has {}",
                previous, incoming
            ),
            UpdateError::BackwardMove { current_high, incoming_high } => write!(
                f,
                "incoming window high frame {} is below current high {}",
                incoming_high, current_high
            ),
        }
    }
}

impl std::error::Error for UpdateError {}

impl BeatWindow {
    /// Construct an empty window. No timing information is available until
    /// [`update`](Self::update) is called with the first received window.
    pub fn new() -> BeatWindow {
        BeatWindow { frames: Vec::new() }
    }

    /// True if no window has been received yet.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Number of beat frames currently held.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Raw frames for clients that want to walk the window themselves.
    pub fn frames(&self) -> &[u64] {
        &self.frames
    }

    /// Frames-per-beat as measured between the most recent pair of published
    /// beats. `None` if fewer than two beats are known.
    ///
    /// This is the *just-completed* beat's duration — exact, sample-
    /// accurate, agreed-on by every client by construction (it's just
    /// `frames[last] - frames[last - 1]`).
    pub fn frames_per_beat(&self) -> Option<u64> {
        if self.frames.len() < 2 {
            return None;
        }
        let n = self.frames.len();
        Some(self.frames[n - 1] - self.frames[n - 2])
    }

    /// Fractional beat position corresponding to `frame`, expressed as
    /// "beats past `frames[0]`."
    ///
    /// Linear interpolation between adjacent published beats. Returns
    /// `None` if the window is empty or if `frame` is outside the range
    /// `[frames[0], frames[last]]` — callers that want extrapolation must
    /// do it themselves.
    ///
    /// The result is a *relative* position within this window, not an
    /// absolute beat index in the conductor's lifetime. Components that
    /// need bar / beat-within-bar position read that from JACK transport.
    pub fn beat_position_at(&self, frame: u64) -> Option<f64> {
        if self.frames.len() < 2 {
            return None;
        }
        if frame < self.frames[0] || frame > *self.frames.last().unwrap() {
            return None;
        }
        // Binary search for the segment containing `frame`.
        // Each segment i covers frames[i]..=frames[i+1].
        let mut lo = 0usize;
        let mut hi = self.frames.len() - 1;
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if self.frames[mid] <= frame {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let f0 = self.frames[lo] as f64;
        let f1 = self.frames[lo + 1] as f64;
        let t = (frame as f64 - f0) / (f1 - f0);
        Some(lo as f64 + t)
    }

    /// Apply a freshly received window.
    ///
    /// Enforces protocol guarantees:
    /// - The incoming window must be strictly increasing (monotonicity).
    /// - The incoming window's high frame must be `>=` the current
    ///   window's high frame (forward extension only).
    /// - If the incoming window's frames overlap the existing window
    ///   (any incoming frame equals an existing frame), all positions
    ///   that should overlap must match exactly (immutability).
    pub fn update(&mut self, incoming: &[u64]) -> Result<(), UpdateError> {
        // Monotonicity check on the incoming window itself.
        for pair in incoming.windows(2) {
            if pair[1] <= pair[0] {
                return Err(UpdateError::NotMonotonic);
            }
        }

        if self.frames.is_empty() || incoming.is_empty() {
            self.frames = incoming.to_vec();
            return Ok(());
        }

        let current_high = *self.frames.last().unwrap();
        let incoming_high = *incoming.last().unwrap();

        if incoming_high < current_high {
            return Err(UpdateError::BackwardMove {
                current_high,
                incoming_high,
            });
        }

        // Find the overlap by searching for the incoming window's first
        // frame in the existing window. If present, every subsequent
        // entry in `incoming` up to the end of the existing window must
        // match the corresponding entry in `self.frames`.
        let incoming_low = incoming[0];
        if let Some(pos) = self.frames.iter().position(|&f| f == incoming_low) {
            // Verify overlap match for all entries from `pos` onward.
            let overlap_len = (self.frames.len() - pos).min(incoming.len());
            for i in 0..overlap_len {
                let prev = self.frames[pos + i];
                let inc = incoming[i];
                if prev != inc {
                    return Err(UpdateError::ImmutabilityViolation {
                        previous: prev,
                        incoming: inc,
                    });
                }
            }
        } else if incoming_low <= current_high {
            // Incoming low falls inside the current window's range but
            // isn't one of our recorded beats — that's a contradiction
            // with what we already know.
            return Err(UpdateError::ImmutabilityViolation {
                previous: 0,
                incoming: incoming_low,
            });
        }
        // Otherwise: incoming starts strictly after our current window
        // ends. That's a clean continuation (or a gap, which we accept
        // because the conductor may have rotated more beats out than our
        // window held). Either way, adopt the incoming window verbatim.

        self.frames = incoming.to_vec();
        Ok(())
    }
}

impl Default for BeatWindow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_window_has_no_facts() {
        let w = BeatWindow::new();
        assert!(w.is_empty());
        assert_eq!(w.len(), 0);
        assert_eq!(w.frames_per_beat(), None);
        assert_eq!(w.beat_position_at(0), None);
        assert_eq!(w.beat_position_at(48_000), None);
    }

    #[test]
    fn single_beat_window_has_no_frames_per_beat() {
        let mut w = BeatWindow::new();
        w.update(&[1_000_000]).unwrap();
        assert_eq!(w.len(), 1);
        assert_eq!(w.frames_per_beat(), None);
    }

    #[test]
    fn frames_per_beat_uses_last_two_published_beats() {
        let mut w = BeatWindow::new();
        // Tempo speeds up across the window: 50, 40, 30 frames per beat.
        w.update(&[0, 50, 90, 120]).unwrap();
        assert_eq!(w.frames_per_beat(), Some(30));
    }

    #[test]
    fn beat_position_at_exact_published_frames() {
        let mut w = BeatWindow::new();
        w.update(&[1000, 2000, 3000, 4000]).unwrap();
        // Result is "beats past frames[0]" — relative, not absolute.
        assert_eq!(w.beat_position_at(1000), Some(0.0));
        assert_eq!(w.beat_position_at(2000), Some(1.0));
        assert_eq!(w.beat_position_at(3000), Some(2.0));
        assert_eq!(w.beat_position_at(4000), Some(3.0));
    }

    #[test]
    fn beat_position_at_interpolates_between_published_beats() {
        let mut w = BeatWindow::new();
        w.update(&[0, 1000, 2000]).unwrap();
        assert_eq!(w.beat_position_at(500), Some(0.5));
        assert_eq!(w.beat_position_at(250), Some(0.25));
        assert_eq!(w.beat_position_at(1500), Some(1.5));
        assert_eq!(w.beat_position_at(1750), Some(1.75));
    }

    #[test]
    fn beat_position_at_returns_none_outside_window() {
        let mut w = BeatWindow::new();
        w.update(&[1000, 2000, 3000]).unwrap();
        assert_eq!(w.beat_position_at(999), None);
        assert_eq!(w.beat_position_at(3001), None);
    }

    #[test]
    fn beat_position_at_works_under_continuous_tempo_change() {
        // Beats at 0, 100, 180, 240 — accelerando (100, 80, 60 fpb).
        let mut w = BeatWindow::new();
        w.update(&[0, 100, 180, 240]).unwrap();
        // Midpoints of each segment.
        assert_eq!(w.beat_position_at(50), Some(0.5));
        assert_eq!(w.beat_position_at(140), Some(1.5));
        assert_eq!(w.beat_position_at(210), Some(2.5));
    }

    #[test]
    fn update_rejects_non_monotonic_incoming() {
        let mut w = BeatWindow::new();
        let err = w.update(&[100, 90, 80]).unwrap_err();
        assert_eq!(err, UpdateError::NotMonotonic);
    }

    #[test]
    fn update_rejects_equal_adjacent_frames() {
        let mut w = BeatWindow::new();
        let err = w.update(&[100, 100]).unwrap_err();
        assert_eq!(err, UpdateError::NotMonotonic);
    }

    #[test]
    fn update_accepts_forward_slide_with_matching_overlap() {
        let mut w = BeatWindow::new();
        w.update(&[0, 100, 200, 300, 400]).unwrap();
        // Slide forward by 2 beats; overlap [200, 300, 400] must match.
        w.update(&[200, 300, 400, 500, 600]).unwrap();
        assert_eq!(w.frames(), &[200, 300, 400, 500, 600]);
    }

    #[test]
    fn update_accepts_extension_without_slide() {
        let mut w = BeatWindow::new();
        w.update(&[0, 100, 200]).unwrap();
        // Same low end, one more beat at the high end.
        w.update(&[0, 100, 200, 300]).unwrap();
        assert_eq!(w.frames(), &[0, 100, 200, 300]);
    }

    #[test]
    fn update_rejects_immutability_violation_in_overlap() {
        let mut w = BeatWindow::new();
        w.update(&[0, 100, 200, 300, 400]).unwrap();
        // The overlap starts at frame 200 (matches), but the next entry
        // (300) is now claimed to be 310. That's an immutability bug.
        let err = w.update(&[200, 310, 400, 500, 600]).unwrap_err();
        assert!(matches!(err, UpdateError::ImmutabilityViolation { .. }));
    }

    #[test]
    fn update_rejects_phantom_frame_inside_known_window() {
        // Incoming claims a frame that falls between two of our known
        // beats but isn't one of them — contradicts what we already know.
        let mut w = BeatWindow::new();
        w.update(&[0, 100, 200, 300]).unwrap();
        let err = w.update(&[150, 250, 350, 450]).unwrap_err();
        assert!(matches!(err, UpdateError::ImmutabilityViolation { .. }));
    }

    #[test]
    fn update_rejects_backward_high_end() {
        let mut w = BeatWindow::new();
        w.update(&[0, 100, 200, 300, 400]).unwrap();
        // Incoming window's high end (350) is below current high end (400).
        let err = w.update(&[0, 100, 200, 350]).unwrap_err();
        assert!(matches!(err, UpdateError::BackwardMove { .. }));
    }

    #[test]
    fn update_accepts_clean_continuation_after_window_gap() {
        // The conductor may produce more beats than our window held, so
        // the next update we see starts strictly after our current high
        // frame. That's a clean continuation: adopt the new window.
        let mut w = BeatWindow::new();
        w.update(&[0, 100, 200]).unwrap();
        w.update(&[500, 600, 700]).unwrap();
        assert_eq!(w.frames(), &[500, 600, 700]);
    }

    #[test]
    fn update_accepts_perfectly_adjacent_non_overlapping_window() {
        let mut w = BeatWindow::new();
        w.update(&[0, 100, 200]).unwrap();
        w.update(&[300, 400, 500]).unwrap();
        assert_eq!(w.frames(), &[300, 400, 500]);
    }

    #[test]
    fn frames_per_beat_reflects_tempo_change_after_update() {
        let mut w = BeatWindow::new();
        w.update(&[0, 100, 200, 300]).unwrap();
        assert_eq!(w.frames_per_beat(), Some(100));
        // Tempo halves: next beat takes 200 frames.
        w.update(&[0, 100, 200, 300, 500]).unwrap();
        assert_eq!(w.frames_per_beat(), Some(200));
    }

    #[test]
    fn beat_position_at_handles_late_connect_anchor() {
        // Simulates a client that connects after st-conductor has been
        // running for some time: frame values are large.
        let mut w = BeatWindow::new();
        w.update(&[48_000_000, 48_024_000, 48_048_000, 48_072_000])
            .unwrap();
        // Relative position within the window.
        assert_eq!(w.beat_position_at(48_012_000), Some(0.5));
        assert_eq!(w.beat_position_at(48_036_000), Some(1.5));
        assert_eq!(w.frames_per_beat(), Some(24_000));
    }
}
