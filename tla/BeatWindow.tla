--------------------------- MODULE BeatWindow ---------------------------
(***************************************************************************)
(* TLA+ model of the st-sync beat-window protocol.                         *)
(*                                                                         *)
(* See st-sync/DESIGN.md for the English statement of what this spec       *)
(* formalizes. Where the two disagree, one of them is a bug -- fix the     *)
(* offender, update the other in the same commit.                          *)
(*                                                                         *)
(* Scope (v1, this spec):                                                  *)
(*   * Controller-side sliding window of bounded capacity (BeatPublisher). *)
(*   * Set of consumers, each holding its own BeatWindow.                  *)
(*   * Logical message delivery: every published window is eventually      *)
(*     seen by every connected consumer. No bytes, no framing, no TCP.    *)
(*   * Late connect: a consumer may join at any step and is seeded with    *)
(*     the current window.                                                 *)
(*                                                                         *)
(* Out of scope (covered by Rust unit tests or by JACK):                   *)
(*   * Wire encoding (wire.rs is exhaustively unit-tested).                *)
(*   * TCP partial reads / buffer drain (same).                            *)
(*   * Mutex / async / scheduling -- modeled as atomic state transitions. *)
(*   * Sample rate, BPM, meter -- frames are opaque Nats.                  *)
(*   * Real time -- TLA+ models event order, not wall clock.               *)
(*                                                                         *)
(* Deferred to a v2 spec:                                                  *)
(*   * Score-envelope tempo automation (policy 3, "stop extending").       *)
(***************************************************************************)

EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    MAX_DELTA,        \* max frames per beat (bounds tempo from below)
    MIN_DELTA,        \* min frames per beat (bounds tempo from above; > 0)
    WINDOW_CAP,       \* sliding-window capacity (>= 2)
    CONSUMERS,        \* set of consumer IDs, e.g. {c1, c2}
    MAX_BEATS         \* termination bound: stop after this many beats produced

ASSUME
    /\ MIN_DELTA \in Nat /\ MIN_DELTA >= 1
    /\ MAX_DELTA \in Nat /\ MAX_DELTA >= MIN_DELTA
    /\ WINDOW_CAP \in Nat /\ WINDOW_CAP >= 2
    /\ MAX_BEATS \in Nat /\ MAX_BEATS >= 1
    /\ IsFiniteSet(CONSUMERS) /\ Cardinality(CONSUMERS) >= 1

VARIABLES
    published,        \* Seq(Nat): every beat frame the controller has ever
                      \*   committed (history variable; not in real impl).
                      \*   Used to state immutability without unbounded
                      \*   quantification.
    window,           \* Seq(Nat): the controller's current sliding window.
    consumer_window,  \* [CONSUMERS -> Seq(Nat)]: each consumer's view.
    consumer_joined   \* [CONSUMERS -> BOOLEAN]: has c received any window yet.

vars == <<published, window, consumer_window, consumer_joined>>

(***************************************************************************)
(* Helpers                                                                 *)
(***************************************************************************)

Last(s) == s[Len(s)]

\* Drop the first n elements of s.
DropFirst(s, n) == SubSeq(s, n + 1, Len(s))

\* True iff s is strictly increasing.
StrictlyIncreasing(s) ==
    \A i \in 1..Len(s)-1 : s[i] < s[i+1]

(***************************************************************************)
(* Type invariant                                                          *)
(***************************************************************************)

TypeOK ==
    /\ published \in Seq(Nat)
    /\ window    \in Seq(Nat)
    /\ Len(window) <= WINDOW_CAP
    /\ consumer_window \in [CONSUMERS -> Seq(Nat)]
    /\ consumer_joined \in [CONSUMERS -> BOOLEAN]
    /\ Len(published) <= MAX_BEATS

(***************************************************************************)
(* Initial state                                                           *)
(***************************************************************************)

Init ==
    /\ published       = <<>>
    /\ window          = <<>>
    /\ consumer_window = [c \in CONSUMERS |-> <<>>]
    /\ consumer_joined = [c \in CONSUMERS |-> FALSE]

