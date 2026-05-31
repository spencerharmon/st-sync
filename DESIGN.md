# st-sync — Beat-Window Protocol Design

**Status:** shipped on `main` (Mar 2026). Conductor-side tempo automation
deferred — see *Future work* below and `plan.org` in the superproject.

**Audience:** anyone writing or modifying an st-sync client (st-click,
st-loop, future scoring tools), or extending the protocol.

---

## 1. Purpose

st-sync is the single source of truth for **sample-accurate beat
timing** in the st-suite. The conductor publishes; every other JACK
client consumes.

It carries *only* timing — absolute JACK frame positions of beat
boundaries. Musical position (which bar, which beat-within-bar) is
read from JACK transport directly, which is already meter-aware.
st-sync deliberately does not duplicate that.

### What it fixes

Before this protocol, every client re-derived `frames_per_beat`,
fractional beat position, etc. from JACK transport using slightly
different math. Rounding diverged. The proximate disaster was an OOM
in st-click that sized a frame-indexed buffer using `next_beat_frame`
(an absolute sample position) as if it were `frames_per_beat`; a
conductor running for a minute before st-click connected would
allocate tens of millions of slots.

The deeper problem was *cross-client drift risk*: nothing forced
agreement on the timing facts.

Fix: publish the facts once, authoritatively. Every consumer of
`Client::frames_per_beat()` gets bit-identical answers by
construction.

---

## 2. Wire format

```
message := u8  length
           length × u64 frame  (little-endian)
```

- `length` ≤ 255 (constrained by the `u8` prefix; far larger than any
  realistic `beats_per_bar + 1`).
- `frame` is an **absolute JACK sample position**.
- Frames within a message are strictly increasing, oldest first.
- The first entry is the most recently completed beat; the last is
  the furthest-future beat the conductor has committed to.

Little-endian was chosen for portability hygiene; cost is negligible
on x86.

There is currently **no version byte at connect**. Single coordinated
cutover across the monorepo was simpler. Add a version byte the moment
a non-monorepo consumer appears.

### Transport

TCP, localhost-only, hard-coded to `127.0.0.1:6142`. No authentication,
no TLS, no discovery. st-suite is a single-user live-performance rig;
the network is the loopback interface.

Multiple messages may be concatenated in a single TCP read. The
decoder (`wire::decode`) returns `Ok(None)` on a partial buffer and
the number of bytes consumed on success, so the caller can drain a
buffer message-by-message.

---

## 3. Protocol guarantees

These are properties the **conductor** upholds; the wire format itself
enforces none of them. Consumers may rely on all three.

### 3.1 Immutability

Once a beat frame appears in any published window, that frame is
fixed for the lifetime of the session. The conductor may not
retroactively change the frame position of any published beat,
regardless of tempo changes from any source (user, score, queue).

### 3.2 Monotonicity

Beat frames are strictly increasing within a window, and across
windows. The transport never moves backward.

st-suite is a **live performance environment**. There is no
scrubbing, no jumping forward, no rewind. Musical repeats are
possible but must inherently occur at future frames.

### 3.3 Forward extension only

Successive windows may add new beats at the high end and drop old
beats at the low end (sliding forward), but never rewrite middle
entries.

### 3.4 What consumers may assume

- `frames_per_beat` for any pair of adjacent published beats is
  **exact** and **agreed-on by every client** — it's just the
  subtraction of two facts received over the wire.
- `beat_position_at(frame)` for any frame whose surrounding beats are
  in the window is well-defined and stable: linear interpolation
  between adjacent published beats. Result is "beats past `frames[0]`"
  — a *relative* position within the window, not an absolute musical
  index. (Get musical position from JACK.)
- Beat positions inside the window will **not change**. Events
  scheduled against the window will not drift, double-fire, or be
  missed due to retroactive tempo updates.
- The transport will never move backward. Components need no
  defensive code for rewind / scrub / jump-back; if a past frame is
  needed, the component is responsible for caching it (e.g. st-loop
  records into a buffer).

---

## 4. Window sizing

- `Controller::new()` default capacity: **8**.
- st-conductor calls `Controller::with_capacity(numerator + 1)` —
  one full bar of lookahead plus the previous beat for
  `frames_per_beat` derivation.
- Fixed at compile time for v1. Negotiable at handshake later if a
  consumer wants more lookahead than `beats_per_bar + 1`.

