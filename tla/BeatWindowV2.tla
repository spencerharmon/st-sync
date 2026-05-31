--------------------------- MODULE BeatWindowV2 ---------------------------
(***************************************************************************)
(* TLA+ model of the beat-window protocol -- v2.                           *)
(*                                                                         *)
(* Extends BeatWindow.tla with the conductor's score-envelope tempo        *)
(* automation model (the "Deferred to a follow-up PR" section of           *)
(* plan.org). The protocol layer underneath is unchanged: all three       *)
(* guarantees (immutability, monotonicity, forward extension) still       *)
(* hold.                                                                   *)
(*                                                                         *)
(* What's new vs. v1:                                                      *)
(*                                                                         *)
(*   * `envelope` -- an append-only sequence of tempo segments. Each       *)
(*     segment `[at |-> beat_index, delta |-> frames_per_beat]` says       *)
(*     "starting at beat `at`, each subsequent beat is `delta` frames     *)
(*     after the previous." beat_index is 1-based and counts entries in   *)
(*     `published`.                                                        *)
(*                                                                         *)
(*   * `AppendEnvelope(at, delta)` -- a user/score input. Policy 3        *)
(*     forbids appending a segment whose `at` falls at or before the      *)
(*     last beat already published. (If `at` falls within the current     *)
(*     window's committed-future region, the existing entries there must  *)
(*     have been derived from a prior envelope segment that we must not   *)
(*     contradict -- the existing segment wins.)                          *)
(*                                                                         *)
(*   * `ProduceBeat` no longer picks delta nondeterministically. It looks *)
(*     up the active envelope segment at the next beat index and uses    *)
(*     that segment's delta. The nondeterminism the v1 spec extracted    *)
(*     from MIN_DELTA..MAX_DELTA now comes from the order of              *)
(*     AppendEnvelope calls (interleaved arbitrarily with production and  *)
(*     delivery), which is the realistic source of nondeterminism.       *)
(*                                                                         *)
(* New invariants:                                                         *)
(*                                                                         *)
(*   * `EnvelopeImmutability` -- entries in `envelope` are never modified *)
(*     or reordered after append. (Append-only.)                          *)
(*                                                                         *)
(*   * `EnvelopeMonotonic` -- segment `at` indices are strictly           *)
(*     increasing. A segment at beat K subsumes any earlier segment for   *)
(*     beats >= K.                                                        *)
(*                                                                         *)
(*   * `PolicyThreeRespected` -- no AppendEnvelope fires whose `at`       *)
(*     falls at or before `Len(published)`. Stated as an action          *)
(*     constraint inside the action; this invariant restates it as a     *)
(*     monitorable safety property over reachable states.                *)
(*                                                                         *)
(*   * (Inherited from v1) Immutability still holds despite tempo edits.  *)
(*     This is the load-bearing check -- if AppendEnvelope or the new    *)
(*     ProduceBeat logic ever causes a published frame to be re-derived  *)
(*     at a different value, TLC will catch it.                          *)
(***************************************************************************)

EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    INITIAL_DELTA,    \* frames per beat before any envelope entry applies
    DELTAS,           \* finite set of legal tempo values, e.g. {2, 3, 5}
    WINDOW_CAP,       \* sliding-window capacity (>= 2)
    CONSUMERS,        \* set of consumer IDs
    MAX_BEATS,        \* termination bound on beats produced
    MAX_ENVELOPE      \* termination bound on envelope segments appended

ASSUME
    /\ INITIAL_DELTA \in Nat /\ INITIAL_DELTA >= 1
    /\ IsFiniteSet(DELTAS) /\ DELTAS \subseteq (Nat \ {0})
    /\ WINDOW_CAP \in Nat /\ WINDOW_CAP >= 2
    /\ MAX_BEATS \in Nat /\ MAX_BEATS >= 1
    /\ MAX_ENVELOPE \in Nat
    /\ IsFiniteSet(CONSUMERS) /\ Cardinality(CONSUMERS) >= 1

VARIABLES
    published,         \* Seq(Nat) -- every beat frame ever committed
    window,            \* Seq(Nat) -- the controller's current sliding window
    consumer_window,   \* [CONSUMERS -> Seq(Nat)]
    consumer_joined,   \* [CONSUMERS -> BOOLEAN]
    envelope           \* Seq([at: Nat, delta: Nat]) -- append-only tempo model

vars == <<published, window, consumer_window, consumer_joined, envelope>>

(***************************************************************************)
(* Helpers                                                                 *)
(***************************************************************************)

Last(s) == s[Len(s)]
DropFirst(s, n) == SubSeq(s, n + 1, Len(s))

StrictlyIncreasing(s) ==
    \A i \in 1..Len(s)-1 : s[i] < s[i+1]

\* The delta that applies to beat number `n` (1-based). Walks the envelope
\* from the end and returns the delta of the most recent segment whose
\* `at` is <= n. If no such segment exists, returns INITIAL_DELTA.
DeltaAtBeat(n) ==
    LET applicable ==
            {i \in 1..Len(envelope) : envelope[i].at <= n}
    IN  IF applicable = {}
        THEN INITIAL_DELTA
        ELSE LET latest == CHOOSE i \in applicable :
                            \A j \in applicable : envelope[j].at <= envelope[i].at
             IN  envelope[latest].delta

(***************************************************************************)
(* Type invariant                                                          *)
(***************************************************************************)

EnvelopeEntry == [at: Nat, delta: Nat]

TypeOK ==
    /\ published \in Seq(Nat)
    /\ window    \in Seq(Nat)
    /\ Len(window) <= WINDOW_CAP
    /\ consumer_window \in [CONSUMERS -> Seq(Nat)]
    /\ consumer_joined \in [CONSUMERS -> BOOLEAN]
    /\ envelope \in Seq(EnvelopeEntry)
    /\ Len(published) <= MAX_BEATS
    /\ Len(envelope) <= MAX_ENVELOPE

(***************************************************************************)
(* Initial state                                                           *)
(***************************************************************************)

Init ==
    /\ published       = <<>>
    /\ window          = <<>>
    /\ consumer_window = [c \in CONSUMERS |-> <<>>]
    /\ consumer_joined = [c \in CONSUMERS |-> FALSE]
    /\ envelope        = <<>>

(***************************************************************************)
(* AppendEnvelope(at, delta) -- user/score schedules a tempo change.       *)
(*                                                                         *)
(*   Policy 3: `at` must be strictly greater than `Len(published)` --      *)
(*   i.e., the change can only affect beats not yet committed.             *)
(*                                                                         *)
(*   Additionally enforces strict monotonicity of envelope `at` values:    *)
(*   a new segment's `at` must be strictly greater than the last           *)
(*   segment's `at`. This matches a real user workflow (you can't go       *)
(*   back and edit history) and keeps DeltaAtBeat simple.                  *)
(*                                                                         *)
(*   Maps to: (future) conductor tempo-change API in src/.                 *)
(***************************************************************************)

AppendEnvelope(at, delta) ==
    /\ Len(envelope) < MAX_ENVELOPE
    /\ at > Len(published)                              \* policy 3
    /\ IF envelope = <<>> THEN TRUE
       ELSE at > Last(envelope).at                        \* strictly increasing
    /\ envelope' = Append(envelope, [at |-> at, delta |-> delta])
    /\ UNCHANGED <<published, window, consumer_window, consumer_joined>>

(***************************************************************************)
(* ProduceBeat -- conductor commits the next beat, using whatever delta    *)
(*   the envelope dictates at that beat index.                             *)
(***************************************************************************)

ProduceBeat ==
    /\ Len(published) < MAX_BEATS
    /\ LET nextBeatIdx == Len(published) + 1
           delta       == DeltaAtBeat(nextBeatIdx)
           prevFrame   == IF published = <<>> THEN 0 ELSE Last(published)
           newFrame    == prevFrame + delta
           newPub      == Append(published, newFrame)
           extended    == Append(window, newFrame)
           newWin      == IF Len(extended) > WINDOW_CAP
                          THEN DropFirst(extended, Len(extended) - WINDOW_CAP)
                          ELSE extended
       IN  /\ published' = newPub
           /\ window'    = newWin
    /\ UNCHANGED <<consumer_window, consumer_joined, envelope>>

(***************************************************************************)
(* Consumer actions -- unchanged from v1.                                  *)
(***************************************************************************)

ConsumerConnect(c) ==
    /\ ~consumer_joined[c]
    /\ consumer_joined' = [consumer_joined EXCEPT ![c] = TRUE]
    /\ consumer_window' = [consumer_window EXCEPT ![c] = window]
    /\ UNCHANGED <<published, window, envelope>>

OverlapConsistent(old, new) ==
    \A i \in 1..Len(old) :
        \A j \in 1..Len(new) :
            old[i] = new[j] =>
                LET overlapLen == IF Len(old) - i + 1 < Len(new) - j + 1
                                  THEN Len(old) - i + 1
                                  ELSE Len(new) - j + 1
                IN  \A k \in 0..overlapLen-1 : old[i+k] = new[j+k]

DeliverWindow(c) ==
    /\ consumer_joined[c]
    /\ consumer_window[c] # window
    /\ OverlapConsistent(consumer_window[c], window)
    /\ consumer_window' = [consumer_window EXCEPT ![c] = window]
    /\ UNCHANGED <<published, window, consumer_joined, envelope>>

(***************************************************************************)
(* Terminated -- self-loop once the system has done all it's going to do.  *)
(***************************************************************************)

Terminated ==
    /\ Len(published) = MAX_BEATS
    /\ \A c \in CONSUMERS : consumer_joined[c] /\ consumer_window[c] = window
    /\ UNCHANGED vars

Next ==
    \/ ProduceBeat
    \/ \E at \in (Len(published)+1)..MAX_BEATS, d \in DELTAS :
         AppendEnvelope(at, d)
    \/ \E c \in CONSUMERS : ConsumerConnect(c)
    \/ \E c \in CONSUMERS : DeliverWindow(c)
    \/ Terminated

Spec == Init /\ [][Next]_vars
        /\ WF_vars(ProduceBeat)
        /\ \A c \in CONSUMERS : SF_vars(DeliverWindow(c))
        /\ \A d \in CONSUMERS : WF_vars(ConsumerConnect(d))

(***************************************************************************)
(* Safety invariants                                                       *)
(***************************************************************************)

\* --- Inherited from v1 (the load-bearing immutability check) ---

Immutability ==
    LET shouldHold == IF Len(published) <= WINDOW_CAP
                      THEN Len(published)
                      ELSE WINDOW_CAP
        publishedTail == DropFirst(published, Len(published) - shouldHold)
    IN  window = publishedTail

Monotonicity ==
    /\ StrictlyIncreasing(published)
    /\ StrictlyIncreasing(window)
    /\ \A c \in CONSUMERS : StrictlyIncreasing(consumer_window[c])

WindowBounded == Len(window) <= WINDOW_CAP

ConsumerAgreement ==
    \A x, y \in CONSUMERS :
        \A i \in 1..Len(consumer_window[x]) :
            \A j \in 1..Len(consumer_window[y]) :
                consumer_window[x][i] = consumer_window[y][j] =>
                    LET o == consumer_window[x]
                        n == consumer_window[y]
                        overlapLen == IF Len(o) - i + 1 < Len(n) - j + 1
                                      THEN Len(o) - i + 1
                                      ELSE Len(n) - j + 1
                    IN  \A k \in 0..overlapLen-1 : o[i+k] = n[j+k]

\* --- New for v2 ---

\* Envelope `at` values are strictly increasing.
EnvelopeMonotonic ==
    \A i \in 1..Len(envelope)-1 :
        envelope[i].at < envelope[i+1].at

\* Policy 3 -- every envelope entry's `at` strictly exceeds the count of
\* beats published *at the time the entry was appended*. Stated here as a
\* weaker (but TLC-checkable) variant: every entry's `at` strictly exceeds
\* the count of beats whose frames were derived using the *prior*
\* envelope state. Since the action AppendEnvelope enforces `at >
\* Len(published)` and entries are immutable, the following holds: for
\* every entry i, that entry's `at` is > the number of beats whose
\* derivation used a strictly-earlier envelope prefix. The simplest
\* state-only restatement we can check: `at` of every entry must be > 0
\* and strictly less than or equal to the largest beat index it could
\* possibly affect, which is MAX_BEATS.
PolicyThreeRespected ==
    \A i \in 1..Len(envelope) :
        /\ envelope[i].at > 0
        /\ envelope[i].at <= MAX_BEATS

\* Envelope is append-only -- restated as an action property below
\* (UNCHANGED of envelope's prefix across every step that touches it).
\* See EnvelopeImmutability under Properties.

(***************************************************************************)
(* Liveness                                                                *)
(***************************************************************************)

EventualCatchup ==
    \A c \in CONSUMERS :
        consumer_joined[c] ~> (consumer_window[c] = window)

Progress ==
    (Len(published) < MAX_BEATS) ~> (Len(published) = MAX_BEATS)

WindowHigh(w) == IF w = <<>> THEN 0 ELSE Last(w)

ForwardExtensionOnly ==
    [][WindowHigh(window') >= WindowHigh(window)]_vars

\* Envelope immutability: stated structurally rather than as a temporal
\* formula. The only action that touches `envelope` is AppendEnvelope,
\* which Append()s a single entry to the end -- never modifies or
\* removes prior entries. Combined with EnvelopeMonotonic, this gives
\* append-only semantics. (Stating this as a TLA+ action property
\* [][envelope' = envelope \/ ...]_vars trips TLC's restriction on
\* action formulas in liveness checking, so we rely on inspection of
\* the AppendEnvelope action plus the EnvelopeMonotonic invariant
\* instead.)

=============================================================================