(***************************************************************************)
(* Actions                                                                 *)
(*                                                                         *)
(* ProduceBeat -- the conductor commits a new beat at some legal frame.    *)
(*   Precondition: room left in MAX_BEATS budget; next frame strictly      *)
(*   greater than the last published, within [MIN_DELTA, MAX_DELTA].       *)
(*   Effect: append to `published`; append to `window`, dropping oldest    *)
(*   entry if at capacity (mirrors BeatPublisher::record_beat).            *)
(*                                                                         *)
(*   Maps to: BeatPublisher::record_beat in st-sync/src/broadcast.rs       *)
(***************************************************************************)

ProduceBeat ==
    /\ Len(published) < MAX_BEATS
    /\ \E delta \in MIN_DELTA..MAX_DELTA :
         LET prev    == IF published = <<>> THEN 0 ELSE Last(published)
             newFrame == prev + delta
             newPub   == Append(published, newFrame)
             extended == Append(window, newFrame)
             newWin   == IF Len(extended) > WINDOW_CAP
                         THEN DropFirst(extended, Len(extended) - WINDOW_CAP)
                         ELSE extended
         IN  /\ published' = newPub
             /\ window'    = newWin
    /\ UNCHANGED <<consumer_window, consumer_joined>>

(***************************************************************************)
(* ConsumerConnect(c) -- a new client connects and is seeded with the      *)
(*   current window. Models the controller's "drain any new clients first  *)
(*   so they see the current window" path in run_broadcaster.              *)
(*                                                                         *)
(*   Maps to: accept_loop + initial seed in st-sync/src/controller.rs      *)
(***************************************************************************)

ConsumerConnect(c) ==
    /\ ~consumer_joined[c]
    /\ consumer_joined' = [consumer_joined EXCEPT ![c] = TRUE]
    /\ consumer_window' = [consumer_window EXCEPT ![c] = window]
    /\ UNCHANGED <<published, window>>

(***************************************************************************)
(* DeliverWindow(c) -- a connected consumer receives the current           *)
(*   controller window. The update must satisfy the overlap-immutability   *)
(*   rule (BeatWindow::update). We encode the rule directly: whatever      *)
(*   frames overlap by value between the old and new consumer window must  *)
(*   be at consistent positions relative to the new window. Since the      *)
(*   controller is the sole source and produces well-formed windows, this  *)
(*   is automatic; encoding it here lets TLC catch any deviation.          *)
(*                                                                         *)
(*   Maps to: run_receiver -> BeatWindow::update in st-sync/src/client.rs  *)
(***************************************************************************)

\* For two strictly-increasing sequences old and new, true iff every frame
\* value appearing in both occupies positions consistent with a sliding-
\* forward update (matching tail of `old` == matching head of `new`).
OverlapConsistent(old, new) ==
    \A i \in 1..Len(old) :
        \A j \in 1..Len(new) :
            old[i] = new[j] =>
                (* The matching prefixes/suffixes must align: the suffix of
                   `old` from i onward must equal the prefix of `new` from
                   j of the same length. *)
                LET overlapLen == IF Len(old) - i + 1 < Len(new) - j + 1
                                  THEN Len(old) - i + 1
                                  ELSE Len(new) - j + 1
                IN  \A k \in 0..overlapLen-1 : old[i+k] = new[j+k]

DeliverWindow(c) ==
    /\ consumer_joined[c]
    /\ consumer_window[c] # window
    /\ OverlapConsistent(consumer_window[c], window)
    /\ consumer_window' = [consumer_window EXCEPT ![c] = window]
    /\ UNCHANGED <<published, window, consumer_joined>>

(***************************************************************************)
(* Next                                                                    *)
(***************************************************************************)

(***************************************************************************)
(* Terminated -- a non-stuttering self-loop once everything is done.       *)
(*   Without this, TLC reports a deadlock when MAX_BEATS is reached and    *)
(*   every connected consumer is caught up. Same pattern as `Done` in     *)
(*   the post's bank_transfer example.                                     *)
(***************************************************************************)

Terminated ==
    /\ Len(published) = MAX_BEATS
    /\ \A c \in CONSUMERS : consumer_joined[c] /\ consumer_window[c] = window
    /\ UNCHANGED vars

Next ==
    \/ ProduceBeat
    \/ \E c \in CONSUMERS : ConsumerConnect(c)
    \/ \E c \in CONSUMERS : DeliverWindow(c)
    \/ Terminated

Spec == Init /\ [][Next]_vars
        /\ WF_vars(ProduceBeat)
        /\ \A c \in CONSUMERS : SF_vars(DeliverWindow(c))
        /\ \A d \in CONSUMERS : WF_vars(ConsumerConnect(d))

(***************************************************************************)
(* Safety invariants                                                       *)
(*                                                                         *)
(* Each maps to a guarantee in DESIGN.md section 3.                        *)
(***************************************************************************)

\* §3.1 Immutability: every frame the controller ever published either
\*   still sits in `window` at the correct slid-forward position, or has
\*   fallen off the low end. Never republished at a different value.
\*
\* The window holds the last min(Len(published), WINDOW_CAP) entries of
\* published, by construction. So: window must equal the corresponding
\* suffix of published.
Immutability ==
    LET shouldHold == IF Len(published) <= WINDOW_CAP
                      THEN Len(published)
                      ELSE WINDOW_CAP
        publishedTail == DropFirst(published, Len(published) - shouldHold)
    IN  window = publishedTail

\* §3.2 Monotonicity: strictly increasing within window, within published,
\*   and within every consumer's last-seen window.
Monotonicity ==
    /\ StrictlyIncreasing(published)
    /\ StrictlyIncreasing(window)
    /\ \A c \in CONSUMERS : StrictlyIncreasing(consumer_window[c])

\* §3.3 Forward extension only: stated as an action property -- the high
\*   end of `window` never decreases across a step. (Empty -> nonempty is
\*   fine; nonempty -> empty is not.)
WindowHigh(w) == IF w = <<>> THEN 0 ELSE Last(w)

ForwardExtensionOnly ==
    [][WindowHigh(window') >= WindowHigh(window)]_vars

\* §3.4 Consumer agreement: any two consumers, on any frame value they
\*   both hold, agree on the *relative* position of that frame within
\*   their windows -- because both windows are suffixes of the same
\*   `published`. (Trivial corollary of Immutability + the way
\*   DeliverWindow installs `window` verbatim, but stating it explicitly
\*   catches regressions in the delivery action itself.)
ConsumerAgreement ==
    \A x, y \in CONSUMERS :
        \A i \in 1..Len(consumer_window[x]) :
            \A j \in 1..Len(consumer_window[y]) :
                consumer_window[x][i] = consumer_window[y][j] =>
                    \* same value implies matching suffixes/prefixes
                    LET o == consumer_window[x]
                        n == consumer_window[y]
                        overlapLen == IF Len(o) - i + 1 < Len(n) - j + 1
                                      THEN Len(o) - i + 1
                                      ELSE Len(n) - j + 1
                    IN  \A k \in 0..overlapLen-1 : o[i+k] = n[j+k]

\* Window capacity bound (BeatPublisher::new(capacity) contract).
WindowBounded == Len(window) <= WINDOW_CAP

(***************************************************************************)
(* Liveness                                                                *)
(***************************************************************************)

\* Every connected consumer eventually sees the controller's current
\* window. Note: if more beats are produced after delivery, the consumer
\* must catch up to *those* in turn -- which is what []<> over time gives
\* us implicitly via repeated DeliverWindow firings.
EventualCatchup ==
    \A c \in CONSUMERS :
        consumer_joined[c] ~> (consumer_window[c] = window)

\* Production progresses until the budget is exhausted.
Progress ==
    (Len(published) < MAX_BEATS) ~> (Len(published) = MAX_BEATS)

=============================================================================
