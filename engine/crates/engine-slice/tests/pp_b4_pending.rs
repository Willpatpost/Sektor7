//! PP-B4 checkpoint 2 — the exception-safe `CommitPipeline`
//! pending-attempt lifecycle (the GO's §6 regressions A–I) over the
//! canonical I/O worker: the four-state candidate rule
//! (constructed-not-submitted / definitely-refused-before-installation
//! / submitted-with-uncertain-disposition /
//! accepted-awaiting-verified-readback), write-at-uncertainty-or-
//! acceptance, clear-only-on-verified-confirmation, and the
//! never-restore-an-older-candidate rule. In-process worker-thread
//! deaths prove the TYPED classifications (the returned outcomes and
//! the retained-slot refusals); the real child-process arms (the
//! durable states after restart) live in `pp_b4_worker.rs`.

use std::path::PathBuf;

use engine_persist::{
    AppendOutcome, AuthorityMode, AwardCheckpoint, CanonicalMedium, ClassifiedSpan, CommitOutcome,
    CommitPipeline, ConsequenceEnvelope, ConsequenceId, CounterValues, Coverage, DurabilityClass,
    FrontierEntry, Hash, MediumError, OccurrenceSpan, PipelineError, ReconcileOutcome,
    Reconciliation, RevisionRecord, SignalLane, SourceBaseline, SourcePosition, StoredObject,
    T1Journal, UnknownReason,
};
use engine_slice::store::{MediumSource, SessionStore, StoreLocation};
use engine_slice::worker::{IoWorker, MediumOp, WorkerFault, WorkerReply};

// ------------------------------------------------------------- fixtures

