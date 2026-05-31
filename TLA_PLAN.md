# Plan — Model the beat-window protocol in TLA+

**Goal.** Build a TLA+ spec (`BeatWindow.tla`) that formally captures the
three protocol guarantees from `DESIGN.md` (immutability, monotonicity,
forward extension) plus the liveness expectations on consumers, and
check it with TLC.

**Why now.** The protocol is shipped and the Rust code has 99 tests, but
those tests are case-driven. The guarantees are universally quantified
("**any** published beat frame, **for the lifetime of the session**…").
TLC can exhaust the state space we actually care about (small windows,
small frame deltas, a handful of consumers) in a way unit tests
structurally cannot. Cheap insurance before the conductor grows
score-envelope tempo automation — exactly the layer that's hardest to
test by example because it's where immutability can quietly break.

**Influence.** Methodology cribbed from
<https://blog.graysonhead.net/posts/tla-plus/>:

- Preconditions in actions ↔ runtime guards in Rust (already present in
  `BeatPublisher::record_beat` and `BeatWindow::update`).
- Invariants ↔ assertions / unit tests (`MoneyConserved` ≈ our
  `Immutability`).
- Error traces ↔ free regression tests (cherry-pick TLC counterexamples
  into Rust tests).
- *Scope is a deliberate choice*; abstract aggressively, especially
  the parts the wire format already isolates (TCP framing, byte
  layout, JACK transport, audio thread).

---

## 0. Bootstrap (one afternoon)

- [ ] **Install TLC.**
  ```sh
  pacman -S --needed jre-openjdk            # Toolbox needs Java
  # Headless: grab tla2tools.jar from
  # https://github.com/tlaplus/tlaplus/releases
  curl -L -o ~/.local/share/tla2tools.jar \
      https://github.com/tlaplus/tlaplus/releases/latest/download/tla2tools.jar
  alias tlc='java -XX:+UseParallelGC -jar ~/.local/share/tla2tools.jar -cp ~/.local/share/tla2tools.jar tlc2.TLC'
  alias sany='java -cp ~/.local/share/tla2tools.jar tla2sany.SANY'
  ```
- [ ] **Repo layout.** Put specs in `st-sync/tla/`:
  ```
  st-sync/tla/
    BeatWindow.tla        # the model
    BeatWindow.cfg        # TLC config (constants, invariants, properties)
    MCSmall.cfg           # tiny constants for fast iteration
    MCStress.cfg          # larger constants for overnight runs
    README.md             # how to run
  ```
- [ ] **Editor.** `tlaplus.vscode-ide` (Microsoft) or just plain text +
  `sany` from the CLI. No need for the full Toolbox unless you want
  the trace explorer GUI.
- [ ] **Smoke test.** Reproduce the post's `bank_transfer.tla`, run it
  locally, confirm output matches. (Quick TLA syntax / TLC plumbing
  check before doing real work.)

---

## 1. Scope decisions (write these down in the spec's header)

What we **model**:

- The controller's published window (sliding buffer of `Nat` frames).
- The wire as a logical message stream — every produced window
  appears at every consumer, no bytes, no framing, no TCP.
- A small fixed pool of consumers (`N = 2` is enough; symmetry across
  consumers is what we want to verify, not scale).
- A nondeterministic *beat producer* that emits monotonically-
  increasing frames with bounded inter-beat deltas.
- Late connect: a consumer may join at any step.
- Optional disconnect/reconnect (for v2 of the spec).

What we **abstract away** (these are unit-test territory):

- Byte layout (`wire::encode/decode` is exhaustively unit-tested
  already; modeling little-endian in TLA+ buys nothing).
