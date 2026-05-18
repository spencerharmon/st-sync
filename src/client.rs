//! TCP consumer: connects to the controller at `127.0.0.1:6142`, receives
//! beat-frame windows, maintains a [`BeatWindow`], and exposes a
//! thread-safe read API to consumers (the audio thread, GUI, etc.).
//!
//! Pure window logic lives in [`crate::window`] (`BeatWindow`) and
//! [`crate::wire`] (decoding); this module is the async / TCP shell.

use crate::wire;
use crate::window::BeatWindow;
use std::io;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

pub const CONNECT_ADDR: &str = "127.0.0.1:6142";

/// Read-only snapshot of the most recent window. Returned by
/// [`Client::snapshot`] for callers that want to reason about multiple
/// beats in one consistent read without holding the lock across work.
#[derive(Debug, Clone)]
pub struct WindowSnapshot {
    pub frames: Vec<u64>,
}

/// Shared mutable state between the receiver task and the caller's threads.
type SharedWindow = Arc<Mutex<BeatWindow>>;

pub struct Client {
    window: SharedWindow,
}

impl Client {
    /// Construct a client and spawn its receiver task on the current
    /// tokio runtime. Returns immediately; the window is empty until the
    /// first message arrives.
    pub fn new() -> Client {
        let window = Arc::new(Mutex::new(BeatWindow::new()));
        let task_window = window.clone();
        tokio::spawn(async move {
            if let Err(e) = run_receiver(task_window).await {
                eprintln!("st-sync client: {}", e);
            }
        });
        Client { window }
    }

    /// `frames_per_beat` of the most recent published pair of beats.
    /// `None` if fewer than two beats are known.
    pub fn frames_per_beat(&self) -> Option<u64> {
        self.window.lock().unwrap().frames_per_beat()
    }

    /// Fractional position (in beats past the window's first entry)
    /// corresponding to `frame`. `None` if the window is empty or
    /// `frame` falls outside it.
    ///
    /// This is a *relative* position within the window. Callers that
    /// need musical position (bar / beat-within-bar) read it from JACK
    /// transport.
    pub fn beat_position_at(&self, frame: u64) -> Option<f64> {
        self.window.lock().unwrap().beat_position_at(frame)
    }

    /// Snapshot the current window.
    pub fn snapshot(&self) -> WindowSnapshot {
        let w = self.window.lock().unwrap();
        WindowSnapshot {
            frames: w.frames().to_vec(),
        }
    }

    /// True iff no window has been received yet.
    pub fn is_empty(&self) -> bool {
        self.window.lock().unwrap().is_empty()
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

async fn run_receiver(window: SharedWindow) -> io::Result<()> {
    let mut stream = TcpStream::connect(CONNECT_ADDR).await?;
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut scratch = [0u8; 1024];
    loop {
        let n = stream.read(&mut scratch).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "controller closed connection",
            ));
        }
        buf.extend_from_slice(&scratch[..n]);

        // Drain as many complete messages as the buffer holds.
        loop {
            match wire::decode(&buf)? {
                None => break,
                Some((frames, consumed)) => {
                    {
                        let mut w = window.lock().unwrap();
                        // Update may fail under a buggy controller; log and
                        // continue rather than tear down the client.
                        if let Err(e) = w.update(&frames) {
                            eprintln!("st-sync client: window update rejected: {}", e);
                        }
                    }
                    buf.drain(..consumed);
                }
            }
        }
    }
}