/// A temporary campaign directory (the established suite pattern).
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-pp-b4-pending-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        TestDir(path)
    }

    fn store(&self) -> PathBuf {
        self.0.join("store")
    }

    fn journal(&self) -> PathBuf {
        self.0.join("journal.t1")
    }

    /// A per-view journal path (the C2-F1 two-view fixture: independent
    /// journals over the one store).
    fn journal_at(&self, suffix: &str) -> PathBuf {
        self.0.join(format!("journal-{suffix}.t1"))
    }

    fn db(&self) -> PathBuf {
        self.0.join("campaign.db")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One capture envelope (the award form — the crossing fixture's
/// shape, built directly from the engine-persist value types).
fn envelope(region: u32, episode: u64, npc: u32, delta: u64, tick: u64) -> ConsequenceEnvelope {
    ConsequenceEnvelope::capture(
        ConsequenceId::new(region, episode, npc),
        vec![ClassifiedSpan {
            span: OccurrenceSpan::new(0, delta).unwrap(),
            class: DurabilityClass::DurableEligible,
        }],
        SignalLane::Captured,
        CounterValues::default(),
        SourceBaseline {
            counters: CounterValues::default(),
            award: AwardCheckpoint {
                durable: Coverage::new(),
                provisional: Coverage::new(),
            },
        },
        SourcePosition { head: None, tick },
    )
    .unwrap()
}

/// The campaign location, FOUNDED pre-worker (the discipline: the
/// founding is the pre-worker session-open act) — one direct provider
/// handle, dropped before the worker spawns.
fn found_at(location: StoreLocation) -> StoreLocation {
    let mut store = SessionStore::open(&location).unwrap();
    if store.head().unwrap().is_none() {
        store
            .append(
                &RevisionRecord::founding(AuthorityMode::Standalone, "pp-b4-pending", 0),
                &[],
                None,
            )
            .unwrap();
    }
    location
}

fn found_location(dir: &TestDir) -> StoreLocation {
    found_at(StoreLocation::Directory(dir.store()))
}

fn found_db_location(dir: &TestDir) -> StoreLocation {
    found_at(StoreLocation::Database(dir.db()))
}

/// The worker over the founded location.
fn found_worker(dir: &TestDir) -> IoWorker {
    IoWorker::spawn(&found_location(dir)).unwrap()
}

/// A pipeline over the worker's source and a fresh journal — the bare
/// open (the journal's committed transactions seed the per-key
/// tracking; a journaled identity can never be re-staged, so the
/// recovery pipelines use this form).
fn open_pipeline(
    dir: &TestDir,
    worker: &IoWorker,
) -> CommitPipeline<engine_slice::worker::WorkerMedium> {
    let journal = T1Journal::open(dir.journal()).unwrap();
    let medium = worker.source().unwrap().open_medium().unwrap();
    CommitPipeline::open(medium, journal).unwrap()
}

/// The C2-F1 two-view fixture's pipeline: an independent journal per
/// view over the one store.
fn open_pipeline_at(
    dir: &TestDir,
    worker: &IoWorker,
    suffix: &str,
) -> CommitPipeline<engine_slice::worker::WorkerMedium> {
    let journal = T1Journal::open(dir.journal_at(suffix)).unwrap();
    let medium = worker.source().unwrap().open_medium().unwrap();
    CommitPipeline::open(medium, journal).unwrap()
}

/// The pending-keys fixture every regression starts from: the bare
/// open plus ONE journaled (T1-durable, unconfirmed) capture.
fn pending_pipeline(
    dir: &TestDir,
    worker: &IoWorker,
) -> CommitPipeline<engine_slice::worker::WorkerMedium> {
    let mut pipeline = open_pipeline(dir, worker);
    stage_and_capture(&mut pipeline, 400);
    pipeline
}

fn stage_and_capture(pipeline: &mut CommitPipeline<engine_slice::worker::WorkerMedium>, tick: u64) {
    pipeline.stage(envelope(1, 1, 7, 3, tick)).unwrap();
    pipeline
        .capture(
            SourcePosition { head: None, tick },
            DurabilityClass::DurableEligible,
            Vec::new(),
            vec![FrontierEntry {
                region: 1,
                episode: 1,
            }],
            Vec::new(),
        )
        .unwrap();
}

/// The test's read-only oracle: the current head and its epoch, read
/// through a direct provider handle (per-operation coordination —
/// safe alongside the worker's connection).
fn oracle_head(dir: &TestDir) -> (Hash, u64) {
    let mut store = SessionStore::open(&found_location(dir)).unwrap();
    let head = store.head().unwrap().unwrap();
    let epoch = store.read_record(&head).unwrap().epoch;
    (head, epoch)
}

fn oracle_records_at(location: &StoreLocation) -> u64 {
    SessionStore::open(location)
        .unwrap()
        .validate()
        .unwrap()
        .records
}

fn oracle_records(dir: &TestDir) -> u64 {
    oracle_records_at(&found_location(dir))
}

/// One foreign revision on the current head (the competing-writer
/// oracle): advances HEAD so the pipeline's next append meets a
/// stale parent.
fn foreign_advance(worker: &IoWorker, parent: Hash, parent_epoch: u64, salt: u64) -> Hash {
    let domain = format!("engine-persist/pp-b4-foreign-{salt}/1");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(domain.len() as u64).to_le_bytes());
    bytes.extend_from_slice(domain.as_bytes());
    bytes.extend_from_slice(b"the foreign writer's payload");
    let object = StoredObject::new(bytes).unwrap();
    let record = RevisionRecord::regular(parent, parent_epoch, 900 + salt)
        .with_idempotency_key(format!("pp-b4-foreign-{salt}").into_bytes())
        .with_content_refs(vec![object.hash()]);
    let id = record.id();
    let reply = worker
        .submit_probe(MediumOp::Append {
            record,
            objects: vec![object],
            expected_parent: Some(parent),
        })
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    assert!(matches!(
        reply,
        WorkerReply::Append(AppendOutcome::Accepted { .. })
    ));
    id
}

// ------------------------------------------------------- the regressions

/// A — a definite pre-commit failure is an ordinary error WITHOUT a
/// retained candidate: the DIRECT retry of `commit_canonical`
/// proceeds (no `AttemptUnresolved`) and installs EXACTLY one batch.
#[test]
fn definite_pre_commit_failure_then_direct_retry_installs_exactly_once() {
    let dir = TestDir::new("a");
    let worker = found_worker(&dir);
    let mut pipeline = pending_pipeline(&dir, &worker);
    worker.fault_control().set(WorkerFault::FailAppendPreCommit);
    let result = pipeline.commit_canonical();
    assert!(matches!(
        result,
        Err(PipelineError::Medium(MediumError::Io { .. }))
    ));
    worker.fault_control().clear();
    // The direct retry — the pending slot was never written, so there is
    // no unresolved-attempt refusal.
    let outcome = pipeline.commit_canonical().unwrap();
    assert!(matches!(outcome, CommitOutcome::Confirmed(_)));
    // Exactly one installation: the founding plus ONE batch revision.
    assert_eq!(oracle_records(&dir), 2);
    // The exactly-once proof over a FRESH pipeline (the clean slot): the
    // walk finds the key already chain-confirmed — nothing pending. The
    // first pipeline is dropped FIRST (the journal's sidecar lock is held
    // for the instance's lifetime — a same-process second open blocks).
    drop(pipeline);
    let mut fresh = open_pipeline(&dir, &worker);
    assert!(matches!(
        fresh.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    ));
    drop(fresh);
    worker.join().unwrap();
}

/// B — an Unknown append (the reply lost after a possibly executed
/// append) retains the candidate; reconciliation establishes the
/// ACTUAL installed outcome through the full read-back walk and the
/// per-key validation.
#[test]
fn unknown_append_retains_the_candidate_and_reconciles_installs() {
    let dir = TestDir::new("b");
    let worker = found_worker(&dir);
    let mut pipeline = pending_pipeline(&dir, &worker);
    worker.fault_control().set(WorkerFault::LoseAppendReply);
    let outcome = pipeline.commit_canonical().unwrap();
    let CommitOutcome::Unknown {
        revision,
        reason: UnknownReason::WorkerReplyLost,
    } = outcome
    else {
        panic!("the lost append reply must surface the uncertain class");
    };
    // The append ACTUALLY landed (the oracle proves it) — never a false
    // acknowledgment, and never a definite failure either.
    assert_eq!(oracle_head(&dir).0, revision);
    // The reconciliation: Installed → the full readback → Confirmed.
    let reconciled = pipeline.reconcile_pending().unwrap();
    assert!(matches!(reconciled, ReconcileOutcome::Confirmed(_)));
    // Exactly one installation; the re-drive is exactly-once.
    assert_eq!(oracle_records(&dir), 2);
    assert!(matches!(
        pipeline.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    ));
    drop(pipeline);
    worker.join().unwrap();
}

/// C — the append was ACCEPTED and the worker died during the initial
/// verified readback: no false confirmation, the attempt retained,
/// and the fresh recovery confirms the key exactly-once.
#[test]
fn worker_death_during_initial_readback_retains_the_attempt() {
    let dir = TestDir::new("c");
    let worker = found_worker(&dir);
    let mut pipeline = pending_pipeline(&dir, &worker);
    worker.fault_control().set(WorkerFault::PanicAfter(
        engine_slice::worker::JobKind::Append,
    ));
    let outcome = pipeline.commit_canonical().unwrap();
    let CommitOutcome::Unknown {
        revision,
        reason: UnknownReason::WorkerReplyLost,
    } = outcome
    else {
        panic!("the readback death must surface the uncertain class, never a confirmation");
    };
    // The failure signal (the reply channel dropping mid-unwind)
    // precedes the DeathGuard's thread-exit flip — the death is
    // guaranteed, never synchronous with the failure return (bounded
    // wait; the CI-runner race the first three-OS dispatch exposed).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while worker.healthy() && std::time::Instant::now() < deadline {
        std::hint::spin_loop();
    }
    assert!(!worker.healthy());
    // The attempt is RETAINED: the dead worker's reconciliation fails
    // worker-class — it is NOT NothingPending (which would mean an empty
    // slot, i.e. a discarded unresolved obligation).
    assert!(matches!(
        pipeline.reconcile_pending(),
        Err(PipelineError::Medium(MediumError::WorkerUnavailable { .. }))
    ));
    // The append landed (the accepted disposition stands).
    assert_eq!(oracle_head(&dir).0, revision);
    // THE FRESH RECOVERY: drop the dead session (releasing the journal's
    // sidecar lock), then a new worker and pipeline over the same
    // durable state — the walk collects the confirmed key, so the
    // re-drive finds nothing pending: exactly-once.
    drop(pipeline);
    let worker2 = found_worker(&dir);
    let mut pipeline2 = open_pipeline(&dir, &worker2);
    assert!(matches!(
        pipeline2.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    ));
    assert_eq!(oracle_records(&dir), 2);
    drop(pipeline2);
    worker2.join().unwrap();
}