- TCP partial reads, buffer drain logic (same — already covered).
- JACK sample-rate, BPM, meter (we model frames as opaque Nats; the
  conductor's job to choose them).
- Real time (TLA+ models *order of events*, not wall clock).
- Mutex / async machinery (model as atomic state transitions).

What we **defer to a v2 spec**:

- Conductor score-envelope tempo model with policy 3 ("stop
  extending"). This is the next-big-feature and is the *exact*
  reason to have the spec — but model it after v1 catches anything
  in the shipped protocol first.

---

## 2. State variables (v1)

```tla
CONSTANTS
    MAX_FRAME,        \* upper bound on frames TLC will explore
    MAX_DELTA,        \* max frames per beat (bounds tempo from below)
    MIN_DELTA,        \* min frames per beat (bounds tempo from above; > 0)
    WINDOW_CAP,       \* sliding-window capacity (BeatPublisher capacity)
    CONSUMERS,        \* set of consumer IDs, e.g. {c1, c2}
    MAX_BEATS         \* termination bound: stop after this many beats produced

VARIABLES
    published,        \* Seq(Nat) -- every beat frame the controller has ever
                      \*   committed (grows monotonically, used as oracle for
                      \*   immutability checks; not in the real implementation)
    window,           \* Seq(Nat) -- the controller's current sliding window
    consumer_window,  \* [CONSUMERS -> Seq(Nat)] -- each consumer's view
    consumer_joined,  \* [CONSUMERS -> BOOLEAN] -- has c received any window yet
    msg_count         \* monotonic clock-substitute for fairness reasoning
```

`published` is a **history variable**: it doesn't exist in the Rust
code, but it lets us state immutability ("for every frame ever
published, it's still at the same index") without quantifying over
unbounded history. Standard TLA+ technique.

---

## 3. Actions

| Action | Precondition | Effect | Rust analog |
|---|---|---|---|
| `ProduceBeat` | `Len(published) < MAX_BEATS` ∧ next frame in `(MIN_DELTA, MAX_DELTA]` past last published | append to `published`, slide `window` | `Controller::send_next_beat_frame` → `BeatPublisher::record_beat` |
| `ConsumerConnect(c)` | `¬consumer_joined[c]` | set `consumer_joined[c] := TRUE`, seed `consumer_window[c] := window` | `accept_loop` + initial seed |
| `DeliverWindow(c)` | `consumer_joined[c]` ∧ `consumer_window[c] ≠ window` | `consumer_window[c] := window` *via `BeatWindow::update` semantics* (must satisfy overlap-immutability or this is a bug we want TLC to surface) | `run_receiver` → `BeatWindow::update` |
| `Stutter` | always | no-op | (TLA+ needs this for `[][Next]_vars` closure) |

Notes:

- `DeliverWindow` is where **the spec must encode the same overlap-
  validation logic** as `BeatWindow::update`. If our implementation's
  validation is *stricter* than the protocol requires, modeling the
  loose version reveals over-conservative consumer code. If it's
  *looser*, TLC will catch it via the safety invariants below.
- We do **not** model packet loss in v1 — TCP is reliable on
  loopback, and modeling drops just inflates the state space without
  surfacing anything the real protocol needs to handle. (Add for v2
  if/when we go off-loopback.)

---

## 4. Invariants (safety)

Each maps directly to a property in `DESIGN.md` § 3.

```tla
\* §3.1 Immutability: nothing the controller ever published changes index.
Immutability ==
    \A i \in 1..Len(published) :
        \/ i > Len(window)                              \* slid out
        \/ window[Len(window) - (Len(published) - i)]   \* still in window
              = published[i]

\* §3.2 Monotonicity: strictly increasing within window, within published,
\*   and across every consumer's last-seen window.
StrictlyIncreasing(s) ==
    \A i \in 1..Len(s)-1 : s[i] < s[i+1]

Monotonicity ==
    /\ StrictlyIncreasing(published)
    /\ StrictlyIncreasing(window)
    /\ \A c \in CONSUMERS : StrictlyIncreasing(consumer_window[c])

\* §3.3 Forward extension: the controller's window high end never moves down.
\*   (Tracked across steps using a history variable `prev_window_high`,
\*   or via TLA+'s [Next]_vars notation.)
ForwardExtensionOnly ==
    [][window' = <<>> \/ Last(window') >= IF window = <<>> THEN 0 ELSE Last(window)]_vars

\* §3.4 Consumer agreement: any two consumers that have received any window
\*   agree on every frame they both have.
ConsumerAgreement ==
    \A c1, c2 \in CONSUMERS :
        \A i \in 1..Len(consumer_window[c1]) :
            \A j \in 1..Len(consumer_window[c2]) :
                consumer_window[c1][i] = consumer_window[c2][j] =>
                    (* same frame value => same offset from each window's
                       low end relative to published[] *) TRUE   \* sketch

\* Window capacity bound (the BeatPublisher::new(capacity) contract).
WindowBounded == Len(window) <= WINDOW_CAP
```

`Immutability` is the load-bearing one. If we ever add policy 3
(stop-extending) and it has a bug, this is the invariant TLC will trip.

---

## 5. Liveness properties

```tla
\* Every connected consumer eventually sees every beat the controller
\* committed (modulo the ones that slid out of the window before they joined).
EventualDelivery ==
    \A c \in CONSUMERS :
        consumer_joined[c] ~>
            (Last(consumer_window[c]) = Last(window))

\* The controller doesn't get stuck: if there are beats left to produce
\* and the producer is enabled, it eventually produces.
Progress ==
    (Len(published) < MAX_BEATS) ~> (Len(published)' > Len(published))
```

`Progress` requires **weak fairness** on `ProduceBeat`;
`EventualDelivery` requires **strong fairness** on `DeliverWindow(c)`
(per the post's discussion of strong vs weak fairness — `DeliverWindow`
is intermittently enabled when the window changes, so strong fairness
is the right tool).

```tla
Spec == Init /\ [][Next]_vars
        /\ WF_vars(ProduceBeat)
        /\ \A c \in CONSUMERS : SF_vars(DeliverWindow(c))
```

---

## 6. TLC configs

**`MCSmall.cfg`** — fast iteration (seconds):
```
CONSTANTS
    MAX_FRAME   = 50
    MIN_DELTA   = 5
    MAX_DELTA   = 10
    WINDOW_CAP  = 4
    CONSUMERS   = {c1, c2}
    MAX_BEATS   = 8
INVARIANTS Immutability Monotonicity WindowBounded ConsumerAgreement
PROPERTIES EventualDelivery Progress
```

**`MCStress.cfg`** — overnight (millions of states):
```
CONSTANTS
    MAX_FRAME   = 500
    MIN_DELTA   = 1
    MAX_DELTA   = 50
    WINDOW_CAP  = 8
    CONSUMERS   = {c1, c2, c3}
    MAX_BEATS   = 20
```

State-space discipline: keep `MAX_BEATS × |CONSUMERS|` small; this is
the dominant cost. The post's TDM example hit ~10M states with five
nontrivial state variables — we'll have ~6, so expect similar with
comparable bounds.

---

## 7. Iteration plan

1. **Compile the spec.** `sany BeatWindow.tla` until no syntax errors.
2. **Run with `MCSmall.cfg`.** Confirm "No error has been found" with
   only safety invariants enabled. If TLC trips: real bug or model bug?
   Replay trace, decide.
3. **Add `Stutter` if TLC reports deadlock** at the natural termination
   (`Len(published) = MAX_BEATS`). Same idea as `Done` in the bank
   example.
4. **Enable liveness.** Add `PROPERTIES` to the cfg, add fairness to
   `Spec`. Iterate until passing.
5. **Stress run.** Switch to `MCStress.cfg`, leave overnight. Record
   states-explored count in `tla/README.md` next to commit SHA.
6. **Codify findings.** For each TLC counterexample we resolve, write
   a Rust regression test that encodes that exact sequence (per the
   post's "error traces become regression tests").

---

## 8. v2 — score-envelope tempo automation

Once v1 is green and merged, extend the spec for the deferred feature
(plan.org → *Conductor: tempo-change automation*):

- New variable `envelope`: sequence of `[at_beat |-> Nat, delta |-> Nat]`
  entries, appended-only.
- `ProduceBeat` reads the next delta from the envelope rather than
  nondeterministically. Or — better for finding bugs — keep
  nondeterminism but constrain it to envelope values.
- New action `AppendEnvelope(at, new_delta)`: appends an entry. The
  precondition encodes policy 3: `at > Last(window)` — you cannot
  schedule a tempo change at a beat that's already on the wire.
- New invariant: `EnvelopeImmutability` — entries in `envelope` never
  change after append.
- New invariant: `PolicyThreeRespected` — when an envelope entry's
  `at_beat` is in the future but `Len(window) = WINDOW_CAP`, no new
  beat is produced until the window drains past `at_beat - 1`.

This is the exact class of bug Rust tests struggle with: an
interleaving of envelope edits and window slides that briefly publishes
a beat at a frame the next envelope entry would have moved. TLC finds
it or proves it can't happen.

---

## 9. Maintenance discipline

Per the post's "specs go stale" caveat:

- Spec lives next to the code (`st-sync/tla/`), not in a wiki.
- `DESIGN.md` § 3 (the three guarantees) is the **canonical English
  statement**; the TLA+ spec is the canonical formal statement.
  Discrepancies are bugs in one or the other — fix one, update the
  other in the same commit.
- Any change to `wire.rs`, `window.rs`, `broadcast.rs` that touches
  the protocol semantics (not just refactors) requires either a
  spec update or an explanation of why it doesn't.
- Add a CI job? Probably not worth it for v1 — `tlc` runs are slow
  and the spec changes rarely. Run on demand pre-merge of any
  protocol-touching PR. Revisit if we ever get burned.

---


## 10. Success criteria

- `tlc -config MCSmall.cfg BeatWindow.tla` exits cleanly.
- `tlc -config MCStress.cfg BeatWindow.tla` exits cleanly (or
  documented state-space exhaustion within a known bound).
- Every guarantee in `DESIGN.md` § 3 corresponds to a named invariant
  or temporal property in `BeatWindow.tla`.
- Every TLC counterexample encountered during development is either
  (a) fixed in the protocol, (b) fixed in the Rust impl, or
  (c) explicitly documented as out-of-scope with rationale.
- `st-sync/tla/README.md` documents how to run, what bounds were
  checked, and the headline state count for the last `MCStress`
  pass.
