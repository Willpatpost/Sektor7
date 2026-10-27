# SLICE 0 INTEGRATION DEMONSTRATION (`--demo-persist`)

> **Status:** Delivered on `slice0-integration-demo` for independent review — the owner's Slice 0 Integration Demonstration GO, Checkpoint 1. **NOT YET PHYSICALLY ACCEPTED:** the graphical verification belongs to the owner's physical Windows rig; everything runnable headlessly is proven by the focused integration tests (`engine/crates/engine-slice/tests/integration_demo.rs`) and the full workspace gates. Delivery is not acceptance.

## What this is

ONE authoritative, persistently-orchestrated campaign behind the existing Vulkan render loop — the first composed execution path over the accepted Stage 1–4 mechanisms (SDL3 shell, Vulkan renderer, fixed-step deterministic simulation, NPC/region LOD) and the accepted production persistence (PP-B1 SQLite store, PP-B4 orchestrated session: the worker-backed crossing, optional snapshot publications, required canonical confirmations, graceful durable close). Nothing below is a new engine mechanism; the demonstration composes the accepted ones.

The five GO behaviors:

- **A — start or recover a campaign.** `StoreLocation::Database` (the owner-ratified SQLite provider; the T1 journal beside it). A first launch founds and immediately publishes a *verified recovery anchor*; a relaunch recovers from the durable state alone. Corrupt, incompatible, divergent, or unverifiable state fails closed with the typed diagnostic — never overwritten, never refounded as a replacement.
- **B — one authoritative simulation.** Exactly one `OrchestratedSession`; the render loop derives everything read-only (the same downstream discipline as the pipeline-free `--render --regions` mode). The app owns only the fixed-step timing — the accumulator and bounded catch-up mirror `SimDriver::advance` exactly; outstanding obligations are retained and reported, never discarded.
- **C — one deliberate player action: `Space`.** A bounded 3-tick *presence excursion* on the region the camera stands in — that region is driven through the accepted consequence lifecycle: durable deactivation (its NPCs' accrued uncommitted milestone signals captured under the T1 journal before the discard) → required canonical confirmation (the `Confirmed` receipt with the consequence keys) → reactivation (the captures applied as **exactly-once credits** on those NPCs). No new event is injected: the workload's one-pending-routine-chain-per-NPC discipline is exactly what the accepted deactivation-plan codec validates, and the consequence flows entirely through the existing consequence mechanism.
- **D — truthful persistence state.** The overlay's campaign block reports the actual lifecycle: capture counts pending/applied, the tracked publication's state (pending-unverified / `ACCEPTED` with the provider tier / stale / unknown / failed / canceled — each its own label), the last required-confirmation outcome, and the action's progressive states. Nothing is ever called "saved" without a verified receipt.
- **E — quit and restart.** Exit runs the graceful durable close (the final required snapshot with readback verification, the SQLite WAL TRUNCATE lifecycle checkpoint, the settled required confirmations, the clean worker join — `failure == None` is the only complete success). A relaunch recovers exactly — no input replay, no duplicated consequence.

## The deliberate action (Space) — what happens, what you see

Standing on a lit region's plate, press `Space`:

1. The region is held at its antipodal presence for 3 tick boundaries — the plate visibly goes dark and its NPC proxies drop (the durable deactivation; the citizens' uncommitted milestone signals are captured under T1).
2. The required canonical confirmation runs — the overlay's `commit` line shows `confirmed N keys @ <revision>`.
3. The live presence resumes — the region reactivates, the plate lights again, and the captured signals are applied as credits exactly once. The overlay's `action` line reports `signals S · confirmed N keys @ <rev> · credits C→C+S`.
4. A snapshot publication is submitted (optional, nonblocking) — the overlay's `last pub` line goes `pending (unverified)` → `ACCEPTED <revision> (TransactionalWalFull)`.

**Timing note (honest):** the captured consequence is the region's *accrued* milestone signals — at the 10 Hz demo cadence a region's citizens need roughly 10–15 seconds of live presence before their first milestones accrue. Pressing `Space` earlier performs the same excursion honestly with an empty capture (the record shows `signals 0`; an empty-scratch deactivation journals nothing, by the accepted rule).

## Running

### Headless — deterministic, no GPU (the development/CI evidence path)

```sh
cd engine
cargo run -q --release -p engine-slice -- --demo-persist /tmp/s7-demo --headless --ticks 240 --action-at 100
cargo run -q --release -p engine-slice -- --demo-persist /tmp/s7-demo --headless --ticks 60
```

The first command founds the campaign, runs 240 fixed ticks under the fixed demo presence, fires the action at tick 100, and closes; the second relaunches the same directory (recovery), continues 60 ticks, and closes. Expected (observed on the development pod 2026-10-27):

```
action @ tick 100: excursion r0 @ tick 100 (3 ticks) — signals 28 · confirmed pending · credits 0→…
Slice 0 integration demonstration — headless report
  campaign: founded · db /tmp/s7-demo/campaign.db · journal /tmp/s7-demo/journal.t1
  recovery anchor: 03dd4928…
  action: excursion r0 @ tick 100 (3 ticks) — signals 28 · confirmed 28 keys @ 90000250… · credits 0→28
  action publication: ACCEPTED 59a7e5fb… (TransactionalWalFull)
  closed action region r0: signals 222 · credits 28
  closed at tick 240 · stage4 fingerprint 52cd84fa54fb6e60
  close: final 82b1feb3… · checkpoint true · required settled true · failure None
```

and the relaunch:

```
  campaign: recovered · db … · journal …
  recovered at tick 240 · restored fingerprint 52cd84fa54fb6e60
  closed at tick 300 · stage4 fingerprint a121de77dd67c20f
  close: final cdf2c0e5… · checkpoint true · required settled true · failure None
```

(The recovered fingerprint equals the closed fingerprint — the exact durable outcome restored, checked against the canonical state, not a message.)

A tampered database fails closed: `error: open the campaign store …: file is not a database — refusing to touch the durable state` (exit 1; the file untouched).

### Graphical — the physical-rig procedure (NOT YET PHYSICALLY ACCEPTED)

```powershell
# 0) Verify the exact reviewed commit and a clean tree (no blind resets):
git fetch origin
git switch slice0-integration-demo
git rev-parse HEAD        # EXPECT: the reviewed delivery SHA (the delivery report's §3)
git status --porcelain    # EXPECT: empty

# 1) Headless pre-check on the rig (no GPU; proves the lifecycle end-to-end natively):
cd engine
cargo run -q --release -p engine-slice -- --demo-persist $env:TEMP\s7-demo --headless --ticks 240 --action-at 100
cargo run -q --release -p engine-slice -- --demo-persist $env:TEMP\s7-demo --headless --ticks 60
# EXPECT: the founded/recovered reports above with `failure None` closes.

# 2) The graphical demonstration (validation active):
Remove-Item -Recurse -Force $env:TEMP\s7-demo-graphical -ErrorAction SilentlyContinue
cargo run --release -p engine-slice -- --demo-persist $env:TEMP\s7-demo-graphical --validation on
#   - Banner: `campaign: founded · db … · journal …` + `recovery anchor verified: <hash>`
#   - Controls: WASD move · mouse look · Q/E roll · R/F up/down · Shift fast ·
#     Space action · O overlay · Esc/close quit (runs the graceful durable close)
#   - Stand on region 0's plate ~10–15 s (the citizens accrue milestone signals),
#     then press Space: the plate goes dark, the overlay's `commit` line confirms
#     `N keys @ <rev>`, the plate returns, and the `action` line shows the credits
#     applied (C→C+N). The `pub` line goes pending → ACCEPTED (TransactionalWalFull).
#   - Walk the ring: distant plates demote (their proxies drop); returning lights
#     them again — background crossings are visible and counted in the overlay.
# 3) Quit (Esc or the close button): the console prints the run/ simulation/
#    regions/ campaign summaries and the campaign close — EXPECT
#    `close: COMPLETE — verified final snapshot + lifecycle checkpoint + settled
#    confirmations + clean join`, plus `relaunch … to recover this campaign`.
# 4) Relaunch the SAME directory: the banner shows `recovered at tick N` — the
#    committed consequence and authoritative state restored exactly.
# 5) Validation: with `--validation on` the exit summary must report
#    `validation err/warn 0/0`; any error is a reportable anomaly (the run fails).
# Record the terminal output (Start-Transcript is fine) without overwriting
# prior evidence files.
```

## What the focused tests prove (`tests/integration_demo.rs`)

| GO category | Test | Proof |
|---|---|---|
| 1 — integrated consequence | `the_deliberate_action_persists_through_close_and_recovery` | the action's captured signals are canonically confirmed (keys > 0) and applied as exactly-once credits (`credits_after == credits_before + signals_before`); the close verifies; the chain head equals the verified final revision |
| 2 — persistence lifecycle | the same | fresh campaign → action → confirmation → close → recovery restores exactly the closed clock and state |
| 3 — no duplication | `the_recovered_continuation_matches_the_never_persisted_twin` | the persisted pair (close + recovery + continuation) ends EXACTLY at the never-persisted twin's stage-4 fingerprint — at the cycle-1 midpoint and the end — with the action region's counters equal |
| 4 — pending vs confirmed | `pending_publications_are_never_reported_as_confirmed` | a pending publication never reports a confirmed revision; the label mapping names every non-accepted class honestly |
| 5 — failure safety | `a_poisoned_worker_refuses_progression_and_the_close_reports_it` · `a_corrupt_database_fails_closed_without_overwriting` | a poisoned worker refuses ticks/action before the clock advances and the close reports the explicit failure (the durable state still recovers cleanly); a corrupt database fails closed with the file untouched |
| 6 — regression preservation | the full workspace gates | the 708-test accepted baseline plus the 5 new tests (713 passed / 0 failed / 1 ignored, debug and release); every frozen fingerprint and the canonical save self-test byte-identical double-run |

## Limitations (stated honestly)

- The headless path is correctness evidence only — no GPU, no window; the graphical verification is the owner's physical rig (this procedure).
- Development-pod timings are non-authoritative; physical rig measurements are the owner's.
- The deliberate action exercises the accepted consequence mechanism (capture → confirm → apply) — it is a demonstration, not production gameplay, combat, physics, or interaction framework.
- The accepted durability boundaries are unchanged: process-crash tier; physical power-loss durability NOT TESTED; no stronger claim is made or implied by anything here.
- Optional snapshot publication is asynchronous through the bounded worker; T1 durability, protected maintenance, and required confirmations remain synchronous, exactly as ratified.