The lookahead cap is deliberate: it bounds the responsiveness horizon
of a future tempo-change input to roughly one bar. A larger window
would let consumers schedule further ahead but would delay any
user-initiated tempo change by however many beats are already on the
wire (since those beats can't be retroactively re-priced — that would
break immutability).

---

## 5. Architecture

### 5.1 Controller (producer)

```
┌───────────────────────────────────────────────────────┐
│ st-conductor (timebase master)                        │
│                                                       │
│   JACK timebase callback                              │
│     ↓ send_next_beat_frame(u64)                       │
│   Controller (sync API, callable from any thread)     │
│     ↓ crossbeam::channel                              │
│   ─────────────────────────────────────────────       │
│   async run_broadcaster                               │
│     ├─ BeatPublisher       (sliding window, pure)     │
│     ├─ wire::encode                                   │
│     └─ per-client tokio::mpsc → TCP writer task       │
│                                                       │
│   async accept_loop                                   │
│     └─ on each new TCP client:                        │
│        • spawn writer task                            │
│        • register sink with broadcaster               │
│        • broadcaster seeds the new client with the    │
│          current window before any new beat arrives   │
└───────────────────────────────────────────────────────┘
                        │ TCP :6142
                        ↓
              (one connection per client)
```

Key modules:

- **`broadcast.rs`** — `BeatPublisher`, the sliding window. Pure
  logic, no I/O. `record_beat(frame)` panics on non-monotonic input
  (producer bug — surface loudly).
- **`controller.rs`** — TCP shell. Drops non-monotonic input from the
  sync API rather than panicking the broadcaster (defensive at the
  process boundary).
- **`wire.rs`** — `encode` / `decode`, single source of truth for
  the byte layout.

### 5.2 Client (consumer)

```
┌───────────────────────────────────────────────────────┐
│ Consumer process (st-click, st-loop, …)               │
│                                                       │
│   tokio task: run_receiver                            │
│     ├─ TCP read → buffer                              │
│     ├─ wire::decode (drains all complete msgs)        │
│     └─ BeatWindow::update (enforces 3 guarantees)     │
│                                                       │
│   Arc<Mutex<BeatWindow>>                              │
│     ↑ Client API (sync, thread-safe)                  │
│       • frames_per_beat() -> Option<u64>              │
│       • beat_position_at(frame) -> Option<f64>        │
│       • snapshot() -> WindowSnapshot                  │
│       • is_empty() -> bool                            │
└───────────────────────────────────────────────────────┘
```

Key modules:

- **`window.rs`** — `BeatWindow`. Pure logic. `update(&[u64])`
  validates monotonicity, forward-extension, and overlap-immutability
  before adopting the new window. Returns `UpdateError` on violation
  (logged by the client, does not tear down the connection).
- **`client.rs`** — async/TCP shell. Spawns the receiver, holds the
  shared `Arc<Mutex<BeatWindow>>`, exposes the sync read API.

The mutex is uncontended in practice (one writer task, infrequent
short reads from the audio thread). If profiling ever shows
contention, swap for `arc-swap` or a seqlock — both preserve the
existing API.

---

## 6. Tempo & meter changes

### 6.1 The immutability constraint, restated

A tempo change cannot retroactively alter beat frames already on the
wire. Any tempo-change input therefore takes effect **after the last
beat already published**.

### 6.2 Policy 3 — "stop extending"

When a tempo change is requested and the queued change can't take
effect until after the current window drains, the conductor **stops
extending the window** until the change's effective beat is reached,
then resumes publication using the new tempo. Clients see the window
naturally shrink, then re-grow.

This was chosen over two alternatives:

- *Truncate / rewrite the window* — violates immutability outright.
  Rejected.
- *Apply the change at the next beat regardless* — works for the
  publication side, but the consumer's scheduled events between
  "now" and "next beat" are still computed against the old tempo;
  this creates a one-beat window of disagreement between the
  scheduler and the audible click. Rejected as too subtle.

### 6.3 What's actually implemented today

The protocol supports policy 3, but **the conductor doesn't yet have
a tempo-change input** beyond its startup config. Live tempo edits,
scored ritardandi, and tap-tempo are all deferred — see *Future work*.

### 6.4 Meter changes