/// D — the worker dies during `reconcile_pending`'s verified readback
/// (after `Installed` was established): the pending candidate
/// survives the failure; the fresh recovery confirms exactly-once.
#[test]
fn worker_death_during_reconcile_readback_retains_the_candidate() {
    let dir = TestDir::new("d");
    let worker = found_worker(&dir);
    let mut pipeline = pending_pipeline(&dir, &worker);
    worker.fault_control().set(WorkerFault::LoseAppendReply);
    let CommitOutcome::Unknown { revision, .. } = pipeline.commit_canonical().unwrap() else {
        panic!("the lost reply must surface the uncertain class");
    };
    worker.fault_control().clear();
    // The death lands INSIDE the reconcile's readback: the reconcile job
    // replies Installed, the following walk job panics.
    worker.fault_control().set(WorkerFault::PanicAfter(
        engine_slice::worker::JobKind::Reconcile,
    ));
    let result = pipeline.reconcile_pending();
    // NOT Confirmed, and NOT Unverifiable (that variant is reserved for
    // genuine chain-verification failures — never conflated with the
    // worker's death): the typed worker error, the candidate retained.
    assert!(matches!(
        result,
        Err(PipelineError::Medium(MediumError::WorkerUnavailable { .. }))
    ));
    // The same guaranteed-death bounded wait (the failure signal
    // precedes the thread's completion).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while worker.healthy() && std::time::Instant::now() < deadline {
        std::hint::spin_loop();
    }
    assert!(!worker.healthy());
    // The retention: a second reconcile is refused worker-class (the slot
    // is still occupied — not NothingPending).
    assert!(matches!(
        pipeline.reconcile_pending(),
        Err(PipelineError::Medium(MediumError::WorkerUnavailable { .. }))
    ));
    assert_eq!(oracle_head(&dir).0, revision);
    drop(pipeline);
    let worker2 = found_worker(&dir);
    let mut pipeline2 = open_pipeline(&dir, &worker2);
    assert!(matches!(
        pipeline2.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    ));
    assert_eq!(oracle_records(&dir), 2);
    drop(pipeline2);
    worker2.join().unwrap();
}

