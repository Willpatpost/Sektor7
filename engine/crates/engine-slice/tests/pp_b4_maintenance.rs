//! PP-B4 checkpoint 4 — the protected maintenance and graceful-close
//! acceptance suite (the GO §8's matrix): the exclusive maintenance
//! lease (the FIFO barrier, the completion harvest, the
//! uncertain-disposition reconciliation), the SK/MK checkpoint under
//! the lease, the journal rotation (the capture-invalidation
//! generation with the HEAD unchanged, the retirement-evidence
//! distinction, the R5 repair), the reclamation restart, the graceful
//! durable close (the required final snapshot with the readback
//! verification, the honest failed-close classes, the SQLite WAL
//! TRUNCATE), the phase exclusivity observed through the REAL paths
//! (the probe seams), the deterministic worker faults, and the REAL
//! child-process termination evidence at the append, checkpoint,
//! rotation, and reclamation boundaries.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;

use engine_core::{EventClass, EventSpec, LodLevel, PresenceSample, Schedule, Spine, Tick};
use engine_persist::{
    AuthorityMode, AwardCheckpoint, CancellationReason, CanonicalMedium, ClassifiedSpan,
    CommitOutcome, ConsequenceEnvelope, ConsequenceId, CounterValues, Coverage, DurabilityClass,
    FrontierEntry, OccurrenceSpan, POINTER_DIR_SYNC_FAULT, RevisionRecord, SignalLane,
    SourceBaseline, SourcePosition, SyncTier, T1Journal, TransactionInput, active_journal_path,
};
use engine_slice::crossing::{ARM_AFTER_SK_EXIT, ARM_RECLAIM_AFTER_MEDIUM, TransitionError};
use engine_slice::npc::{NpcConfig, NpcWorld, install_initial_schedules};
use engine_slice::orchestration::{
    CloseFailure, CompletionDisposition, MaintenanceError, OrchestratedSession, PublicationOutcome,
    ScheduleOpError, SessionPhase, SubmitRefusal,
};
use engine_slice::region::{RegionConfig, RegionDirector};
use engine_slice::store::{SessionStore, StoreLocation};
use engine_slice::worker::{JobKind, WorkerFault};

// ------------------------------------------------------------- fixtures

/// A temporary campaign directory (the established suite pattern).
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-pp-b4-maintenance-{name}-{}",
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

    fn db(&self) -> PathBuf {
        self.0.join("campaign.db")
    }

    fn wal(&self) -> PathBuf {
        self.0.join("campaign.db-wal")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The campaign location, FOUNDED pre-worker (the discipline).
fn found_location(dir: &TestDir, database: bool) -> StoreLocation {
    let location = if database {
        StoreLocation::Database(dir.db())
    } else {
        StoreLocation::Directory(dir.store())
    };
    let mut store = SessionStore::open(&location).unwrap();
    if store.head().unwrap().is_none() {
        store
            .append(
                &RevisionRecord::founding(AuthorityMode::Standalone, "pp-b4-m", 0),
                &[],
                None,
            )
            .unwrap();
    }
    location
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

fn far_presence() -> [PresenceSample; 1] {
    [far_from_region_4()]
}

fn near_presence() -> [PresenceSample; 1] {
    [near_region_4()]
}

/// The B2 fixture: 150 ticks under the near presence — region 4 live
/// and accruing, the far regions dormant since tick 0.
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

fn orchestrated_at(dir: &TestDir, database: bool) -> OrchestratedSession {
    let location = found_location(dir, database);
    let (spine, director) = scenario_pair(150, &near_region_4());
    let journal = T1Journal::open(dir.journal()).unwrap();
    OrchestratedSession::open(spine, director, &location, journal, "pp-b4-m").unwrap()
}

fn orchestrated(dir: &TestDir) -> OrchestratedSession {
    orchestrated_at(dir, false)
}

fn oracle_head_at(location: &StoreLocation) -> engine_persist::Hash {
    SessionStore::open(location)
        .unwrap()
        .head()
        .unwrap()
        .unwrap()
}

fn oracle_records_at(location: &StoreLocation) -> u64 {
    SessionStore::open(location)
        .unwrap()
        .validate()
        .unwrap()
        .records
}

/// The child-side session over the parent-provided directory (the
/// real-child pattern: the env names the root, the child founds and
/// runs in its own process).
fn child_orchestrated(root: &str) -> OrchestratedSession {
    let location = StoreLocation::Directory(PathBuf::from(root).join("store"));
    let mut store = SessionStore::open(&location).unwrap();
    if store.head().unwrap().is_none() {
        store
            .append(
                &RevisionRecord::founding(AuthorityMode::Standalone, "pp-b4-m", 0),
                &[],
                None,
            )
            .unwrap();
    }
    drop(store);
    let (spine, director) = scenario_pair(150, &near_region_4());
    let journal = T1Journal::open(PathBuf::from(root).join("journal.t1")).unwrap();
    OrchestratedSession::open(spine, director, &location, journal, "pp-b4-m").unwrap()
}

/// The checkpoint-able prelude (the C3-F2-proven shape): the T1
/// deactivation + the confirmation + the reactivation (the captures
/// apply) + the accrual + the range publication S1 (the checkpoint's
/// range and the rotation's anchor).
fn checkpoint_prelude(session: &mut OrchestratedSession) {
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    session.tick(&near_presence()).unwrap();
    for _ in 0..100 {
        session.tick(&near_presence()).unwrap();
    }
    let handle = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Accepted { .. }
    ));
}

