# Sektor7 — Stage 5 Batch 5 record (lifecycle, verification, performance, and the final acceptance consolidation)

> **Status:** Implementation record for the Stage 5 Batch 5 delivery on `stage5`, per the lead's GO authorization (2026-10-23 — "Authorization: GO — Batch 5, Lifecycle, CI, Performance, and Final Verification", immediately following the Batch 4 CLOSED-PASS at `185a1e5`; the standing CONTINUE EXECUTION directive: operate autonomously within the approved scope, stop only at the acceptance gate). Baseline: `stage5@7bc1a11` (Batch 4 accepted implementation) + the closure `185a1e5`. Scope: the GO's eight items — the end-to-end session lifecycle, the crash/corruption verification matrix, the cross-platform verification, the persistence performance measurement, the storage-growth and retention analysis, the compaction decision evidence, the physical Windows-rig acceptance instructions, and the final Stage 5 implementation report. **Batch 5 is the final Stage 5 batch; every later work remains gated on separate authorization.** Not authorized and not undertaken: the Windows production storage-provider selection (the owner's decision), SQLite, asynchronous persistence without measured evidence, irreversible compaction or historical-evidence deletion without a reviewed protocol, production networking, and unrelated gameplay features.
>
> **Verification summary:** **479 tests + 1 intentionally ignored, 0 failed** (the 476 + 1 Batch 4 closure baseline, every one still green, plus the 3 lifecycle regressions — b5_lifecycle 0→3; the benchmark harness adds a measurement mode, not tests). `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace` green. Every historical fingerprint byte-identical, verified firsthand with pinned greps, double-run (selftest, save, ticks-300 `executed 45717` + both lane fingerprints, presence stationary/walk/tour + held). The three-platform CI evidence at the delivery head is recorded in `docs/DECISIONS.md`.

## 1. The end-to-end session lifecycle (the GO's first item)

`tests/b5_lifecycle.rs`, three regressions over the real 1000-NPC workload:

- **`session_lifecycle_graceful_close_and_restart_matches_the_twin`** — the complete arc: startup, campaign opening (the founding), verified snapshot publication and loading (the chain walk), journal recovery, deterministic continuation, milestone persistence, canonical confirmation, the **graceful close** (the final snapshot + the drop), and the **restart**. The closed-and-restarted durable session ends EXACTLY where the never-persisted legacy twin does (the combined Stage 4 fingerprint, the milestones, the credits; the continuation's captures canonically confirmed).
- **`lifecycle_crash_after_confirmation_restarts_coherently`** — the crash AFTER the canonical confirmation with only the pre-deactivation snapshot on disk: the restart recovers (the confirmed plan re-installs, the confirmation suppresses the replay, the confirmed identities stand InConfirmedChain) and the continuation reaches the twin's state — the **engine fingerprint** (the spine's full logical state: the executions, the follow-up chains, the ledger, the statistics), the milestones, the credits, and both ledger lanes. The presence-derived regions (3/5's empty-scratch deactivations, unjournaled by design) legitimately diverge at the intermediate points — re-derived at the continuation's boundary and re-reactivated by the near-presence run — reaching the twin's final level identically; the divergence and its resolution are asserted, not hidden.
- **`repeated_recovery_is_idempotent`** — the recovery is a pure function of the durable state: recovering twice from the same disk reconstructs the identical session (the combined fingerprint), and the re-drive after the second recovery confirms exactly once.

**The schedule-fidelity corrective the lifecycle test surfaced** (a latent Batch 4 defect, fixed in the same commit `455c817`): the deactivation plan flattened every obligation's schedule to `AtLogicalTime { due }`, but the routine chains self-sustain through **execution-relative** follow-ups (`due = origin + delay`, anchored to actual execution) — so a recovered chain resumed with wrong anchoring semantics (the execution ledger's schedule-kind record diverged; the spine's save was exactly 1,000 bytes shorter — 125 obligations × the 8-byte form difference). The correction: the plan carries the **full schedule form verbatim** — engine-core's `ObligationView` exposes the obligation's exact schedule (additive), `PlanObligation` carries it, and **`deactivation-plan/2`** is the explicitly versioned extension (the schedule discriminant + fields + the due-coherence gate `schedule.due() == due` on restore; v1's flattened form appeared only within B4-review test campaigns — no durable artifact carries one — so the walk reads v2 only, the version bump the documented migration boundary); `plan_events` reconstructs the exact original schedule, so the resumed chains execute with their original anchoring and the ledger's schedule-kind records match the never-crashed path.

## 2. The crash and corruption verification matrix (the GO's second item)

The complete lifecycle matrix, mapped window-by-window to its regression (each already green in the delivered suites):

| The GO's required window | The regression | Where |
|---|---|---|
| Unresolved T1 intents (the crash with pending unconfirmed captures) | `recovery_reinstalls_the_deactivation_after_a_crash` (the W2 window) + `recovery_restores_both_episodes_from_an_older_snapshot` (the multi-episode interleave) + `repeated_recovery_is_idempotent` | crossing.rs / b5_lifecycle.rs |
| Ambiguous journal writes | The B3 `WriteReconciliation` machinery composed at the crossing's phase 4 (auto-reconciled reread+verify; `ConflictingPayload` never `Committed`) — pinned at the B3 matrix | engine-persist (B3) |
| Post-confirmation crashes | `lifecycle_crash_after_confirmation_restarts_coherently` (the full lifecycle arc: the restart + the continuation to the twin's engine state) | b5_lifecycle.rs |
| Post-snapshot crashes | `recovery_reinstalls_the_deactivation_after_a_crash` (the crash after the snapshot + the durable deactivation) | crossing.rs |
| Repeated recovery | `repeated_recovery_is_idempotent` | b5_lifecycle.rs |
| Missing historical journal records | `recovery_recognizes_confirmed_pending_without_journal` + `recovery_recognizes_confirmed_applied_without_journal` (the journal deleted post-confirmation) | crossing.rs |
| Corruption | `recovery_fails_closed_on_invalid_newest_snapshot` (both the corrupt `/1` and the future `/2` domains) + the B2/B3 corruption corpora (torn HEAD never rolled back, flipped bytes preserved as evidence, the fail-closed journal rules) | crossing.rs + engine-persist |
| Incompatible schemas | `recovery_fails_closed_on_invalid_newest_snapshot` (the future-version session-snapshot domain recognized, fail-closed) + the B1 decoder's version gates | crossing.rs + engine-core |
| Divergent canonical histories | The B2 divergence corpus (external branch switch, vanished HEAD, missing divergent head, descendant suffix absorb; fail-closed `divergence` on non-descendant histories) — consumed unchanged by the crossing's per-operation medium handles | engine-persist (B2) |
| Mid-install process termination | `child_death_between_journal_and_install_recovers_from_disk` (the pre-install gap, exit 6) + `child_death_mid_install_recovers_from_disk` (the genuine mid-install kill, exit 7) — real child processes, independent parents | b4_process.rs |
| Cancellation/supersession | `recovery_skips_canceled_deactivation_transactions` (durably canceled plans never replay) + the episode frontier's three-case skip | crossing.rs |
| Confirmed-but-unmaterialized (insufficient evidence) | `recovery_fails_closed_on_confirmed_unmaterialized_without_evidence` (the `ConfirmedUnmaterialized` halt) | crossing.rs |
| Unsupported plan-less confirmed forms | `recovery_fails_closed_on_planless_confirmed_effects` (the explicit rejection — never a materialization that strands the session) | crossing.rs |

## 3. The cross-platform verification (the GO's third item)

The engine-ci matrix (Ubuntu, Windows, macOS ARM64) runs every push to `stage5` — the delivery head's run is recorded in `docs/DECISIONS.md` (verified firsthand job-level). The deterministic wire formats are pinned by the CI's own gates (the fingerprint self-tests double-run and pinned on all three platforms; the save-selftest pinned; the Stage 3/4 simulation smokes). The **Windows `SyncTier::ProcessCrashOnly` honesty is preserved**: the process-kill tests (the real child-death matrix) establish process-crash-tier recovery, never power-loss durability — the production provider selection remains the owner's (the physical-rig instructions in §7 include the rig-side benchmark that completes the Windows evidence).

## 4. The persistence performance measurement (the GO's fourth item)

**Methodology.** The `--benchmark-persistence` mode (`engine-slice/src/benchmark.rs`, commit `271aa7b`) measures the real workload — 1000 NPCs, 8 regions, the Lod scenario at the accrual peak (150 ticks), budget 200 / near-reserve 60 — as distributions (min/median/p95/max, n per operation): the snapshot serialization (`save_session`, the pure codec cost, n=20); the snapshot publication (the B2 nine-step append incl. its fsyncs, n=20, each publication a distinct session state — the session advances one tick per publication so the chain-unique idempotency key never collides); the T1 journal transaction (the six-phase deactivation's durable composite — the envelope staging, the plan blob, the journal append with its fsync, the post-fsync verify, the in-memory install — n=10 on fresh campaigns, every sample a first transaction on a fresh journal); the canonical confirmation (`commit_canonical`, n=10); the session recovery (`recover`, n=10); and the chain cold-open validation (the B2 founding→HEAD walk at growing chain lengths).

**Pod results (THIS environment — the coder pod, `/tmp` on overlayfs; explicitly NOT authoritative physical-rig evidence):**

| Operation | min | median | p95 | max |
|---|---|---|---|---|
| Snapshot serialization (no I/O) | 2.220 ms | 2.444 ms | 2.951 ms | 3.046 ms |
| Snapshot publication (the B2 append incl. fsync) | 56.3 ms | 152.9 ms | 240.9 ms | 251.9 ms |
| T1 journal transaction (the durable composite) | 8.363 ms | 8.471 ms | 8.710 ms | 8.710 ms |
| Canonical confirmation | 9.181 ms | 9.305 ms | 9.595 ms | 9.595 ms |
| Session recovery | 64.46 ms | 64.60 ms | 65.08 ms | 65.08 ms |
| Chain cold-open (6 / 11 / 21 revisions) | 64.9 / 105.7 / 216.4 ms — **linear, ~10.5 ms per revision** | | | |

**The 60/144/240 FPS interpretation.** The frame budgets: 16.667 / 6.944 / 4.167 ms per frame. The v1.8 design keeps every persistence operation OFF the per-frame path (the B4 non-negotiable: no journal I/O in per-NPC hot paths) — the measurements confirm the boundary discipline rather than test it: the only per-frame-adjacent operation, the pure serialization (2.44 ms median), fits all three budgets on the pod; the journal transaction (8.47 ms) and the confirmation (9.31 ms) fit a 60 FPS frame but exceed the 144/240 budgets — they are boundary operations (LOD transitions, save points), never frame work; the publication (152.9 ms median, p95 240.9 ms on overlayfs) and the recovery (64.6 ms) are save-point/session-start operations with no frame-budget interaction. **The overlayfs caveat:** the pod's `/tmp` is the container's overlay union filesystem — its fsync costs are not representative of a native filesystem; the Windows rig's native-NTFS measurements (§7's instructions) remain the authoritative physical evidence, and the B2 record's rig-side measurements remain the accepted durability evidence.

## 5. The storage-growth analysis (the GO's fifth item)

The five §5.6 storage classes, quantified on the real workload:

1. **Execution history** — the T1 journal grows **55,127 B per capture-bearing deactivation transaction** (the 125 capture envelopes + the plan blob + the framing); the spine's in-memory execution ledger is bounded by the outstanding obligation count (the budget-limited steady state) and rides inside the snapshot.
2. **Active indexes** — the session's whole active state (the spine lanes/held/blocked/known + the world arrays + the director states + the ledger) serializes to **220,438 B** — bounded by the workload (1000 NPCs), not per-tick growth.
3. **Snapshots/anchors** — the store grows **~230 KB per snapshot publication** (the ~220 KB snapshot object + the revision record); the growth rate is the application's save-point policy (the publication frequency is a policy choice, not a mechanical consequence).
4. **Canonical revision history** — one revision per publication and per confirmation batch; the confirmation batches are small (the per-key evidence, not the session state).
5. **Validation-walk cost** — the cold open is **linear at ~10.5 ms per revision** (the full verification — the cost of trusting nothing, per the B2 §5.3.2 design); the trusted cursor keeps every append O(1) (the B2 measurements: flat 24–31 µs at every chain size, `walked=0` proven by deterministic counters — the per-operation cost never grows with history).

**The long-term implication:** at one snapshot per minute of play, a 10-hour session accumulates ~600 revisions (~138 MB of snapshots) and a cold open of ~6.3 s; the journal grows with the LOD-transition rate (the capture-bearing deactivations — the tour scenario's 11 deactivations over 600 ticks ≈ one per ~55 ticks). The growth is unbounded by design (no retention protocol exists); the compaction decision follows.

## 6. The compaction decision evidence (the GO's sixth item)

**The assessment.** The accepted architecture (the v1.8 §5.6 authenticated-prefix + complement-of-unfinished design) provides the basis: the canonical chain's verified prefix can be summarized by an authenticated checkpoint (a new founding-class revision carrying the verified state), and the unfinished complement (the actionable unconfirmed T1 intents + the unmaterialized confirmed evidence) must be retained in full — the B3 contracts require exactly that set for coherent recovery. The measured evidence: the cold-open walk is the only cost that grows with history (linear, ~10.5 ms/revision — a session-start cost, amortized over the session, never a per-operation cost); the per-append cost is O(1) by the trusted cursor; the storage growth is ~230 KB per save-point (policy-controlled) + 55 KB per LOD transition. **There is no runtime-performance pressure: compaction is a startup-cost and disk-space optimization, not a correctness or responsiveness need.**

**The recommendation: compaction implementation belongs in a separately reviewed follow-up, not Batch 5.** The reasons: (1) the GO's restriction — irreversible historical-evidence deletion requires a reviewed retention and verify-before-reclaim protocol, which does not exist and is itself a design deliverable (the retention requirements below are its evidence base); (2) the measured urgency is low (the O(1) append path; the linear walk is session-start-only); (3) the retention requirements are subtle: the T1 journal's actionable-unconfirmed intents must be retained until confirmed-or-canceled (the B3 supersession contract); the canonical chain's confirmed evidence until the corresponding state is materialized in a verified snapshot (the coherence contract — the no-journal recovery case); the snapshots until superseded by a newer verified snapshot plus the journal's post-snapshot intents; a wrong cut in any class breaks a proven recovery guarantee. No retention generations are invented here; the follow-up's protocol design starts from these requirements with the measured growth rates as its input.

## 7. The physical Windows-rig acceptance instructions (the GO's seventh item — PENDING THE OWNER'S EXECUTION)

The reproducible rig plan (the RTX 5070 Ti rig, the Stage 4 acceptance environment), with the expected outputs and the PASS/HOLD criteria:

1. `git fetch && git checkout stage5 && git pull` (the delivery head — the SHA recorded in `docs/DECISIONS.md`).
2. `cd engine && cargo test --workspace` — **expect: 479 passed, 0 failed, 1 intentionally ignored** (any failure = HOLD).
3. `cargo run -q -p engine-slice -- --fingerprint-selftest` twice — **expect: `engine=6504123a47cab66f` / `state=937bd3b6f56f07e0` both runs** (any difference between runs or from the pins = HOLD).
4. `cargo run -q -p engine-slice -- --save-selftest` twice — **expect: `save-len=1209` / `save-fnv=c6c1b46c7d21843b` both runs**.
5. `cargo run -q -p engine-slice -- --sim --ticks 300 --cadence 60` — **expect: `executed 45717` + `engine-lanes=5591d3eb05643e78` / `state-lanes=9d90cb3bbd45b032`**.
6. `cargo run -q -p engine-slice -- --sim --presence stationary` / `walk` / `tour` — **expect: `stage4=da1b2d1a7e5cd49e` / `1df391ad488848bf` / `495ab27b37549e93`**.
7. `cargo run -q -p engine-slice -- --benchmark-persistence` — **expect: the full report without error; RECORD the distributions** (the authoritative native-NTFS fsync measurements — the pod's overlayfs numbers in §4 are explicitly non-authoritative). The serialization, journal transaction, and confirmation latencies on native NTFS are the production-relevant numbers; the report itself succeeding is the PASS criterion (the numbers are evidence, not gates — no threshold is asserted without the owner's first measurement).
8. `cargo test -p engine-slice --test b4_process --test b5_lifecycle -- --nocapture` — **expect: 2 + 3 passed** (the real process-kill matrix and the lifecycle arcs on native Windows — the process-crash tier, per the `SyncTier::ProcessCrashOnly` honesty).

**PASS = every command green with the pinned outputs.** **HOLD = any failure, any fingerprint divergence, or any panic.** The rig evidence remains pending until the owner executes and supplies it; the automated evidence (§1–§6 + the CI matrix) stands independently.

## 8. The consolidated Stage 5 acceptance record (the GO's eighth item)

**The batch chain (all on `stage5`, none merged to `main`):**
- **Batch 1** (canonical encoding + the full-fidelity spine round-trip) — CLOSED-PASS 2026-10-21 at `2addc19` (CI 39620965071; 199 tests + 1 ignored); the wire format specified in `docs/STAGE5-B1-WIRE-FORMAT.md`.
- **Batch 2** (the content-addressed revision store + the local canonical medium) — CLOSED-PASS 2026-10-21 at `e36b122` (CI 39649874651; 281 + 1; the Windows durability selection gate delivered open); `docs/STAGE5-B2-STORAGE-RECORD.md`.
- **Batch 3** (the commit pipeline + the durable T1 journal + the transactional encounter machinery) — CLOSED-PASS 2026-10-22 at `b6e9ca5` (CI 39813087578; 440 + 1; the effect commitment, the tier-4 semantic gate, `consequence-batch/2`); `docs/STAGE5-B3-PIPELINE-RECORD.md`.
- **Batch 4** (the application integration — the six-phase SessionCrossing over the real simulation) — CLOSED-PASS 2026-10-23 at `7bc1a11` (CI 40007940234; 476 + 1; the three review-corrective passes); `docs/STAGE5-B4-INTEGRATION-RECORD.md`.
- **Batch 5** (this delivery — the lifecycle, the verification matrix, the performance measurement, the storage analysis, the compaction decision, the rig instructions, this record) — the commits `455c817` (the lifecycle + the schedule-fidelity corrective) + `271aa7b` (the benchmark harness) + the record commit.

**The proven recovery guarantees** (the regression-mapped matrix in §2): no lost, duplicated, or falsely confirmed consequential progress within the documented failure model; the canonical confirmation remains authoritative (pending T1 never labeled confirmed); the snapshot/journal/chain recovery coherent under crashes, repeated recovery, and legitimate journal retirement; the EventId and ConsequenceId frontiers monotonic through save, restart, and regional transitions; every recognized corruption and irreconcilable divergence fails closed.

**The durability tiers:** Unix carries the documented full std fsync sequence (`FullPosixSequence`); **Windows carries `SyncTier::ProcessCrashOnly`** — the process-crash tier only, never power-loss durability; the production provider selection (SQLite the identified fallback) remains the owner's outstanding decision.

**The remaining risks and outstanding decisions:** (1) the Windows production storage-provider decision (the owner's — the rig benchmark in §7 completes the evidence); (2) the compaction/retention follow-up (§6 — separately reviewed, the measured evidence its input); (3) the physical-rig evidence pending (§7); (4) the known limitations below. **The production-readiness boundaries:** no production networking (Architecture E remains research), no asynchronous persistence (no measured evidence of a responsiveness violation — the synchronous boundary operations measured at 8–9 ms fit the 60 FPS discipline), no gameplay features beyond the demonstrations.

## 9. Known limitations and deferred obligations

1. **Windows `SyncTier::ProcessCrashOnly`** — unchanged throughout Stage 5: the process-crash tier only on Windows; the owner's provider decision outstanding.
2. **The single-writer crossing** — the journal's sidecar lock is session-scoped; the cross-process/multi-writer crossing composition remains deferred.
3. **No compaction or retention** — the journal, the snapshot chain, and the canonical history grow unboundedly (§5's measured rates); the follow-up protocol is separately reviewed (§6).
4. **The linear cold-open** — the session-start cost grows with the chain length (~10.5 ms/revision on the pod); the trusted cursor keeps every operation after the open O(1).
5. **The presence-re-derivable divergence** — the empty-scratch deactivations are not journaled by design; a crash between one and the next snapshot leaves that LOD transition to the director's presence-driven re-derivation (the lifecycle regression asserts the resolution).
6. **The unsupported plan-less confirmed form** — the explicit fail-closed rejection (never a fabricated lifecycle); a future batch could define a legitimate plan-less lifecycle if the lead wants one.
7. **The pod benchmark's environment** — overlayfs, not a native filesystem; the rig measurements are the authoritative physical evidence (§7).

---

**Delivery state:** the commits `455c817` + `271aa7b` + this record on `stage5`; `main` and `macos-port` untouched; the CI evidence at the delivery head recorded in `docs/DECISIONS.md`. **Stopping for independent Batch 5 PASS/HOLD review — the Stage 5 acceptance gate.**
