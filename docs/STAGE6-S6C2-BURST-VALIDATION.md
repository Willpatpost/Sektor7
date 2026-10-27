# STAGE 6 — S6-C2 BURST VALIDATION

> **Status:** v2 · 2026-10-27 · **S6-C2 implementation artifact** — the
> finite-burst event stress, identity-conservation, and no-silent-drop
> verification matrix (Stage 6 Checkpoint 2, the owner GO of 2026-10-27),
> corrected per the source review's three test-harness findings (C2-F1
> the B3 reservation verification, C2-F2 the unique scenario identities,
> C2-F3 the deterministic B8 gates). Delivered on the
> `s6-c2-burst-accounting` branch; NOT yet independently reviewed or
> owner-accepted. S6-C3 (the 900-second soak), S6-C4 (hardware), and
> S6-C5 (the report) remain gated.

## 1. The burst cohort

All injection goes through the accepted public boundary
`OrchestratedSession::schedule_event(EventSpec)` — never a mutation of
spine internals. The cohort is finite, deterministic, and clearly
identified:

- **Payload:** `magic(16) | cohort(16) | index(32)` with
  `BURST_MAGIC = 0x5336` — distinct from the routine-chain tag
  `ROUTINE_MAGIC = 0x5E47`. The NPC handler's region-LOD path decodes a
  payload as a routine chain ONLY when the top-16 tag equals
  `ROUTINE_MAGIC` (`npc.rs::routine_payload_parts` — foreign payloads
  stay foreign), so a burst payload **cannot** advance NPC steps,
  milestones, pending signals, or credits, and never schedules a
  follow-up. The legacy path's identity decode would also reject it
  (`payload >> 16` carries the magic bits, far beyond `NPC_COUNT`).
  This is proven end-to-end by the `foreign_payloads_never_advance_npc_
  routine_state` gate: the world's total steps advance by EXACTLY the
  non-cohort ledger executions over the burst window.
- **Identity (C2-F2):** every successful `schedule_event` return value
  is recorded in the registry with its scenario identity
  `(cohort, index)`; the indices are ALLOCATED deterministically per
  cohort (a per-cohort allocator in the harness), so repeated slices —
  B6's 30 injection windows, B8's 10 load slices, the repeated
  foreign-payload windows — never reuse a payload; the uniqueness is
  ASSERTED independently of EventId uniqueness. Every typed refusal is
  recorded separately with its reason and scenario identity — never
  counted as accepted.
- **Routing:** the class targets a valid region and rides the existing
  region-aware policy (a dormant target takes the held path —
  `region.rs`'s schedule routes dormant classes through
  `Spine::schedule_held`); the field selects the near/far lane
  explicitly. The burst policy lives entirely in the test harness.

## 2. The independent identity oracle

