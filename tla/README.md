# TLA+ model — beat-window protocol

Formal model of the st-sync beat-window protocol. See
[`../DESIGN.md`](../DESIGN.md) for the English statement of what this
spec formalizes; this directory is the canonical *formal* statement.

If the two disagree, one is a bug — fix it and update the other in the
same commit.

## Files

| File | Purpose |
|---|---|
| `BeatWindow.tla` | The v1 spec. State, actions, safety invariants, liveness properties. |
| `BeatWindowV2.tla` | The v2 spec. Extends v1 with a score-envelope tempo model + policy 3. |
| `MCSmall.cfg` | v1 tiny constants for fast iteration (~3s, ~16k states). |
| `MCStress.cfg` | v1 larger constants for confidence (~30s, ~240k states). |
| `MCSmallV2.cfg` | v2 tiny constants (~1s, ~660 states). |
| `MCStressV2.cfg` | v2 larger constants (~9s, ~52k states). |
| `REPORT_v1.md` | v1 verification report. |
| `REPORT_v2.md` | v2 verification report. |
| `smoke/bank_transfer.tla` | Toolchain smoke test (the example from the post that motivated this). |

## Requirements

- Java 17+ (`pacman -S jre-openjdk`)
- `tla2tools.jar` from <https://github.com/tlaplus/tlaplus/releases/latest>

```sh
mkdir -p ~/.local/share
curl -L -o ~/.local/share/tla2tools.jar \
    https://github.com/tlaplus/tlaplus/releases/latest/download/tla2tools.jar
```

## Running

```sh
# v1 -- fast iteration (a few seconds):
java -XX:+UseParallelGC -cp ~/.local/share/tla2tools.jar tlc2.TLC \
    -config MCSmall.cfg BeatWindow

# v1 -- stress run (~30s, larger window + extra consumer):
java -XX:+UseParallelGC -Xmx12g -cp ~/.local/share/tla2tools.jar tlc2.TLC \
    -workers auto -config MCStress.cfg BeatWindow

# v2 -- envelope-based tempo automation, fast:
java -XX:+UseParallelGC -cp ~/.local/share/tla2tools.jar tlc2.TLC \
    -config MCSmallV2.cfg BeatWindowV2

# v2 -- stress:
java -XX:+UseParallelGC -Xmx12g -cp ~/.local/share/tla2tools.jar tlc2.TLC \
    -workers auto -config MCStressV2.cfg BeatWindowV2

# Smoke test the toolchain against the bank_transfer example from the post:
cd smoke && java -cp ~/.local/share/tla2tools.jar tlc2.TLC bank_transfer
```

Recommended shell aliases:

```sh
alias tlc='java -XX:+UseParallelGC -cp ~/.local/share/tla2tools.jar tlc2.TLC'
alias sany='java -cp ~/.local/share/tla2tools.jar tla2sany.SANY'
```

## What's modeled

- Controller sliding window of bounded capacity (`BeatPublisher`).
- A small fixed set of consumers, each with their own `BeatWindow`.
- Logical message delivery: every controller window is eventually
  visible to every connected consumer. No bytes, no framing, no TCP.
- Late connect: a consumer may join at any step; it's seeded with the
  current window immediately.
- Nondeterministic beat producer emitting monotonically increasing
  frames with bounded inter-beat deltas.

### What's deliberately abstracted

| Real concern | Why omitted |
|---|---|
| Wire encoding (`wire.rs`) | Exhaustively unit-tested in Rust. Modeling little-endian buys nothing. |
| TCP partial reads / buffer drain | Same. |
| Mutex / async / scheduler | Modeled as atomic state transitions. The Mutex is uncontended in practice. |
| JACK sample rate, BPM, meter | Frames are opaque `Nat`s. The conductor's job to choose them. |
| Real time | TLA+ models event order, not wall clock. |

### Deferred to v2

The conductor's score-envelope tempo automation (policy 3 — "stop
extending the window when a queued tempo change can't take effect
without violating immutability"). Listed in
[`../../plan.org`](../../plan.org) under *Conductor: tempo-change
automation [/]*. This is the exact class of bug TLC is best at finding
and Rust unit tests are worst at — so it's the obvious next target,
but only after v1 is in tree as a baseline.

## What's checked

### Safety invariants (per `DESIGN.md` § 3)

