# STAGE 6 — S6-C3 AUTOMATED INTEGRATED SOAK RUNNER

> **Status:** v2 · 2026-10-27 · **S6-C3 implementation artifact** — the
> deterministic integrated scenario, the automated wall-clock soak
> runner, the corrected time-debt termination protocol, the
> checkpoint-scale identity accounting, and the bounded CI smokes
> (Stage 6 Checkpoint 3, the owner GO of 2026-10-27), CORRECTED per
> the source review's five findings (C3-F1 the after-tick checkpoint
> observation, C3-F2 the fail-closed evidence verification / time
> accounting / summary publication / final-verdict ordering, C3-F3
> the workload-completeness gates, C3-F4 the coverage-feasibility
> correction, C3-F5 the cutoff attribution and top-level CLI
> consistency). Delivered on the `s6-c3-integrated-soak` branch; NOT
> yet independently re-reviewed or owner-accepted. S6-C4 (physical
> hardware measurements) and S6-C5 (the report) remain gated; this
> document never claims physical performance acceptance from pod or
> CI observations.

## 1. The interface

The Stage 1 `--soak <seconds>` command is preserved exactly (the pure
SDL shell soak — no simulation, no persistence; it never interacted
with the demo and still does not). S6-C3 adds a distinct, namespaced,
STRICT surface inside `--demo-persist` (every invalid value and
incompatible combination is a visible error with a nonzero exit —
never a silent default, never a silent clamp; all existing non-soak
CLI behavior is unchanged, including the legacy lenient sub-flags):

```sh
# The accelerated headless replay (determinism and CI correctness —
# never GPU evidence):
--demo-persist <campaign-dir> --headless --scenario stage6-v1-short \
  [--scenario-seed <n>] [--ticks <cap>] [--evidence-dir <fresh-dir>]

# The integrated graphical soak (the real Vulkan application; the
# formal gate is 900 seconds on physical hardware):
--demo-persist <campaign-dir> --soak-wall-seconds 900 \
  --scenario stage6-v1 --evidence-dir <fresh-evidence-dir> \
  [--scenario-seed <n>] [--stabilization-deadline-secs <n>] \
  [--fps <n>|uncapped] [--validation auto|on|off] \
  [--present auto|fifo|mailbox|immediate] [--no-overlay]
```

Rules (all enforced visibly): `--soak-wall-seconds` is valid only
with `--demo-persist` and requires `--scenario` and `--evidence-dir`;
it is incompatible with `--headless`, `--frames`, and `--ticks` (its
exit is the measured wall boundary plus the bounded drain, never a
frame count). `--scenario` is valid in exactly two modes — headless
(the accelerated replay) or the graphical soak with the wall window.
`--scenario` and `--action-at` are incompatible (the scenario owns
the action schedule). `--evidence-dir` requires `--scenario`; the
S6-C1 collector's exclusive-create destination makes a prior package
refuse with the typed `DestinationExists` error — a package is never
overwritten. `--soak-wall-seconds` accepts 1..=86400 (the formal gate
is 900; invalid durations are rejected, never clamped into an
accepted result). A scenario replay without `--ticks` runs the
scenario's full horizon.

The graphical soak runs the accepted `--demo-persist` render loop
exactly (SDL3 + Vulkan + the one authoritative orchestrated session);
the camera remains the VISUAL source while the scenario's scripted
presence is the AUTHORITATIVE input. Normal interactive behavior is
unchanged when no scenario is attached.

## 2. The tick-indexed scenario

