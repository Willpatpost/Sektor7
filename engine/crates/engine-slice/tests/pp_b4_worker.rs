//! PP-B4 checkpoint 2 — the canonical I/O worker core (the GO's §5
//! suite): one dedicated thread per campaign owning the ONE
//! `SessionStore` connection; one bounded FIFO queue; the
//! `WorkerMedium` queue-proxy `CanonicalMedium`; the cloneable
//! worker-backed `MediumSource`; the synchronous request/reply for
//! required operations; the health cell readable without a blocking
//! request; the explicit spawn/poison/stop/join lifecycle; and the
//! cached truthful `SyncTier`. The real child-process arms (exits 13
//! and 14) establish the durable states after restart — distinguished
//! from the in-process thread-failure tests, which inspect the typed
//! returns. The crossing integration proves the checkpoint-1 seam
//! composes over the worker and that a POISONED worker fails every
//! authoritative crossing operation closed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use engine_core::{LodLevel, PresenceSample, Spine};
use engine_persist::{
    AppendOutcome, AuthorityMode, CanonicalMedium, ChainSummary, CommitOutcome, CommitPipeline,
    ConsequenceEnvelope, ConsequenceId, MediumError, RevisionRecord, StoredObject, T1Journal,
    UnknownReason,
};
use engine_slice::crossing::{SessionCrossing, TransitionError};
use engine_slice::npc::{NpcConfig, NpcWorld, REGION_COUNT, install_initial_schedules};
use engine_slice::region::{RegionConfig, RegionDirector};
use engine_slice::store::{MediumSource, SessionStore, StoreLocation};
use engine_slice::worker::{DEFAULT_QUEUE_CAPACITY, IoWorker, MediumOp, WorkerFault};

// ------------------------------------------------------------- fixtures

