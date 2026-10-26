# PP-B4 ORCHESTRATION — VALIDATION RECORD

**Status: VALIDATION DELIVERED — PENDING INDEPENDENT FINAL ACCEPTANCE.**
This is an evidence and acceptance-preparation record (the owner's final
validation checkpoint GO). PP-B4 is NOT marked CLOSED-PASS here — that
ratification belongs to the independent final acceptance review.

**The first physical Windows acceptance run (2026-10-26, at `fd9ca7f`)
FAILED at the racing-writer backup boundary** — not on the invariant
(the backup refused BEFORE any capture; the manifest is written last)
but on the error CLASS: the open path's seed-head-row write under the
test's continuous canonical writer surfaced the generic
`MediumError::Sqlite` where every other write surface maps the busy
timeout's expiry to the typed contention. The owner-authorized
corrective `2e20414` (§5, dispatch 4) types the open-path busy expiry
as `MediumError::Contention` at exactly the four schema/seed sites and
adds the deterministic held-lock regression; it is CI-verified green
on all three OSes — **the physical Windows re-run against `2e20414`
remains PENDING (§6), the acceptance authority.**

Every status below uses the explicit vocabulary the GO mandated:
**VERIFIED** (evidence observed firsthand in this delivery),
**REPORTED** (a measurement or claim with its provenance stated),
**PENDING** (authorized but not yet executed), **NOT TESTED** (no
evidence exists), **FAILED** (evidence contradicts the claim — the
first physical Windows run above; otherwise none in this record).

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
  HEAD at the final-validation GO**) → the validation package
  (`63d0cfd` + the `b696779` test corrective) → the documentation-only
  record correctives (`2aede3b`, `cfcf271`, `fd9ca7f`) → `2e20414`
  (the Windows contention corrective — the owner-directed four-site
  class fix after the first physical acceptance run; CI-verified ×3)
  → this documentation-only record amend (the scope held throughout:
  tests, test-only fault controls, benchmark instrumentation,
  documentation, and the authorized contention correction ONLY).
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

**THE VERIFICATION DISPATCHES on this validation HEAD:**
- Dispatch 2 — run **40375410378** on the first validation commit
  `63d0cfd` (the health-flip and read-placement fixes in): macos-15
  ARM64 green, **windows-latest green** (the pre-submission head-read
  fix holding), ubuntu-latest **FAILED** — the O test again, a
  DIFFERENT interleaving: the maintenance gate failed
  `Err(Gate(worker/unavailable))` instead of the expected
  `Err(Compound)`. The second investigation: the `PanicAfter` check
  runs in the worker loop AFTER the reply is sent — arming the fault
  immediately after the prelude's wait can land BEFORE that check,
  firing it on the PRELUDE'S OWN append and killing the maintenance
  BARRIER (the next job) instead of the compound. The engine is
  correct in both interleavings (an honest failure either way — the
  gate refuses worker-class, nothing falsely succeeds); the fixture
  fix: one worker round-trip (the capture's head observation)
  between the prelude and the arming proves the loop advanced past
  the check — the armed Append then applies only to the compound's
  SK append, deterministically. The three pending-suite
  `PanicAfter` sites audited SAFE (their armed kinds never match the
  just-completed job). The corrective commit `b696779` (test-only,
  +11 lines, one file).
- Dispatch 3 — run **40376559970** on the corrective HEAD
  `b696779`: **ubuntu-latest green, windows-latest green, macos-15
  ARM64 green** — the full matrix (the pinned toolchain, fmt --check,
  clippy -D warnings, the workspace build, the workspace tests with
  both providers and the real-child pairs, the double fingerprint
  self-test with the Stage-0 pins) verified firsthand at the job
  level. **VERIFIED.** The then-delivered HEAD (`2aede3b` and the
  later record correctives) was `b696779`'s documentation-only
  descendant — no separate CI run existed or was required for
  documentation-only changes (no executable source or workflow
  behavior changes).
- Dispatch 4 — run **40402249372** on the Windows contention
  corrective HEAD `2e20414` (the owner-directed fix after the first
  physical Windows acceptance run failed at the racing-writer backup
  boundary with the generic `MediumError::Sqlite` — §6's preamble):
  **ubuntu-latest green, windows-latest green, macos-15 ARM64
  green**, the run's headSha verified equal to
  `2e20414fe1d33870081f32e8676cca126434f5e7`. Verified firsthand at
  the job level (the actual test and fingerprint results, not just
  the conclusions): every suite `test result: ok` with **0 failed**
  on all three OSes — engine-persist's lib suite **268 passed** (the
  net-new held-lock open-contention regression included, green on
  every OS), the PP-B4 suites 35/24/13/17, the remaining workspace
  suites at their accepted counts — and the double fingerprint
  self-test printed the pinned `engine=6504123a47cab66f` /
  `state=937bd3b6f56f07e0` on every OS (the pin checks would have
  failed the job otherwise). **VERIFIED.** `2e20414` supersedes
  `b696779` as the CI-verified source/test commit the physical
  Windows acceptance runs against (§6); the delivered HEAD is
  `2e20414`'s documentation-only descendant (this record's amend —
  no executable source after the corrective).