**This is the mandatory determinism contract:** every authoritative
scenario input — the scripted presence, a burst injection, a
deliberate action, an optional publication — is a pure function of
the scenario version, the seed, and the LOGICAL TICK NUMBER, applied
inside `PersistDemo::run_ticks` per EXECUTED tick (never per rendered
frame). A catch-up batch of N ticks receives N distinct scripted
inputs, so frame cadence, FPS, and catch-up depth cannot change
logical behavior — verified by the four-pattern equivalence test
(one tick per call · the real-time cadence · 4-tick catch-up batches
· one deep stall followed by the no-new-time drain: identical
accepted identity sets, executions, checkpoint fingerprints, terminal
state, and durable revisions). No OS time, host scheduling, filesystem
enumeration order, random-device state, or render frame count
determines an authoritative input; the seed feeds only the documented
deterministic due-offset jitter (SplitMix64 over seed/tick/salt), so
the seed is a real part of the schedule identity (a different seed
changes the state; the schedule's shape is version-fixed). The
scenario never fabricates NPC routine consequences — burst payloads
carry the stage-6 burst tag `0x5336` (foreign to the routine-chain
decode, exactly as S6-C2's accepted cohort), and the deliberate
action runs through the accepted excursion lifecycle.

Two versioned profiles (logical-tick phase windows — never wall-clock
timers):

| Profile | Horizon | Use |
|---|---|---|
| `stage6-v1` | 9000 ticks (≈ 900 s at the 10 Hz cadence) | the formal soak workload |
| `stage6-v1-short` | 300 ticks | the compressed CI/determinism fixture |

The `stage6-v1` phases (the §5 plan verbatim): baseline 0–1199 (a
stationary plate occupancy, then a scripted walk across the ring) ·
clustered bursts 1200–1799 (the historical 200-event synchronized
same-due cluster at 1200, then 60-event near/far streams every 60
ticks — genuine lane contention) · backlog accounting and drain
1800–2999 (light due-far bursts every 150 ticks) · the real
consequential-action cycle 3000–3299 (the scripted excursion on the
long-occupied far plate at 3050 — capture → attributed confirmation →
reactivation-gated exactly-once credits → the optional publication) ·
bounded optional-publication pressure 3300–4199 (a scripted `publish`
every 30 ticks — 30 asynchronous submissions; required confirmations
are never starved by them, and Busy/refusals are recorded through the
accepted persistence evidence families) · sustained finite overload
4200–5399 (240 events/tick — ABOVE the total 200 budget: the routine
chains self-throttle, the lanes saturate, `budget_exhausted_ticks`
accrues, and the finite cohort's backlog grows measurably and
bounded) · overload termination and drain 5400–6599 (no injection;
the finite cohort drains — the living NPC campaign is never required
to reach zero) · the region/LOD tour 6600–7799 (a 150-tick-per-region
tour of all eight centers — dormant holds and reactivations; a
40-event cohort injected at 6650 targeting a then-dormant region
takes the held path and releases at the tour's arrival; the second
action cycle at 7750, 100 ticks into the final region visit, so the
reactivation-gated application completes in-window) · stabilization
7800–8999 (the final stationary presence, the last checkpoints, the
terminal accounting).

The `stage6-v1-short` fixture scales the same nine phases to 300
ticks. Its documented differences (never silently equivalent): the
injection rates are scaled, the overload window is 40 ticks, the
action cycle runs ONCE (phase 3 only), and the tour is compressed
(4 ticks per region; no second action).

Cohorts are phase-tagged and finite: the cluster (1), the stream
(2), the drain (3), the overload (5), the dormant-hold (7) — every
member's identity `(cohort, index)` allocated deterministically per
cohort (repeated slices never reuse a payload), every injection
through `OrchestratedSession::schedule_event` with `AtLogicalTime`
dues computed as the upcoming campaign tick plus a scripted offset
(never a late arrival by construction), every typed refusal recorded
separately (never counted as accepted).

## 3. The measured window and the corrected termination protocol

The measured period begins at READINESS — after the renderer, the
campaign open, the evidence attach, and the founding transitions —
with an explicit monotonic timestamp (the scenario's phase wall
stamps are offsets from it). At the wall boundary the loop breaks
BEFORE the next frame's interval reaches the accumulator: no further
wall time accrues and no further scripted inputs apply. The cutoff
state is recorded first — the actual wall duration, the campaign
tick, the fixed-step debt, the scripted ticks completed INSIDE the
window (the scenario's own counter at the cutoff — the drain's
post-cutoff ticks are reported separately, never conflated), and the
OBSERVED in-window coverage (whether each phase ran INSIDE the
measured window; phases reached during post-soak catch-up are
catch-up, never in-window evidence).

The controlled checkpoints are captured ONLY AFTER the boundary tick
has actually executed (and its confirmation and action lifecycle
processed): checkpoint N is the state after precisely N intended
logical ticks — the actual campaign clock, scenario tick, and
fixed-step debt at that boundary, aligned; a failed authoritative
tick can never be represented as a completed checkpoint.

The post-soak stabilization phase is DISTINCT and bounded: the
ALREADY-ACCRUED fixed-step debt drains through the accepted tick
path with NO additional wall-time accrual (`advance(Duration::ZERO)`
— the pinned no-new-time drain primitive; `PersistDemo::drain_bounded`
executes it under the separately configurable
`--stabilization-deadline-secs`, default 300). Unexecuted fixed-step
debt is NOT a canonical recovered obligation (the recovery
initializes the accumulator anew) — draining it here is the only
honest completion. The completion checks: `outstanding == 0` AND the
absolute accounting `ticks due == campaign clock + outstanding` (a
completed drain with false time conservation FAILS the workload
gate). A debt that cannot drain by the deadline marks the workload
gate FAILED/INCOMPLETE — the remainder reported, the measurements
preserved, the graceful close still attempted — never silently
discarded, never a manufactured success. A drain tick refusal
surfaces as an application failure through the single finalization
site. The drain's executed ticks consume the deterministic scripted
inputs for already-accrued logical time — honestly attributed to the
drain, never counted as in-window coverage. Event-spine
queued/blocked/held work is separate and keeps its accepted
durability contract.

Then, in order: the finite cohorts' terminal outcomes confirmed
against the versioned expectations (or their failures named) · the
existing graceful canonical close exactly once (the single
`PersistDemo::finalize` site) · the evidence package finalized AND
independently verified from disk (fail-closed: `Ok(verified
complete)` is the only passing outcome; an incomplete verification
OR a typed verifier error fails the run with the failure preserved —
never swallowed) · recovery in a fresh process (the runbook's
relaunch; the test binary's fresh-process pair proves the exact
durable state restored).

**The final verdict is computed once, over all checks, before the
summary is published** — the window, the drain with its time
conservation, the in-window workload coverage, the scenario's hard
gates, the validation layer, and the independent evidence
verification — so stdout, the summary, and the exit status never
contradict (the close outcome and the in-run evidence verdict
compose through the accepted `Finalization::combined()` mechanism).
The summary's publication is atomic (staging file + rename — a
failed write can never truncate an existing accepted summary) and a
failed publication is a terminal diagnostic plus a failed run: the
failed summary cannot record its own failure.

## 3a. The 900-second coverage-feasibility analysis (C3-F4)

**The structural fact:** the cutoff fires at the frame top, BEFORE
`begin_frame` — so the window's accrued time is the LAST ADVANCED
frame's start minus the measurement origin, which is STRICTLY less
than the window. At an exact-horizon window (9000 ticks = 900.000 s
of logical time at 10 Hz inside a 900 s window), the horizon's final
tick needs the full 900.000 s of accrual — which no run can ever
reach: the in-window scripted count is `floor((window −
frame_interval) / tick_period)`, strictly below the horizon for
EVERY frame interval (144 FPS, 60 FPS, irregular, or arbitrarily
fast — the final frame's interval is never accrued), and no accrued
debt remains for the drain (the shortfall is sub-tick). The
horizon's final scripted input is therefore NEVER consumed inside an
exact-horizon-window run — in-window, in-drain, or otherwise. This
is proven deterministically by the cutoff-feasibility regression
(the exact-horizon window over the real driver: 299/300 scripted
in-window, the drain settles zero debt, the honest unreached
horizon) and by the frame-rate arithmetic matrix.

**The gate correction (implemented):** the workload gate requires
the WORKLOAD PROPER to complete inside the measured window — every
phase entered AND the scripted count at the cutoff reaching the
workload-completeness boundary (the final stabilization phase's
start: tick 7800 in the full profile, 266 in the compressed fixture
— ~120 s / ~3.4 s of headroom respectively at the 10 Hz cadence).
`horizon_reached` at the cutoff stays an honest observation (the
CI's lavapipe smoke shows both truths: a 40 s window overruns the
compressed horizon — reached; a formal 900 s exact-horizon window
reports 8999/9000 — unreached by the structural one tick). No
logical tick is fabricated, no post-cutoff work is attributed to the
window, the 900-second requirement and the 9000-tick schedule are
unchanged.

**The owner decision offered (NOT implemented):** if an
in-window-complete horizon is wanted for the formal gate, the
smallest truthful adjustment is a versioned schedule revision —
`stage6-v1-r1` with the stabilization phase ending at 8990 ticks
(10 ticks = 1.0 s of planned headroom; the workload proper is
unchanged) — presented for separate owner review; the current
gate semantics make it unnecessary for a truthful PASS.

## 4. The checkpoint-scale accounting and the workload gates

The S6-C2 per-identity oracle is O(ledger)+O(queue) — correct at
controlled checkpoints and finalization, never per tick. Every
checkpoint verifies the O(1) whole-campaign conservation identity
with the spine's own definitions (the living NPC campaign's
recurring obligations are part of the accounted state and never
required to drain). The per-cohort live states come from single
queue/held passes bucketed by the registry's identity→cohort map
(never a per-cohort rescan). The full identity-level zero-silent-drop
verdict (executed exactly-once, duplicate detection, missing
detection, per-identity lateness) is established at finalization
through the FULL ledger when `HistoryKind::Full` holds (the demo
scenarios never compact — the full profile's 289,060 identities and
~1.5 M ledger records retain Full with ample headroom) — and a
compacted history DOWNGRADES the identity gate to NOT VERIFIED (the
by-subtraction accounting preserved and labeled, never silently
upgraded to an exact-once PASS). Fixed-step time debt is reported
separately from event-spine backlog at every boundary.

**The scenario verdict proves the WORKLOAD, not just arithmetic**
(each failure named; an empty list is the PASS): every intended
injection attempted and admitted against the versioned expected
totals (an unexpected rejection fails); every required cohort
present, fully accounted, and DRAINED at the terminal boundary;
exactly-once per identity with no missing and no duplicates under
the full-history proof; the consequential actions attempted at their
scripted ticks, accepted, and COMPLETED through the
applied-credits lifecycle (a pending, refused, awaiting, or
zero-capture action never passes); and the publication workload
attempted in full with every submission carrying a CONFIRMED durable
completion in the close's harvest (the pre-close pending count is
the unpolled ledger — an observation; an attempted call is never
confused with a completed durable publication).

