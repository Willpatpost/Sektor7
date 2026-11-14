# F-10 — Fault-Seam Coverage Matrix

**Agent B (coder-a8), 2026-11-14** — the post-audit remediation directive's §9
assignment ("audit existing fault seams against the realistic failures they
are supposed to represent"), delivered on the branch
`s6-remediation-b-persistence-ci` from the mainline baseline `a4fe0ec`.

**Method.** Every seam below was read at this tree (not recalled); every
"classification" cites the seam or test that carries it. The rubric:

- **DIRECTLY TESTED** — a real fault executed at the exact boundary (a real
  child-process death, a real injected failure) with the outcome asserted.
- **SIMULATED** — the failure class represented by a synthetic state or a
  synthesized reply (no real fault executes; the *consequence* class is what
  is tested).
- **COVERED-BY-CONSTRUCTION** — the architecture structurally excludes the
  failure, and the exclusion itself carries a test.
- **UNTESTABLE-EXTERNAL** — depends on hardware, OS, or other-process
  behavior the harness cannot produce; the in-harness consequence class is
  stated.

**Disposition: MEASUREMENT COMPLETE.** One concrete isolation gap found
(the compile-time gating of seven process-exit statics is inconsistent —
P1 below); no production fault surface is added; the narrow proposals are
PROPOSED ONLY (the directive's boundary: "Do not add production
fault-injection surfaces without a separately reviewed necessity").

---

## 1. The seam inventory (read at this tree)

| Seam family | Where | Points / arms | Arming mechanism | Compile-time isolation |
|---|---|---|---|---|
| `FaultControl` / `FaultPoint` | `engine-persist/src/fault.rs` (the `LocalFileMedium` nine-step append + reconcile + begin reaffirmation) | 15 points (`BeforeLock` … `AfterRootDirSync`, `ReconcileLocked`, `ReconcileHeadRead`, `ReaffirmFilesSynced`) × `Abort` / `InjectError` | Per-instance, `LocalFileMedium::open_with`; a medium constructed without a control never consults the module | `#[doc(hidden)]`-class: always compiled, inert-by-construction (production paths carry one `Option` check per boundary) |
| `SqliteFaultControl` / `SqliteFaultPoint` | `engine-persist/src/sqlite.rs` (the append transaction phases) | 5 points (`MidObjectInserts`, `ObjectsInserted`, `CommitFailure`, `AtRollback`, `AfterCommit`) × `Abort` / `InjectError` | Per-instance, `SqliteMedium::open_with` | Same |
| `JournalFaultControl` / `JournalFaultPoint` | `engine-persist/src/journal.rs` (the two-phase journal windows) | 5 points (`BeforeCaptures` … `AfterSync`) × `Abort` / `InjectError` | Per-instance, journal `open`-with | Same |
| `WorkerFaultControl` / `WorkerFault` | `engine-slice/src/worker.rs` (the I/O worker loop) | 17 arms (panic/exit/stall/park/lose-reply/drop-append/unverifiable-reconcile/lose-begin/checkpoint-panic) | Per-instance, `IoWorker::spawn_with_faults` (the `#[doc(hidden)]` test-only spawn; `IoWorker::spawn` never arms) | Always compiled, per-instance inert-by-construction; the exit/panic arms are the first audit's F-10 residual (P2 below) |
| The crossing `ARM_*` statics | `engine-slice/src/crossing.rs` | 6 statics: `ARM_MID_INSTALL_EXIT`, `ARM_AFTER_COVERING_EXIT`, `ARM_AFTER_SK_EXIT`, `ARM_RECLAIM_AFTER_AUDIT`, `ARM_RECLAIM_AFTER_MEDIUM`, `ARM_RECLAIM_BEFORE_COMPLETION` (exit 13/14 at staged-recovery boundaries) | Process-global `AtomicBool` statics armed by the real-child tests | **INCONSISTENT — the F-10 finding:** only `ARM_RECLAIM_BEFORE_COMPLETION` is `#[cfg(feature = "crash-test-seams")]` (CI nm-verified absent from the feature-disabled build); the other five are `#[doc(hidden)]` only |
| The reclamation `ARM_*` statics | `engine-persist/src/local.rs` | 2 statics: `ARM_RECLAIM_MID_UNIT`, `ARM_RECLAIM_LAST_UNIT` (exit inside the unit-reclamation loop) | Process-global, armed by `engine-slice/tests/pp_b3_reclaim.rs` (the heavy shard only) | `#[doc(hidden)]` only (engine-persist has no crash-test-seams feature) |
| `ROTATION_SWAP_FAULT` | `engine-slice/src/crossing.rs` | The post-flip pipeline rebuild's injected I/O failure | Process-global `AtomicBool` | `#[doc(hidden)]` only |
| `EvidenceFaultControl` | `engine-slice/src/soak_evidence.rs` | The evidence writer's injected write/flush failures (the soak corpus) | Per-instance | Always compiled, inert-by-construction |
| The engine-persist activation/adversarial surfaces | `engine-persist` (`test_enable_composite_publication`, `test_only_install_completion_marker`) | The composite activation helper; the adversarial completion-marker installer | Feature-gated | **`composite-test-fixture` feature — compile-time isolated, CI nm-verified ABSENT from the feature-disabled rlib** |

**The CI's isolation verification** (the workflow's "Activation isolation"
step, Linux): the feature-disabled build must NOT carry
`test_enable_composite_publication`, `test_only_install_completion_marker`,
or `ARM_RECLAIM_BEFORE_COMPLETION` (each nm-counted to zero), and MUST carry
the four authorized production surfaces (`verify_installed_ancestry_witness`,
`walk_authenticated_history`, `account_batch_retirement`,
`publish_ancestry_witness`). The heavy shard runs the complete engine-slice
battery WITH `--features engine-slice/crash-test-seams`; the main shard
excludes engine-slice entirely — so every test that arms a crossing or
worker seam executes under the feature.

---

## 2. The failure-class matrix (the directive's twelve classes)

| # | Failure class | Coverage | Classification | The carrying seams / tests |
|---|---|---|---|---|
| 1 | Failure before durable publication | The pre-commit boundaries on both media + the queue-relayed pre-commit class | **DIRECTLY TESTED** | `FaultPoint::BeforeLock/AfterLock/AfterValidation/AfterComparison` (InjectError + Abort — the b3 matrix/process corpora); `SqliteFaultPoint::MidObjectInserts/ObjectsInserted`; `WorkerFault::FailAppendPreCommit` (the definite typed pre-commit error through the queue, nothing installed) |
| 2 | Lost acknowledgment after successful commit | The post-commit pre-ack death window on every layer | **DIRECTLY TESTED** | `SqliteFaultPoint::AfterCommit` (Abort); `FaultPoint::AfterHeadRename/AfterRootDirSync` (Abort); `JournalFaultPoint::AfterSync`; `WorkerFault::LoseAppendReply` / `ExitAfterAppendPreReply` (exit 13, the real-child arm) / `LoseBeginReply` — each resolving through the typed uncertain class, never a fabricated acknowledgment |
| 3 | Ambiguous publication outcome | The commit-uncertain / rollback-uncertain / reply-lost / verified-absent classes and their reconciliation | **DIRECTLY TESTED** | `SqliteFaultPoint::CommitFailure/AtRollback` (the OC-C3 CP-2 corpus — `Unknown{CommitUncertain}/{RollbackUncertain}`); `WorkerFault::LoseAppendReply` / `DropAppendWithoutExecution` / `DropNextAppends` (possibly-committed vs verified-absent); `UnverifiableNextReconcile` (the synthesized unverifiable outcome the gates must refuse) |
| 4 | Process termination at a documented boundary | The real-child matrix at every staged boundary | **DIRECTLY TESTED** | The 15+5+5 fault points' `Abort` arms; the 8 `ARM_*` statics (the pp_b3_reclaim / crossing child-process corpora); `WorkerFault::PanicNext/ExitBeforeNextJob/PanicBeforeCheckpoint/PanicAfter` — **residual:** a death inside the SQLite C library's own commit fsync is not injectable finer than `AfterCommit`, but every point in that window resolves through the same commit-uncertain class |
| 5 | Corrupt original content | The hash-verification refusal classes | **DIRECTLY TESTED (simulated corruption)** — the real bit-rot event is **UNTESTABLE-EXTERNAL**, its consequence class is covered | The adversarial fixtures: the truncated/malformed manifest/shard arms (r1a4); the corrupted-object rows (the sqlite typed-failure regressions — a corrupt row is never silent absence); `EvidenceObjectCorrupt` (the L0 hash-verify refusal); the crafted-max-epoch regression (F-01); the doctored-payload degradation matrix (r1a3) |
| 6 | Missing history | The linkage/orphan/attribution refusals | **DIRECTLY TESTED** | The orphan/parent-missing refusals; the fabricated same-key marker refused at the append; `a_missing_retained_audit_object` (pp_b3_reclaim); the two-retirement attribution fixtures; the dark-reachability reader (k2b) |
| 7 | Incomplete recovery evidence | The full classification ladder | **DIRECTLY TESTED** | The recovery-classification corpus (r1a1 synthetic, r1a2 production-path, r1a3 degradation, r1a4 witness degradation): `NoCoveringCheckpoint` → `AllBatchMarkersStructurallyMatched`, every reachable `EvidenceDegraded` and `WitnessDegradation` arm, the unreachability findings pinned as boundary tests |
| 8 | Canonical authority loss | The begin-authority grades, the tier fences, the crossing-level death + autonomous convergence | **DIRECTLY TESTED** | The begin reaffirmation/recovery (the OC-C3 CP-1/CP-2 corpora); the platform-tier refusals; the crossing `ARM_*` death tests with `SessionCrossing::recover` convergence (the K2-C2B P2 autonomous-recovery proof — the child dies mid-reclaim, the parent converges with no manual completion) |
| 9 | Storage read failure | The read-decode refusals and the transport-loss classes | **MIXED** — the corrupt-content consequence DIRECTLY TESTED (simulated); the raw EIO-on-read injection NOT directly tested (the fault points are write boundaries); the handling is the identical typed-`Result` plumbing as the write-side `InjectError` | The sqlite read-decode failures; the unreadable-manifest/shard arms; the worker's side-effect-free-read transport loss (the definite `WorkerUnavailable` class, pp_b4_worker) |
| 10 | WAL or checkpoint failure | The worker-side checkpoint death; the hot-WAL recovery; the checkpoint's typed failure | **DIRECTLY TESTED + COVERED-BY-CONSTRUCTION** | `WorkerFault::PanicBeforeCheckpoint` (the death between the final publication and the lifecycle checkpoint); the killed-writer hot-WAL relaunch (the s6_soak interrupted run; the busy-timeout-first discipline at `configure_writable`); the external-reader checkpoint block is excluded by the single-connection invariant (the production topology owns the wal — **UNTESTABLE-EXTERNAL** by design, and the CAS/`RejectedStaleParent` discipline is the tested foreign-writer defense) |
| 11 | Race between observation and publication | In-process serialization + the coordinated-observation contract | **COVERED-BY-CONSTRUCTION (tested)** | The single worker serializes every observation and publication through the one FIFO queue — now pinned by the F-11 §6a deterministic ordering proof (the park barrier + the parent-dependency order detector); the C5-series provider-coordinated observations (crossing.rs) test the coordination contract under that serialization; the foreign-writer fixtures test the external-connection observation paths |
| 12 | Worker shutdown during pending operations | The poison/admission/drain/abandoned-receiver contracts | **DIRECTLY TESTED** | The C2-F2 corpus (pp_b4_worker: work admitted before poison gets the definite honest refusal; an executing job runs to its completion boundary); the abandoned-receiver discipline (the reply send failure is ignored, never blocks the worker); the drain-on-close; the proxy-drop-before-join contract (pinned in the F-07 suites themselves) |

**The honest summary:** all twelve directive classes carry DIRECT or
CONSTRUCTION-COVERED evidence; the residuals are (a) the compile-time
isolation inconsistency (§1's finding — the corrective P1), (b) the
write-boundary-only fault points for read-side EIO (P3, low value), and
(c) the genuinely external classes (hardware fsync lies, power loss, OS
page-cache loss, external processes contending on the database file) —
outside any in-process harness by nature, with their consequence classes
covered through the uncertain/refusal disciplines above.

---

## 3. The narrow proposals (each requiring its own authorization)

### P1 — Complete the compile-time isolation of the process-exit statics

**The finding:** the production (feature-disabled) build carries **seven**
process-exit fault arms as reachable `pub static` symbols
(`ARM_MID_INSTALL_EXIT`, `ARM_AFTER_COVERING_EXIT`, `ARM_AFTER_SK_EXIT`,
`ARM_RECLAIM_AFTER_AUDIT`, `ARM_RECLAIM_AFTER_MEDIUM` in engine-slice's
crossing.rs; `ARM_RECLAIM_MID_UNIT`, `ARM_RECLAIM_LAST_UNIT` in
engine-persist's local.rs) while the CI verifies only ONE's absence
(`ARM_RECLAIM_BEFORE_COMPLETION` — the T2-B corrective's pattern). The
first audit's F-10 named exactly this class: "`#[doc(hidden)]` is
documentation suppression, not a compile-time gate."

**The corrective (narrow, the established pattern):** gate the five
crossing statics behind the existing `engine-slice/crash-test-seams`
feature; for the two engine-persist statics, add the matching
`engine-persist` feature with the passthrough (engine-slice's
`crash-test-seams` enables it — the same dev-dependency feature-chain the
composite fixture already uses), then extend the CI nm absence check to
all eight.

**The dependency analysis (verified at this tree):** every test that arms
these statics lives in the heavy shard, which runs the complete
engine-slice battery WITH `--features engine-slice/crash-test-seams`
(`pp_b3_reclaim.rs` is the sole armer of the engine-persist pair); the
main shard excludes engine-slice entirely and never arms them. The gating
is therefore CI-safe across both shards.

**Value:** closes the first audit's F-10 residual with the mechanism
already ratified at T2-B — the shipping artifact stops carrying seven
process-exit hooks the SDK never needs.

**STATUS: IMPLEMENTED (the RMD-03 corrective, 2026-11-14, on
`s6-remediation-rmd02-rmd03` from the integration candidate `5c45b8f`).**
The final remediation review's RMD-03 asked for the authorization
determination and the bounded implementation; the determination (the
standing directive's §9 "preserve compile-time isolation of test-only
behavior" covers the gating — it changes ONLY the compile-time visibility
of test hooks, touches no ratified durability or campaign-authority
semantic, and follows the T2-B precedent) was CONFIRMED against this
analysis, and the implementation landed exactly as the corrective above
specifies: the five crossing statics + their seam functions + their five
call sites behind `engine-slice/crash-test-seams`; the two engine-persist
statics + functions + call sites behind engine-persist's OWN new
`crash-test-seams` feature, forwarded from engine-slice's (the
`crash-test-seams = ["engine-persist/crash-test-seams"]` chain); the
arming tests carry the matching cfgs (the established
`ARM_RECLAIM_BEFORE_COMPLETION` pattern — the whole-fn gating for the
child-death tests, the match-arm gating for b4_process's shared
child-role helper), so the feature-enabled shards (the heavy battery on
both OSes + the macOS SDL-free suite — verified to already enable the
feature) run every arming test exactly as before, while the
feature-disabled compilations (the plain workspace build, the CI's
clippy, the main shard — engine-slice excluded) carry none of the
symbols; and the CI's activation-isolation step now nm-verifies the
absence of ALL EIGHT statics across both rlibs. **The disposition:
implemented + cross-reviewed (A's adversarial pass over the corrective
follows through the coordinator); no unrestricted fault-system change;
the WorkerFault panic/exit arms remain P2's documented acceptance.**

### P2 — The worker's panic/exit fault arms (the lower-value option)

`WorkerFault::PanicNext`, `ExitAfterAppendPreReply`, `ExitBeforeNextJob`,
`PanicBeforeCheckpoint`, `PanicAfter` are always-compiled (enum variants
cannot be individually `cfg`-gated). The honest options: (a) `cfg`-gate
the loop's exit/panic *executions* (without the feature an armed fault
becomes inert — which weakens the test contract silently, a real cost);
(b) keep them and extend the CI nm inventory to document their presence
as accepted (they are per-instance, armed only through the
`#[doc(hidden)]` `spawn_with_faults` — a production caller using
`IoWorker::spawn` can never arm them; the bar is higher than the global
statics P1 closes). **Recommendation: (b), documented acceptance** — the
inert-by-construction argument is strong for per-instance controls, and
(a)'s silent-inert risk outweighs the residual. A future SDK-hardening
pass may revisit with a `#[cfg(test)]`-only construction route.

### P3 — Read-side `InjectError` points (low priority, recorded for completeness)

Two or three read-boundary fault points (object read, record read, head
read) mirroring the write-side `InjectError`. **Value assessed as LOW:**
the read paths flow the same typed-`Result` plumbing as the write side,
and the corrupt-content consequence class is already covered by the
synthetic fixtures (class 5/9). Not proposed for implementation now.

### P4 — No new fault hooks

The directive's "the next step should not be to invent arbitrary new
fault hooks" is respected: the coverage above is broad enough that the
only justified changes are isolation hygiene (P1) and documentation (P2).

---

## 4. The preservation guarantees

- **No production fault-injection surface is added** by this analysis.
- The per-instance controls' inert-by-construction property (a medium or
  worker constructed through the production constructors never consults a
  fault module; production paths carry at most one `Option` check per
  boundary) is unchanged.
- The authorized production surfaces' PRESENCE in the feature-disabled
  build (the four nm-asserted symbols) is untouched by every proposal.
- The heavy shard's feature-enabled execution of every arming test is
  untouched (P1's gating is invisible to the shard commands).

**Disposition: MEASUREMENT COMPLETE — the matrix above is the deliverable;
P1 is the one concrete corrective found (PROPOSED, with its verified
dependency analysis); P2/P3 are recorded with honest value assessments;
nothing implemented.**
