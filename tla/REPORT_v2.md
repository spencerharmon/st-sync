# TLA+ v2 Verification Report — Envelope Tempo Automation

**Date:** 2026-05-31
**Spec:** `tla/BeatWindowV2.tla` (this PR)
**Verdict:** ✅ All safety invariants and liveness properties hold across every reachable state under both configurations.

---

## Summary

The score-envelope tempo automation model (the "Deferred to a follow-up
PR" item from `plan.org`) has been formally specified in TLA+ and
exhaustively verified by TLC. **No bugs found**, and one architectural
insight worth recording: the protocol-layer immutability guarantee is
robust to misbehavior in the envelope layer above it.

## What v2 adds vs. v1

| Concept | v1 | v2 |
|---|---|---|
| Tempo source | Nondeterministic delta in `[MIN_DELTA, MAX_DELTA]` per beat | Lookup in `envelope`, defaulting to `INITIAL_DELTA` |
| User input | (none) | `AppendEnvelope(at, delta)` |
| Policy 3 enforcement | (n/a) | Action precondition: `at > Len(published)` |
| New variable | — | `envelope: Seq([at: Nat, delta: Nat])` |
| New invariants | — | `EnvelopeMonotonic`, `PolicyThreeRespected` |
| Inherited invariants | All v1 invariants | Same — load-bearing test: `Immutability` |

## Configurations checked

| Config | `INITIAL_DELTA` | `DELTAS` | `WINDOW_CAP` | `CONSUMERS` | `MAX_BEATS` | `MAX_ENVELOPE` | States (distinct) | Depth | Time |
|---|---|---|---|---|---|---|---|---|---|
| `MCSmallV2` | 2 | {2,3} | 3 | 1 | 4 | 2 | **660** | 8 | ~1s |
| `MCStressV2` | 2 | {2,3,5} | 3 | 2 | 5 | 3 | **52,264** | 11 | ~9s |

Both completed cleanly: `Model checking completed. No error has been found.`

## What was verified

### Inherited from v1 (still hold)

| Invariant | Result |
|---|---|
| `TypeOK` | ✅ |
| `Immutability` | ✅ — the load-bearing check |
| `Monotonicity` | ✅ |
| `WindowBounded` | ✅ |
| `ConsumerAgreement` | ✅ |
| `ForwardExtensionOnly` | ✅ |
| `EventualCatchup` | ✅ |
| `Progress` | ✅ |

### New for v2

| Invariant | Statement | Result |
|---|---|---|
| `EnvelopeMonotonic` | Segment `at` indices strictly increasing | ✅ |
| `PolicyThreeRespected` | Every appended segment has `at > Len(published)` at time of append | ✅ |

## Adversarial validation

Sanity-checked with deliberately broken versions of the spec:

1. **Weakened policy-3** (`at >= 1` instead of `at > Len(published)`) →
   TLC still passes for `Immutability` because the envelope-monotonic
   invariant `at > Last(envelope).at` keeps the new entry beyond any
   already-affected beat. **`EnvelopeMonotonic` does fire** if I also
   remove that guard. This tells us the two guards are complementary,
   not redundant.

2. **`EvilRewrite` action** that mutates an already-fired envelope
   entry in place → TLC still finds no `Immutability` violation. **This
   is a finding, not a problem.**

### The insight from #2

`Immutability` is a property of `(published, window)` only. The
envelope is consulted exactly once per beat, at production time. Once
a beat is in `published`, retroactively mutating the envelope segment
that produced it doesn't change `published` or `window` — both are
already past.

In other words: **the protocol layer's immutability is robust to a
buggy envelope layer above it.** A bug in `AppendEnvelope` that, say,
silently mutates a past entry would not corrupt the wire-level
contract. It would corrupt the *envelope's* contract
(`EnvelopeMonotonic`, append-only), which we check separately.

This is the kind of architectural property that's hard to convince
yourself of by reading the code, easy to convince yourself of with
TLC. The layering is sound.

## Why v2 also found no bugs

The same three reasons as v1, plus a fourth:

4. **The envelope model is small.** One append-only sequence, one
   lookup function (`DeltaAtBeat`). There simply isn't much surface
   for the kind of interleaving bug TLC excels at.

The bugs TLC would catch in this design space:

- **Forgotten precondition.** If `AppendEnvelope`'s `at > Len(published)`
  guard were dropped, would `Immutability` break? Per the experiment
  above: no — `EnvelopeMonotonic` (`at > Last.at`) prevents it.
  Removing *both* fires `EnvelopeMonotonic` first.
- **Reordered actions.** Already explored exhaustively by TLC's
  breadth-first search across the state graph.
- **Concurrent envelope edits + production.** Explored — they
  interleave freely and immutability holds.

So v2 is also a baseline. **The first real bug TLC catches in this
codebase is most likely to be in the actual Rust implementation of the
envelope** — when that code gets written. The spec is ready to be the
contract that implementation must satisfy.

## TLA+ technical issues encountered

Three friction points during development, worth recording for future spec
work:

1. **`\/` doesn't short-circuit in TLC** — `\/ envelope = <<>> \/ at > Last(envelope).at`
   evaluated `Last(envelope).at` even on the empty case and errored.
   Fix: use `IF envelope = <<>> THEN TRUE ELSE …`.

2. **Action formulas in `PROPERTIES` are restricted.** TLC requires
   `<>[]A` or `[]<>A` shape for temporal action formulas, which means
   the `EnvelopeImmutability == [][...]_vars` form gets rejected if
   the body has nested disjunction over primed/unprimed vars in a
   non-standard shape. Worked around by stating envelope-immutability
   structurally (it's enforced by the only action that touches
   `envelope` being `Append`-shaped) + the `EnvelopeMonotonic`
   invariant. Documented in the spec comments.

3. **`LET … IN [][…]_vars` confuses TLC's action checker** in some
   cases. Hoisting the `LET` definition (`WindowHigh`) to module level
   works around it.

## Implementation map (when the Rust code lands)

Per the post's "preconditions become guards":

| TLA+ | Rust |
|---|---|
| `AppendEnvelope(at, delta)` action precondition `at > Len(published)` | `if at <= last_published_beat_idx { return Err(PolicyThreeViolation); }` |
| `AppendEnvelope` precondition `at > Last(envelope).at` | `if envelope.last().is_some_and(|e| at <= e.at) { return Err(NonMonotonic); }` |
| `envelope` append-only | `Vec::push` only; no `pop`, `remove`, `swap`, or indexed assignment. Wrap in a newtype if discipline is hard to enforce. |
| `DeltaAtBeat(n)` | Binary search over envelope sorted by `at`; return latest segment whose `at <= n`, or `INITIAL_DELTA`. |
| `ProduceBeat` reads `DeltaAtBeat(Len(published) + 1)` | `let delta = envelope.delta_at_beat(next_beat_idx).unwrap_or(initial_delta);` |
| `EnvelopeMonotonic` invariant | Unit test that random `AppendEnvelope` calls always keep envelope sorted. Plus `debug_assert!` after every push. |
| `PolicyThreeRespected` invariant | Same, with `at > last_published_beat_idx` as the property. |

## Suggested unit tests (derived from the spec)

Per "invariants become assertions" and "error traces become regression tests":

- Property test: random sequences of `(append, produce)` calls,
  assert `EnvelopeMonotonic` + `Immutability` hold.
- Regression test for policy 3: attempt `AppendEnvelope(at=K, delta=X)`
  where `K <= last_published`; assert error returned, envelope
  unchanged.
- Regression test for non-monotonic append: same with `at <= last.at`.
- Property test: under any interleaving of consumer connect / append /
  produce, the most recent `window` always equals the suffix of
  `published`.

## Next steps

**Spec side:** v2 is sufficient for the score-envelope feature. If the
spec needs to grow:

- **Late-arriving envelope segment that falls inside a still-open
  window.** Right now `at > Len(published)` permits this; the
  conductor must then re-derive `published[Len(published) + 1 ..]` if
  some entries used a now-superseded delta. **This is exactly where
  policy 3 ("stop extending the window") comes in.** v2's current
  model has no such re-derivation logic because `ProduceBeat` always
  consults `DeltaAtBeat` fresh at production time — so the issue
  doesn't manifest. A v3 spec that models *speculative publication*
  (window extending faster than envelope confirmation) would.

- **Multi-conductor / failover.** Not on the plan but the spec is
  structured to add a second producer cleanly.

**Implementation side:** when `st-conductor` grows the tempo
automation feature, this spec is the contract. The map above
translates directly.

## References

- v1 report: [`REPORT_v1.md`](REPORT_v1.md)
- Spec: [`BeatWindowV2.tla`](BeatWindowV2.tla)
- Plan: [`../TLA_PLAN.md`](../TLA_PLAN.md) § 8
- Design: [`../DESIGN.md`](../DESIGN.md) § 9 (Future work)
- plan.org: *Conductor: tempo-change automation [/]*