## 6. The physical Windows acceptance procedure (PENDING — the owner's re-run against the corrective)

Copy-pastable on the physical Windows machine (PowerShell, from the
repository's root; safe for an existing local checkout). TWO pinned
revisions, both stated in the transcript: the **CI-verified
source/test commit — where the three-OS run 40402249372 executed
green — is `2e20414fe1d33870081f32e8676cca126434f5e7`** (the Windows
contention corrective: the open-path schema/seed writes' busy expiry
typed as `MediumError::Contention`, the deterministic held-lock
regression added — the fix for this procedure's own first-run
failure, below), and the **reviewed pre-corrective
documentation-delivery HEAD is
`cfcf2713b3b4bab87a30d10afaec5e9fbcc7ee6a`**. The branch tip may
carry this record's own later documentation-only correctives (a
commit cannot contain its own SHA; each corrective's exact SHA is
reported at delivery and visible on the branch). The procedure below
verifies, failing BEFORE compilation on any mismatch: HEAD == origin
exactly; the CI-verified corrective `2e20414` an ancestor with
everything after it DOCUMENTATION-ONLY (only this record file — the
executable content IDENTICAL to the CI-verified revision); and the
delta from the reviewed pre-corrective HEAD `cfcf271` EXACTLY the
authorized corrective's two engine files plus this record (never any
other executable change between the reviewed state and the
corrective). All evidence artifacts are written OUTSIDE the tracked
repository.

**The first run's failure (why this procedure pins the corrective):**
executed 2026-10-26 at `fd9ca7f`, the `pp_b1_backup` racing-writer
test (`concurrent_canonical_writer_never_yields_a_coherent_backup`)
failed with `BackupError::Medium(MediumError::Sqlite { op: "seed
head row", detail: "database is locked" })` — the backup's first
database touch (`SqliteMedium::open`, backup.rs:395 — a full
WRITABLE open, the TRUNCATE checkpoint needing write access)
executes the schema batch and the unfounded head-row seed, both
WRITE statements on the same SQLite write lock as the append's
`BEGIN IMMEDIATE`; under the test's continuous canonical writer the
physical machine held the lock past the 5-second busy timeout. The
INVARIANT held (the backup refused BEFORE any capture — the manifest
is written last; no falsely coherent backup); the error CLASS was
wrong. The corrective `2e20414` fixes exactly that class — **the
re-run must show the previously observed error no longer surfacing
as the generic `MediumError::Sqlite`** (the typed `Contention`
refusal or the clean backup paths only); the focused regression and
the racing-writer test in step 5a below are the direct checks.