/// E — the worker dies during `retry_pending`'s preliminary chain
/// walk: the ORIGINAL candidate survives (the slot never emptied),
/// and the fresh recovery completes the confirmation exactly-once.
#[test]
fn worker_death_during_retry_preliminary_walk_preserves_the_original() {
    let dir = TestDir::new("e");
    let worker = found_worker(&dir);
    let mut pipeline = pending_pipeline(&dir, &worker);
    // The worker panics BEFORE executing the append job — the append never
    // ran, but the proxy cannot know that: the uncertain class, the
    // candidate retained.
    worker.fault_control().set(WorkerFault::PanicNext);
    let CommitOutcome::Unknown { revision, .. } = pipeline.commit_canonical().unwrap() else {
        panic!("the pre-execution death must surface the uncertain class");
    };
    // The same guaranteed-death bounded wait (the failure signal
    // precedes the thread's completion).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while worker.healthy() && std::time::Instant::now() < deadline {
        std::hint::spin_loop();
    }
    assert!(!worker.healthy());
    // The retry's preliminary walk fails worker-class — and the ORIGINAL
    // candidate survives: a second retry is refused (not NothingPending —
    // the slot is occupied by the unresolved candidate).
    assert!(matches!(
        pipeline.retry_pending(),
        Err(PipelineError::Medium(MediumError::WorkerUnavailable { .. }))
    ));
    assert!(matches!(
        pipeline.retry_pending(),
        Err(PipelineError::Medium(MediumError::WorkerUnavailable { .. }))
    ));
    // The append never landed (the worker died before executing it).
    assert_ne!(oracle_head(&dir).0, revision);
    // The fresh recovery: the journal evidence re-drives — the batch
    // installs NOW, exactly once.
    drop(pipeline);
    let worker2 = found_worker(&dir);
    let mut pipeline2 = open_pipeline(&dir, &worker2);
    assert!(matches!(
        pipeline2.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    assert_eq!(oracle_records(&dir), 2);
    drop(pipeline2);
    worker2.join().unwrap();
}

/// F — the R3-1 distinguishing regression: candidate A is established
/// NotInstalled; the retry constructs B; B commits; the worker dies
/// during B's readback — B remains the reconciliation identity, NEVER
/// restored to A; the fresh recovery confirms B's installation
/// exactly-once.
#[test]
fn retry_candidate_b_replaces_a_never_restored() {
    let dir = TestDir::new("f");
    let worker = found_worker(&dir);
    let mut pipeline = pending_pipeline(&dir, &worker);
    // A: the pipeline's expected parent is the founding head; advance it
    // first (the competing writer), so the append CAS-rejects — and the
    // LOST reply hides the rejection: an Unknown whose candidate is
    // actually NOT installed.
    let (founding, epoch) = oracle_head(&dir);
    foreign_advance(&worker, founding, epoch, 1);
    worker.fault_control().set(WorkerFault::LoseAppendReply);
    let CommitOutcome::Unknown { revision: a, .. } = pipeline.commit_canonical().unwrap() else {
        panic!("the lost reply must surface the uncertain class");
    };
    worker.fault_control().clear();
    // A is genuinely NotInstalled (the CAS rejected it).
    assert_ne!(oracle_head(&dir).0, a);
    assert!(matches!(
        pipeline.reconcile_pending().unwrap(),
        ReconcileOutcome::NotInstalled
    ));
    // B: the retry re-walks, re-derives the remaining keys, and submits on
    // the observed parent — the append is ACCEPTED, then the worker dies
    // during B's verified readback.
    worker.fault_control().set(WorkerFault::PanicAfter(
        engine_slice::worker::JobKind::Append,
    ));
    let outcome = pipeline.retry_pending().unwrap();
    let CommitOutcome::Unknown {
        revision: b,
        reason: UnknownReason::WorkerReplyLost,
    } = outcome
    else {
        panic!("B's readback death must surface the uncertain class");
    };
    assert_ne!(a, b, "B is the reconciliation identity, never A");
    // B landed (its accepted disposition stands).
    assert_eq!(oracle_head(&dir).0, b);
    // The same guaranteed-death bounded wait (the failure signal
    // precedes the thread's completion).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while worker.healthy() && std::time::Instant::now() < deadline {
        std::hint::spin_loop();
    }
    assert!(!worker.healthy());
    // The slot holds B: the dead-worker reconciliation is refused (not
    // NothingPending), and the fresh recovery confirms the key from B's
    // installation — exactly-once.
    assert!(matches!(
        pipeline.reconcile_pending(),
        Err(PipelineError::Medium(MediumError::WorkerUnavailable { .. }))
    ));
    drop(pipeline);
    let worker2 = found_worker(&dir);
    let mut pipeline2 = open_pipeline(&dir, &worker2);
    assert!(matches!(
        pipeline2.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    ));
    // The founding + the foreign revision + B's batch — exactly one
    // installation of the consequence batch.
    assert_eq!(oracle_records(&dir), 3);
    drop(pipeline2);
    worker2.join().unwrap();
}

