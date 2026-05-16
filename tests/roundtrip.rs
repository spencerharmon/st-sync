//! End-to-end round-trip test for the st-sync TCP protocol.
//!
//! st-sync intentionally hardcodes `127.0.0.1:6142`, so this test cannot run
//! in parallel with anything else that binds the same port (including a real
//! st-conductor). It is `#[ignore]`d by default; run with:
//!
//!     cargo test --test roundtrip -- --ignored
//!
//! It exercises the actual wire format: little-endian u64 beat frames pushed
//! from `Controller` to a `Client` over TCP.

use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn controller_to_client_roundtrip() {
    let controller = st_sync::controller::Controller::new();

    // Give the listener a moment to bind.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Client::new() is blocking, so run it on a dedicated thread.
    let client = tokio::task::spawn_blocking(|| st_sync::client::Client::new())
        .await
        .expect("client thread join");

    // Give the client a moment to connect & for the controller to register it.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Send a sequence of frames; the bounded(1) latest-wins semantics mean
    // we expect to receive *at least one* frame, and the last one should be
    // observable if we drain.
    for frame in [42u64, 100, 12345, 9_999_999_999] {
        controller.send_next_beat_frame(frame);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Receive on a blocking thread (Client API is sync).
    let got = tokio::task::spawn_blocking(move || {
        // Drain whatever is available, return the last value seen.
        let mut last = None;
        for _ in 0..10 {
            match client.try_recv_next_beat_frame() {
                Ok(v) => last = Some(v),
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        last
    })
    .await
    .expect("recv thread join");

    assert!(got.is_some(), "client should have received at least one frame");
}