The protocol carries no meter information; `beats_per_bar` is read
from JACK transport per-cycle by each consumer. The mid-recording
meter bug in st-loop (using a stale `beats_per_bar` captured at
sequence construction) was fixed in the same PR set by refreshing
from each fanout message before any record/stop logic.

---

## 7. Failure modes & recovery

| Failure | Behavior |
|---|---|
| Conductor sends non-monotonic frame from sync API | Controller drops it silently (logging would happen on the realtime thread). `BeatPublisher` would panic if reached; the drop guards this. |
| Client receives non-monotonic / immutability-violating window | `BeatWindow::update` returns `UpdateError`. Client logs and continues with the previous window. Does *not* tear down the connection — a transient bad window should not blank the audio thread. |
| TCP read returns 0 (controller closed) | Client receiver task exits with `UnexpectedEof`. Window keeps its last value; consumers see beat timing freeze. No auto-reconnect today. |
| Late client connect | Controller seeds the new connection with the current window before any new beat. Client's first `snapshot()` returns the full window. |
| Wire buffer holds partial message | `wire::decode` returns `Ok(None)`; receiver loops to read more. |
| Wire buffer holds multiple messages | Receiver drains them in a tight loop; only the *last* `BeatWindow::update` reflects in observable state. Earlier updates are validated but their facts are subsumed. |

---

## 8. Testing

99 tests across the suite at the time of writing.

- **`st-sync` unit (38):** wire encode/decode roundtrips including
  concatenated messages and oversize rejection; `BeatPublisher`
  sliding-window correctness and monotonicity panics; `BeatWindow`
  immutability/monotonicity/forward-extension enforcement,
  `beat_position_at` interpolation including late-connect anchors and
  continuous tempo change.
- **`st-sync` integration (3, `#[ignore]` — port 6142 is exclusive):**
  TCP round-trip with capacity sliding; late-connect mid-session seed;
  continuous tempo change reflected at client.
- **`st-conductor` (10):** monotonicity stress over 4000 cycles at
  120 BPM / 48 kHz / 4/4 and 8000 cycles at 240 BPM / 48 kHz / 7/8
  with 256-frame buffers (stress odd meter + fast boundary crossings).
- **`st-click` (37):** scheduler float-precision (event at integer
  beat fires exactly once across 10,000 drifting cycles), wrap, multi-
  wrap cycles, continuous tempo change, boundary tempo jump, late-
  connect anchoring, and the explicit `sequence_memory_independent_of_tempo_or_runtime`
  regression guard against the original OOM.
- **`st-loop` (4):** stop-on-bar-boundary, mid-bar round-up (4/4 and
  7/8), and `stop_recording_uses_live_meter_after_signature_change`
  (regression for the meter-change bug).

Run integration tests serially:

```sh
cargo test --test roundtrip -- --ignored --test-threads=1
```

---

## 9. Future work (deferred)

Tracked in `plan.org` under *Timing & sync architecture — beat-window
protocol [3/4]* → *Deferred to a follow-up PR*.

### 9.1 Conductor score-envelope tempo model

- Internal `Vec<(beat_index, bpm, beats_per_bar)>` envelope.
- Beat publisher consults the envelope when extending the window.
- Tempo-change API: append-to-end of envelope; never rewrite past or
  already-published entries.
- Honor policy 3 (§ 6.2) when a queued change can't fit before the
  window drains.

### 9.2 Live tempo input UX

Tap-tempo / set-tempo controls in the conductor GUI, hooked up as
envelope-append inputs. Not blocked by anything else.

### 9.3 NSM persistence

When NSM Save/Open lands for st-conductor, the score envelope needs
to round-trip to disk. Clients still need persist no derived timing
state — the envelope is canonical.

### 9.4 Open questions (not blocking)

- **Window-size negotiation at handshake.** Defer until a consumer
  wants more lookahead than `beats_per_bar + 1`.
- **Higher-order interpolation in `beat_position_at`.** Linear is
  sufficient for the metronome and looper. A dense sub-beat
  sequencer might want cubic. Defer until that consumer exists.
- **Protocol version byte at connect.** Skip until a non-monorepo
  consumer appears; cheap to add then.

---

## 10. References

- Source: `st-sync/src/{wire,window,broadcast,controller,client}.rs`
- Plan & history: `plan.org` § *Timing & sync architecture*
- Suite orientation: `AGENTS.md`
- Original OOM bug & design discussion that drove this rework: see
  the *Background* subsection in `plan.org`.
