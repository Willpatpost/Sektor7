# PP-B4 ORCHESTRATION — VALIDATION RECORD

**Status: VALIDATION DELIVERED — PENDING INDEPENDENT FINAL ACCEPTANCE.**
This is an evidence and acceptance-preparation record (the owner's final
validation checkpoint GO). PP-B4 is NOT marked CLOSED-PASS here — that
ratification belongs to the independent final acceptance review.

Every status below uses the explicit vocabulary the GO mandated:
**VERIFIED** (evidence observed firsthand in this delivery),
**REPORTED** (a measurement or claim with its provenance stated),
**PENDING** (authorized but not yet executed), **NOT TESTED** (no
evidence exists), **FAILED** (evidence contradicts the claim — none in
this record).

---

## 1. Frozen baselines and implementation history

- The frozen PP-B3 closure baseline: `92b057aaeff3abecfff36bc87cbce1c2a8abb42a`
  (`production-persistence-b3`, CLOSED-PASS, owner-approved 2026-10-25) —
  **VERIFIED** an ancestor of the PP-B4 HEAD.
- The PP-B4 chain (all pushed, all retained in history):
  `b398c13` (checkpoint 1 — the medium generalization) → `8744971`
  (checkpoint 2 — the worker core + the pending-attempt lifecycle) →
  `d3ba9c9` (the C2 corrective) → `b7e3800` (checkpoint 3 — the
  orchestrated session) → `6eb07dd` (the C3 corrective) → `f4781a8`
  (checkpoint 4 — protected maintenance + the graceful close) →
  `c0b6553` (the C4 corrective) → `573327a` (the C4-R1/R2 corrective) →
  `9a92ef8` (the C4-R3/R4 corrective — **the accepted implementation
  HEAD at the final-validation GO**) → the validation commit at the
  tip (tests, test-only fault controls, benchmark instrumentation, and
  this record ONLY — §8's scope).
- The independent review history: checkpoint 1 PASS; checkpoint 2
  HOLD→corrected→CLOSED-PASS; checkpoint 3 HOLD→corrected→CLOSED-PASS;
  checkpoint 4 GO→HOLD (C4-F1/F2/F3)→corrected→HOLD (C4-R1/R2)→
  corrected→HOLD (C4-R3/R4)→corrected→**PASS (CLOSED-PASS at
  `9a92ef8`)** — the full per-finding narratives live in the four
  review directives and the corrective commit messages.

## 2. The independent failure-matrix audit

For each family: the invariant, the source, the existing tests, the
fault mechanism, the provider coverage, and the evidence class.
**Evidence-class vocabulary:** *in-process* = the fault is injected
inside one process (simulated error returns, injected panics on real
threads); *real process-death* = a spawned child is killed at a
deterministic seam and the parent verifies the recovery (exit-code
assertions); *physical-device durability* = power-loss/hardware
evidence — **NONE EXISTS for any PP batch** (the standing limitation,
the owner's rig).

1. **Worker health, poisoning, panic, reply loss** — invariant: the
   health cell flips DEAD on every exit path (the drop guard covers
   unwinds); poisoned workers refuse NEW operations at the proxy
   handle and queued jobs at the execution boundary (the DEFINITE
   class); a lost reply on an append is NEVER definite
   (`Unknown{WorkerReplyLost}`, the candidate preserved). Source:
   `engine-slice/src/worker.rs` (the Health cell, DeathGuard, the
   loop's boundaries, WorkerMedium). Tests: pp_b4_worker 17 (the
   admission/boundary refusals, serialization, saturation),
   pp_b4_orchestrated (the poison-during-executing-append, the
   death-without-polling), pp_b4_maintenance L (the poisoned/panicked
   failed closes). Faults: `PanicNext`, `PanicAfter(JobKind)`,
   `poison()`, `LoseAppendReply`. Providers: file+SQLite. Evidence:
   in-process (real thread panics) + real process-death for the
   append/checkpoint seams (exits 13/8, below).
2. **Queue admission, saturation, fairness, cancellations** —
   invariant: the bounded queue bounds ADMISSION (never accounting);
   `try_submit` returns the typed Busy/Unavailable with nothing
   admitted; the fairness gate refuses optional admissions while a
   required operation waits; a cancellation is effective only before
   the job starts (a started job runs to its completion boundary).
   Source: `worker.rs` (try_submit, the sync_channel), `orchestration.rs`
   (submit_publication, `required_waiting`, cancel_publication).
   Tests: pp_b4_orchestrated (saturation, fairness,
   cancellation-before/after-start), benchmark §9d. Faults:
   `ParkNext`, `ParkExecutingAppend`. Providers: file+SQLite.
   Evidence: in-process.
3. **Snapshot capture, stale-parent rejection, generation
   invalidation** — invariant: a capture is immutable (never
   rebased); a HEAD advance surfaces the stale CAS rejection; the
   journal flip count invalidates pre-rotation captures EVEN WITH THE
   HEAD UNCHANGED. Source: `orchestration.rs` (capture_snapshot,
   submit_publication, the generation check), `crossing.rs`
   (journal_flips). Tests: pp_b4_orchestrated + pp_b4_maintenance F
   (the HEAD-unchanged generation rejection), K (the stale capture).
   Faults: the R5 pointer-durability fault. Providers: file+SQLite.
   Evidence: in-process + real process-death (the rotation child).
4. **Required canonical confirmation and exception-safe pending
   attempts** — invariant: the four-state candidate rule
   (write-at-uncertainty-or-acceptance; clear-only-on-verified-
   Confirmed; a definite pre-commit Err writes nothing — the
   direct-retry contract); the exact foreign provenance receipt.
   Source: `engine-persist/src/pipeline.rs` (append_for, submit_keys,
   confirm_retained, reconcile_pending, retry_pending). Tests:
   pp_b4_pending 13 (the two-view foreign confirmation, the C2-F1
   slot-clearing). Faults: `LoseAppendReply`, CAS-rejection
   constructions. Providers: file+SQLite. Evidence: in-process.
5. **Maintenance barrier, completion harvesting, unknown
   reconciliation** — invariant: the FIFO barrier's reply proves every
   previously admitted job executed and replied; every completion is
   harvested with its typed disposition; an Unknown reconciles to
   Installed/NotInstalled/Unverifiable — the Unverifiable REFUSES the
   gate, never a guessed classification. Source: `orchestration.rs`
   (enter_maintenance, enter_close_gate, harvest_pending,
   reconcile_uncertain), `worker.rs` (submit_required, Barrier).
   Tests: pp_b4_maintenance A/B/C + the §3A regressions (below).
   Faults: `LoseAppendReply`, `UnverifiableNextReconcile`. Providers:
   file+SQLite. Evidence: in-process.
6. **SK/MK checkpoint publication and partial failures** — invariant:
   MK.parent == SK exactly; the crash-after-SK-before-MK leaves an
   ORDINARY snapshot (never a fabricated checkpoint); the empty range
   refuses; a checkpoint after a reclamation continues (the audit is a
   retained anchor). Source: `crossing.rs` (publish_checkpoint,
   walk_checkpoint_range), `engine-persist/src/checkpoint.rs`. Tests:
   pp_b3_checkpoint 4, pp_b4_maintenance D, the R2 nine-step
   lifecycle. Faults: the ARM_AFTER_SK_EXIT seam. Providers:
   file+SQLite. Evidence: in-process + **real process-death** (exit 8,
   `a_real_child_checkpoint_death_after_sk_recovers`).
7. **Journal rotation, pointer durability, repair** — invariant: the
   generations manifest is maintained BEFORE the flip (every manifest
   failure is pre-flip — the authority unchanged); the pointer rename
   is the last fallible step; an unresolved pointer durability
   FAILS CLOSED (no new authoritative T1 writes) until the verified
   repair or a fresh recovery; a failed post-flip rebuild poisons the
   crossing. Source: `engine-persist/src/rotation.rs`, `crossing.rs`
   (rotate_journal, reestablish_pointer_durability, JournalAuthority).
   Tests: pp_b3_rotation 10 (the R4/R5 fault-injection child pairs),
   pp_b4_maintenance G (the real-child R5 rotation + repair + the
   permanently-stale pre-flip capture). Faults: GENERATIONS_IO_FAULT,
   POINTER_DIR_SYNC_FAULT, ROTATION_SWAP_FAULT. Providers: file+SQLite.
   Evidence: in-process (deterministic fault injection in isolated
   child processes) — the pointer dir-sync natural failure is
   Unix-only (the ratified Windows no-op); **no physical power-loss
   test performed or claimed**.
8. **Canceled versus confirmed-and-subsumed retirement** — invariant:
   the classifier splits durably-canceled from confirmed-and-subsumed
   (the retirement evidence is driver-populated, the mechanism never
   guesses). Source: `crossing.rs` (the survival classifier),
   `rotation.rs` (retired_canceled/retired_subsumed). Tests:
   pp_b4_maintenance H (canceled [foreign] / subsumed [t1] / retained
   [t2]). Providers: file+SQLite. Evidence: in-process.
9. **Reclamation and restart** — invariant: phase A's audit is
   durable BEFORE any deletion; the resume recognizes the installed
   audit (no duplicate, no re-read of deleted bytes); the whole-
   manifest completion check (the records, objects, quarantined
   copies, the audited journals); a hand deletion without an audit is
   hard corruption. Source: `engine-persist/src/reclaim.rs`,
   `crossing.rs` (reclaim_checkpoint), `local.rs`/`sqlite.rs`
   (reclaim/reclaim_pending). Tests: pp_b3_reclaim 22 (the real-death
   resume pairs), pp_b4_maintenance I. Faults: ARM_RECLAIM_AFTER_AUDIT
   (exit 12), ARM_RECLAIM_MID_UNIT/LAST_UNIT (exits 10/11). Providers:
   file+SQLite (the sqlite one-transaction atomicity). Evidence:
   in-process + **real process-death** (exits 9-12).
10. **Final required publication, readback, lifecycle checkpoint,
    join** — invariant: the final snapshot is a REQUIRED admission
    from the newest state, its installation verified by readback; the
    bounded recapture never rebases; the Unknown-reconcile resolves
    or fails the close; the lifecycle checkpoint only after verified
    durability; the teardown drops the crossing before the join; the
    CloseReport's overall-success contract (failure == None only on
    the complete success). Source: `orchestration.rs`
    (publish_final_snapshot, verify_final_publication, close_inner).
    Tests: pp_b4_maintenance J/K/L/M/N/O/P + the C4 corrective suite
    (the idempotent close, the lost-reply reconciliation, the
    recapture, the contract test). Faults: `LoseAppendReply`,
    `DropAppendWithoutExecution`, `PanicBeforeCheckpoint`. Providers:
    file+SQLite. Evidence: in-process + **real process-death** (exit
    13, `a_real_child_close_death_at_the_final_append`).
11. **Identical-state idempotence and recovery-anchor supersession** —
    invariant: the close's idempotence walk satisfies the final-save
    obligation ONLY from the latest recoverable session snapshot (the
    accepted family recognizer); an intervening different snapshot —
    of ANY recognizable version — supersedes (the typed conflict);
    non-snapshot revisions (batches, MKs, audits) never supersede;
    the bounded window's residual falls to the provider's
    duplicate-key refusal — never a false success. Source:
    `orchestration.rs` (find_representing_snapshot,
    verify_representing_snapshot), `crossing.rs` (is_session_object,
    latest_snapshot_at). Tests: the C4-F1/R2/R4 suites + §3B below.
    Providers: file. Evidence: in-process.
12. **Never-submitted T1 consequences and unsettled-close reporting**
    — invariant: `required_settled == true` means NO actionable
    consequential confirmation remains outstanding (never merely that
    no candidate was retained); the two-phase settlement (the retained
    attempt, then the pipeline's actionable-keys collection) under the
    Closing gate; the honest unsettled close retains the T1 evidence
    for the crash-recovery re-drive. Source: `orchestration.rs`
    (settle_required_attempt), `pipeline.rs` (collect_pending_keys,
    commit_canonical). Tests: the C4-R3 suite (A/B/C/D1/D2) + the
    strengthened closing-phase test. Faults: `LoseAppendReply`,
    `DropNextAppends{2}`. Providers: file+SQLite. Evidence: in-process.

**Physical-device durability evidence: NOT TESTED** (no PP batch has
ever claimed it — the Windows rig is the owner's; see §6).

## 3. The evidence-limitation resolutions

### A. Unverifiable reconciliation — VERIFIED (was NOT TESTED)

The gap: matrix C's `Unverifiable` arm was not deterministically
constructible through the orchestrated path (a provider-internal
fault, not exposed through the worker). The resolution (the GO's
preferred shape — an isolated, per-worker, test-only passthrough):
**`WorkerFault::UnverifiableNextReconcile`** — the next
ReconcileUnknown job is not executed; the worker synthesizes the typed
`Reconciliation::Unverifiable` outcome as its reply (one-shot, the
worker healthy). No production API widened; no provider format
touched; no architectural change required. The regressions
(pp_b4_maintenance):
- `the_maintenance_gate_refuses_an_unverifiable_reconciliation` — the
  gate refuses with the typed detail; the lease releases; the worker
  stays healthy; the uncertainty is RETAINED (the subsequent clean
  close reconciles the same completion to the VERIFIED installation —
  a false NotInstalled would surface as `installed: false`).
- `the_close_refuses_an_unverifiable_publication_reconciliation` —
  the explicit `UnverifiableReconciliation` close failure; no claimed
  save; the disposition still the honest Unknown; a fresh recovery
  coherent.
- `the_close_refuses_an_unverifiable_attempt_reconciliation` — the
  settlement's attempt arm refuses; the T1 obligation retained; the
  fresh recovery re-drives it exactly once.

### B. The bounded idempotence lookup — VERIFIED (the documented boundary)

`an_identical_snapshot_beyond_the_lookup_window_fails_honestly_and_recovers`:
the identical snapshot S separated from the close by 65 non-snapshot
canonical revisions. The walk exhausts its 64-revision window, falls
through to the publication path, and the provider's
duplicate-idempotency-key refusal surfaces there — the close reports
the HONEST `PublicationFailed`, never a false success, and a fresh
recovery is UNAFFECTED (the discovery walk is bounded by the chain
length): it restores the identical S. **The quantified realistic
conditions:** the window is exceeded only by ≥65 consecutive
non-snapshot canonical revisions (confirmation batches, MKs, audits)
with NO intervening snapshot publication — under the accepted
~32-publication checkpoint cadence, roughly two full maintenance
generations without a single snapshot publication; an operator-behavior
edge, not a gameplay-driven one. The residual is honest and
recoverable (re-close after a state change or a fresh publication).
No persistent index added; the idempotency contract unchanged.

### C. Test determinism — VERIFIED

The audit found three timing-dependent `sleep(200ms)` orderings (the
cancel-after-start and poison-in-flight tests in pp_b4_orchestrated;
the poison-boundary test in pp_b4_worker — all relying on a guessed
duration for "the worker holds the job"). Resolution: the ParkLatch
gained an ENTRY flag (`park_entered()`), and all three sites now wait
on the actual park-entry event (the §3C barrier — spin on the real
event, never a guessed duration); the worker-suite stall was converted
from `StallEach` to `ParkNext` with the same barrier. The remaining
sleeps are the child-runner's 50ms bounded polls (a deadline mechanism,
not a correctness race) and the `StallEach` fault used by the
serialization observations (a fault BY DESIGN, its duration never a
correctness input). The repeated-run evidence: pp_b4_maintenance ×5,
pp_b4_orchestrated ×3, pp_b4_worker ×3, pp_b4_pending ×3 — **zero
intermittent failures** (the suite counts 35/24/17/13 every run).

## 4. Performance and gameplay responsiveness — REPORTED (pod dev-profile; NOT the owner's rig)

Methodology: the real 1000-NPC Lod scenario at the 150-tick accrual
peak, fresh campaigns per sample family, warmup separated from
measurement, the sample counts printed per line, the distributions
min/median/p95/p99/max. One `--benchmark-persistence` run prints
sections 1-9 (the PP-B1/2/3 sections unchanged; §8 reproduces the
PP-B3 crossing-level numbers on the same run). **Honesty: the optional
publication path is asynchronous for the AUTHORITY THREAD (a bounded
enqueue; a typed Busy at the boundary); T1 durability and protected
maintenance retain their accepted SYNCHRONOUS boundaries — never a
claim that all persistence is nonblocking. The 16.667 ms frame budget
is a comparison reference, never an invented guaranteed maximum.**

The section-9 results (pod, dev profile, 2026-10-26):
- the authoritative tick (idle worker): median 0.055 / p95 0.107 /
  p99 0.124 / max 0.148 ms (n=300);
- the tick during an executing (parked) optional publication: median
  0.055 / p99 0.129 / max 0.137 ms (n=100) — **the responsiveness
  proof: a stalled canonical I/O worker does not block the authority
  thread** (the tick's T1 durability is journal-side);
- the snapshot capture (the serialization + the observed head + the
  frontier): median 28.770 ms (n=50) — the dominant share is the
  save_session serialization of the full state (the base section's
  full serialization: median 2.477 ms at 150 ticks; the capture runs
  on the post-450-tick state of the measurement session);
- the optional admission (the enqueue): median 0.017 ms; the
  saturation boundary 65 admitted / 5 Busy (the bounded queue);
- the T1 deactivation tick (the journal fsync inside the tick):
  median 17.330 / max 30.383 ms (n=10) — the synchronous T1 boundary;
- the required canonical confirmation (the worker-backed commit with
  the readback): median 36.135 / max 48.123 ms (n=10);
- the maintenance barrier/drain (the full gate, no compound): median
  0.031 ms (n=10) — the barrier cost on the drained queue;
- the graceful close (the full §6 sequence): 574 ms (n=1, file);
- the orchestrated maintenance compounds (two publications per range
  — §8's workload shape; the orchestrated fixture's inherent extra is
  the eligibility prelude's confirmed cycle): the checkpoint median
  167.4 ms, the rotation median 286.4 ms, the reclamation median
  170.3 ms (n=5) — versus the same run's crossing-level §8 numbers
  294.9/342.7/243.9 ms. The plausible mechanism (interpretation, not
  a claim): the worker's single long-lived provider connection with
  its trusted cursor versus the crossing's per-operation handle opens;
- the SQLite parity: the tick median 0.070 ms; the deactivation tick
  15.266 ms; the confirmation 15.147 ms; the graceful close 167.982
  ms (n=1 each).

The gameplay-frame interpretation: the authoritative tick (median
0.055 ms; p99 0.124 ms) is far inside every frame budget INCLUDING
while a publication executes on the worker; the T1 deactivation tick
(median 17.3 ms) sits at the 60 FPS budget's edge on this pod — the
accepted synchronous boundary (a durability event, not a per-frame
cost); the maintenance compounds and the close are explicit operator
operations, never gameplay-frame (the standing PP-B3 interpretation,
unchanged). **Development-pod evidence only — the owner's physical
Windows results remain the acceptance authority (§6).**

## 5. Three-OS verification — the CI evidence

Governance (inspected): `engine-ci.yml` triggers on push ONLY for
`[main, stage5, macos-port]` (paths `engine/**`), on pull_request
(paths `engine/**` — the PR vehicles #1/#2/#3), and on
**workflow_dispatch**. The b4 branch has NO pull-request vehicle (PR
#4 is not authorized) and its pushes match no push branch — so the
only governance-preserving path to run the three-OS matrix on the b4
code is the workflow dispatch (the GO's §5 "or run"): no branch
modified, no PR created.

**THE FIRST DISPATCH — run 40373697427, on the accepted HEAD
`9a92ef8`** (dispatched while the remote stood at the accepted
implementation, before this validation commit): ubuntu-latest
**FAILED** at the Tests step, windows-latest **FAILED** at the Tests
step, macos-15 ARM64 green. The investigation (the GO's mandate — any
CI failure investigated before final acceptance) identified two
TEST-LEVEL RACES on the slower shared runners, **no production
defect**:
- ubuntu, `a_worker_panic_during_a_compound_fails_explicitly`: the
  compound failed CORRECTLY (the typed Compound error, the lease
  released) but the test's immediate `!healthy()` assert raced the
  worker thread's unwind — the in-flight job's reply channel drops
  MID-UNWIND (the failure signal reaching the caller) BEFORE the
  DeathGuard's thread-exit health flip. The death is GUARANTEED by
  the panic; the assertion wrongly required its COMPLETION to be
  synchronous with the failure signal.
- windows, `completion_accounting_distinguishes_the_outcomes`: the
  oracle head read placed between the asynchronous submission and its
  wait raced the append's landing — the failing assert's two sides
  were the SAME hash (the read happened after the landing).

**The fixes (in this validation commit — the §3C determinism
mandate):** the bounded wait for the GUARANTEED death (a 10s deadline
spinning on the real event; the failure signal precedes the flip,
never the reverse) at ALL SEVEN same-pattern sites (the O test; the
orchestrated death-without-polling; the worker panic-detection; the
pending suite ×4), and the pre-submission head read (the oracle read
moved before the submission — the append's landing can no longer
precede the observation). The affected suites re-verified green
(35/24/17/13), the full gates re-run (the double release-mode
workspace selftest 707/0 ×2; the pinned fingerprints ×2).

**THE VERIFICATION DISPATCH on this validation HEAD** (the commit
this record travels with): the run id and the job-level verdicts are
stated in the delivery report, verified firsthand — a dispatch run is
NOT described as green until its jobs completed green.

## 6. The physical Windows acceptance procedure (PENDING — the owner's run)

Copy-pastable on the physical Windows machine (PowerShell; the
repository at your usual location):

```
# 0. The exact baseline
git fetch origin
git rev-parse origin/production-persistence-b4
#   MUST print the validation HEAD recorded in this record's §12.
git switch production-persistence-b4
git status --short          # MUST be empty

# 1. The toolchain (the pinned rust-toolchain.toml installs itself)
cd engine
rustc --version

# 2. The static gates
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace

# 3. The full test matrix (includes the SQLite provider suites and
#    the deterministic real-child process-death tests)
cargo test --workspace

# 4. The focused PP-B4 suites (one filter at a time)
cargo test -p engine-slice --test pp_b4_maintenance
cargo test -p engine-slice --test pp_b4_orchestrated
cargo test -p engine-slice --test pp_b4_worker
cargo test -p engine-slice --test pp_b4_pending
cargo test -p engine-slice --test pp_b4_seam

# 5. The fingerprints (double run, the pinned Stage-0 values)
$a = cargo run -q -p engine-slice -- --fingerprint-selftest
$b = cargo run -q -p engine-slice -- --fingerprint-selftest
#   $a -eq $b must hold; the output must contain
#   engine=6504123a47cab66f and state=937bd3b6f56f07e0

# 6. The benchmark (release mode, output captured for the review)
cargo run --release -q -p engine-slice -- --benchmark-persistence |
  Out-File -Encoding utf8 pp-b4-windows-benchmark.txt
Get-Content pp-b4-windows-benchmark.txt

# 7. The evidence package: this transcript plus the benchmark file,
#    compared against this record's §4 (the pod numbers) and the
#    PP-B3 record's owner measurements — for the independent final
#    acceptance review.
```

The accepted Windows-specific durability limitations preserved: the
journal pointer's directory sync is the ratified no-op (std exposes no
Windows directory flush) — no Windows dir-fsync claim; the medium's
Windows tier is process-crash durability, never presented as
power-loss durability; no physical power-loss test is performed or
claimed anywhere in PP-B4.

## 7. Provider parity — VERIFIED

Every PP-B4 suite runs over both providers where the operation
touches the medium: the worker/pending suites' worker/file and
worker/sqlite fixtures; the maintenance suite's SQLite WAL-truncation
close with the file parity; the C4-R3 settlement over both; the
benchmark's §9i SQLite pass. The `SessionStore` delegation and the
`LocalFileMedium` oracle are unchanged from the accepted PP-B1 seam.

## 8. Known limitations, uncertainties, open risks

- **Physical Windows evidence: PENDING** (§6) — the acceptance
  authority; no pod number substitutes for it (D-PP-7).
- **Physical power-loss durability: NOT TESTED** (every batch; the
  process-crash tier is the honest Windows claim).
- **The 64-revision idempotence window: VERIFIED honest** (§3B) —
  the residual surfaces as the provider's duplicate-key refusal;
  the realistic conditions quantified; no index, no contract change.
- **The R5 pointer-durability natural failure is Unix-only** (the
  ratified Windows no-op) — the deterministic injection exercises the
  machinery on all platforms; no Windows dir-fsync claim.
- **The maintenance compounds and the graceful close exceed frame
  budgets** — explicit operator operations, never gameplay-frame
  (the PP-B3 interpretation, unchanged).
- **The CI dispatch run is a one-time vehicle** — the b4 branch has
  no standing CI vehicle (PR #4 unauthorized); future b4 pushes will
  not trigger CI until one is authorized.

## 9. Proposed final acceptance criteria

1. The three-OS CI verification dispatch on the validation HEAD green
   at the job level (§5 — the first dispatch's failure investigated
   and resolved there; the verdicts verified firsthand).
2. The owner's physical Windows run of §6 succeeds: the gates, the
   full workspace tests (both providers, the real-child pairs), the
   pinned fingerprints, and the release benchmark captured.
3. The benchmark comparison against the PP-B3 owner measurements
   shows no unexplained regression in the operation classes
   (tick/confirmation/compounds) — pod-vs-rig differences attributed,
   never generalized.
4. No FAILED statuses anywhere in this record; the PENDING items (§6)
   resolved by the owner's transcript.
5. The independent final acceptance review concludes PASS.

## 10. The delivery state

Recorded at the commit: the exact starting SHA
(`9a92ef8c1139432efd01f18e371afad5e4da90e5`), the final validation
SHA, the changed-file inventory, the commands and results, and the
remote/worktree state — §12 of the delivery report (the commit
message carries the same).

**STOP after delivering the validation package.** No PR #4, no
merges, no CLOSED-PASS ratification, no Stage 6 — the final
acceptance and every subsequent action require the separate
independent review and the explicit owner authorization.