/// G — an already-verified confirmation (the walk discovered the
/// candidate's own batch installed) clears the pending state ONLY
/// after the exact-provenance receipt verifies.
#[test]
fn walk_discovered_confirmation_clears_pending_only_after_verified_receipt() {
    let dir = TestDir::new("g");
    let worker = found_worker(&dir);
    let mut pipeline = pending_pipeline(&dir, &worker);
    worker.fault_control().set(WorkerFault::LoseAppendReply);
    let CommitOutcome::Unknown { revision, .. } = pipeline.commit_canonical().unwrap() else {
        panic!("the lost reply must surface the uncertain class");
    };
    worker.fault_control().clear();
    // The retry's walk discovers the candidate's own batch on the chain —
    // every attempted key confirmed — and the foreign receipt verifies the
    // exact provenance (the revision that ACTUALLY confirmed the keys)
    // BEFORE the slot clears.
    let outcome = pipeline.retry_pending().unwrap();
    let CommitOutcome::Confirmed(receipt) = outcome else {
        panic!("the walk-discovered confirmation must confirm");
    };
    assert_eq!(receipt.revision, revision);
    // The slot cleared: nothing further to reconcile or re-drive.
    assert!(matches!(
        pipeline.reconcile_pending().unwrap(),
        ReconcileOutcome::NothingPending
    ));
    assert!(matches!(
        pipeline.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    ));
    assert_eq!(oracle_records(&dir), 2);
    drop(pipeline);
    worker.join().unwrap();
}