/// One foreign pre-seeded transaction (a far-region identity, an
/// empty plan — the canceled-retirement class's construction), canceled
/// before the session opens (the pp_b3 rotation fixture's proven
/// shape).
fn foreign_input() -> TransactionInput {
    let identity = ConsequenceId::new(5, 99, 1);
    let envelope = ConsequenceEnvelope::capture(
        identity,
        vec![ClassifiedSpan {
            span: OccurrenceSpan::new(0, 2).unwrap(),
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
        SourcePosition {
            head: None,
            tick: 400,
        },
    )
    .unwrap();
    TransactionInput {
        envelopes: vec![envelope],
        position: SourcePosition {
            head: None,
            tick: 400,
        },
        classification: DurabilityClass::DurableEligible,
        dependencies: Vec::new(),
        frontier: vec![FrontierEntry {
            region: 5,
            episode: 99,
        }],
        plan: Vec::new(),
    }
}

/// The bounded real-child runner (the established pattern): the child
/// runs the SAME test binary with `--exact` and the env naming its
/// root; the parent polls against a deadline, kills and fails on the
/// bound.
fn run_child(test_name: &str, env_key: &str, dir: &TestDir) -> std::process::ExitStatus {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([test_name, "--exact", "--nocapture"])
        .env(env_key, dir.0.display().to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        match child.try_wait().unwrap() {
            Some(status) => return status,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("the child {test_name} did not terminate within the bound");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
}

// ------------------------------------------------------- the lease matrix

/// A+B — the maintenance barrier settles every admitted optional job
/// FIFO and the ledger harvest is COMPLETE: the consumed outcome (its
/// caller already polled it) still participates in the maintenance
/// accounting, and the unpolled publications are harvested with their
/// honest dispositions before the compound runs.
#[test]
fn maintenance_settles_admitted_work_and_harvests_every_completion() {
    let dir = TestDir::new("barrier-settle");
    found_location(&dir, false);
    let mut session = orchestrated(&dir);
    // The anchor publication (consumed — its outcome polled).
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    let consumed = {
        let handle = session
            .submit_publication(session.capture_snapshot().unwrap())
            .unwrap();
        match session.wait_publication(handle).unwrap() {
            PublicationOutcome::Accepted { .. } => handle,
            other => panic!("{other:?}"),
        }
    };
    // The state advances (a fresh content identity — the anchor
    // publication's key must not collide with the drain pair's).
    session.tick(&near_presence()).unwrap();
    // Two unpolled publications (BOTH captures taken before either
    // submission — a later capture's observe_head would queue behind
    // the first append and read the ADVANCED head, producing a
    // different record with the SAME content key, the duplicate-key
    // rejection): two byte-identical records — the first accepts,
    // the second the stale CAS rejection.
    let capture_a = session.capture_snapshot().unwrap();
    let capture_b = session.capture_snapshot().unwrap();
    let a = session.submit_publication(capture_a).unwrap();
    let b = session.submit_publication(capture_b).unwrap();
    assert_eq!(session.pending_publications(), 2);
    // THE MAINTENANCE COMPOUND: the barrier settles both unpolled jobs
    // before the rotation; the ledger is complete.
    session
        .rotate_journal()
        .expect("the rotation drains and runs");
    assert_eq!(
        session.pending_publications(),
        0,
        "the drain harvested both"
    );
    assert!(matches!(
        session.completion_record(a).unwrap().disposition,
        CompletionDisposition::Accepted { .. }
    ));
    assert!(matches!(
        session.completion_record(b).unwrap().disposition,
        CompletionDisposition::Stale { .. }
    ));
    // B — the CONSUMED outcome still participates: the retained
    // ledger carries it with its accepted disposition.
    assert!(matches!(
        session.completion_record(consumed).unwrap().disposition,
        CompletionDisposition::Accepted { .. }
    ));
    // The compound actually ran after the drain (the pointer flipped).
    assert_ne!(active_journal_path(&dir.journal()).unwrap(), dir.journal());
    assert_eq!(session.phase(), SessionPhase::Active);
}

/// C — the drain RECONCILES the uncertain dispositions against the
/// canonical chain, never a guessed confirmation: a lost-reply append
/// that EXECUTED AND LANDED reconciles to the VERIFIED installation;
/// a lost-reply append that executed against a STALE captured parent
/// (the CAS rejected it — nothing installed) reconciles to the
/// VERIFIED absence.
#[test]
fn the_drain_reconciles_unknown_dispositions_without_guessed_confirmation() {
    let dir = TestDir::new("drain-reconcile");
    found_location(&dir, false);
    let mut session = orchestrated(&dir);
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    // INSTALLED: the lost-reply append executes and lands at the
    // current head; the reply is lost — the honest Unknown.
    session.fault_control().set(WorkerFault::LoseAppendReply);
    let installed = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    assert!(matches!(
        session.wait_publication(installed).unwrap(),
        PublicationOutcome::Unknown { .. }
    ));
    // NOT INSTALLED: a capture at the current head whose append will
    // be CAS-rejected (the head advances past it first).
    let stale_candidate = session.capture_snapshot().unwrap();
    // The state advances (a fresh content identity) and the head
    // advances with a normal publication.
    session.tick(&near_presence()).unwrap();
    let anchor = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    assert!(matches!(
        session.wait_publication(anchor).unwrap(),
        PublicationOutcome::Accepted { .. }
    ));
    session.fault_control().set(WorkerFault::LoseAppendReply);
    let absent = session.submit_publication(stale_candidate).unwrap();
    assert!(matches!(
        session.wait_publication(absent).unwrap(),
        PublicationOutcome::Unknown { .. }
    ));
    // THE MAINTENANCE DRAIN: both uncertain dispositions reconcile.
    session
        .rotate_journal()
        .expect("the rotation drains and reconciles");
    let installed_record = session.completion_record(installed).unwrap();
    match &installed_record.disposition {
        CompletionDisposition::Reconciled {
            installed: true, ..
        } => {}
        other => panic!("the landed append reconciles installed: {other:?}"),
    }
    let absent_record = session.completion_record(absent).unwrap();
    match &absent_record.disposition {
        CompletionDisposition::Reconciled {
            installed: false, ..
        } => {}
        other => panic!("the CAS-rejected append reconciles absent: {other:?}"),
    }
    assert_eq!(session.phase(), SessionPhase::Active);
}

/// D — the checkpoint lease: through the REAL gate (the barrier, the
/// harvest, the reconciliation, all under the Maintenance phase) the
/// interleaving attempts refuse — the tick, the scheduling, and the
/// optional admission — and the ledger is drained; the real compound
/// then publishes SK and MK with the ratified adjacency (MK's parent
/// IS SK; the head IS MK) and no optional interleaving between them.
#[test]
fn the_checkpoint_lease_holds_adjacency_and_refuses_interleaving() {
    let dir = TestDir::new("lease-adjacency");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    checkpoint_prelude(&mut session);
    // THE PROBE: through the real lease.
    session
        .debug_maintenance_probe_for_test(|lease| {
            assert_eq!(lease.phase(), SessionPhase::Maintenance);
            assert_eq!(lease.pending_publications(), 0, "the ledger drained");
            assert!(matches!(
                lease.tick(&far_presence()),
                Err(TransitionError::PhaseRefused {
                    detail: "maintenance-active"
                })
            ));
            assert!(matches!(
                lease.schedule_event(EventSpec::new(
                    EventClass::new(4),
                    Schedule::AtLogicalTime { due: Tick(1000) },
                    1,
                )),
                Err(ScheduleOpError::Gate(TransitionError::PhaseRefused {
                    detail: "maintenance-active"
                }))
            ));
            assert_eq!(
                lease
                    .submit_publication(lease.capture_snapshot().unwrap())
                    .unwrap_err(),
                SubmitRefusal::Suspended
            );
        })
        .expect("the probe lease drains cleanly");
    assert_eq!(session.phase(), SessionPhase::Active);
    // THE REAL COMPOUND: the SK/MK adjacency on the chain.
    let report = session.publish_checkpoint(10).expect("the checkpoint");
    let mut store = SessionStore::open(&location).unwrap();
    let mk_record = store.read_record(&report.mk).unwrap();
    let head = store.head().unwrap().unwrap();
    drop(store);
    assert_eq!(mk_record.parent, Some(report.sk), "MK's parent is SK");
    assert_eq!(head, report.mk, "the head is MK — nothing interleaved");
}

/// E (+P's checkpoint boundary) — the REAL-CHILD death after SK but
/// before MK: the child arms the accepted crash seam and dies at it;
/// the parent verifies the crashed SK stands as an ORDINARY snapshot
/// (the recovery works from it) and the retry checkpoint completes.
#[test]
fn a_real_child_checkpoint_death_after_sk_recovers() {
    if let Ok(root) = std::env::var("S7_PP_B4_CKPT_CHILD") {
        let mut session = child_orchestrated(&root);
        checkpoint_prelude(&mut session);
        ARM_AFTER_SK_EXIT.store(true, Ordering::SeqCst);
        let _ = session.publish_checkpoint(10);
        panic!("the child must die at the after-SK seam");
    }
    let dir = TestDir::new("ckpt-child");
    let location = found_location(&dir, false);
    let status = run_child(
        "a_real_child_checkpoint_death_after_sk_recovers",
        "S7_PP_B4_CKPT_CHILD",
        &dir,
    );
    assert_eq!(
        status.code(),
        Some(8),
        "the child died at the after-SK seam"
    );
    // THE RECOVERY: the crashed SK stands as an ordinary snapshot —
    // the orchestrated recovery re-derives from it coherently.
    let mut recovered = OrchestratedSession::recover(&location, &dir.journal(), "pp-b4-m").unwrap();
    // The spine's ledger must grow past the crashed SK's retained
    // window before the retry's compaction has material (the b3
    // crash-arc pattern: ticks first, then the fresh checkpoint).
    for _ in 0..20 {
        recovered.tick(&near_presence()).unwrap();
    }
    // THE RETRY: a fresh checkpoint completes over the crashed SK
    // (an ordinary indexed publication in the new range).
    let report = recovered
        .publish_checkpoint(10)
        .expect("the retry completes");
    assert_ne!(report.mk, report.sk);
    assert_eq!(oracle_head_at(&location), report.mk);
}

/// F — the capture-invalidation generation with the canonical HEAD
/// UNCHANGED: a capture taken before the rotation is REJECTED stale
/// by the generation check even though the CAS would pass (the head
/// never moved — the distinguishing interleaving case), and a fresh
/// capture at the SAME unchanged head publishes successfully.
#[test]
fn rotation_without_head_advancement_rejects_earlier_generation_captures() {
    let dir = TestDir::new("generation-reject");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    let anchor = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    assert!(matches!(
        session.wait_publication(anchor).unwrap(),
        PublicationOutcome::Accepted { .. }
    ));
    // The pre-rotation capture (generation 0).
    let pre_rotation = session.capture_snapshot().unwrap();
    let head_before = oracle_head_at(&location);
    // THE ROTATION (the maintenance lease): the flip increments the
    // capture-validity generation; the canonical HEAD is unchanged
    // (the rotation appends nothing canonical).
    session.rotate_journal().expect("the rotation completes");
    assert_eq!(
        oracle_head_at(&location),
        head_before,
        "the rotation advanced no canonical record"
    );
    // THE OLD-GENERATION CAPTURE: rejected by the generation check —
    // the CAS alone could NOT catch this (the parent still matches).
    assert_eq!(
        session.submit_publication(pre_rotation).unwrap_err(),
        SubmitRefusal::Stale
    );
    // THE FRESH-GENERATION CAPTURE: the state advances (a fresh
    // content identity) and the capture at the unchanged head
    // publishes successfully.
    session.tick(&near_presence()).unwrap();
    let handle = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Accepted { .. }
    ));
}

/// G (+P's rotation boundary) — the REAL-CHILD rotation with the
/// injected post-flip durability failure: the rotation fails closed
/// (the R5 unresolved state), the flip count ALREADY advanced (the
/// pre-flip capture permanently rejected even with the head
/// unchanged), the R5 repair restores the authority, and the fresh
/// capture publishes — all in the child; the parent verifies the
/// durable state and the coherent recovery.
#[test]
fn a_real_child_rotation_durability_failure_and_repair() {
    if let Ok(root) = std::env::var("S7_PP_B4_ROT_CHILD") {
        let location = StoreLocation::Directory(PathBuf::from(&root).join("store"));
        let mut session = child_orchestrated(&root);
        session.tick(&far_presence()).unwrap();
        assert!(matches!(
            session.commit_canonical().unwrap(),
            CommitOutcome::Confirmed(_)
        ));
        let anchor = session
            .submit_publication(session.capture_snapshot().unwrap())
            .unwrap();
        assert!(matches!(
            session.wait_publication(anchor).unwrap(),
            PublicationOutcome::Accepted { .. }
        ));
        let pre_flip = session.capture_snapshot().unwrap();
        let head_before = SessionStore::open(&location)
            .unwrap()
            .head()
            .unwrap()
            .unwrap();
        // THE INJECTED POST-FLIP DURABILITY FAILURE.
        POINTER_DIR_SYNC_FAULT.store(true, Ordering::SeqCst);
        match session.rotate_journal() {
            Err(MaintenanceError::Compound(TransitionError::InstallIncompatible {
                detail: "rotation/pointer-durability",
                ..
            })) => {}
            other => panic!("the unresolved durability refuses the rotation: {other:?}"),
        }
        // THE FLIP COUNT ALREADY ADVANCED: the pre-flip capture is
        // permanently rejected — the head is unchanged (the CAS would
        // pass; the generation check must not).
        assert_eq!(
            SessionStore::open(&location)
                .unwrap()
                .head()
                .unwrap()
                .unwrap(),
            head_before
        );
        assert_eq!(
            session.submit_publication(pre_flip).unwrap_err(),
            SubmitRefusal::Stale
        );
        // THE R5 REPAIR: the fault disarmed, the durability
        // re-established, the authority healthy.
        POINTER_DIR_SYNC_FAULT.store(false, Ordering::SeqCst);
        session
            .reestablish_pointer_durability()
            .expect("the repair");
        // THE FRESH CAPTURE publishes (the state advances first — a
        // fresh content identity, never the anchor's key).
        session.tick(&near_presence()).unwrap();
        let handle = session
            .submit_publication(session.capture_snapshot().unwrap())
            .unwrap();
        assert!(matches!(
            session.wait_publication(handle).unwrap(),
            PublicationOutcome::Accepted { .. }
        ));
        eprintln!("the child repaired the rotation and published");
        return;
    }
    let dir = TestDir::new("rot-child");
    let location = found_location(&dir, false);
    let status = run_child(
        "a_real_child_rotation_durability_failure_and_repair",
        "S7_PP_B4_ROT_CHILD",
        &dir,
    );
    assert!(
        status.success(),
        "the child repaired and exited cleanly: {status}"
    );
    // THE DURABLE STATE: the pointer names the rotated generation; a
    // fresh recovery is coherent.
    assert_ne!(active_journal_path(&dir.journal()).unwrap(), dir.journal());
    let mut recovered = OrchestratedSession::recover(&location, &dir.journal(), "pp-b4-m").unwrap();
    recovered.tick(&near_presence()).unwrap();
    assert_ne!(recovered.region_level(4), LodLevel::Dormant);
}

/// H — the rotation's retirement evidence distinguishes the DURABLY
/// CANCELED transactions from the CONFIRMED-AND-SUBSUMED ones: the
/// pre-seeded canceled foreign transaction retires in the canceled
/// class, the confirmed-and-materialized-below-the-checkpoint-anchor
/// T1 retires in the subsumed class, and the outstanding unconfirmed
/// T2 survives — three distinct lists, the flat retired set their
/// union.
#[test]
fn the_rotation_retirement_evidence_distinguishes_canceled_from_subsumed() {
    let dir = TestDir::new("retirement-evidence");
    found_location(&dir, false);
    // The pre-seeded canceled foreign transaction (the canceled class).
    let foreign_id = {
        let mut journal = T1Journal::open(dir.journal()).unwrap();
        let receipt = journal.append_transaction(&foreign_input()).unwrap();
        journal
            .append_cancellation(
                receipt.id,
                CancellationReason::ApplicationPolicy,
                b"superseded",
            )
            .unwrap();
        receipt.id
    };
    let mut session = orchestrated(&dir);
    // T1 + the compaction-eligible reactivation + S1 + THE CHECKPOINT
    // ANCHOR (the compacted boundary that subsumes T1).
    checkpoint_prelude(&mut session);
    session
        .publish_checkpoint(10)
        .expect("the anchor checkpoint");
    // T2: the outstanding unconfirmed transaction (the survivor).
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    session.tick(&near_presence()).unwrap();
    for _ in 0..100 {
        session.tick(&near_presence()).unwrap();
    }
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    let t1 = scan.committed[1].id;
    let t2 = scan
        .committed
        .last()
        .map(|transaction| transaction.id)
        .expect("the second boundary journals");
    assert_ne!(t2, foreign_id);
    assert_ne!(t2, t1);
    // THE ROTATION: the three distinct lists.
    let rotation = session.rotate_journal().expect("the rotation");
    assert_eq!(rotation.retired_canceled, vec![foreign_id]);
    assert_eq!(rotation.retired_subsumed, vec![t1]);
    assert_eq!(rotation.retained, vec![t2]);
    assert_eq!(rotation.retired.len(), 2);
    assert!(rotation.retired.contains(&foreign_id));
    assert!(rotation.retired.contains(&t1));
}

/// I (+P's reclamation boundary) — the REAL-CHILD reclamation death
/// after the medium deletions but before the journal retirement: the
/// parent's recovery re-derives coherently and the reclamation
/// RESUMES (the installed audit recognized, the journal retirement
/// completes), then the completed campaign refuses nothing-eligible.
#[test]
fn a_real_child_reclamation_death_resumes() {
    if let Ok(root) = std::env::var("S7_PP_B4_RECLAIM_CHILD") {
        let mut session = child_orchestrated(&root);
        // THE TWO-CHECKPOINT CAMPAIGN (the reclamation's eligibility:
        // more checkpoints than the retention window).
        checkpoint_prelude(&mut session);
        session
            .publish_checkpoint(10)
            .expect("the first checkpoint");
        session.tick(&far_presence()).unwrap();
        assert!(matches!(
            session.commit_canonical().unwrap(),
            CommitOutcome::Confirmed(_)
        ));
        session.tick(&near_presence()).unwrap();
        for _ in 0..100 {
            session.tick(&near_presence()).unwrap();
        }
        let s2 = session
            .submit_publication(session.capture_snapshot().unwrap())
            .unwrap();
        assert!(matches!(
            session.wait_publication(s2).unwrap(),
            PublicationOutcome::Accepted { .. }
        ));
        session
            .publish_checkpoint(10)
            .expect("the second checkpoint");
        // THE ROTATION (the journal generations the reclamation
        // retires — the pending work the child's death leaves).
        session.rotate_journal().expect("the rotation");
        // THE DEATH: after the medium deletions, before the journal
        // retirement.
        ARM_RECLAIM_AFTER_MEDIUM.store(true, Ordering::SeqCst);
        let _ = session.reclaim_checkpoint(1);
        panic!("the child must die at the after-medium seam");
    }
    let dir = TestDir::new("reclaim-child");
    let location = found_location(&dir, false);
    let status = run_child(
        "a_real_child_reclamation_death_resumes",
        "S7_PP_B4_RECLAIM_CHILD",
        &dir,
    );
    assert_eq!(
        status.code(),
        Some(12),
        "the child died at the after-medium seam"
    );
    // THE RECOVERY + THE RESUME: the audit is installed; the resume
    // removes zero medium bytes and completes the journal retirement.
    let mut recovered = OrchestratedSession::recover(&location, &dir.journal(), "pp-b4-m").unwrap();
    let report = recovered
        .reclaim_checkpoint(1)
        .expect("the resume completes");
    assert_eq!(
        report.removed_objects, 0,
        "the audit recognized — no medium bytes re-deleted"
    );
    // THE COMPLETED CAMPAIGN: the nothing-eligible refusal.
    match recovered.reclaim_checkpoint(1) {
        Err(MaintenanceError::Compound(TransitionError::InstallIncompatible {
            detail: "reclaim/nothing-eligible",
            ..
        })) => {}
        other => panic!("the completed campaign refuses nothing-eligible: {other:?}"),
    }
}

// ------------------------------------------------------- the close matrix

/// J — the close with earlier optional publications: the final
/// capture represents the NEWEST authoritative state (the post-tick
/// content identity), never the earlier queued snapshot's, and the
/// earlier publication's completion is retained in the final ledger.
#[test]
fn the_close_publishes_the_newest_state_not_an_earlier_snapshot() {
    let dir = TestDir::new("close-newest");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    // The earlier optional publication (the OLD state's content).
    let old = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    assert!(matches!(
        session.wait_publication(old).unwrap(),
        PublicationOutcome::Accepted { .. }
    ));
    // The state advances.
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    let newest_content = session.capture_snapshot().unwrap().content_identity();
    // THE CLOSE.
    let report = session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    let final_revision = report.final_revision.expect("the verified final revision");
    let old_record = report
        .completions
        .iter()
        .find(|record| record.id == old.id)
        .expect("the earlier publication retained");
    assert_ne!(final_revision, old_record.revision);
    // The final record's content IS the newest state's identity.
    let mut store = SessionStore::open(&location).unwrap();
    let record = store.read_record(&final_revision).unwrap();
    drop(store);
    assert_eq!(record.content_refs, vec![newest_content]);
    assert!(matches!(
        old_record.disposition,
        CompletionDisposition::Accepted { .. }
    ));
    assert_eq!(oracle_head_at(&location), final_revision);
}

/// K — the close after a stale optional publication: the required
/// final snapshot is the FRESH capture at the advanced head (never
/// the stale one rebased), verified by readback, with the provider's
/// truthful tier.
#[test]
fn the_close_after_a_stale_optional_publishes_the_fresh_capture() {
    let dir = TestDir::new("close-stale");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    let stale_capture = session.capture_snapshot().unwrap();
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    let handle = session.submit_publication(stale_capture).unwrap();
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Stale { .. }
    ));
    // THE CLOSE: the fresh capture publishes.
    let report = session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    let final_revision = report.final_revision.expect("the verified final revision");
    assert_eq!(oracle_head_at(&location), final_revision);
    assert_eq!(
        report.final_tier,
        Some(SessionStore::open(&location).unwrap().durability_tier())
    );
    // The stale candidate never landed.
    assert!(matches!(
        report
            .completions
            .iter()
            .find(|record| record.id == handle.id)
            .unwrap()
            .disposition,
        CompletionDisposition::Stale { .. }
    ));
}

