//! Pure-logic core of the controller's beat-window broadcasting.
//!
//! This module knows nothing about TCP, async, or channels — it just
//! maintains the sliding window of recently-published beat frames and
//! exposes a single operation: `record_beat(frame)`, which records a new
//! beat and returns the current window suitable for broadcasting.
//!
//! Separating this from the network shell makes the protocol's core
//! invariants (immutability, monotonicity, forward-extension) directly
//! unit-testable, and gives the conductor a place to query "what's our
//! committed timeline right now?" without going through the network.

/// Sliding-window publisher. Holds the most recent `capacity` beat frames.
///
/// A fresh `BeatPublisher` is empty; the first `record_beat` call seeds
/// the window. Subsequent calls extend the window forward, dropping the
/// oldest entries once the window reaches its capacity.
#[derive(Debug)]
pub struct BeatPublisher {
    capacity: usize,
    frames: Vec<u64>,
}

impl BeatPublisher {
    /// Construct a publisher that will keep at most `capacity` beat frames
    /// in its window. Must be `>= 2` (a single-beat window can't express
    /// frames-per-beat to consumers).
    pub fn new(capacity: usize) -> BeatPublisher {
        assert!(capacity >= 2, "BeatPublisher capacity must be >= 2");
        BeatPublisher {
            capacity,
            frames: Vec::with_capacity(capacity),
        }
    }

    /// Record a new beat at the given absolute frame position. Returns
    /// the current window snapshot (the slice the controller should
    /// broadcast).
    ///
    /// # Panics
    /// Panics if `frame` is not strictly greater than the last recorded
    /// beat — violating monotonicity. This indicates a bug in the
    /// producer (e.g. st-conductor's timebase callback feeding stale
    /// data) and must surface loudly rather than silently desync the
    /// suite.
    pub fn record_beat(&mut self, frame: u64) -> &[u64] {
        if let Some(&last) = self.frames.last() {
            assert!(
                frame > last,
                "BeatPublisher: non-monotonic beat (last={}, new={}). \
                 The transport must move forward.",
                last,
                frame
            );
        }
        self.frames.push(frame);
        if self.frames.len() > self.capacity {
            let drop = self.frames.len() - self.capacity;
            self.frames.drain(..drop);
        }
        &self.frames
    }

    /// Current window as a slice. Empty before the first `record_beat`.
    pub fn window(&self) -> &[u64] {
        &self.frames
    }

    /// True if no beats have been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_publisher_is_empty() {
        let p = BeatPublisher::new(4);
        assert!(p.is_empty());
        assert_eq!(p.window(), &[] as &[u64]);
    }

    #[test]
    fn record_beat_seeds_window() {
        let mut p = BeatPublisher::new(4);
        let w = p.record_beat(1000).to_vec();
        assert_eq!(w, vec![1000]);
    }

    #[test]
    fn record_beat_extends_until_capacity() {
        let mut p = BeatPublisher::new(4);
        p.record_beat(100);
        p.record_beat(200);
        p.record_beat(300);
        let w = p.record_beat(400).to_vec();
        assert_eq!(w, vec![100, 200, 300, 400]);
    }

    #[test]
    fn record_beat_slides_after_capacity_reached() {
        let mut p = BeatPublisher::new(4);
        for f in [100, 200, 300, 400, 500] {
            p.record_beat(f);
        }
        // Window should now hold the last 4.
        assert_eq!(p.window(), &[200, 300, 400, 500]);
    }

    #[test]
    fn record_beat_continues_sliding_correctly() {
        let mut p = BeatPublisher::new(3);
        for f in [100u64, 200, 300, 400, 500, 600] {
            p.record_beat(f);
        }
        assert_eq!(p.window(), &[400, 500, 600]);
    }

    #[test]
    #[should_panic(expected = "non-monotonic beat")]
    fn record_beat_panics_on_backward_frame() {
        let mut p = BeatPublisher::new(4);
        p.record_beat(1000);
        p.record_beat(500); // backward — must panic
    }

    #[test]
    #[should_panic(expected = "non-monotonic beat")]
    fn record_beat_panics_on_repeated_frame() {
        let mut p = BeatPublisher::new(4);
        p.record_beat(1000);
        p.record_beat(1000); // not strictly increasing — must panic
    }

    #[test]
    #[should_panic(expected = "capacity must be >= 2")]
    fn capacity_one_is_rejected() {
        BeatPublisher::new(1);
    }

    #[test]
    fn capacity_two_works() {
        let mut p = BeatPublisher::new(2);
        p.record_beat(100);
        p.record_beat(200);
        p.record_beat(300);
        assert_eq!(p.window(), &[200, 300]);
    }

    #[test]
    fn record_beat_returns_current_window_each_time() {
        // The slice returned from record_beat must equal window() after.
        let mut p = BeatPublisher::new(3);
        for f in [10u64, 20, 30, 40, 50] {
            let returned: Vec<u64> = p.record_beat(f).to_vec();
            assert_eq!(returned, p.window().to_vec());
        }
    }
}