The spine's own `scheduled`/`executed` counters are NOT the verdict.
The registry (built only from the scheduler's actual return values) is
compared against the spine's **recorded execution identities** —
`try_ledger_full` (asserted `HistoryKind::Full`; the bounded scenarios
never compact) gives per-identity execution counts (exactly-once and
duplicate detection) and first-class lateness;
`collect_matching`/`held_view` give the queued / blocked / held states.
The silent-drop signal is `missing` — an accepted identity in NO state.
The whole-campaign conservation identity
`scheduled == executed + queued + blocked + held` (the spine's own
definitions) is verified at every observation boundary; the living NPC
campaign's recurring obligations are part of the accounted state and
are never required to drain. A correct zero-silent-drop verdict is
therefore supported by the identity-level oracle, not a zero error
counter.

## 3. The B1–B8 matrix (all VERIFIED as correctness gates)

| Scenario | Gate |
|---|---|
| **B1** baseline synchronized burst — the historically relevant 200-event same-due cluster | full accounting at the due-tick boundary; exactly-once; no missing; the honest lateness implication (identities outstanding at the due tick necessarily execute late); conservation; full drain; **double-run determinism** (identical accepted-identity sets, execution sets, verdicts) |
| **B2** budget exhaustion (budget 100 vs a 150-burst plus the baseline) | exhaustion observed and recorded (`budget_exhausted_ticks`); work explicitly outstanding; subsequent progress; eventual exactly-once completion |
| **B3** near/far reservation (budget 60, reserve 20; 60 near + a 240 far flood, both due AT the verified boundary tick) — **the C2-F1 correction** | the eligible cohort obligations established at the boundary (60 near / 240 far queued); the FULL lane eligibility measured (the deterministic NPC baseline included) and the exact **three-phase policy** verified per tick (`near up to the reservation, far up to the nonreserved capacity, the remainder redistributed near-first` — the ratified `Spine::run_tick` contract, never a new fairness policy): at the saturated due tick the near lane receives EXACTLY its guaranteed reservation (20) and the far lane EXACTLY the nonreserved capacity (40), the total exactly the budget, `near_backlogged` as the honest total-saturation signal; the same exact contract over the loaded window; both cohorts drain (neither lane drops); and the **near-first redistribution EXHIBITED** in the post-drain regime (far eligibility below the nonreserved capacity, the near lane's service above the reservation) |
| **B4** causal DAG (three chains + a cross-class diamond, dependents due BEFORE prerequisites so the blocking window is observable) | the execution-LEDGER order preserves every prerequisite→dependent edge (execution order, never dequeue priority); blocked work retained and released; exactly-once completion; unknown dependencies explicitly rejected (recorded as rejections, never accepted) |
| **B5** dormant holds and reactivation (region 5 dormant at founding) | the cohort enters the held path with identities unchanged; nothing executes while dormant; no NPC-state change in the region; the six-phase reactivation makes the work eligible; exactly-once completion; the routine reconstruction resumes |
| **B6** sustained overload and recovery (budget 60 vs 100 injected/tick for 30 ticks, every window's scenario identities asserted unique) | the outstanding cohort GROWS during the window and is never falsely declared recovered; peak backlog and max lateness recorded; complete drain after the injection stops; an `s6-evidence/2` package emitted through the accepted S6-C1 collector and verified by `verify_package` |
| **B7** late arrivals (dues behind the clock) | `late_arrivals` reported; the safe watermark never overclaims past a pending earlier-due event and stays ≤ max executed due; exactly-once; lateness first-class |
| **B8** persistence under burst load — **the C2-F3 correction** | a 200-burst load with unique scenario identities; the publication queue **deterministically saturated** through the accepted `WorkerFault::ParkExecutingAppend` seam (the captures collected FIRST — the observed-head read is a worker round-trip — then the consumer parked at its entry barrier with a bounded `park_entered` wait, the queue filled against the KNOWN blocked consumer, and the release on every path BEFORE any assertion: the typed `Busy` refusal is proven regardless of host scheduling); a **WITNESSED** capture-bearing six-phase deactivation (the region's level and the director's captured-delta counter read before and after — never assumed) with the required canonical confirmation **consistent with the witnessed capture** (a `Confirmed` receipt only over captured consequences; a valid `NothingPending` only when nothing was captured — never a claimed new confirmation); per-identity **exactly-once occurrence counting** (each accepted identity exactly once in the ledger — no missing, no duplicates — with the full-history availability checked before the scan); a graceful close; an EXACT recovery (clock, stage-4 fingerprint, conserved obligations) and a continued tick |

Focused failure/boundary gates: the authority-gate refusal (poisoned
worker → typed `Gate` rejections, the composed tick refuses, the close
honestly reports the failure — never a claimed success); invalid
dependency rejection with the scenario identity recorded; repeated
`is_executed` observations stable and side-effect-free; a non-dormant
target scheduling into the live queue; observation boundaries with
undrained cohorts accounted exactly.

## 4. Observations (NOT gated)

B6's recorded measurements (pod, dev-profile — never presented as
physical hardware results, never CI thresholds): injected 100/tick;
the cumulative executed count and outstanding/backlog series per tick;
the peak cohort backlog; the maximum queue depth; the drain tick
count; the max lateness in ticks and the late-execution count. B1's
coincident-baseline lateness depends on the due tick's NPC demand —
on-time full service is equally valid and not penalized. B3's
measured lane eligibilities per tick are recorded in the assertion
context; the gated property is the exact three-phase contract.

## 5. Limitations

The identity-level oracle requires the full (uncompacted) execution
ledger — asserted in every scenario (the bounded scenarios never
compact; S6-C3's soak-scale runs must re-check `HistoryKind` and use
the retained-tail APIs plus the compaction capability checks if
compaction is in play). The queued/blocked per-identity resolution
scans the live queues (O(queue) per observation) — bounded and
acceptable for the matrix, not a soak-scale per-tick pattern. B8 is a
bounded integration test, not a sustained soak. B7's watermark check
is presence-based (None or ≤ the pending due); constructing a precise
watermark-retreat scenario is deferred to S6-C3's checkpoint-tick
comparisons. B8's parked-consumer saturation relies on the accepted
worker fault seam's entry-barrier semantics (the captures must be
collected while the consumer is live — the observed-head read is a
worker round-trip). No new read-only engine-core capability was
required — the existing public APIs sufficed.
