//! End-to-end round-trip tests for the st-sync TCP protocol.
//!
//! st-sync intentionally hardcodes `127.0.0.1:6142`, so these tests cannot
//! run in parallel with anything else that binds the same port (including
//! a real st-conductor) or with each other. Each test is `#[ignore]`d by
//! default; run a single test with:
//!
//!     cargo test --test roundtrip <test_name> -- --ignored
//!
//! or all of them serially with:
//!
//!     cargo test --test roundtrip -- --ignored --test-threads=1

use std::time::Duration;

/// Wait until `cond()` returns `Some`, polling at 20ms intervals up to
/// `timeout_ms`. Returns the produced value or panics with `panic_msg`.
async fn wait_until<T, F>(timeout_ms: u64, panic_msg: &str, mut cond: F) -> T
where
    F: FnMut() -> Option<T>,
{
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if let Some(v) = cond() {
            return v;
        }
        if std::time::Instant::now() >= deadline {
            panic!("{}", panic_msg);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn controller_to_client_windowed_roundtrip() {
    let controller = st_sync::controller::Controller::with_capacity(4);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send two beats *before* the client connects to ensure the seed-on-
    // connect path delivers them.
    controller.send_next_beat_frame(1000);
    controller.send_next_beat_frame(2000);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = st_sync::client::Client::new();

    // Client should receive the seed window within a reasonable time.
    let snap = wait_until(2000, "client never received seed window", || {
        let s = client.snapshot();
        if s.frames.len() >= 2 { Some(s) } else { None }
    })
    .await;
    assert_eq!(snap.frames, vec![1000u64, 2000]);
    assert_eq!(client.frames_per_beat(), Some(1000));

    // Send three more beats. The window capacity is 4, so after recording
    // 5 beats the client should see the last 4.
    controller.send_next_beat_frame(3000);
    controller.send_next_beat_frame(4000);
    controller.send_next_beat_frame(5000);

    let snap = wait_until(2000, "client never saw post-slide window", || {
        let s = client.snapshot();
        if s.frames == vec![2000u64, 3000, 4000, 5000] { Some(s) } else { None }
    })
    .await;
    assert_eq!(snap.frames, vec![2000, 3000, 4000, 5000]);
    assert_eq!(client.frames_per_beat(), Some(1000));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn client_connects_mid_session_and_gets_seed_window() {
    // Long-running controller scenario: many beats published before any
    // client connects. The client must receive whatever window the
    // controller currently holds, with correct frame values.
    let controller = st_sync::controller::Controller::with_capacity(5);
    tokio::time::sleep(Duration::from_millis(100)).await;

    for f in [10_000u64, 20_000, 30_000, 40_000, 50_000, 60_000, 70_000, 80_000] {
        controller.send_next_beat_frame(f);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = st_sync::client::Client::new();
    let snap = wait_until(2000, "late client never received seed", || {
        let s = client.snapshot();
        if s.frames.len() >= 5 { Some(s) } else { None }
    })
    .await;
    // 8 beats recorded, capacity 5 → window holds last 5.
    assert_eq!(snap.frames, vec![40_000u64, 50_000, 60_000, 70_000, 80_000]);

    // beat_position_at interpolates cleanly within the window (relative
    // position; "musical position" is JACK's responsibility).
    assert_eq!(client.beat_position_at(45_000), Some(0.5));
    assert_eq!(client.beat_position_at(75_000), Some(3.5));
    assert_eq!(client.frames_per_beat(), Some(10_000));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn client_reflects_continuous_tempo_change() {
    // Stepwise accelerando: each beat takes fewer frames than the last.
    let controller = st_sync::controller::Controller::with_capacity(4);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = st_sync::client::Client::new();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // beats at 0, 1000, 1800, 2500, 3100 (deltas 1000, 800, 700, 600).
    let beats = [0u64, 1000, 1800, 2500, 3100];
    let expected_fpb = [None, Some(1000u64), Some(800), Some(700), Some(600)];

    for (i, f) in beats.iter().enumerate() {
        controller.send_next_beat_frame(*f);
        let want = expected_fpb[i];
        if let Some(w) = want {
            wait_until(2000, "frames_per_beat never reached expected value", || {
                if client.frames_per_beat() == Some(w) { Some(()) } else { None }
            })
            .await;
        }
    }
}