/// L (part 1) — the close after an Unknown publication: the drain
/// reconciles the uncertain disposition through the verified
/// canonical evidence (the append executed and landed — Installed),
/// and the close completes with the verified final revision.
#[test]
fn the_close_reconciles_an_unknown_publication() {
    let dir = TestDir::new("close-unknown");
    found_location(&dir, false);
    let mut session = orchestrated(&dir);
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    session.fault_control().set(WorkerFault::LoseAppendReply);
    let handle = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Unknown { .. }
    ));
    // The state advances (a fresh content identity — the landed
    // uncertain publication's key must not collide with the final
    // snapshot's).
    session.tick(&near_presence()).unwrap();
    // THE CLOSE: the drain reconciles, the final snapshot publishes.
    let report = session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert!(report.final_revision.is_some());
    match &report
        .completions
        .iter()
        .find(|record| record.id == handle.id)
        .unwrap()
        .disposition
    {
        CompletionDisposition::Reconciled {
            installed: true, ..
        } => {}
        other => panic!("the uncertain disposition reconciled installed: {other:?}"),
    }
}

/// L (part 2) + O's close half — the explicit FAILED close, never a
/// claimed save: the poisoned worker (the gate fails, the thread
/// still drains and joins cleanly) and the panicked worker (the
/// barrier dies, the join carries the panic).
#[test]
fn the_close_fails_explicitly_on_an_unavailable_or_panicked_worker() {
    // (a) THE POISONED WORKER.
    let dir = TestDir::new("close-unavailable");
    found_location(&dir, false);
    let session = orchestrated(&dir);
    session.poison();
    let report = session.close();
    assert!(matches!(
        report.failure,
        Some(CloseFailure::WorkerUnavailable)
    ));
    assert_eq!(report.final_revision, None, "never a claimed save");
    assert!(report.worker.is_ok(), "the poisoned worker still drains");
    // (b) THE PANICKED WORKER (the barrier dies in flight).
    let dir = TestDir::new("close-panic");
    found_location(&dir, false);
    let session = orchestrated(&dir);
    session.fault_control().set(WorkerFault::PanicNext);
    let report = session.close();
    assert!(matches!(
        report.failure,
        Some(CloseFailure::WorkerUnavailable)
    ));
    assert_eq!(report.final_revision, None, "never a claimed save");
    assert!(report.worker.is_err(), "the panic surfaces in the join");
}