## 5. Evidence, provenance, and the summary

`s6-evidence/2` unchanged (no schema weakening; the five-stream
package and its complete-package verification are exercised exactly
as S6-C1 accepted them — the scenario run's package test asserts the
complete verdict, the single `close` latency sample, and the
per-scripted-tick `ticks` stream). Scenario outcomes never enter the
evidence schema: the run's own report and summary carry them. The
manifest metadata records the run's provenance — the build-time git
commit and clean/dirty status (the honest "built-from" record; the
runbooks verify the runtime checkout independently), the platform,
the scenario/seed, the wall window and deadline, and the renderer's
device/presentation metadata (graphical runs). The graphical soak
writes `soak-summary.json` (`s6-soak-summary/1`) into the CAMPAIGN
directory — never inside the evidence package — with the measured
window, the cutoff state, the in-window coverage, the drain, the
terminal cohort accounting, the verdicts, the timing echo, and the
provenance. A complete evidence package is never confused with a
passing gate: the workload gate composes the window, drain, coverage,
scenario, close, and evidence verdicts into the run's exit code.

## 6. Verification

`tests/s6_soak.rs` (20 tests, all platforms, headless): the strict
CLI rejections · the four-pattern frame-cadence equivalence · the
seed sensitivity · the checkpoint conservation/hold/backlog series ·
the after-tick checkpoint boundary with the off-by-one
discriminators · the failed-evidence visibility · the fail-closed
evidence verification (complete / incomplete / typed error) · the
missing soak configuration failing before the window · the verified
package with the single-site close · the atomic fail-closed summary
publication · the drain gate with the false time conservation · the
workload-verdict arms (the missing cohort, the rejection, the
refused and pending actions, the compacted downgrade, the
outstanding cohort, the unpublished workload) · the interrupted
scenario's named failures · the compacted-history downgrade over a
real maintenance compaction · the unable-to-drain honesty · the
cutoff feasibility and attribution at an exact-horizon window · the
fresh-process recovery (the real binary, exact durable state
restored) · the externally terminated run's honest recovery · the
S6-C2-deferred watermark-retreat check · the top-level flag
rejection with the Stage 1 `--soak` legacy preserved. The pod and CI
carry no Vulkan loader for the graphical path — the bounded lavapipe
graphical smoke (below) belongs to CI's Linux runners, and physical
graphical acceptance belongs to the owner's rigs (the runbooks).