| Invariant | Maps to | Statement |
|---|---|---|
| `TypeOK` | — | All variables are well-typed; bounds respected. |
| `Immutability` | § 3.1 | `window` always equals the matching tail of `published`. Every committed frame either still sits in the window at the slid-forward position, or fell off the low end. Never reissued at a different value. |
| `Monotonicity` | § 3.2 | `published`, `window`, and every `consumer_window[c]` are strictly increasing. |
| `WindowBounded` | § 4 | `Len(window) <= WINDOW_CAP`. The `BeatPublisher::new(capacity)` contract. |
| `ConsumerAgreement` | § 3.4 | Any two consumers, on any frame value they both hold, agree on its relative position within their windows. |

### Liveness (action and temporal properties)

| Property | Statement | Fairness used |
|---|---|---|
| `ForwardExtensionOnly` | § 3.3. The high end of `window` never decreases. Action property — checked on every transition. | — |
| `EventualCatchup` | Every connected consumer eventually sees the controller's current window. | `SF_vars(DeliverWindow(c))` |
| `Progress` | Production reaches `MAX_BEATS`. | `WF_vars(ProduceBeat)` |

`SF` (strong fairness) for `DeliverWindow` because the action is only
intermittently enabled (only when there's a mismatch between
controller and consumer window). Per the post's discussion of `SF` vs
`WF`, this is the right tool when an action can be enabled, disabled,
and re-enabled repeatedly.

## Results

### v1 (shipped protocol)

| Config | Constants | States (distinct) | Depth | Time |
|---|---|---|---|---|
| `MCSmall.cfg` | window=3, consumers=2, beats=5, delta=2..4 | 15,673 | 8 | ~3s |
| `MCStress.cfg` | window=3, consumers=2, beats=7, delta=1..3 | 239,476 | 10 | ~30s |

Full report: [`REPORT_v1.md`](REPORT_v1.md).

### v2 (envelope tempo automation)

| Config | Constants | States (distinct) | Depth | Time |
|---|---|---|---|---|
| `MCSmallV2.cfg` | window=3, consumers=1, beats=4, envelopes=2, deltas={2,3} | 660 | 8 | ~1s |
| `MCStressV2.cfg` | window=3, consumers=2, beats=5, envelopes=3, deltas={2,3,5} | 52,264 | 11 | ~9s |

Full report: [`REPORT_v2.md`](REPORT_v2.md).

All safety invariants and liveness properties hold in every reachable
state under every configuration.

### Why `MCStress` is not bigger

An earlier `MCStress` (window=4, consumers=3, beats=8, delta=1..5)
generated 36M states / 28M distinct in 105 minutes and OOM'd at the
liveness-checking stage. State-space cost is roughly
`(MAX_BEATS choose consumers) × MAX_DELTA^MAX_BEATS × …` — the
exponent in `MAX_DELTA` and the cross-product with `|CONSUMERS|`
dominate. The shipped `MCStress` keeps wall-clock under a minute
while still being meaningfully larger than `MCSmall` (15× the state
count). If you want overnight-style confidence, push `MAX_BEATS` to
9 or 10 first; `MAX_DELTA` last.

## Acting on counterexamples

If TLC ever reports a violation in this directory:

1. **Read the trace.** TLC prints the exact action sequence.
2. **Decide:**
   - Real protocol bug → fix in `src/`, update spec only if the
     guarantee statement itself changes.
   - Spec bug (model doesn't faithfully represent the protocol) → fix
     the spec.
   - Trace exposes an over-conservative consumer (Rust code rejects a
     state the protocol actually permits) → loosen the Rust check.
3. **Encode the trace as a Rust regression test** in
   `st-sync/tests/`. The action sequence is mechanical: each `ProduceBeat`
   becomes a `Controller::send_next_beat_frame`, each `DeliverWindow`
   becomes the assertion that follows. Per the post: "error traces
   become regression tests."

## Maintenance

- This spec lives next to the code (`st-sync/tla/`) so it travels with
  the protocol.
- Any change to `wire.rs`, `window.rs`, `broadcast.rs` that touches
  semantics (not just refactors) requires either a spec update or an
  explanation in the commit message of why it doesn't.
- No CI job: TLC runs are slow and the spec changes rarely. Run
  on demand pre-merge of any protocol-touching PR. Revisit if we ever
  get burned.

## References

- Plan: [`../TLA_PLAN.md`](../TLA_PLAN.md)
- Design doc: [`../DESIGN.md`](../DESIGN.md)
- Source: `../src/{wire,window,broadcast,controller,client}.rs`
- TLA+ home: <https://lamport.azurewebsites.net/tla/tla.html>
- Methodology: <https://blog.graysonhead.net/posts/tla-plus/>