/// M — the SQLite close: the verified final publication followed by
/// the provider lifecycle checkpoint (the ratified WAL TRUNCATE —
/// the -wal sidecar empty or gone after the close) with the
/// file-provider parity (the same close, the no-op checkpoint).
#[test]
fn the_sqlite_close_truncates_the_wal_with_file_parity() {
    let dir = TestDir::new("close-sqlite");
    let location = found_location(&dir, true);
    let mut session = orchestrated_at(&dir, true);
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    let report = session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    let final_revision = report.final_revision.expect("the verified final revision");
    assert_eq!(report.final_tier, Some(SyncTier::TransactionalWalFull));
    assert_eq!(oracle_head_at(&location), final_revision);
    // The WAL TRUNCATE: the sidecar is empty (or removed by the clean
    // connection close that follows it).
    let wal_len = std::fs::metadata(dir.wal())
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    assert_eq!(wal_len, 0, "the ratified TRUNCATE emptied the WAL");
    // THE FILE-PROVIDER PARITY: the same close over the file medium
    // (the lifecycle checkpoint's no-op — its publications already
    // fsync through the nine-step contract).
    let file_dir = TestDir::new("close-file-parity");
    let file_location = found_location(&file_dir, false);
    let mut file_session = orchestrated(&file_dir);
    file_session.tick(&far_presence()).unwrap();
    assert!(matches!(
        file_session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    let file_report = file_session.close();
    assert!(file_report.failure.is_none(), "{:?}", file_report.failure);
    assert!(file_report.final_revision.is_some());
    assert_eq!(
        file_report.final_tier,
        Some(
            SessionStore::open(&file_location)
                .unwrap()
                .durability_tier()
        )
    );
}

/// N — the Closing state refuses further authoritative gameplay and
/// optional admission, observed through the REAL close (the probe
/// runs after the drain, during the Closing phase).
#[test]
fn the_closing_phase_refuses_gameplay_and_admission() {
    let dir = TestDir::new("closing-refuses");
    found_location(&dir, false);
    let mut session = orchestrated(&dir);
    session.tick(&far_presence()).unwrap();
    let report = session.debug_close_with_probe_for_test(|closing| {
        assert_eq!(closing.phase(), SessionPhase::Closing);
        assert!(matches!(
            closing.tick(&far_presence()),
            Err(TransitionError::PhaseRefused { detail: "closing" })
        ));
        assert!(matches!(
            closing.schedule_event(EventSpec::new(
                EventClass::new(4),
                Schedule::AtLogicalTime { due: Tick(1000) },
                1,
            )),
            Err(ScheduleOpError::Gate(TransitionError::PhaseRefused {
                detail: "closing"
            }))
        ));
        assert_eq!(
            closing
                .submit_publication(closing.capture_snapshot().unwrap())
                .unwrap_err(),
            SubmitRefusal::Suspended
        );
    });
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert!(report.final_revision.is_some());
}

/// O (the maintenance half) — the worker panic DURING a compound
/// fails explicitly: the barrier passes (not the next job), the
/// compound's post-SK work dies with the worker, the maintenance
/// returns the typed compound failure, the lease RELEASES (the phase
/// returns Active), and nothing is falsely reported successful.
#[test]
fn a_worker_panic_during_a_compound_fails_explicitly() {
    let dir = TestDir::new("compound-panic");
    found_location(&dir, false);
    let mut session = orchestrated(&dir);
    checkpoint_prelude(&mut session);
    // The panic lands AFTER the SK append executes normally — inside
    // the compound (the range-walk reads that follow SK).
    session
        .fault_control()
        .set(WorkerFault::PanicAfter(JobKind::Append));
    match session.publish_checkpoint(10) {
        Err(MaintenanceError::Compound(_)) => {}
        other => panic!("the compound fails explicitly: {other:?}"),
    }
    assert!(!session.healthy(), "the worker died in the compound");
    assert_eq!(
        session.phase(),
        SessionPhase::Active,
        "the lease released on the failure"
    );
}

/// P (the append boundary) — the REAL-CHILD death at the close's
/// final append: the child arms the worker's after-append exit; the
/// final publication's append EXECUTES and the process dies before
/// the reply; the parent recovers coherently (the chain valid either
/// way) and a fresh close completes with a verified final revision.
#[test]
fn a_real_child_close_death_at_the_final_append() {
    if let Ok(root) = std::env::var("S7_PP_B4_CLOSE_CHILD") {
        let mut session = child_orchestrated(&root);
        session.tick(&far_presence()).unwrap();
        assert!(matches!(
            session.commit_canonical().unwrap(),
            CommitOutcome::Confirmed(_)
        ));
        session
            .fault_control()
            .set(WorkerFault::ExitAfterAppendPreReply);
        let _report = session.close();
        panic!("the child must die at the final append");
    }
    let dir = TestDir::new("close-child");
    let location = found_location(&dir, false);
    let status = run_child(
        "a_real_child_close_death_at_the_final_append",
        "S7_PP_B4_CLOSE_CHILD",
        &dir,
    );
    assert_eq!(
        status.code(),
        Some(13),
        "the child died at the after-append seam"
    );
    // THE RECOVERY: the chain is valid whether the final append
    // committed or not (the B4 semantics) — the recovery re-derives.
    let mut recovered = OrchestratedSession::recover(&location, &dir.journal(), "pp-b4-m").unwrap();
    // The state advances (a fresh content identity — the child's
    // final publication, if it landed, already holds the crashed
    // state's key).
    recovered.tick(&near_presence()).unwrap();
    // A FRESH CLOSE completes with the verified final revision.
    let report = recovered.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    let final_revision = report.final_revision.expect("the verified final revision");
    assert_eq!(oracle_head_at(&location), final_revision);
}

// --------------------------------------------- the C4 corrective regressions

// A (C4-F1) — the close is IDEMPOTENT after an already-published
// identical snapshot: an optional snapshot accepted, no gameplay
// state or clock change, then close — the final-save obligation is
// satisfied by the EXISTING verified representation (never a
// duplicate-key failure for a state already durably represented,
// never rebased bytes), and a fresh recovery is coherent. BOTH
// PROVIDERS.
#[test]
fn the_close_is_idempotent_after_an_identical_published_snapshot() {
    // THE FILE PROVIDER.
    let dir = TestDir::new("idempotent-close-file");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    let records_before = oracle_records_at(&location);
    // The optional snapshot S accepted (the current state).
    let s = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    let s_revision = match session.wait_publication(s).unwrap() {
        PublicationOutcome::Accepted { revision, .. } => revision,
        other => panic!("{other:?}"),
    };
    // NO gameplay change. THE CLOSE.
    let report = session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(
        report.final_revision,
        Some(s_revision),
        "the existing verified representation satisfies the obligation"
    );
    assert!(report.lifecycle_checkpoint, "the no-op file checkpoint");
    assert!(report.required_settled);
    // No new canonical record: the head and the record count stand.
    assert_eq!(oracle_head_at(&location), s_revision);
    assert_eq!(oracle_records_at(&location), records_before + 1);
    // THE COHERENT FRESH RECOVERY — the oracle: S is exactly what a
    // fresh recovery restores (region 4 LIVE — the 150-near
    // scenario's state), never anything else.
    let mut recovered = OrchestratedSession::recover(&location, &dir.journal(), "pp-b4-m").unwrap();
    assert_ne!(
        recovered.region_level(4),
        LodLevel::Dormant,
        "the recovery restores S's state"
    );
    recovered.tick(&near_presence()).unwrap();
    assert_ne!(recovered.region_level(4), LodLevel::Dormant);

    // THE SQLITE PROVIDER.
    let db_dir = TestDir::new("idempotent-close-sqlite");
    let db_location = found_location(&db_dir, true);
    let mut db_session = orchestrated_at(&db_dir, true);
    let s = db_session
        .submit_publication(db_session.capture_snapshot().unwrap())
        .unwrap();
    let s_revision = match db_session.wait_publication(s).unwrap() {
        PublicationOutcome::Accepted { revision, .. } => revision,
        other => panic!("{other:?}"),
    };
    let report = db_session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.final_revision, Some(s_revision));
    assert!(report.lifecycle_checkpoint, "the WAL TRUNCATE");
    assert!(report.required_settled);
    assert_eq!(oracle_head_at(&db_location), s_revision);
    let wal_len = std::fs::metadata(db_dir.wal())
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    assert_eq!(wal_len, 0);
    let db_recovered =
        OrchestratedSession::recover(&db_location, &db_dir.journal(), "pp-b4-m").unwrap();
    assert_ne!(db_recovered.region_level(4), LodLevel::Dormant);
}

// B (C4-R2) — an older matching snapshot is SUPERSEDED by a newer
// DIFFERENT session snapshot (the second session's publication —
// now the authoritative recovery anchor): the close must NOT claim
// fully successful durable final state — it fails closed with the
// typed SupersededSnapshot conflict (the current state cannot be
// made authoritative without violating the chain-wide idempotency
// rule), and a fresh recovery explicitly selects the NEWER snapshot.
#[test]
fn a_superseded_snapshot_fails_the_close_closed() {
    let dir = TestDir::new("superseded-snapshot");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    // The optional snapshot S of the session's current state (region
    // 4 LIVE — the 150-near scenario).
    let s = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    let s_revision = match session.wait_publication(s).unwrap() {
        PublicationOutcome::Accepted { revision, .. } => revision,
        other => panic!("{other:?}"),
    };
    // THE NEWER DIFFERENT SNAPSHOT T: a second session (its own
    // journal, a FAR-presence scenario — region 4 DORMANT, the
    // distinguishable recovery oracle) publishes at HEAD.
    let (twin_spine, twin_director) = scenario_pair(120, &far_from_region_4());
    let twin_journal = T1Journal::open(dir.0.join("journal-twin.t1")).unwrap();
    let mut twin = OrchestratedSession::open(
        twin_spine,
        twin_director,
        &location,
        twin_journal,
        "pp-b4-m",
    )
    .unwrap();
    let twin_handle = twin
        .submit_publication(twin.capture_snapshot().unwrap())
        .unwrap();
    let twin_revision = match twin.wait_publication(twin_handle).unwrap() {
        PublicationOutcome::Accepted { revision, .. } => revision,
        other => panic!("{other:?}"),
    };
    drop(twin);
    // THE CLOSE of the unchanged S session: the older match is
    // superseded — the typed conflict, never a success.
    let report = session.close();
    match &report.failure {
        Some(CloseFailure::SupersededSnapshot) => {}
        other => panic!("the superseded snapshot is the typed conflict: {other:?}"),
    }
    assert_eq!(
        report.final_revision, None,
        "no snapshot verified this close"
    );
    // The chain stands: S below T at HEAD — no rebase, no new append.
    assert_ne!(oracle_head_at(&location), s_revision);
    assert_eq!(oracle_head_at(&location), twin_revision);
    // THE RECOVERY ORACLE: a fresh recovery selects the NEWER
    // snapshot T (the twin's FAR-scenario state — region 4
    // DORMANT), never the superseded S (region 4 live).
    let recovered = OrchestratedSession::recover(&location, &dir.journal(), "pp-b4-m").unwrap();
    assert_eq!(
        recovered.region_level(4),
        LodLevel::Dormant,
        "the recovery selects T (region 4 dormant), never S (region 4 live)"
    );
}

// C (C4-F2) — the lost reply on the REQUIRED final append: the
// append EXECUTES and installs, its reply is lost, the worker
// remains healthy — the reconciliation proves the installation, the
// full final-content readback succeeds, and the close returns the
// verified revision (never a false unavailability).
#[test]
fn the_close_reconciles_a_lost_reply_on_the_final_append() {
    let dir = TestDir::new("close-lost-reply");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    // A tick so the current state's content is FRESH (never
    // published — the idempotence walk must not find it).
    session.tick(&near_presence()).unwrap();
    // THE LOST REPLY armed for the close's required append (the
    // drain's jobs are reads and the barrier — the first append is
    // the final publication).
    session.fault_control().set(WorkerFault::LoseAppendReply);
    let report = session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    let final_revision = report.final_revision.expect("the verified final revision");
    // The append INSTALLED (the reconciliation proved it) and the
    // readback verified the exact snapshot.
    assert_eq!(oracle_head_at(&location), final_revision);
    assert!(report.required_settled);
    assert!(report.lifecycle_checkpoint);
}

// D (C4-F2) — the lost reply where the final candidate was GENUINELY
// NOT INSTALLED (the append skipped, the reply dropped, the worker
// healthy): the reconciliation proves the VERIFIED absence, the
// fresh recapture/retry lands, and the close succeeds WITHOUT
// duplicate installation.
#[test]
fn the_close_recaptures_after_a_lost_reply_without_installation() {
    let dir = TestDir::new("close-lost-reply-absent");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    session.tick(&near_presence()).unwrap();
    let records_before = oracle_records_at(&location);
    // THE SKIPPED-AND-LOST final append (attempt 1 never touches the
    // store; the fault is one-shot).
    session
        .fault_control()
        .set(WorkerFault::DropAppendWithoutExecution);
    let report = session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    let final_revision = report.final_revision.expect("the verified final revision");
    // EXACTLY ONE installation of the snapshot content (attempt 1
    // skipped; attempt 2's fresh capture landed).
    assert_eq!(oracle_head_at(&location), final_revision);
    assert_eq!(
        oracle_records_at(&location),
        records_before + 1,
        "no duplicate installation"
    );
}

// E (C4-F3) — the required pending attempt classified NotInstalled
// at close: a required commit whose append is CAS-rejected (a second
// session's publication advanced the head past the stale verified
// parent) with its reply lost — the retained Unknown candidate. The
// close's settlement reconciles it as NotInstalled, RETRIES through
// the accepted exception-safe path (the re-walk at the advanced
// head, the rebase with identical identities — no duplicate
// consequential identity), the confirmation completes canonically
// BEFORE the successful close, and the fresh recovery retains
// exactly-once consequence semantics.
#[test]
fn the_close_settles_the_unconfirmed_required_attempt() {
    let dir = TestDir::new("close-unconfirmed-attempt");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    session.tick(&far_presence()).unwrap();
    // THE HEAD ADVANCE past the session's stale verified parent: the
    // second session's publication (its own journal).
    let (twin_spine, twin_director) = scenario_pair(120, &near_region_4());
    let twin_journal = T1Journal::open(dir.0.join("journal-twin.t1")).unwrap();
    let mut twin = OrchestratedSession::open(
        twin_spine,
        twin_director,
        &location,
        twin_journal,
        "pp-b4-m",
    )
    .unwrap();
    let twin_handle = twin
        .submit_publication(twin.capture_snapshot().unwrap())
        .unwrap();
    assert!(matches!(
        twin.wait_publication(twin_handle).unwrap(),
        PublicationOutcome::Accepted { .. }
    ));
    drop(twin);
    // THE CAS-REJECTED, REPLY-LOST required commit: the retained
    // Unknown candidate.
    session.fault_control().set(WorkerFault::LoseAppendReply);
    match session.commit_canonical().unwrap() {
        CommitOutcome::Unknown { .. } => {}
        other => {
            panic!("the stale-parent append with the lost reply is the uncertain class: {other:?}")
        }
    }
    // THE CLOSE: the settlement reconciles NotInstalled and RETRIES.
    let report = session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert!(
        report.required_settled,
        "the required confirmation settled canonically before the close"
    );
    assert!(report.final_revision.is_some());
    // THE EXACTLY-ONCE RECOVERY: a fresh session re-derives — the
    // confirmed keys are suppressed (NothingPending), never
    // re-confirmed.
    let mut recovered = OrchestratedSession::recover(&location, &dir.journal(), "pp-b4-m").unwrap();
    assert!(matches!(
        recovered.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    ));
}

// The CloseReport CONTRACT (the review's consistency section): a
// checkpoint failure AFTER a verified final snapshot retains the
// verification evidence (final_revision Some, the honest
// lifecycle_checkpoint false) while the OVERALL outcome is the
// explicit CheckpointFailed — the distinctions unambiguous. The
// dedicated PanicBeforeCheckpoint seam lands the death between the
// verified final publication (the readback complete) and the
// lifecycle-checkpoint job — the C4-R2 walk's content-object reads
// invalidated the earlier ReadObject-timing construction (the first
// ReadObject after the probe is now the WALK's).
#[test]
fn the_checkpoint_failure_close_report_contract() {
    let dir = TestDir::new("close-checkpoint-failure");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    session.tick(&near_presence()).unwrap();
    let report = session.debug_close_with_probe_for_test(|closing| {
        closing
            .fault_control()
            .set(WorkerFault::PanicBeforeCheckpoint);
    });
    match &report.failure {
        Some(CloseFailure::CheckpointFailed(_)) => {}
        other => panic!("the checkpoint failure is the explicit overall outcome: {other:?}"),
    }
    // THE RETAINED EVIDENCE: the final snapshot IS verified and
    // installed (the head) — the failure never erases it.
    let final_revision = report
        .final_revision
        .expect("the verified evidence retained");
    assert!(
        !report.lifecycle_checkpoint,
        "the checkpoint did not complete"
    );
    assert!(
        report.worker.is_err(),
        "the panicked join coexists with the failure — never a fully successful close"
    );
    assert_eq!(oracle_head_at(&location), final_revision);
    // A fresh recovery works from the verified snapshot.
    let mut recovered = OrchestratedSession::recover(&location, &dir.journal(), "pp-b4-m").unwrap();
    recovered.tick(&near_presence()).unwrap();
}

// ------------------------------------------- the C4-R1/R2 corrective regressions

/// C4-R1 — the overall-success contract ENFORCED: a retained required
/// candidate A (its append CAS-rejected against the session's own
/// earlier optional publication — the publication path bypasses the
/// pipeline, so the pipeline's verified head is stale — with its
/// reply lost) is reconciled NotInstalled
/// at close; its retry B is skipped with its reply lost
/// (DropAppendWithoutExecution) and reconciled NotInstalled — the
/// T1-durable obligation retained for the crash-recovery re-drive.
/// The close reports required_settled == false and the explicit
/// UnsettledRequired overall failure — NEVER a fully successful
/// close — while the verified final-snapshot evidence stands
/// separately; the fresh recovery is coherent and exactly-once (the
/// recovered commit CONFIRMS the unsettled obligation, then
/// NothingPending).
#[test]
fn an_unsettle_required_attempt_close_reports_not_full_success() {
    let dir = TestDir::new("close-unsettled-required");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    session.tick(&far_presence()).unwrap();
    // THE HEAD ADVANCE past the pipeline's stale verified parent:
    // the session's OWN optional publication (worker-direct — the
    // publication path never touches the pipeline, so its verified
    // head still names the founding while HEAD names S).
    let s = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    let s_revision = match session.wait_publication(s).unwrap() {
        PublicationOutcome::Accepted { revision, .. } => revision,
        other => panic!("{other:?}"),
    };
    // THE CAS-REJECTED, REPLY-LOST required commit A (the retained
    // Unknown candidate).
    session.fault_control().set(WorkerFault::LoseAppendReply);
    match session.commit_canonical().unwrap() {
        CommitOutcome::Unknown { .. } => {}
        other => {
            panic!("the stale-parent append with the lost reply is the uncertain class: {other:?}")
        }
    }
    // THE RETRY B armed to be SKIPPED (its reply lost, the worker
    // healthy — the one-shot fault).
    session
        .fault_control()
        .set(WorkerFault::DropAppendWithoutExecution);
    // THE CLOSE: the drain reconciles A NotInstalled, retries B,
    // observes B's lost reply, reconciles B NotInstalled, and
    // retains the unconfirmed T1 obligation.
    let report = session.close();
    assert!(
        !report.required_settled,
        "the obligation is explicitly unsettled"
    );
    match &report.failure {
        Some(CloseFailure::UnsettledRequired) => {}
        other => panic!("the unsettled obligation is the explicit overall failure: {other:?}"),
    }
    // THE VERIFIED SNAPSHOT EVIDENCE stands separately — the
    // idempotent close via the existing publication (no new
    // canonical record).
    assert_eq!(report.final_revision, Some(s_revision));
    assert!(report.lifecycle_checkpoint);
    assert_eq!(oracle_head_at(&location), s_revision);
    // THE FRESH RECOVERY: coherent and exactly-once — the unsettled
    // obligation is re-driven and CONFIRMED (the first canonical
    // confirmation of those identities), never duplicated.
    let mut recovered = OrchestratedSession::recover(&location, &dir.journal(), "pp-b4-m").unwrap();
    assert!(matches!(
        recovered.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    assert!(matches!(
        recovered.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    ));
}

/// A (C4-R2) — an identical snapshot behind an intervening
/// NON-SNAPSHOT canonical revision (the session's own required
/// confirmation batch): the batch does NOT supersede the snapshot —
/// S remains the latest recoverable session state and satisfies the
/// idempotent close, with the consequential obligations settled and
/// the recovery oracle verifying S is what a fresh recovery
/// restores.
#[test]
fn an_identical_snapshot_behind_a_confirmation_still_satisfies_the_close() {
    let dir = TestDir::new("idempotent-behind-batch");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    session.tick(&far_presence()).unwrap();
    // S of the post-boundary state (region 4 dormant).
    let s = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    let s_revision = match session.wait_publication(s).unwrap() {
        PublicationOutcome::Accepted { revision, .. } => revision,
        other => panic!("{other:?}"),
    };
    let records_after_s = oracle_records_at(&location);
    // THE NON-SNAPSHOT HEAD ADVANCE: the required confirmation (the
    // consequence batch at HEAD — the state unchanged).
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    assert_eq!(oracle_records_at(&location), records_after_s + 1);
    // THE CLOSE: S (the latest recoverable snapshot) satisfies.
    let report = session.close();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.final_revision, Some(s_revision));
    assert!(report.required_settled, "the obligations settled");
    assert!(report.lifecycle_checkpoint);
    assert_eq!(
        oracle_records_at(&location),
        records_after_s + 1,
        "no new canonical record"
    );
    // THE RECOVERY ORACLE: a fresh recovery restores S (region 4
    // dormant — the post-boundary state), never anything else.
    let recovered = OrchestratedSession::recover(&location, &dir.journal(), "pp-b4-m").unwrap();
    assert_eq!(
        recovered.region_level(4),
        LodLevel::Dormant,
        "S is the restored state"
    );
}