CI (bounded, never a 900-second job): the headless matrix rides the
existing `cargo test --workspace` gates on all three platforms; a
dedicated Linux lavapipe step runs the REAL binary offscreen for a
~40-second wall window over the compressed fixture — the integrated
graphical soak exercised end-to-end (window → cutoff → drain → close
→ evidence → fresh-process recovery) with validation active and an
explicit step timeout. Lavapipe is a software rasterizer: correctness
smoke only, never GPU performance evidence.

## 7. Limitations (stated honestly)

- The headless replay is correctness evidence only — no GPU, no
  window; graphical measurements belong to the soak and the rig.
- The formal 900-second wall-clock soak is NOT executed in CI or on
  the pod — S6-C4 (physical hardware) is separately gated.
- The workload gate's coverage check requires the WORKLOAD PROPER
  (all phases + the workload-completeness boundary) inside the
  measured window — NOT the horizon's final tick, which is
  structurally post-cutoff at an exact-horizon window (§3a); a slow
  runner reports the honest partial coverage (a failed gate,
  preserved measurements).
- The sustained-overload calibration (240/tick against the 200
  budget) is sized to the demo's 1000-NPC baseline; it exercises the
  budget machinery, not worst-case game scale.
- The dormant-hold observation window is pinned to the deterministic
  tour geometry (the full-hop lookahead releases the held cohort
  when the tour approaches the target region — the checkpoint
  boundaries record the actual deterministic schedule).
- `SoakMeasurement` wall stamps come from the platform monotonic
  clock; cross-platform wall comparisons are never logical evidence.
- The external-watchdog discipline (bounded process termination
  outside normal application control) is the runbook's invocation
  wrapper; the in-process drain deadline bounds everything the
  application controls. An externally terminated process's close
  status is honestly unknown — the campaign recovers from whatever
  durable state exists.