/// H — the stale-parent rebase preserves each consequence identity:
/// the producing transaction, the occurrence coverage, and the effect
/// commitment — the per-key provenance the confirmation validates.
#[test]
fn stale_parent_rebase_preserves_each_consequence_identity() {
    let dir = TestDir::new("h");
    let worker = found_worker(&dir);
    let mut pipeline = pending_pipeline(&dir, &worker);
    let (founding, epoch) = oracle_head(&dir);
    foreign_advance(&worker, founding, epoch, 1);
    // The append meets the advanced head: rejected stale → the suffix walk
    // → the rebase on the observed parent with IDENTICAL per-key
    // identities and coverages → confirmed.
    let outcome = pipeline.commit_canonical().unwrap();
    let CommitOutcome::Confirmed(receipt) = outcome else {
        panic!("the rebase must confirm");
    };
    let key = ConsequenceId::new(1, 1, 7);
    assert!(receipt.keys.contains(&key));
    // The per-key provenance: the producing journal transaction (readable
    // from the journal the fixture captured), and the occurrence coverage
    // exactly as staged.
    let producing = pipeline
        .confirmed_transaction(key)
        .expect("the key is confirmed");
    let journal = T1Journal::scan_file(dir.journal()).unwrap();
    assert_eq!(journal.committed.len(), 1);
    assert_eq!(journal.committed[0].id, producing);
    let coverage = pipeline.confirmed_coverage(key).unwrap();
    assert_eq!(coverage.len(), 1);
    assert_eq!(coverage[0].span.lo, 0);
    assert_eq!(coverage[0].span.hi, 3);
    assert_eq!(oracle_records(&dir), 3);
    drop(pipeline);
    worker.join().unwrap();
}

