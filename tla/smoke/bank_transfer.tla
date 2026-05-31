---------------------------- MODULE bank_transfer ----------------------------
EXTENDS Naturals, TLC

VARIABLES a_balance, b_balance, in_flight

vars == <<a_balance, b_balance, in_flight>>

TypeInvariant ==
    /\ a_balance \in 0..100
    /\ b_balance \in 0..100
    /\ in_flight \in 0..100

MoneyConserved == a_balance + b_balance + in_flight = 100

Init ==
    /\ a_balance = 100
    /\ b_balance = 0
    /\ in_flight = 0

Debit ==
    /\ a_balance >= 10
    /\ a_balance' = a_balance - 10
    /\ in_flight' = in_flight + 10
    /\ UNCHANGED b_balance

Credit ==
    /\ in_flight > 0
    /\ b_balance' = b_balance + in_flight
    /\ in_flight' = 0
    /\ UNCHANGED a_balance

Done ==
    /\ a_balance = 0
    /\ in_flight = 0
    /\ UNCHANGED vars

Next == Debit \/ Credit \/ Done

Spec == Init /\ [][Next]_vars
=============================================================================
