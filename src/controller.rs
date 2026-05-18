//! TCP broadcaster: accepts client connections on `127.0.0.1:6142`, sends
//! the current beat-frame window to each connected client whenever a new
//! beat is recorded, and seeds new clients with the current window
//! immediately on connect.
//!
//! Pure protocol logic lives in [`crate::broadcast`] (`BeatPublisher`) and
//! [`crate::wire`] (encoding); this module is the async / TCP shell.

use crate::broadcast::BeatPublisher;
use crate::wire;
use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// Default bind address. Localhost-only by design (see plan.org).
pub const BIND_ADDR: &str = "127.0.0.1:6142";

/// Default window capacity. `max(4, beats_per_bar + 1)` is the stated
/// policy; for callers that don't know `beats_per_bar` at construction
/// time, this is a generous default that fits any common meter.
pub const DEFAULT_CAPACITY: usize = 8;

/// Per-client snapshot dispatched by the broadcaster thread: the bytes to
/// send and a one-shot way to learn the client died.
type ClientSink = mpsc::Sender<Vec<u8>>;

/// Internal commands sent from `Controller::send_next_beat_frame` (a sync
/// API callable from any thread) to the async broadcaster task.
enum BroadcastCommand {
    NewBeat(u64),
}

pub struct Controller {
    cmd_tx: Sender<BroadcastCommand>,
}

impl Controller {
    /// Build a controller with the default window capacity ([`DEFAULT_CAPACITY`]).
    pub fn new() -> Controller {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// Build a controller publishing windows of at most `capacity` beats.
    /// Must be `>= 2`.
    pub fn with_capacity(capacity: usize) -> Controller {
        // Sync channel for `send_next_beat_frame` callers. Bounded; if the
        // broadcaster falls behind, callers (the timebase callback) block —
        // but the broadcaster does no I/O on this path, so backup is
        // vanishingly unlikely.
        let (cmd_tx, cmd_rx) = bounded::<BroadcastCommand>(16);

        tokio::spawn(async move {
            run_broadcaster(capacity, cmd_rx).await;
        });

        Controller { cmd_tx }
    }

    /// Record a beat that just (or just-about-to) occurred at the given
    /// absolute JACK frame position. Broadcasts the updated window to all
    /// connected clients.
    ///
    /// Callable from any thread (e.g. JACK's realtime timebase callback,
    /// via a wrapper that ensures no allocation happens on the RT thread).
    pub fn send_next_beat_frame(&self, next_beat_frame: u64) {
        let _ = self.cmd_tx.send(BroadcastCommand::NewBeat(next_beat_frame));
    }
}

impl Default for Controller {
    fn default() -> Self {
        Self::new()
    }
}

/// The async broadcaster: owns the `BeatPublisher`, listens for new TCP
/// clients, and pushes the current window to each on every new beat or
/// new connection.
async fn run_broadcaster(capacity: usize, cmd_rx: Receiver<BroadcastCommand>) {
    let mut publisher = BeatPublisher::new(capacity);

    // Channel through which the acceptor task hands new clients to the
    // broadcaster. Each entry is a sender to that client's outbound queue.
    let (new_client_tx, mut new_client_rx) = mpsc::unbounded_channel::<ClientSink>();

    // Spawn the TCP accept loop.
    tokio::spawn(accept_loop(new_client_tx));

    let mut clients: Vec<ClientSink> = Vec::new();
    loop {
        // Drain any new clients first so they see the current window even
        // if no new beat has arrived yet.
        while let Ok(sink) = new_client_rx.try_recv() {
            if !publisher.is_empty() {
                if let Ok(bytes) = wire::encode(publisher.window()) {
                    let _ = sink.send(bytes).await;
                }
            }
            clients.push(sink);
        }

        // Drain any pending beat commands.
        let mut had_new_beat = false;
        loop {
            match cmd_rx.try_recv() {
                Ok(BroadcastCommand::NewBeat(frame)) => {
                    // Guard against non-monotonic frames (e.g. stale data
                    // from a confused timebase callback). Silently drop
                    // rather than panic the broadcaster.
                    let ok = publisher
                        .window()
                        .last()
                        .map(|&last| frame > last)
                        .unwrap_or(true);
                    if ok {
                        publisher.record_beat(frame);
                        had_new_beat = true;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if had_new_beat {
            if let Ok(bytes) = wire::encode(publisher.window()) {
                clients.retain(|sink| {
                    // try_send: drop dead clients without blocking.
                    sink.try_send(bytes.clone()).is_ok()
                });
            }
        }

        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

async fn accept_loop(new_client_tx: mpsc::UnboundedSender<ClientSink>) {
    let listener = match TcpListener::bind(BIND_ADDR).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("st-sync: failed to bind {}: {}", BIND_ADDR, e);
            return;
        }
    };
    loop {
        let (mut socket, _peer) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("st-sync: accept error: {}", e);
                continue;
            }
        };

        // Per-client outbound queue. The acceptor task spawns a writer
        // that drains this queue to the socket; the broadcaster task
        // sends pre-encoded bytes into it.
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Vec<u8>>(16);
        tokio::spawn(async move {
            while let Some(bytes) = outbound_rx.recv().await {
                if let Err(_) = socket.write_all(&bytes).await {
                    break;
                }
            }
            let _ = socket.shutdown().await;
        });

        let _ = new_client_tx.send(outbound_tx);
    }
}