```powershell
# 0. THE EVIDENCE DIRECTORY — outside the tracked repository; the
#    transcript starts BEFORE the first Git command and Stop-Transcript
#    runs even on a failure (try/finally).
$ErrorActionPreference = "Stop"
$evidence = Join-Path $env:USERPROFILE "pp-b4-acceptance-evidence"
New-Item -ItemType Directory -Force -Path $evidence | Out-Null
function Assert-Step([string]$step) {
    if ($LASTEXITCODE -ne 0) { throw "$step failed (exit $LASTEXITCODE)" }
}
try {
    Start-Transcript -Path (Join-Path $evidence "pp-b4-windows-acceptance.txt")

    # 1. THE EXACT CHECKOUT VERIFICATION — every native command's exit
    #    code is checked IMMEDIATELY, before its result is used (a
    #    failed diff is NEVER interpreted as an empty diff). The
    #    revision diffs use the two-argument form with the explicit
    #    path separator — NEVER the A..B range token, which the
    #    physical Windows PowerShell tokenizer splits into separate
    #    arguments (the observed script regression).
    $reviewed = "cfcf2713b3b4bab87a30d10afaec5e9fbcc7ee6a"  # the reviewed pre-corrective documentation-delivery HEAD
    $verified = "2e20414fe1d33870081f32e8676cca126434f5e7"  # the CI-verified source/test commit (the contention corrective; run 40402249372)
    Write-Output "reviewed documentation-delivery HEAD: $reviewed"
    Write-Output "CI-verified source/test commit:      $verified"

    git fetch origin;                        Assert-Step "git fetch"
    git switch production-persistence-b4;    Assert-Step "git switch"
    $dirty = git status --porcelain
    if ($LASTEXITCODE -ne 0) { throw "git status failed (exit $LASTEXITCODE)" }
    if ($dirty) { throw "the worktree is not clean:`n$dirty" }
    # Fast-forward ONLY — any non-fast-forward divergence refuses here.
    git merge --ff-only origin/production-persistence-b4
    Assert-Step "the fast-forward"
    $head   = git rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw "git rev-parse HEAD failed (exit $LASTEXITCODE)" }
    $remote = git rev-parse origin/production-persistence-b4
    if ($LASTEXITCODE -ne 0) { throw "git rev-parse origin failed (exit $LASTEXITCODE)" }
    if ($head -ne $remote) { throw "HEAD ($head) != origin ($remote)" }

    # The reviewed pre-corrective HEAD: the delta from it is EXACTLY
    # the authorized corrective (the two engine files) plus this
    # record — never any other executable change between the
    # reviewed state and the tip.
    git merge-base --is-ancestor $reviewed HEAD
    Assert-Step "the reviewed pre-corrective HEAD is an ancestor"
    $since = @(git diff --name-only $reviewed HEAD --)
    if ($LASTEXITCODE -ne 0) { throw "git diff (reviewed..HEAD) failed (exit $LASTEXITCODE) — never interpreted as an empty diff" }
    $authorized = @(
        "engine/crates/engine-persist/src/sqlite.rs",
        "engine/crates/engine-persist/src/sqlite_tests.rs",
        "docs/PP-B4-ORCHESTRATION-VALIDATION-RECORD.md"
    )
    if ($since.Count -eq 0 -or (Compare-Object $since $authorized)) {
        throw "the delta from the reviewed pre-corrective HEAD is not the authorized corrective plus the record: $($since -join ', ')"
    }
    Write-Output "the delta from the reviewed pre-corrective HEAD is exactly the authorized contention corrective (sqlite.rs, sqlite_tests.rs) plus this record"

    # The CI-verified source commit must be present, with everything
    # after it documentation-only (only this record file).
    git merge-base --is-ancestor $verified HEAD
    Assert-Step "the CI-verified commit is an ancestor"
    $delta = @(git diff --name-only $verified HEAD --)
    if ($LASTEXITCODE -ne 0) { throw "git diff (verified..HEAD) failed (exit $LASTEXITCODE) — never interpreted as an empty diff" }
    if ($delta.Count -eq 0) {
        Write-Output "HEAD is the CI-verified commit itself: $head"
    } elseif (-not (Compare-Object $delta @("docs/PP-B4-ORCHESTRATION-VALIDATION-RECORD.md"))) {
        Write-Output "HEAD is a documentation-only descendant of the CI-verified commit: $head"
    } else {
        throw "the delta from the CI-verified commit is not documentation-only: $($delta -join ', ')"
    }
    Write-Output "checkout verified: HEAD == origin == $head; the reviewed HEAD $reviewed and the CI source $verified present; the executable content identical to the CI-verified revision"

    # 2. The toolchain.
    cd engine
    rustc --version;   Assert-Step "the toolchain"

    # 3. The static gates.
    cargo fmt --all -- --check;   Assert-Step "cargo fmt"
    cargo build --workspace;      Assert-Step "cargo build"
    cargo clippy --workspace --all-targets -- -D warnings
    Assert-Step "cargo clippy"

    # 4. THE PHYSICAL RELEASE-MODE TESTS — BOTH runs must complete
    #    successfully; the transcript captures their actual outputs and
    #    counts (the delivered suite total: 708 passed / 0 failed —
    #    the corrective's net-new held-lock regression included).
    cargo test --release --workspace;   Assert-Step "the first release run"
    cargo test --release --workspace;   Assert-Step "the second release run"

    # 5. The focused PP-B4 suites (release mode, one filter at a time).
    cargo test --release -p engine-slice --test pp_b4_maintenance;  Assert-Step "maintenance"
    cargo test --release -p engine-slice --test pp_b4_orchestrated; Assert-Step "orchestrated"
    cargo test --release -p engine-slice --test pp_b4_worker;       Assert-Step "worker"
    cargo test --release -p engine-slice --test pp_b4_pending;      Assert-Step "pending"
    cargo test --release -p engine-slice --test pp_b4_seam;         Assert-Step "seam"

    # 5a. The first physical run's exact failure boundary — the focused
    #     held-lock contention regression and the racing-writer backup
    #     test (the previously observed error must NOT surface as the
    #     generic MediumError::Sqlite again).
    cargo test --release -p engine-persist pp_b4_an_open_under_a_held_write_lock_is_the_typed_contention
    Assert-Step "the held-lock open-contention regression"
    cargo test --release -p engine-slice --test pp_b1_backup concurrent_canonical_writer_never_yields_a_coherent_backup
    Assert-Step "the racing-writer backup test"

    # 6. The provider checks (the SQLite suites also run inside the
    #    workspace tests above; the focused release runs).
    cargo test --release -p engine-slice --test pp_b1_sqlite_lifecycle; Assert-Step "sqlite lifecycle"
    cargo test --release -p engine-slice --test pp_b1_backup;           Assert-Step "backup"
    cargo test --release -p engine-slice --test pp_b1_migration;        Assert-Step "migration"

    # 7. The double frozen-fingerprint verification (release mode).
    $a = cargo run --release -q -p engine-slice -- --fingerprint-selftest
    Assert-Step "the first fingerprint run"
    $b = cargo run --release -q -p engine-slice -- --fingerprint-selftest
    Assert-Step "the second fingerprint run"
    if (Compare-Object $a $b) { throw "the fingerprint runs diverged" }
    if (-not ($a -match "engine=6504123a47cab66f")) { throw "the engine pin changed" }
    if (-not ($a -match "state=937bd3b6f56f07e0"))  { throw "the state pin changed" }
    Write-Output "fingerprints verified: $($a -join ' / ')"

    # 8. The benchmark (release mode, captured to the EVIDENCE
    #    directory — outside the tracked repository).
    cargo run --release -q -p engine-slice -- --benchmark-persistence |
      Out-File -Encoding utf8 (Join-Path $evidence "pp-b4-windows-benchmark.txt")
    Assert-Step "the benchmark"

    # 9. The final commit SHA into the transcript.
    cd ..
    $final = git rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw "the final git rev-parse failed (exit $LASTEXITCODE)" }
    Write-Output "final commit: $final (reviewed $reviewed; CI source $verified)"
} finally {
    Stop-Transcript
}
```

The evidence package for the independent final acceptance review
lives OUTSIDE the tracked repository, under
`%USERPROFILE%\pp-b4-acceptance-evidence`:
`pp-b4-windows-acceptance.txt` (the transcript — the verified
checkout with BOTH exact SHAs stated, the actual toolchain, every
command's output and counts, the double fingerprints, the final
commit SHA) and `pp-b4-windows-benchmark.txt`, compared against this
record's §4 (the pod numbers — never conflated with the rig's) and
the PP-B3 record's owner measurements. **A development-pod result or
a GitHub-hosted Windows runner is NEVER the owner's physical
hardware acceptance** — only this run on the physical machine is.

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
  authority; no pod number substitutes for it (D-PP-7). The FIRST
  run (2026-10-26, at `fd9ca7f`) FAILED on the error class at the
  racing-writer backup boundary (§6's preamble) — the invariant
  held, the class was wrong; the owner-authorized corrective
  `2e20414` (CI-verified ×3, dispatch 4) fixes the class, and the
  re-run against it is the remaining acceptance.
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

1. The three-OS CI verification (run 40402249372) green at the job
   level — executed at the CI-VERIFIED SOURCE COMMIT
   `2e20414fe1d33870081f32e8676cca126434f5e7` (§5, dispatch 4; the
   earlier dispatches' failures investigated and resolved — the
   dispatch-1/2 races, then run 40376559970 green at `b696779` —
   and the first physical Windows run's error-class failure
   corrected by this commit; the verdicts verified firsthand). The
   delivered HEAD is that commit's DOCUMENTATION-ONLY descendant
   (the record commits): no separate CI run exists or is required
   for documentation-only changes — the Windows checkout procedure
   (§6) proves the documentation-only descent before anything
   compiles.
2. The owner's physical Windows RE-run of §6 against the corrective
   `2e20414` succeeds: the gates, the full workspace tests (both
   providers, the real-child pairs — 708 passed / 0 failed), the
   pinned fingerprints, the release benchmark captured, and the
   first run's failure boundary green — the held-lock regression
   passing and the racing-writer backup test passing with the
   previously observed error no longer surfacing as the generic
   `MediumError::Sqlite`.
3. The benchmark comparison against the PP-B3 owner measurements
   shows no unexplained regression in the operation classes
   (tick/confirmation/compounds) — pod-vs-rig differences attributed,
   never generalized.
4. No FAILED statuses anywhere in this record; the PENDING items (§6)
   resolved by the owner's transcript.
5. The independent final acceptance review concludes PASS.

## 10. The delivery state

The chain: the starting accepted implementation
`9a92ef8c1139432efd01f18e371afad5e4da90e5` →
`63d0cfd9db147447021e4d4b8b11408174c4413b` (the validation package:
7 files, +1207/−20) →
`b6967793039e9556bf14477b8cf343fcbdd0227b` (the arming-barrier
corrective: 1 file, +11 — the CI-verified source commit of run
40376559970) → `2aede3bba6eafc9c45a2aa1d9286af6acbe57a20` (the
first docs-only record amend) →
`cfcf2713b3b4bab87a30d10afaec5e9fbcc7ee6a` (the V-F1..F4 docs
corrective) → `fd9ca7f78675f4a087e43e6c9ca82997b173dfa9` (the
V-R1/R2 docs corrective — the HEAD the first physical Windows run
checked out) → `2e20414fe1d33870081f32e8676cca126434f5e7` (the
Windows contention corrective: 2 files, +78/−4 — **the CI-verified
source/test commit**, run 40402249372 green ×3) → this
documentation-only record amend (its exact SHA is reported in the
delivery report and visible on the branch; the §6 checkout
procedure verifies the tail: everything after `2e20414`
documentation-only, the delta from `cfcf271` exactly the authorized
corrective plus this record). Every commit pushed; HEAD == origin
verified; the worktree clean; the per-commit changed-file inventory
in the delivery report; the protected branches and the PR discipline
unchanged (no PR #4).

**STOP after delivering the validation package.** No PR #4, no
merges, no CLOSED-PASS ratification, no Stage 6 — the final
acceptance and every subsequent action require the separate
independent review and the explicit owner authorization.