/// I — the LEGACY synchronous pipeline (a direct provider handle, no
/// worker) retains its existing results and retry behavior.
#[test]
fn legacy_operations_retain_their_results_and_retry_behavior() {
    let dir = TestDir::new("i");
    let location = found_location(&dir);
    let store = SessionStore::open(&location).unwrap();
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut pipeline = CommitPipeline::open(store, journal).unwrap();
    stage_and_capture_legacy(&mut pipeline, 400);
    assert!(matches!(
        pipeline.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    assert!(matches!(
        pipeline.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    ));
    assert!(matches!(
        pipeline.reconcile_pending().unwrap(),
        ReconcileOutcome::NothingPending
    ));
    assert!(matches!(
        pipeline.retry_pending().unwrap(),
        CommitOutcome::NothingPending
    ));
    assert_eq!(oracle_records(&dir), 2);
}

/// The legacy fixture's stage+capture (the same envelope over the
/// direct medium).
fn stage_and_capture_legacy(pipeline: &mut CommitPipeline<SessionStore>, tick: u64) {
    pipeline.stage(envelope(1, 1, 7, 3, tick)).unwrap();
    pipeline
        .capture(
            SourcePosition { head: None, tick },
            DurabilityClass::DurableEligible,
            Vec::new(),
            vec![FrontierEntry {
                region: 1,
                episode: 1,
            }],
            Vec::new(),
        )
        .unwrap();
}

/// The worker's `reconcile_unknown` forwarding is the accepted
/// classification (the parity spot-check the lifecycle relies on).
#[test]
fn the_reconcile_forwarding_matches_the_accepted_classification() {
    let dir = TestDir::new("reconcile-parity");
    let worker = found_worker(&dir);
    let mut medium = worker.source().unwrap().open_medium().unwrap();
    let (head, _) = oracle_head(&dir);
    assert!(matches!(
        medium.reconcile_unknown(&head).unwrap(),
        Reconciliation::Installed { .. }
    ));
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&25u64.to_le_bytes());
    bytes.extend_from_slice(b"pp-b4-absent-domain-objec");
    bytes.extend_from_slice(b"not on the chain");
    let absent = StoredObject::new(bytes).unwrap().hash();
    assert!(matches!(
        medium.reconcile_unknown(&absent).unwrap(),
        Reconciliation::NotInstalled { .. }
    ));
    drop(medium);
    worker.join().unwrap();
}

// ------------------------------------------- the C2-F1 corrective regressions

/// C2-F1 (worker-backed, file medium): a verified foreign confirmation
/// clears the pending slot — two independent pipeline views over the one
/// store, each with its OWN journal, hold the SAME attempted
/// consequential claim (byte-identical envelopes → the same
/// content-derived TransactionId — exactly equivalent provenance); the
/// first confirms, advancing HEAD; the second's append is CAS-rejected,
/// its walk finds every attempted key already confirmed with the exact
/// provenance, the foreign receipt verifies — Confirmed WITH the slot
/// cleared, and the next commit_canonical/reconcile_pending are both
/// NothingPending (never AttemptUnresolved). Exactly ONE canonical
/// confirmation of the identity exists.
#[test]
fn foreign_confirmation_clears_the_pending_slot_worker_file() {
    let dir = TestDir::new("c2f1-worker-file");
    let worker = found_worker(&dir);
    // Both views open while HEAD is the founding — the second's append
    // will meet the advanced head.
    let mut pipeline_a = open_pipeline_at(&dir, &worker, "a");
    stage_and_capture(&mut pipeline_a, 400);
    let mut pipeline_b = open_pipeline_at(&dir, &worker, "b");
    stage_and_capture(&mut pipeline_b, 400);
    // The first view confirms.
    assert!(matches!(
        pipeline_a.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    // The second: the stale parent → the walk → every attempted key
    // confirmed with the exact provenance → the foreign receipt.
    assert!(matches!(
        pipeline_b.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    // THE FIX'S PROOF: the slot cleared — NothingPending, never
    // AttemptUnresolved.
    assert!(matches!(
        pipeline_b.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    ));
    assert!(matches!(
        pipeline_b.reconcile_pending().unwrap(),
        ReconcileOutcome::NothingPending
    ));
    // Exactly ONE canonical confirmation: the founding + the first view's
    // batch — the second view published nothing.
    assert_eq!(oracle_records(&dir), 2);
    drop(pipeline_a);
    drop(pipeline_b);
    worker.join().unwrap();
}

/// C2-F1 (legacy synchronous, file medium): the same two-view scenario
/// over direct `SessionStore` handles — the per-operation locking
/// serializes the competing appends.
#[test]
fn foreign_confirmation_clears_the_pending_slot_legacy_file() {
    let dir = TestDir::new("c2f1-legacy-file");
    let location = found_location(&dir);
    let store_a = SessionStore::open(&location).unwrap();
    let store_b = SessionStore::open(&location).unwrap();
    let mut pipeline_a =
        CommitPipeline::open(store_a, T1Journal::open(dir.journal_at("a")).unwrap()).unwrap();
    stage_and_capture_legacy(&mut pipeline_a, 400);
    let mut pipeline_b =
        CommitPipeline::open(store_b, T1Journal::open(dir.journal_at("b")).unwrap()).unwrap();
    stage_and_capture_legacy(&mut pipeline_b, 400);
    assert!(matches!(
        pipeline_a.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    assert!(matches!(
        pipeline_b.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    assert!(matches!(
        pipeline_b.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    ));
    assert!(matches!(
        pipeline_b.reconcile_pending().unwrap(),
        ReconcileOutcome::NothingPending
    ));
    assert_eq!(oracle_records(&dir), 2);
}

/// C2-F1 (worker-backed, SQLite): the same two-view scenario over the
/// database provider.
#[test]
fn foreign_confirmation_clears_the_pending_slot_worker_sqlite() {
    let dir = TestDir::new("c2f1-worker-sqlite");
    let worker = IoWorker::spawn(&found_db_location(&dir)).unwrap();
    let mut pipeline_a = open_pipeline_at(&dir, &worker, "a");
    stage_and_capture(&mut pipeline_a, 400);
    let mut pipeline_b = open_pipeline_at(&dir, &worker, "b");
    stage_and_capture(&mut pipeline_b, 400);
    assert!(matches!(
        pipeline_a.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    assert!(matches!(
        pipeline_b.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    assert!(matches!(
        pipeline_b.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    ));
    assert!(matches!(
        pipeline_b.reconcile_pending().unwrap(),
        ReconcileOutcome::NothingPending
    ));
    assert_eq!(oracle_records_at(&found_db_location(&dir)), 2);
    drop(pipeline_a);
    drop(pipeline_b);
    worker.join().unwrap();
}
