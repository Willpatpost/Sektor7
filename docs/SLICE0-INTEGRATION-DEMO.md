# SLICE 0 INTEGRATION DEMONSTRATION (`--demo-persist`)

> **Status:** **CLOSED-PASS — independently reviewed and owner-ratified, October 26, 2026.** The acceptance chain and the physical Windows evidence: [`SLICE0-INTEGRATION-ACCEPTANCE-RECORD-2026-10-26.md`](SLICE0-INTEGRATION-ACCEPTANCE-RECORD-2026-10-26.md). The accepted revision: `29ccfa601bf5bc67b91c4f704fbe53553760b4e5` (the corrective pass that resolved the independent review's five findings — the error-path finalization, the absolute time accounting, the target-attributed action lifecycle, the overlapping-action admission rule, and the strengthened pending-publication coverage). The instructions below remain the reproducible run.

## What this is

ONE authoritative, persistently-orchestrated campaign behind the existing Vulkan render loop — the first composed execution path over the accepted Stage 1–4 mechanisms (SDL3 shell, Vulkan renderer, fixed-step deterministic simulation, NPC/region LOD) and the accepted production persistence (PP-B1 SQLite store, PP-B4 orchestrated session: the worker-backed crossing, optional snapshot publications, required canonical confirmations, graceful durable close). Nothing below is a new engine mechanism; the demonstration composes the accepted ones.

The five GO behaviors, as corrected:

- **A — start or recover a campaign.** `StoreLocation::Database` (the owner-ratified SQLite provider; the T1 journal beside it). A first launch founds and immediately publishes a *verified recovery anchor*; a relaunch recovers from the durable state alone. Corrupt, incompatible, divergent, or unverifiable state fails closed with the typed diagnostic — never overwritten, never refounded as a replacement.
- **B — one authoritative simulation, with the absolute time accounting.** Exactly one `OrchestratedSession`; the render loop derives everything read-only (the same downstream discipline as the pipeline-free `--render --regions` mode). The app owns only the fixed-step timing — the accumulator and bounded catch-up mirror `SimDriver::advance` exactly; outstanding obligations are retained and reported, never discarded. The time accounting is **absolute across sessions**: the recovered starting clock is included, so `ticks due == campaign clock + outstanding` holds for founded, recovered, recovered-with-outstanding, and stalled-frame runs alike.
- **C — one deliberate player action: `Space`, target-attributed.** A bounded 3-tick *presence excursion* on the region the camera stands in — that region is driven through the accepted consequence lifecycle: durable deactivation (its NPCs' accrued uncommitted milestone signals captured under the T1 journal before the discard) → required canonical confirmation (the `Confirmed` receipt, **attributed to the target's actual consequence identities only** — the receipt filtered against the target's expected `ConsequenceId` set; unrelated same-ticket regional captures are never claimed) → reactivation (the captures applied as **exactly-once credits**, the applied change verified against the action's actual captured delta *before* completion is reported). **One action at a time**: a press during an active excursion, or while the previous action's captured consequences still await their target's reactivation, is refused without changing the active action's target or attribution.
- **D — truthful persistence state.** The overlay's campaign block reports the actual lifecycle: the action's phase, the capture counts pending/applied, the tracked publication (pending-unverified / `ACCEPTED` with the provider tier / stale / unknown / failed / canceled — each its own label), the required-confirmation outcomes, and the absolute time accounting. Nothing is ever called "saved" without a verified receipt.
- **E — quit and restart, finalized on every path.** EVERY exit after the campaign opened — the user quit, the `--frames` limit, **or an application-level failure (a refused tick, a render error)** — runs the single `PersistDemo::finalize` site: the accepted graceful close (the final required snapshot with readback verification, the SQLite WAL TRUNCATE lifecycle checkpoint, the settled required confirmations — `failure == None` the only complete success), with the original application error preserved and the close outcome reported independently; a failed close is never represented as successful. A relaunch recovers exactly — no input replay, no duplicated consequence.

## The deliberate action (Space) — the honest phase lifecycle

Standing on a lit region's plate, press `Space` (one action at a time):

1. **Excursion** — the region is held at its antipodal presence for 3 tick boundaries: the plate visibly goes dark, its NPC proxies drop (the durable deactivation; the citizens' uncommitted milestone signals are captured under T1).
2. **Confirmation, attributed** — the required canonical confirmation runs; the action records the receipt's TARGET-REGION identities only (`applied N keys @ <revision>`), never the same-ticket captures of other regions that also deactivated.
3. **Reactivation-gated application** — when the live presence returns, the region reactivates and the captured signals apply as credits exactly once; the action reports `applied` only after the target actually reactivated AND the applied change matched the captured delta.

The honest non-complete states (never falsely claimed):

- **`AwaitingReactivation`** — the capture is durably confirmed, but the target has not reactivated (e.g., the player walked away during the excursion): `confirmed N keys @ <rev> — awaiting the target's reactivation (credits NOT yet applied)`. It completes honestly at the target's next reactivation.
- **`ZeroCapture`** — the excursion ran before milestone signals accrued: a zero-consequence no-op (`nothing captured, nothing applied` — an empty-scratch deactivation journals nothing, by the accepted rule; no nonexistent confirmation is claimed).
- **`Incomplete`** — attribution could not be verified honestly (an unknown/stale confirmation, or an applied change that mismatched the captured delta): reported with its reason, never claimed complete.

**Timing note (honest):** the captured consequence is the region's *accrued* milestone signals — at the 10 Hz demo cadence a region's citizens need roughly 10–15 seconds of live presence before their first milestones accrue. Pressing `Space` earlier performs the same excursion honestly as a `ZeroCapture` no-op.

## Running

### Headless — deterministic, no GPU (the development/CI evidence path)

```sh
cd engine
cargo run -q --release -p engine-slice -- --demo-persist /tmp/s7-demo --headless --ticks 240 --action-at 100
cargo run -q --release -p engine-slice -- --demo-persist /tmp/s7-demo --headless --ticks 60
```

The first command founds the campaign, runs 240 fixed ticks, fires the action at tick 100, and closes; the second relaunches the same directory (recovery), continues 60 ticks, and closes. Expected (observed on the development pod 2026-10-27, after the corrective pass):

```
action @ tick 100: excursion r0 @ tick 100 (3 ticks) — captured-delta 28 · excursion running · credits 0→…
Slice 0 integration demonstration — headless report
  campaign: founded · db /tmp/s7-demo/campaign.db · journal /tmp/s7-demo/journal.t1
  recovery anchor: 03dd4928…
  action: excursion r0 @ tick 100 (3 ticks) — captured-delta 28 · applied 28 keys @ 90000250… · credits 0→28
  action publication: ACCEPTED 59a7e5fb… (TransactionalWalFull)
  closed action region r0: signals 222 · credits 28
  closed at tick 240 · ticks due 240 · outstanding 0 · conservation true · stage4 fingerprint 52cd84fa54fb6e60
  close: final 82b1feb3… · checkpoint true · required settled true · failure None
```

and the relaunch — the **absolute time accounting** correct after recovery (the pre-corrective defect displayed `60` here):

```
  campaign: recovered · db … · journal …
  recovered at tick 240 · restored fingerprint 52cd84fa54fb6e60
  closed at tick 300 · ticks due 300 · outstanding 0 · conservation true · stage4 fingerprint a121de77dd67c20f
  close: final cdf2c0e5… · checkpoint true · required settled true · failure None
```

(The recovered fingerprint equals the closed fingerprint — the exact durable outcome restored, checked against the canonical state, not a message. `conservation true` is the report-derived invariant `ticks due == campaign clock + outstanding`.)

A tampered database fails closed: `error: open the campaign store …: file is not a database — refusing to touch the durable state` (exit 1; the file untouched). An application-level failure mid-run (e.g., a persistence-worker death) exits THROUGH the finalization: the report preserves the failure and the independent close outcome (the binary exits nonzero on either).

### Graphical — the physical-rig procedure (the owner's physical acceptance COMPLETED 2026-10-26 — PASS; the record §E)

```powershell
# 0) Verify the exact reviewed commit and a clean tree (no blind resets):
git fetch origin
git switch slice0-integration-demo
git rev-parse HEAD        # EXPECT: the reviewed corrective SHA (the delivery report's §8)
git status --porcelain    # EXPECT: empty

# 1) Headless pre-check on the rig (no GPU; proves the lifecycle end-to-end natively):
cd engine
cargo run -q --release -p engine-slice -- --demo-persist $env:TEMP\s7-demo --headless --ticks 240 --action-at 100
cargo run -q --release -p engine-slice -- --demo-persist $env:TEMP\s7-demo --headless --ticks 60
# EXPECT: the founded/recovered reports above — the action `applied N keys`, the
# publication ACCEPTED, `conservation true` on both runs, `failure None` closes.

# 2) The graphical demonstration (validation active):
Remove-Item -Recurse -Force $env:TEMP\s7-demo-graphical -ErrorAction SilentlyContinue
cargo run --release -p engine-slice -- --demo-persist $env:TEMP\s7-demo-graphical --validation on
#   - Banner: `campaign: founded · db … · journal …` + `recovery anchor verified: <hash>`
#   - Controls: WASD move · mouse look · Q/E roll · R/F up/down · Shift fast ·
#     Space action (one at a time) · O overlay · Esc/close quit (runs the graceful durable close)
#   - Stand on region 0's plate ~10–15 s (the citizens accrue milestone signals),
#     then press Space: the plate goes dark, the overlay's `commit` line confirms,
#     the plate returns, and the `action` line shows `applied N keys @ <rev>` with
#     the credits applied (C→C+N). Pressing Space again DURING the excursion or
#     before the target reactivates is refused (one action at a time).
#   - Walk away immediately after the excursion instead of returning: the action
#     line stays honestly `confirmed N keys — awaiting the target's reactivation
#     (credits NOT yet applied)`; walk back and it completes.
#   - Press Space within the first ~10 s (before milestones accrue): the action
#     completes as an honest `zero-capture no-op`.
#   - The `pub` line goes pending → ACCEPTED (TransactionalWalFull) — a separate
#     acknowledgment from the required confirmation.
#   - Walk the ring: distant plates demote (their proxies drop); returning lights
#     them again — background crossings are visible and counted in the overlay.
# 3) Quit (Esc or the close button): the console prints the run/ simulation/
#    regions/ campaign summaries and the campaign close — EXPECT
#    `close: COMPLETE — verified final snapshot + lifecycle checkpoint + settled
#    confirmations + clean join`, plus `relaunch … to recover this campaign`.
#    (If anything failed mid-run instead, the console shows the APPLICATION
#    FAILURE and the independent close outcome — never a false success.)
# 4) Relaunch the SAME directory: the banner shows `recovered at tick N` — the
#    committed consequence and authoritative state restored exactly.
# 5) Validation: with `--validation on` the exit summary must report
#    `validation err/warn 0/0`; any error is a reportable anomaly (the run fails).
# Record the terminal output (Start-Transcript is fine) without overwriting
# prior evidence files.
```

## What the focused tests prove (`tests/integration_demo.rs`, 11)

| The review's finding | Test | Proof |
|---|---|---|
| GO 1+2 — integrated consequence + lifecycle | `the_deliberate_action_persists_through_close_and_recovery` | the action's capture is confirmed (target keys > 0), applied as exactly-once credits (`credits_after == credits_before + captured_delta`), closed (the chain head == the verified final revision), and recovered exactly (the restored clock == the closed clock) |
| GO 3 — no duplication | `the_recovered_continuation_matches_the_never_persisted_twin` | the persisted pair ends EXACTLY at the never-persisted twin's stage-4 fingerprint — at the cycle-1 midpoint and the end — with the action region's counters equal |
| The pending-publication finding | `pending_publications_are_never_reported_as_confirmed` | a GENUINE nonempty capture: the required confirmation verified and attributed while the optional publication separately remains pending (deterministically — unpolled); the two acknowledgments never conflated; only the verified `Accepted` receipt updates the label |
| GO 5 — failure safety | `a_poisoned_worker_refuses_progression_and_the_close_reports_it` · `a_corrupt_database_fails_closed_without_overwriting` | refusal before the clock advances; the explicit failed close; the corrupt file untouched |
| **HOLD-1** — error-path finalization | `an_application_failure_after_open_still_finalizes_the_close_once` | a controlled mid-run worker death (the test seam) exits through the finalization: the original error preserved, the single close attempted and observable, the failed close never reported as success, the recovery consistent with the actual durable state |
| **HOLD-2** — absolute time accounting | `the_time_accounting_equation_holds_across_founding_and_recovery` | the equation holds at founding (240==240), at recovery (300==300 — the old `60 = 300 + 0` defect), and with a retained 12-tick catch-up obligation through its full drain (the stalled-frame mechanism) |
| **HOLD-3 A+C** — target attribution | `an_action_claims_only_its_target_regions_consequences` | with the target AND an unrelated region capturing in the SAME deactivation batch, the action's keys are exactly the target's own identities and the applied credits equal the TARGET's captured delta; the unrelated region's credits apply separately |
| **HOLD-3 B** — reactivation gating | `an_action_awaits_its_targets_reactivation_before_claiming_credits` | with the presence kept far after the excursion, the action stays `AwaitingReactivation` (confirmed, credits NOT applied) and completes only at the target's actual reactivation |
| **HOLD-3 D** — zero capture | `a_zero_capture_excursion_completes_as_an_honest_no_op` | an early press completes as a zero-consequence no-op — not pending, no false confirmation |
| The overlapping-action finding | `overlapping_space_presses_are_refused_without_overwriting` | presses during the active and unsettled states are refused with the first action's provenance preserved; its reward applies exactly once; a subsequent action is accepted and reported independently |
| GO 6 — regression preservation | the full workspace gates | the 708-test accepted baseline plus the 11-test suite (719 passed / 0 failed / 1 ignored, debug and release); every frozen fingerprint and the canonical save self-test byte-identical double-run |

## Limitations (stated honestly)

- The headless path is correctness evidence only — no GPU, no window; the graphical verification is the owner's physical rig (this procedure).
- Development-pod timings are non-authoritative; physical rig measurements are the owner's.
- The deliberate action exercises the accepted consequence mechanism (capture → confirm → apply) — it is a demonstration, not gameplay, combat, physics, or an interaction framework.
- The accepted durability boundaries are unchanged: process-crash tier; physical power-loss durability NOT TESTED; no stronger claim is made or implied by anything here.
- Optional snapshot publication is asynchronous through the bounded worker; T1 durability, protected maintenance, and required confirmations remain synchronous, exactly as ratified.
- The `AwaitingReactivation` state's later completion depends on the target's reactivation (the player returning); if the application closes first, the captured consequence remains durably confirmed and applies at the recovered session's next reactivation — never lost, never double-applied.
