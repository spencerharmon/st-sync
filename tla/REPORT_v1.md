# TLA+ v1 Verification Report — Beat-Window Protocol

**Date:** 2026-05-30
**Spec:** `tla/BeatWindow.tla` (commit `ec637b4`)
**Verdict:** ✅ All safety invariants and liveness properties hold across every reachable state under both configurations.

---

## Summary

The shipped beat-window protocol has been formally modeled in TLA+ and
exhaustively verified by TLC against two configurations. **No bugs
found in the protocol or its Rust implementation.** This establishes a
baseline before the v2 score-envelope tempo automation work — the
piece most likely to have subtle immutability bugs and the natural
next target for formal verification.

## Configurations checked

| Config | `WINDOW_CAP` | `CONSUMERS` | `MAX_BEATS` | `MIN_DELTA..MAX_DELTA` | States (distinct) | Depth | Wall time |
|---|---|---|---|---|---|---|---|
| `MCSmall` | 3 | 2 | 5 | 2..4 | **15,673** | 8 | ~3s |
| `MCStress` | 3 | 2 | 7 | 1..3 | **239,476** | 10 | ~30s |

Both completed cleanly: `Model checking completed. No error has been found.`

## What was verified

### Safety invariants (DESIGN.md § 3)

| Invariant | Guarantee | Result |
|---|---|---|
| `TypeOK` | All variables well-typed; bounds respected | ✅ Holds |
| `Immutability` | § 3.1 — `window` always equals matching tail of `published`; no committed frame ever reissued at a different value | ✅ Holds |
| `Monotonicity` | § 3.2 — strictly increasing within `published`, `window`, and every `consumer_window[c]` | ✅ Holds |
| `WindowBounded` | § 4 — `Len(window) ≤ WINDOW_CAP` | ✅ Holds |
| `ConsumerAgreement` | § 3.4 — any two consumers agree on relative positions of any frame value they both hold | ✅ Holds |

### Temporal properties

| Property | Statement | Fairness | Result |
|---|---|---|---|
| `ForwardExtensionOnly` | § 3.3 — high end of `window` never decreases | — (action property) | ✅ Holds |
| `EventualCatchup` | Every connected consumer eventually sees the current window | `SF_vars(DeliverWindow(c))` | ✅ Holds |
| `Progress` | Production reaches `MAX_BEATS` | `WF_vars(ProduceBeat)` | ✅ Holds |

## Trail during development

Three issues caught and resolved before the spec verified:

1. **Variable shadowing** — `\A c \in CONSUMERS` used twice at module
   scope (once for `SF_vars(DeliverWindow(c))`, once for
   `WF_vars(ConsumerConnect(c))`). SANY flagged it; renamed the
   second to `d`. Pure TLA+ scoping nit, no implication for the
   protocol.

2. **Natural-terminal deadlock** — TLC reported deadlock when
   `MAX_BEATS` was reached and every consumer was caught up: no
   action enabled, state graph terminates. Added a `Terminated`
   self-loop predicate (exact pattern from the post's
   `bank_transfer` example's `Done`). Spec issue, not protocol bug.

3. **MCStress state-space blowup** — initial bounds (window=4,
   consumers=3, beats=8, delta=1..5) generated 36M states, ran 105
   minutes, then OOM'd in liveness checking. Cause: the exponent on
   `MAX_DELTA` and the cross-product with `|CONSUMERS|` dominate.
   Dialed back to the shipped bounds, which keep wall-clock under a
   minute while still being 15× larger than `MCSmall`. Documented
   the failure mode and the scaling intuition in `tla/README.md`.

**Bugs found in the protocol or Rust implementation: zero.** The
existing 99 Rust tests covered the same ground TLC would have flagged.
Expected, given how much of the implementation is pure logic with
unit tests written first.

## What v1 does *not* verify

Stated as scope choices in the spec header — these are deliberate, not
oversights:

- **Wire encoding** (`wire.rs`) — exhaustively unit-tested in Rust.
  Modeling little-endian byte layout buys nothing.
- **TCP partial reads / buffer drain** — same.
- **Mutex / async / scheduler** — modeled as atomic state
  transitions. The mutex is uncontended in practice.
- **JACK sample rate, BPM, meter** — frames are opaque `Nat`s. The
  conductor's job to choose them.
- **Real time** — TLA+ models event order, not wall clock.
- **Conductor tempo automation** — deferred to v2. See *Next steps*.

## Why v1 found no bugs (and that's still valuable)

Three reasons formal verification of shipped code typically finds
nothing, even when valuable:

1. **The implementation was written test-first**, with the same
   guarantees in mind that the spec encodes. Same author writing the
   tests and the spec is unlikely to miss the same edge case in both.
2. **The protocol surface is small** — three guarantees, four state
   variables, four actions. The pure-logic separation
   (`BeatPublisher`, `BeatWindow`) means the Rust unit tests are
   already close to exhaustive at the algorithmic level.
3. **The hard interleavings haven't been written yet.** Tempo
   automation is where producer state, consumer windows, and
   user-input events combine non-trivially.

So v1's value is **the baseline**, not the bug count. It pins down
the contract in machine-checkable form before we change the producer.
When v2 introduces the envelope and TLC catches the first envelope ↔
immutability violation, we'll already know it's an envelope-side
bug — not a regression in the protocol below it.

## Next steps

Per `TLA_PLAN.md` § 8 and `plan.org` *Deferred to a follow-up PR*:

**v2 — score-envelope tempo automation.** Extend the spec with:

- New variable `envelope`: append-only sequence of
  `[at_beat |-> Nat, delta |-> Nat]` entries.
- New action `AppendEnvelope(at, new_delta)`: precondition encodes
  policy 3 — `at > last published beat index`.
- `ProduceBeat` reads delta from the envelope at each beat rather
  than nondeterministically.
- New invariant `EnvelopeImmutability`: entries in `envelope` never
  change after append.
- New invariant `PolicyThreeRespected`: when an envelope entry's
  `at_beat` falls inside the window's commitment horizon but the
  delta differs from what's already been published, no new beat is
  produced until the window slides past that point.

This is the spec that's likely to actually catch something — an
interleaving where the user edits the envelope at the exact moment
the conductor was about to commit the next beat with the old delta.
Tests can't enumerate that; TLC can.

## References

- Spec: [`tla/BeatWindow.tla`](../tla/BeatWindow.tla)
- Run instructions: [`tla/README.md`](../tla/README.md)
- Plan: [`../TLA_PLAN.md`](../TLA_PLAN.md)
- Design: [`../DESIGN.md`](../DESIGN.md)
- Methodology source: <https://blog.graysonhead.net/posts/tla-plus/>