/// A temporary campaign directory (the established suite pattern).
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-pp-b4-worker-{name}-{}",
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

    fn db(&self) -> PathBuf {
        self.0.join("campaign.db")
    }

    fn journal(&self) -> PathBuf {
        self.0.join("journal.t1")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The campaign location, FOUNDED pre-worker (the discipline: the
/// founding is the pre-worker session-open act).
fn found_location(at: &Path, database: bool) -> StoreLocation {
    let location = if database {
        StoreLocation::Database(at.to_path_buf())
    } else {
        StoreLocation::Directory(at.to_path_buf())
    };
    let mut store = SessionStore::open(&location).unwrap();
    if store.head().unwrap().is_none() {
        store
            .append(
                &RevisionRecord::founding(AuthorityMode::Standalone, "pp-b4-worker", 0),
                &[],
                None,
            )
            .unwrap();
    }
    location
}

fn found_worker(at: &Path, database: bool) -> IoWorker {
    IoWorker::spawn(&found_location(at, database)).unwrap()
}

/// One deterministic content object (the foreign-writer payload the
/// FIFO and parity sequences append).
fn sequence_object(salt: u64) -> StoredObject {
    let domain = format!("engine-persist/pp-b4-sequence-{salt}/1");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(domain.len() as u64).to_le_bytes());
    bytes.extend_from_slice(domain.as_bytes());
    bytes.extend_from_slice(b"a deterministic payload");
    StoredObject::new(bytes).unwrap()
}

/// Appends one revision on `parent` through the given medium and
/// returns its identity.
fn append_revision(
    medium: &mut impl CanonicalMedium,
    parent: engine_persist::Hash,
    parent_epoch: u64,
    salt: u64,
) -> engine_persist::Hash {
    let object = sequence_object(salt);
    let record = RevisionRecord::regular(parent, parent_epoch, 100 + salt)
        .with_idempotency_key(format!("pp-b4-sequence-{salt}").into_bytes())
        .with_content_refs(vec![object.hash()]);
    let id = record.id();
    match medium
        .append(&record, std::slice::from_ref(&object), Some(parent))
        .unwrap()
    {
        AppendOutcome::Accepted { .. } => id,
        other => panic!("the sequence append must accept: {other:?}"),
    }
}

fn founding_head(location: &StoreLocation) -> (engine_persist::Hash, u64) {
    let mut store = SessionStore::open(location).unwrap();
    let head = store.head().unwrap().unwrap();
    let epoch = store.read_record(&head).unwrap().epoch;
    (head, epoch)
}

// ---------------------------------------------------- the §5 worker suite

/// FIFO processing across actually submitted jobs: the chain's parent
/// order equals the submission order, with reads interleaved on the
/// same queue.
#[test]
fn fifo_order_across_submitted_jobs() {
    let dir = TestDir::new("fifo");
    let worker = found_worker(&dir.store(), false);
    let mut medium = worker.source().unwrap().open_medium().unwrap();
    let (founding, epoch) = founding_head(&found_location(&dir.store(), false));
    let a = append_revision(&mut medium, founding, epoch, 1);
    let b = append_revision(&mut medium, a, epoch + 1, 2);
    // A read interleaved between the writes — the same queue.
    assert_eq!(medium.read_record(&b).unwrap().parent, Some(a));
    let c = append_revision(&mut medium, b, epoch + 2, 3);
    let summary = medium.validate().unwrap();
    assert_eq!(summary.head, c);
    assert_eq!(summary.records, 4);
    assert_eq!(medium.read_record(&c).unwrap().parent, Some(b));
    assert_eq!(medium.read_record(&b).unwrap().parent, Some(a));
    assert_eq!(medium.read_record(&a).unwrap().parent, Some(founding));
    drop(medium);
    worker.join().unwrap();
}

/// The identical operation sequence through any medium (the parity
/// driver): three deterministic appends, validated.
fn run_sequence(medium: &mut impl CanonicalMedium, location: &StoreLocation) -> ChainSummary {
    let (founding, epoch) = founding_head(location);
    let a = append_revision(medium, founding, epoch, 1);
    let b = append_revision(medium, a, epoch + 1, 2);
    let c = append_revision(medium, b, epoch + 2, 3);
    let summary = medium.validate().unwrap();
    assert_eq!(summary.head, c);
    summary
}

/// Parity with the legacy synchronous provider — the file medium: the
/// identical operation sequence through a direct `SessionStore` and
/// through the worker produces the EXACT same canonical chain
/// (identity, content, and summary equality).
#[test]
fn worker_parity_with_the_legacy_provider_file() {
    let direct_dir = TestDir::new("parity-file-direct");
    let worker_dir = TestDir::new("parity-file-worker");
    let mut direct = SessionStore::open(&found_location(&direct_dir.store(), false)).unwrap();
    let direct_summary = run_sequence(&mut direct, &found_location(&direct_dir.store(), false));
    let worker = found_worker(&worker_dir.store(), false);
    let mut proxied = worker.source().unwrap().open_medium().unwrap();
    let worker_summary = run_sequence(&mut proxied, &found_location(&worker_dir.store(), false));
    assert_eq!(direct_summary, worker_summary);
    drop(direct);
    drop(proxied);
    worker.join().unwrap();
}

/// Parity with the legacy synchronous provider — SQLite: the same
/// equality over the database provider.
#[test]
fn worker_parity_with_the_legacy_provider_sqlite() {
    let direct_dir = TestDir::new("parity-sqlite-direct");
    let worker_dir = TestDir::new("parity-sqlite-worker");
    let mut direct = SessionStore::open(&found_location(&direct_dir.db(), true)).unwrap();
    let direct_summary = run_sequence(&mut direct, &found_location(&direct_dir.db(), true));
    let worker = found_worker(&worker_dir.db(), true);
    let mut proxied = worker.source().unwrap().open_medium().unwrap();
    let worker_summary = run_sequence(&mut proxied, &found_location(&worker_dir.db(), true));
    assert_eq!(direct_summary, worker_summary);
    drop(direct);
    drop(proxied);
    worker.join().unwrap();
}

/// The cached durability tier is TRUTHFUL (read from the actual
/// provider at spawn) and needs NO worker round trip: it returns
/// while the worker is stalled mid-job.
#[test]
fn the_cached_durability_tier_is_truthful_without_a_round_trip() {
    let file_dir = TestDir::new("tier-file");
    let sqlite_dir = TestDir::new("tier-sqlite");
    let file_worker = found_worker(&file_dir.store(), false);
    let sqlite_worker = found_worker(&sqlite_dir.db(), true);
    assert_eq!(
        file_worker.tier(),
        SessionStore::open(&found_location(&file_dir.store(), false))
            .unwrap()
            .durability_tier()
    );
    assert_eq!(
        sqlite_worker.tier(),
        engine_persist::SyncTier::TransactionalWalFull
    );
    // No round trip: stall the worker mid-job — the tier still returns
    // immediately (a round trip would block on the stalled queue).
    sqlite_worker
        .fault_control()
        .set(WorkerFault::StallEach { ms: 60_000 });
    let _ = sqlite_worker.tier();
    assert_eq!(
        sqlite_worker.tier(),
        engine_persist::SyncTier::TransactionalWalFull
    );
    sqlite_worker.fault_control().clear();
    let mut medium = sqlite_worker.source().unwrap().open_medium().unwrap();
    medium.head().unwrap();
    drop(medium);
    file_worker.join().unwrap();
    sqlite_worker.join().unwrap();
}

/// The SQLite WAL `TRUNCATE` checkpoint forwards through the worker's
/// explicit `checkpoint` override (never the trait's default no-op):
/// the WAL file truncates and the chain stays valid.
#[test]
fn the_sqlite_truncate_checkpoint_forwards() {
    let dir = TestDir::new("truncate");
    let worker = found_worker(&dir.db(), true);
    let mut medium = worker.source().unwrap().open_medium().unwrap();
    let (founding, epoch) = founding_head(&found_location(&dir.db(), true));
    let a = append_revision(&mut medium, founding, epoch, 1);
    let b = append_revision(&mut medium, a, epoch + 1, 2);
    let wal = dir.db().with_extension("db-wal");
    let wal_len = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    assert!(wal_len > 0, "the appends must grow the WAL first");
    medium.checkpoint().unwrap();
    let wal_len_after = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    assert_eq!(
        wal_len_after, 0,
        "the TRUNCATE checkpoint must empty the WAL"
    );
    assert_eq!(medium.validate().unwrap().head, b);
    drop(medium);
    worker.join().unwrap();
}

/// Every proxy handle — however many the cloneable source opens — is
/// a queue sender to the ONE serialization point: with the worker
/// stalled, operations from independent handles all wait on the same
/// single-threaded execution, then complete.
#[test]
fn worker_proxies_share_the_one_serialization_point() {
    let dir = TestDir::new("serialized");
    let worker = found_worker(&dir.store(), false);
    let source = worker.source().unwrap();
    let mut medium_a = source.open_medium().unwrap();
    let mut medium_b = source.open_medium().unwrap();
    worker
        .fault_control()
        .set(WorkerFault::StallEach { ms: 40 });
    let thread_a = std::thread::spawn(move || medium_a.head().unwrap());
    let thread_b = std::thread::spawn(move || medium_b.head().unwrap());
    thread_a.join().unwrap();
    thread_b.join().unwrap();
    worker.fault_control().clear();
    assert!(worker.healthy());
    drop(source);
    worker.join().unwrap();
}

/// The bounded queue saturates and the REQUIRED operations complete
/// after the earlier queued work drains: 96 jobs (capacity 64) from 8
/// threads, all green.
#[test]
fn bounded_queue_saturation_completes_required_work() {
    let dir = TestDir::new("saturation");
    let worker = found_worker(&dir.store(), false);
    worker.fault_control().set(WorkerFault::StallEach { ms: 5 });
    let source = Arc::new(worker.source().unwrap());
    let mut threads = Vec::new();
    for _ in 0..8 {
        let source = source.clone();
        threads.push(std::thread::spawn(move || {
            let mut medium = source.open_medium().unwrap();
            for _ in 0..12 {
                medium.head().unwrap();
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    worker.fault_control().clear();
    assert!(worker.healthy());
    drop(source);
    worker.join().unwrap();
}

/// A worker PANIC is detected through the health cell — without
/// polling any completion — and every subsequent operation fails the
/// typed worker-unavailable error.
#[test]
fn panic_detection_without_completion_polling() {
    let dir = TestDir::new("panic");
    let worker = found_worker(&dir.store(), false);
    let mut medium = worker.source().unwrap().open_medium().unwrap();
    worker.fault_control().set(WorkerFault::PanicNext);
    let result = medium.head();
    assert!(matches!(result, Err(MediumError::WorkerUnavailable { .. })));
    // THE CELL: no completion was polled anywhere — the health flips
    // through the panicking thread's drop guard. The failure signal
    // (the reply channel dropping mid-unwind) precedes the thread's
    // completion — the death is guaranteed, never synchronous with
    // the failure return (bounded wait; the CI-runner race the first
    // three-OS dispatch exposed).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while worker.healthy() && std::time::Instant::now() < deadline {
        std::hint::spin_loop();
    }
    assert!(!worker.healthy());
    assert!(matches!(
        medium.head(),
        Err(MediumError::WorkerUnavailable { .. })
    ));
    assert!(!worker.source().unwrap().alive());
    drop(medium);
    worker.join().unwrap_err();
}

/// An abandoned reply receiver never deadlocks the worker: the reply
/// is deposited into the dead slot (a typed send failure the worker
/// ignores) and the very next operation completes.
#[test]
fn abandoned_reply_receivers_never_deadlock_the_worker() {
    let dir = TestDir::new("abandoned");
    let worker = found_worker(&dir.store(), false);
    let slot = worker.submit_probe(MediumOp::Head).unwrap();
    drop(slot); // the abandoned receiver
    let mut medium = worker.source().unwrap().open_medium().unwrap();
    medium.head().unwrap();
    drop(medium);
    worker.join().unwrap();
}

/// The graceful stop and join: after accepted work, with every source
/// and proxy dropped, `join` drains to the thread's clean exit — the
/// Ok return itself (a panicked worker joins Err, the panic test's
/// `unwrap_err`); the health cell's flip on ANY exit path is the
/// DeathGuard's, proven by the same panic test.
#[test]
fn graceful_stop_and_join_after_accepted_work() {
    let dir = TestDir::new("join");
    let worker = found_worker(&dir.store(), false);
    let source = worker.source().unwrap();
    let mut medium = source.open_medium().unwrap();
    let (founding, epoch) = founding_head(&found_location(&dir.store(), false));
    let a = append_revision(&mut medium, founding, epoch, 1);
    assert_eq!(medium.validate().unwrap().head, a);
    drop(medium);
    drop(source);
    assert!(worker.healthy());
    worker.join().unwrap();
}

/// A lost reply after a POSSIBLY EXECUTED append is never a false
/// acknowledgment and never a definite failure: the proxy surfaces
/// `Unknown { WorkerReplyLost }` while the store actually holds the
/// revision (the oracle proves it — reconciliation's job).
#[test]
fn no_false_acknowledgment_after_a_lost_append_reply() {
    let dir = TestDir::new("lost-reply");
    let worker = found_worker(&dir.store(), false);
    let mut medium = worker.source().unwrap().open_medium().unwrap();
    let (founding, epoch) = founding_head(&found_location(&dir.store(), false));
    worker.fault_control().set(WorkerFault::LoseAppendReply);
    let object = sequence_object(9);
    let record = RevisionRecord::regular(founding, epoch, 109)
        .with_idempotency_key(b"pp-b4-lost-reply-9".to_vec())
        .with_content_refs(vec![object.hash()]);
    let id = record.id();
    let outcome = medium
        .append(&record, std::slice::from_ref(&object), Some(founding))
        .unwrap();
    assert_eq!(
        outcome,
        AppendOutcome::Unknown {
            reason: UnknownReason::WorkerReplyLost
        }
    );
    worker.fault_control().clear();
    // The append ACTUALLY landed — the oracle (a direct provider read)
    // proves the uncertain class was honest.
    assert_eq!(founding_head(&found_location(&dir.store(), false)).0, id);
    // The reconciliation establishes Installed (the accepted machinery).
    assert!(matches!(
        medium.reconcile_unknown(&id).unwrap(),
        engine_persist::Reconciliation::Installed { .. }
    ));
    drop(medium);
    worker.join().unwrap();
}

// ------------------------------------------------- the real child-process arms

/// The real-child fixture: founds the store pre-worker, opens a
/// pipeline over the worker, journals one capture, then arms the
/// exit fault and calls `commit_canonical` — the process dies at the
/// armed point (exit 13: after the append committed, before its
/// reply; exit 14: before the append executes). No env → no-op (the
/// established `--exact` child pattern — this fn IS the test the
/// child selects).
#[test]
fn pp_b4_child_exit() {
    let Ok(store_path) = std::env::var("S7_PP_B4_CHILD_STORE") else {
        return;
    };
    let journal_path = std::env::var("S7_PP_B4_CHILD_JOURNAL").unwrap();
    let mode: u8 = std::env::var("S7_PP_B4_CHILD_FAULT")
        .unwrap()
        .parse()
        .unwrap();
    let location = StoreLocation::Directory(store_path.into());
    let mut store = SessionStore::open(&location).unwrap();
    if store.head().unwrap().is_none() {
        store
            .append(
                &RevisionRecord::founding(AuthorityMode::Standalone, "pp-b4-child", 0),
                &[],
                None,
            )
            .unwrap();
    }
    drop(store);
    let fault = engine_slice::worker::WorkerFaultControl::new();
    let worker =
        IoWorker::spawn_with_faults(&location, DEFAULT_QUEUE_CAPACITY, fault.clone()).unwrap();
    let journal = T1Journal::open(journal_path).unwrap();
    let medium = worker.source().unwrap().open_medium().unwrap();
    let mut pipeline = CommitPipeline::open(medium, journal).unwrap();
    pipeline
        .stage(
            ConsequenceEnvelope::capture(
                ConsequenceId::new(1, 1, 7),
                vec![engine_persist::ClassifiedSpan {
                    span: engine_persist::OccurrenceSpan::new(0, 3).unwrap(),
                    class: engine_persist::DurabilityClass::DurableEligible,
                }],
                engine_persist::SignalLane::Captured,
                engine_persist::CounterValues::default(),
                engine_persist::SourceBaseline {
                    counters: engine_persist::CounterValues::default(),
                    award: engine_persist::AwardCheckpoint {
                        durable: engine_persist::Coverage::new(),
                        provisional: engine_persist::Coverage::new(),
                    },
                },
                engine_persist::SourcePosition {
                    head: None,
                    tick: 400,
                },
            )
            .unwrap(),
        )
        .unwrap();
    pipeline
        .capture(
            engine_persist::SourcePosition {
                head: None,
                tick: 400,
            },
            engine_persist::DurabilityClass::DurableEligible,
            Vec::new(),
            vec![engine_persist::FrontierEntry {
                region: 1,
                episode: 1,
            }],
            Vec::new(),
        )
        .unwrap();
    fault.set(if mode == 13 {
        WorkerFault::ExitAfterAppendPreReply
    } else {
        WorkerFault::ExitBeforeNextJob
    });
    let _ = pipeline.commit_canonical();
}

fn run_child(dir: &TestDir, mode: u8) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "pp_b4_child_exit", "--nocapture"])
        .env("S7_PP_B4_CHILD_STORE", dir.store())
        .env("S7_PP_B4_CHILD_JOURNAL", dir.journal())
        .env("S7_PP_B4_CHILD_FAULT", mode.to_string())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(i32::from(mode)));
}

/// Real child death AFTER the append committed, BEFORE its reply
/// (exit 13): the revision IS durably installed, and the fresh
/// recovery confirms the key exactly-once (nothing left to commit).
#[test]
fn real_child_exit_13_after_commit_before_reply() {
    let dir = TestDir::new("child-13");
    run_child(&dir, 13);
    // The durable state: the founding PLUS the batch (the append
    // committed before the death).
    let mut oracle = SessionStore::open(&found_location(&dir.store(), false)).unwrap();
    assert_eq!(oracle.validate().unwrap().records, 2);
    // The fresh recovery (the child's process death released the
    // journal's sidecar lock): a new pipeline over the same durable
    // state confirms exactly-once — the key is already chain-confirmed.
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut pipeline = CommitPipeline::open(oracle, journal).unwrap();
    assert!(matches!(
        pipeline.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    ));
}

/// Real child death BEFORE the append executes (exit 14): the
/// revision is NOT installed, and the fresh recovery completes the
/// confirmation from the journal evidence — exactly one installation.
#[test]
fn real_child_exit_14_before_execution() {
    let dir = TestDir::new("child-14");
    run_child(&dir, 14);
    // The durable state: ONLY the founding (the append never ran).
    let mut oracle = SessionStore::open(&found_location(&dir.store(), false)).unwrap();
    assert_eq!(oracle.validate().unwrap().records, 1);
    // The fresh recovery: the journal's T1 evidence re-drives — the
    // batch installs NOW, exactly once.
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut pipeline = CommitPipeline::open(oracle, journal).unwrap();
    assert!(matches!(
        pipeline.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    let mut verify = SessionStore::open(&found_location(&dir.store(), false)).unwrap();
    assert_eq!(verify.validate().unwrap().records, 2);
}

// ------------------------------------------- the crossing integration + poison

/// The B2 fixture (the pp_b3 suite's shape): 150 ticks under the near
/// presence — region 4 live, five far regions dormant since tick 0.
fn scenario_pair(ticks: u64, presence: &PresenceSample) -> (Spine<NpcWorld>, RegionDirector) {
    let config = NpcConfig {
        region_lods_enabled: true,
        ..NpcConfig::default()
    };
    let mut spine = Spine::with_reservation(NpcWorld::new(config.clone()), 200, 60);
    install_initial_schedules(&mut spine, &config);
    let mut director = RegionDirector::new(RegionConfig::default()).unwrap();
    for _ in 0..ticks {
        director.apply(&mut spine, std::slice::from_ref(presence));
        spine.run_tick();
    }
    (spine, director)
}

fn near_region_4() -> PresenceSample {
    PresenceSample {
        position: (-100.0, 0.0),
        velocity: (0.0, 0.0),
    }
}

fn far_from_region_4() -> PresenceSample {
    PresenceSample {
        position: (100.0, 0.0),
        velocity: (0.0, 0.0),
    }
}

/// The durable boundary (the pp_b3 fixture shape): one presence-driven
/// deactivation through the crossing's six phases.
fn one_durable_boundary(
    crossing: &mut SessionCrossing<engine_slice::worker::WorkerSource>,
    spine: &mut Spine<NpcWorld>,
    director: &mut RegionDirector,
    presence: &PresenceSample,
) {
    let now = spine.clock();
    for region in 0..REGION_COUNT {
        let current = director.levels()[region];
        let next = director.target_level(region, std::slice::from_ref(presence));
        if next == LodLevel::Dormant && current != LodLevel::Dormant {
            crossing
                .deactivate(spine, director, region, now)
                .expect("the deactivation commits");
        }
    }
}

/// The checkpoint-1 seam composes over the worker end-to-end, and a
/// POISONED worker fails every authoritative crossing operation
/// closed — the journal-only deactivation path included — while the
/// fresh recovery over the legacy source remains coherent.
#[test]
fn the_crossing_composes_over_the_worker_source_and_poisons_closed() {
    let dir = TestDir::new("crossing");
    let near = near_region_4();
    let worker = found_worker(&dir.store(), false);
    let source = worker.source().unwrap();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open_at(&source, journal, "pp-b4-crossing").unwrap();
    // The full composition: a snapshot publication, a durable
    // deactivation (T1 evidence — the FAR presence drives region 4's
    // live-to-dormant boundary over the accrued crossings), and a
    // canonical confirmation — all through the worker's single
    // serialization point.
    crossing.publish_snapshot(&spine, &director).unwrap();
    let far = far_from_region_4();
    one_durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    assert!(matches!(
        crossing.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    let mut medium = source.open_medium().unwrap();
    assert!(medium.validate().unwrap().records >= 3);
    drop(medium);
    // POISON: every authoritative operation refuses closed — the
    // journal-only deactivation included (no silent continuation).
    worker.poison();
    let now = spine.clock();
    let refusal = crossing.deactivate(&mut spine, &mut director, 1, now);
    assert!(matches!(
        refusal,
        Err(TransitionError::InstallIncompatible { detail, .. }) if detail == "worker/unavailable"
    ));
    // The fresh recovery over the LEGACY source (the pre-worker
    // discipline) is coherent — the poison never touched the durable
    // state.
    drop(crossing);
    drop(source);
    let (_spine, _director, recovered) = SessionCrossing::recover_at(
        &found_location(&dir.store(), false),
        &dir.journal(),
        "pp-b4-crossing",
    )
    .unwrap();
    drop(recovered);
    worker.join().unwrap();
}

/// C2-F2 — explicit poisoning prevents new canonical submissions: an
/// EXISTING proxy handle refuses the typed admission error (never the
/// uncertain Unknown class — nothing was admitted), the canonical HEAD
/// is unchanged, a proxy acquired AFTER the poison is equally refused,
/// and the poisoned worker still drains and joins cleanly (no restart,
/// no reauthorization).
#[test]
fn explicit_poison_prevents_new_canonical_submissions() {
    let dir = TestDir::new("poison");
    let worker = found_worker(&dir.store(), false);
    let source = worker.source().unwrap();
    let mut medium = source.open_medium().unwrap();
    let (founding, epoch) = founding_head(&found_location(&dir.store(), false));
    let object = sequence_object(21);
    let record = RevisionRecord::regular(founding, epoch, 121)
        .with_idempotency_key(b"pp-b4-poison-21".to_vec())
        .with_content_refs(vec![object.hash()]);
    // POISON — the terminal admission contract.
    worker.poison();
    // The EXISTING proxy refuses: the definite admission error.
    let result = medium.append(&record, std::slice::from_ref(&object), Some(founding));
    assert!(matches!(result, Err(MediumError::WorkerUnavailable { .. })));
    // The canonical HEAD is unchanged.
    assert_eq!(
        founding_head(&found_location(&dir.store(), false)).0,
        founding
    );
    // A proxy acquired AFTER the poison is equally refused.
    let mut medium2 = source.open_medium().unwrap();
    assert!(matches!(
        medium2.head(),
        Err(MediumError::WorkerUnavailable { .. })
    ));
    // The poisoned worker still drains and joins cleanly (poisoned, not
    // dead — the crossing-level guard remains the additional protection).
    drop(medium);
    drop(medium2);
    drop(source);
    worker.join().unwrap();
}

/// C2-F2's queued/in-flight boundary: work admitted BEFORE the poison
/// but not yet started is refused at the execution boundary with the
/// DEFINITE typed error (the refusal is deterministic — the append
/// never ran; never falsely classified as the uncertain Unknown class),
/// and the worker continues to drain and join.
#[test]
fn work_admitted_before_poison_gets_an_honest_refusal() {
    let dir = TestDir::new("poison-boundary");
    let worker = found_worker(&dir.store(), false);
    let source = worker.source().unwrap();
    let mut medium = source.open_medium().unwrap();
    let (founding, epoch) = founding_head(&found_location(&dir.store(), false));
    // Park the worker so the append is admitted (in the queue or in the
    // loop's hands, pre-execution) but not started. The park-entry
    // barrier below proves the worker HOLDS the job before the poison
    // lands, so the boundary refusal fires deterministically when the
    // park releases.
    worker.fault_control().set(WorkerFault::ParkNext);
    let object = sequence_object(22);
    let record = RevisionRecord::regular(founding, epoch, 122)
        .with_idempotency_key(b"pp-b4-poison-22".to_vec())
        .with_content_refs(vec![object.hash()]);
    let append_call = std::thread::spawn(move || {
        medium.append(&record, std::slice::from_ref(&object), Some(founding))
    });
    // THE DETERMINISTIC PARK-ENTRY BARRIER (the final-validation
    // determinism audit): the worker takes the job and parks — the
    // real event, never a guessed stall duration — then poison and
    // release: the execution-boundary check refuses the admitted job.
    while !worker.fault_control().park_entered() {
        std::hint::spin_loop();
    }
    worker.poison();
    worker.fault_control().release_park();
    let result = append_call.join().unwrap();
    assert!(matches!(result, Err(MediumError::WorkerUnavailable { .. })));
    // The append never ran: the HEAD unchanged.
    assert_eq!(
        founding_head(&found_location(&dir.store(), false)).0,
        founding
    );
    drop(source);
    worker.join().unwrap();
}
