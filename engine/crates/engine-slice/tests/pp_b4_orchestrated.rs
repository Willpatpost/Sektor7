//! PP-B4 checkpoint 3 — the orchestrated session's acceptance suite
//! (the GO's fifteen regressions plus the flip-count generation): the
//! presence-driven authoritative tick with its exclusive transition
//! routing, the separate schedule_event, the capture/submit
//! publication discipline (stale-never-rebased, Busy, cancellation,
//! the honest unknowns), the worker-death gate without completion
//! polling, both providers, the completion accounting, and the
//! legacy-path preservation.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use engine_core::{
    EventClass, EventId, EventSpec, LodLevel, PresenceSample, Schedule, Spine, Tick,
};
use engine_persist::{
    AuthorityMode, CanonicalMedium, CommitOutcome, FaultAction, JournalFaultControl,
    JournalFaultPoint, RevisionRecord, SyncTier, T1Journal, TransactionId, UnknownReason,
    active_journal_path,
};
use engine_slice::crossing::{SessionCrossing, TransitionError};
use engine_slice::npc::{NpcConfig, NpcWorld, REGION_COUNT, install_initial_schedules};
use engine_slice::orchestration::{
    CompletionDisposition, OrchestratedSession, PublicationOutcome, SubmitRefusal,
};
use engine_slice::region::{RegionConfig, RegionDirector};
use engine_slice::store::{SessionStore, StoreLocation};
use engine_slice::worker::{IoWorker, WorkerFault};

// ------------------------------------------------------------- fixtures

/// A temporary campaign directory (the established suite pattern).
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-pp-b4-orchestrated-{name}-{}",
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
                &RevisionRecord::founding(AuthorityMode::Standalone, "pp-b4-orch", 0),
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

/// The single-sample presence slices the orchestrated tick takes.
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
    OrchestratedSession::open(spine, director, &location, journal, "pp-b4-orch").unwrap()
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

fn is_worker_refusal(error: &TransitionError) -> bool {
    matches!(
        error,
        TransitionError::InstallIncompatible { detail, .. } if *detail == "worker/unavailable"
    )
}

// ------------------------------------------------------- the tick regressions

/// 1 — the healthy orchestrated tick matches the accepted
/// durable-crossing behavior: the far boundary deactivates region 4
/// (T1 journaled BEFORE the install), the confirmation lands, the
/// near boundary reactivates, and the recovery is coherent.
#[test]
fn the_healthy_tick_matches_the_durable_crossing_behavior() {
    let dir = TestDir::new("tick-healthy");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    // The far boundary: region 4's live-to-dormant transition.
    let report = session.tick(&far_presence()).unwrap();
    // The far boundary transitions both sides (the fixture's far
    // presence sits at region 0's center, so the far side activates
    // while the near side deactivates) — region 4 is the
    // capture-bearing deactivation under test.
    assert!(report.regions_transitioned.contains(&4));
    assert_eq!(session.region_level(4), LodLevel::Dormant);
    // The T1 evidence exists (the journaling preceded the install).
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    assert_eq!(scan.committed.len(), 1);
    // The confirmation lands through the single worker.
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    // The near boundary: the durable reactivation.
    session.tick(&near_presence()).unwrap();
    assert_ne!(session.region_level(4), LodLevel::Dormant);
    // Publish the reactivated state — the recovery anchor (the
    // activation itself journals nothing by the accepted design, so
    // the recovery restores the last PUBLISHED state).
    let handle = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Accepted { .. }
    ));
    // The recovery is coherent.
    drop(session);
    let mut recovered =
        OrchestratedSession::recover(&location, &dir.journal(), "pp-b4-orch").unwrap();
    assert_ne!(recovered.region_level(4), LodLevel::Dormant);
    recovered.tick(&far_presence()).unwrap();
    assert_eq!(recovered.region_level(4), LodLevel::Dormant);
}

/// 2 — the deactivation persists T1 BEFORE the simulation discards:
/// the journal carries the region-4 transaction (with the plan), the
/// region is dormant, and the store's chain still validates.
#[test]
fn deactivation_persists_t1_before_the_simulation_discards() {
    let dir = TestDir::new("t1-before-discard");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    session.tick(&far_presence()).unwrap();
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    assert_eq!(scan.committed.len(), 1);
    assert!(!scan.committed[0].plan.is_empty(), "the plan is journaled");
    assert_eq!(session.region_level(4), LodLevel::Dormant);
    assert!(oracle_records_at(&location) >= 1);
}

/// 3 — a required journal failure prevents the authoritative clock
/// from advancing: the pre-window fault refuses the deactivation's
/// journal append, the tick aborts BEFORE run_tick, the region stays
/// live, and a fresh recovery retries coherently.
#[test]
fn a_journal_failure_prevents_the_clock_advance() {
    let dir = TestDir::new("journal-failure");
    let location = found_location(&dir, false);
    let control =
        JournalFaultControl::new().arm(JournalFaultPoint::BeforeCaptures, FaultAction::InjectError);
    let journal = T1Journal::open_with(dir.journal(), Some(control)).unwrap();
    let (spine, director) = scenario_pair(150, &near_region_4());
    let mut session =
        OrchestratedSession::open(spine, director, &location, journal, "pp-b4-orch").unwrap();
    // A snapshot first (the recovery's anchor).
    let handle = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Accepted { .. }
    ));
    let clock_before = session.clock();
    // The far boundary: the journal append refuses; the tick aborts.
    let refusal = session.tick(&far_presence());
    assert!(refusal.is_err());
    assert_eq!(session.clock(), clock_before, "the clock did not advance");
    assert_ne!(session.region_level(4), LodLevel::Dormant);
    // Nothing was journaled (the pre-window fault writes nothing).
    assert_eq!(
        T1Journal::scan_file(dir.journal()).unwrap().committed.len(),
        0
    );
    // The coherent retry: a fresh recovery (its own fault-free journal).
    drop(session);
    let mut recovered =
        OrchestratedSession::recover(&location, &dir.journal(), "pp-b4-orch").unwrap();
    recovered.tick(&far_presence()).unwrap();
    assert_eq!(recovered.region_level(4), LodLevel::Dormant);
}

/// 4 — partial completion preserves already-durable transitions and
/// permits coherent retry: the far boundary's deactivation is durable
/// and confirmed, the near boundary's reactivation completes, then a
/// poisoned worker refuses the next far tick (the clock unadvanced,
/// the durable work preserved) and the fresh recovery retries to
/// completion.
#[test]
fn partial_completion_preserves_durable_transitions_and_permits_coherent_retry() {
    let dir = TestDir::new("partial-completion");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    // The durable work: the deactivation + the confirmation + a
    // published snapshot (the recovery anchor).
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    let handle = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Accepted { .. }
    ));
    // A second completed transition: the reactivation.
    session.tick(&near_presence()).unwrap();
    assert_ne!(session.region_level(4), LodLevel::Dormant);
    // Publish the reactivated state — the recovery anchor (the
    // activation journals nothing by the accepted design, so the
    // recovery restores the last PUBLISHED state; without this
    // publication the recovery correctly returns region 4 to the
    // pre-reactivation snapshot).
    let handle = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Accepted { .. }
    ));
    let clock_before = session.clock();
    // The failing tick: the poisoned worker refuses at the gate —
    // BEFORE any progression; the clock and the durable state stand.
    session.poison();
    let refusal = session.tick(&far_presence());
    assert!(refusal.is_err());
    assert_eq!(session.clock(), clock_before);
    assert_ne!(session.region_level(4), LodLevel::Dormant);
    // The durable T1 evidence is preserved on the journal.
    assert_eq!(
        T1Journal::scan_file(dir.journal()).unwrap().committed.len(),
        1
    );
    // The coherent retry: the fresh recovery completes the transition.
    drop(session);
    let mut recovered =
        OrchestratedSession::recover(&location, &dir.journal(), "pp-b4-orch").unwrap();
    assert_ne!(recovered.region_level(4), LodLevel::Dormant);
    recovered.tick(&far_presence()).unwrap();
    assert_eq!(recovered.region_level(4), LodLevel::Dormant);
}

/// 5 — schedule_event preserves the ordinary and dormant-region
/// scheduling semantics: the spine's own dependency validation (the
/// unknown-id rejection), the identity allocation, and the
/// director's dormant-hold dispatch — never a reimplementation.
#[test]
fn schedule_event_preserves_the_scheduling_semantics() {
    let dir = TestDir::new("schedule");
    let mut session = orchestrated(&dir);
    let region4 = EventClass::new(4);
    // The unknown-dependency rejection (the spine's own validation).
    let bogus = EventSpec::new(region4, Schedule::AtLogicalTime { due: Tick(1000) }, 1)
        .with_dependencies(vec![EventId { seq: 4_000_000 }]);
    assert!(session.schedule_event(bogus).is_err());
    // The ordinary (live-region) scheduling allocates an identity.
    let first = session
        .schedule_event(EventSpec::new(
            region4,
            Schedule::AtLogicalTime { due: Tick(1000) },
            2,
        ))
        .unwrap();
    // A valid dependency schedules.
    let second = session
        .schedule_event(
            EventSpec::new(region4, Schedule::AtLogicalTime { due: Tick(1001) }, 3)
                .with_dependencies(vec![first]),
        )
        .unwrap();
    assert_ne!(first, second);
    // The dormant-hold: after the far boundary, scheduling into the
    // dormant region still succeeds (the held path — the director's
    // own dispatch).
    session.tick(&far_presence()).unwrap();
    let held = session
        .schedule_event(EventSpec::new(
            region4,
            Schedule::AtLogicalTime { due: Tick(1002) },
            4,
        ))
        .unwrap();
    assert_ne!(held, second);
}

/// 6 — a pending optional snapshot does not stall authorized gameplay
/// ticks: with the worker parked on the publication job, a tick with
/// NO capture-bearing deactivation completes — its durability is
/// journal-side, never worker-side. C1 sharpens the boundary
/// honestly: the covering prerequisite is the ONE worker-side
/// dependency a tick can carry (a capture-bearing deactivation
/// submits its covering snapshot through the worker BEFORE the T1,
/// so that tick shape waits for the worker by design — the §6
/// matrix proves the ordering separately); every other tick —
/// including this near-presence one — never touches the worker, and
/// the parked release completes the publication.
#[test]
fn a_pending_optional_snapshot_does_not_stall_ticks() {
    let dir = TestDir::new("pending-no-stall");
    let mut session = orchestrated(&dir);
    let capture = session.capture_snapshot().unwrap();
    session.fault_control().set(WorkerFault::ParkNext);
    let handle = session.submit_publication(capture).unwrap();
    assert_eq!(session.pending_publications(), 1);
    // The tick runs while the publication is parked in the queue: the
    // near presence keeps every region at its current level — no
    // deactivation, no covering submission, no worker dependency.
    let report = session.tick(&near_presence()).unwrap();
    assert!(
        report.regions_transitioned.is_empty(),
        "the near tick transitions nothing — it needs no crossing at all"
    );
    // The release completes the publication.
    session.fault_control().release_park();
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Accepted { .. }
    ));
}

// ---------------------------------------------- the publication regressions

/// 7 — a capture at H0 submitted after a confirmation advanced the
/// head to H1 is REJECTED stale, never rebased: the CAS rejects the
/// captured parent, the chain holds no A-content revision, and the
/// head remains H1.
#[test]
fn capture_a_stale_after_confirmation_never_rebased() {
    let dir = TestDir::new("stale-capture");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    let capture = session.capture_snapshot().unwrap();
    let captured_parent = capture.captured_parent();
    // The confirmation advances the head.
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    let advanced = oracle_head_at(&location);
    assert_ne!(advanced, captured_parent);
    // The stale submission: rejected, never rebased.
    let handle = session.submit_publication(capture).unwrap();
    match session.wait_publication(handle).unwrap() {
        PublicationOutcome::Stale { revision, observed } => {
            assert_eq!(observed, Some(advanced));
            assert_ne!(revision, advanced);
        }
        other => panic!("the stale capture must reject: {other:?}"),
    }
    // The chain holds no A-candidate revision: the head is still the
    // confirmation's, and the chain is the founding, the boundary's
    // C1 covering (the same exact state the capture took, published
    // by the boundary's own prerequisite), and the batch.
    assert_eq!(oracle_head_at(&location), advanced);
    assert_eq!(oracle_records_at(&location), 3);
}

/// 8 — a fresh capture at the confirmation's head publishes
/// successfully.
#[test]
fn a_fresh_capture_publishes_successfully() {
    let dir = TestDir::new("fresh-capture");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    let stale = session.capture_snapshot().unwrap();
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    let handle = session.submit_publication(stale).unwrap();
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Stale { .. }
    ));
    // The fresh capture at the advanced head publishes.
    let fresh = session.capture_snapshot().unwrap();
    let handle = session.submit_publication(fresh).unwrap();
    match session.wait_publication(handle).unwrap() {
        PublicationOutcome::Accepted { revision, .. } => {
            assert_eq!(oracle_head_at(&location), revision);
        }
        other => panic!("the fresh capture must publish: {other:?}"),
    }
    // The founding, the boundary's C1 covering, the confirmation
    // batch, and the fresh capture.
    assert_eq!(oracle_records_at(&location), 4);
}

/// 9 — T1 advancing with the canonical HEAD carried by the C1
/// covering: the far boundary journals its T1 (journal-side) AND
/// publishes its covering snapshot (the canonical side — the head
/// advances BY THE COVERING, never by the T1 itself; the covering
/// binds the state the boundary's plan was prepared from, the
/// prepare-then-capture order); the earlier capture — taken before
/// the boundary, parented at the founding — submits STALE against
/// the covering (honest: the head advanced past its parent), and
/// the commit confirms the T1's captures.
#[test]
fn t1_advances_the_head_by_the_covering_and_the_earlier_capture_stays_honest() {
    let dir = TestDir::new("t1-advances");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    let capture = session.capture_snapshot().unwrap();
    assert!(capture.journal_frontier().is_empty());
    let captured_parent = capture.captured_parent();
    // The far boundary journals T1 — and its covering snapshot is the
    // canonical advance.
    session.tick(&far_presence()).unwrap();
    let head = oracle_head_at(&location);
    assert_ne!(
        head, captured_parent,
        "the boundary's covering advanced the canonical head"
    );
    {
        // The head's record IS the boundary's covering: a
        // content-keyed session snapshot (the idempotency key equals
        // the single content reference — the snapshot publication
        // discipline). The covering binds the PREPARED plan's state,
        // not the earlier capture's pre-plan state.
        let mut oracle = SessionStore::open(&location).unwrap();
        let record = oracle.read_record(&head).unwrap();
        assert_eq!(
            record.idempotency_key.as_deref(),
            Some(record.content_refs[0].as_bytes().as_slice()),
            "the head is the boundary's covering — a content-keyed snapshot"
        );
    }
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    assert_eq!(scan.committed.len(), 1, "the T1 itself is journal-side");
    // The earlier capture submits STALE against the covering — the
    // head advanced past its captured parent.
    let handle = session.submit_publication(capture).unwrap();
    match session.wait_publication(handle).unwrap() {
        PublicationOutcome::Stale { observed, .. } => {
            assert_eq!(
                observed,
                Some(head),
                "the stale observation is the covering"
            );
        }
        other => panic!("the earlier capture is honestly stale against the covering: {other:?}"),
    }
    // The newer T1 progress was honestly pending: the capture's
    // frontier predates it, and the commit confirms it NOW.
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
}

/// 10 — queue saturation produces the typed optional Busy without an
/// append, and the fairness gate (a waiting required operation)
/// refuses optional admissions the same way; the release drains every
/// admitted job through the single worker.
#[test]
fn queue_saturation_produces_busy_without_append() {
    let dir = TestDir::new("saturation");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    // The fairness gate alone (a non-full queue): the armed
    // required-waiting flag refuses the optional admission.
    session.set_required_waiting_for_test(true);
    let refusal = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap_err();
    assert_eq!(refusal, SubmitRefusal::Busy);
    session.set_required_waiting_for_test(false);
    // The bounded queue: 66 captures (the read jobs complete before
    // the park), then the park, then the submissions — 64 admitted,
    // the 65th and 66th refused Busy, nothing appended.
    let captures: Vec<_> = (0..66)
        .map(|_| session.capture_snapshot().unwrap())
        .collect();
    session.fault_control().set(WorkerFault::ParkNext);
    let mut handles = Vec::new();
    let mut refusals = 0;
    for capture in captures {
        match session.submit_publication(capture) {
            Ok(handle) => handles.push(handle),
            Err(refusal) => {
                assert_eq!(refusal, SubmitRefusal::Busy);
                refusals += 1;
            }
        }
    }
    // The capacity is 64 — plus the one job the parked worker already
    // holds in hand — so the admission count is 64 or 65 depending on
    // the take timing; every submission is accounted and at least one
    // refused.
    assert_eq!(handles.len() + refusals, 66);
    assert!(refusals >= 1);
    assert_eq!(session.pending_publications(), handles.len());
    // Nothing appended while the worker is parked.
    let head_before = oracle_head_at(&location);
    // The release drains: the first publication accepts, the rest are
    // stale (the identical content, the advanced parent) — every
    // admitted job executes through the single worker.
    session.fault_control().release_park();
    let mut accepted = 0;
    let mut stale = 0;
    for handle in &handles {
        match session.wait_publication(*handle).unwrap() {
            PublicationOutcome::Accepted { .. } => accepted += 1,
            PublicationOutcome::Stale { .. } => stale += 1,
            other => panic!("the drained publication: {other:?}"),
        }
    }
    assert_eq!(accepted, 1);
    assert_eq!(stale, handles.len() - 1);
    assert_ne!(oracle_head_at(&location), head_before);
}

/// 11 — cancellation: before the job starts it prevents the
/// publication (a definite non-event, the head unchanged); after the
/// job has started it cannot erase a possibly committed outcome (the
/// append executes and reports Accepted).
#[test]
fn cancellation_before_start_prevents_publication() {
    let dir = TestDir::new("cancel-before");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    let head_before = oracle_head_at(&location);
    let capture = session.capture_snapshot().unwrap();
    session.fault_control().set(WorkerFault::ParkNext);
    let handle = session.submit_publication(capture).unwrap();
    assert!(session.cancel_publication(handle));
    session.fault_control().release_park();
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Canceled { .. }
    ));
    assert_eq!(oracle_head_at(&location), head_before, "no append");
}

#[test]
fn cancellation_after_start_cannot_erase_the_outcome() {
    let dir = TestDir::new("cancel-after");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    let capture = session.capture_snapshot().unwrap();
    // The park lands INSIDE the executing append — past the
    // cancellation boundary (the job has started).
    session
        .fault_control()
        .set(WorkerFault::ParkExecutingAppend);
    let handle = session.submit_publication(capture).unwrap();
    // THE DETERMINISTIC PARK-ENTRY BARRIER (the final-validation
    // determinism audit): wait for the worker to actually enter the
    // executing-append park — the real event, never a guessed
    // duration.
    while !session.fault_control().park_entered() {
        std::hint::spin_loop();
    }
    // The cancellation is too late: the job already started.
    assert!(session.cancel_publication(handle));
    session.fault_control().release_park();
    match session.wait_publication(handle).unwrap() {
        PublicationOutcome::Accepted { revision, .. } => {
            assert_eq!(oracle_head_at(&location), revision, "the append ran");
        }
        other => panic!("the started job completes: {other:?}"),
    }
}

/// 12 — worker death without polling any completion: the next
/// authoritative tick AND the scheduling operation refuse through the
/// composed gate (the health cell — never a completion poll).
#[test]
fn worker_death_without_polling_refuses_progression() {
    let dir = TestDir::new("death-unpolled");
    let mut session = orchestrated(&dir);
    // The T1 evidence for the confirmation that triggers the death.
    session.tick(&far_presence()).unwrap();
    session.fault_control().set(WorkerFault::PanicNext);
    // The confirmation's append job kills the worker (its outcome is
    // the honest Unknown class — the pipeline's retained candidate).
    match session.commit_canonical().unwrap() {
        CommitOutcome::Unknown { .. } => {}
        other => panic!("the death surfaces the uncertain class: {other:?}"),
    }
    // The failure signal (the reply channel dropping mid-unwind)
    // precedes the DeathGuard's thread-exit health flip — the death
    // is guaranteed, never synchronous with the failure return
    // (bounded wait; the CI-runner race the first three-OS dispatch
    // exposed).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while session.healthy() && std::time::Instant::now() < deadline {
        std::hint::spin_loop();
    }
    assert!(!session.healthy());
    // NO completion was polled anywhere — the next tick and the
    // scheduling refuse through the gate.
    let refusal = session.tick(&far_presence());
    assert!(refusal.is_err());
    assert!(refusal.is_err_and(|error| is_worker_refusal(&error)));
    let schedule_refusal = session.schedule_event(EventSpec::new(
        EventClass::new(4),
        Schedule::AtLogicalTime { due: Tick(1000) },
        1,
    ));
    assert!(schedule_refusal.is_err());
}

/// 13 — both providers preserve chain integrity and the correct
/// durability labeling: the orchestrated lifecycle over the file and
/// the SQLite providers, the accepted publication's tier equal to the
/// provider's own truthful label.
#[test]
fn both_providers_chain_integrity_and_tier_labeling() {
    let file_dir = TestDir::new("provider-file");
    let file_location = found_location(&file_dir, false);
    let mut file_session = orchestrated_at(&file_dir, false);
    assert_eq!(
        file_session.worker_tier(),
        SessionStore::open(&file_location)
            .unwrap()
            .durability_tier()
    );
    let handle = file_session
        .submit_publication(file_session.capture_snapshot().unwrap())
        .unwrap();
    match file_session.wait_publication(handle).unwrap() {
        PublicationOutcome::Accepted { tier, .. } => {
            assert_eq!(tier, file_session.worker_tier());
        }
        other => panic!("the file publication: {other:?}"),
    }
    file_session.tick(&far_presence()).unwrap();
    assert!(matches!(
        file_session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    assert!(oracle_records_at(&file_location) >= 3);

    let db_dir = TestDir::new("provider-sqlite");
    let db_location = found_location(&db_dir, true);
    let mut db_session = orchestrated_at(&db_dir, true);
    assert_eq!(db_session.worker_tier(), SyncTier::TransactionalWalFull);
    let handle = db_session
        .submit_publication(db_session.capture_snapshot().unwrap())
        .unwrap();
    match db_session.wait_publication(handle).unwrap() {
        PublicationOutcome::Accepted { tier, .. } => {
            assert_eq!(tier, SyncTier::TransactionalWalFull);
        }
        other => panic!("the sqlite publication: {other:?}"),
    }
    db_session.tick(&far_presence()).unwrap();
    assert!(matches!(
        db_session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    assert!(oracle_records_at(&db_location) >= 3);
}

/// 14 — the completion accounting distinguishes the outcomes (the
/// accepted, the stale, the canceled, the unknown) and retains the
/// candidate identity and the captured frontier on every completion
/// record.
#[test]
fn completion_accounting_distinguishes_the_outcomes() {
    let dir = TestDir::new("accounting");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    // Accepted.
    let handle = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    let accepted_revision = match session.wait_publication(handle).unwrap() {
        PublicationOutcome::Accepted { revision, .. } => revision,
        other => panic!("{other:?}"),
    };
    let record = session.completion_record(handle).unwrap();
    assert_eq!(record.revision, accepted_revision);
    assert!(!record.coverage.materialized.is_empty());
    // Stale.
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
    assert!(session.completion_record(handle).is_some());
    // Canceled.
    let capture = session.capture_snapshot().unwrap();
    session.fault_control().set(WorkerFault::ParkNext);
    let handle = session.submit_publication(capture).unwrap();
    session.cancel_publication(handle);
    session.fault_control().release_park();
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Canceled { .. }
    ));
    // Unknown: the append EXECUTES (it may commit) but its reply is
    // lost — the honest uncertain class, the head advanced.
    session.fault_control().set(WorkerFault::LoseAppendReply);
    // The PRE-SUBMISSION head (the append executes asynchronously —
    // reading the head after the submission races its landing, the
    // windows-CI failure of the first three-OS dispatch).
    let head_before_unknown = oracle_head_at(&location);
    let handle = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    match session.wait_publication(handle).unwrap() {
        PublicationOutcome::Unknown { revision, .. } => {
            assert_ne!(revision, head_before_unknown);
        }
        other => panic!("the lost reply is the uncertain class: {other:?}"),
    }
    // The append actually landed (never a definite failure claim).
    assert_ne!(oracle_head_at(&location), head_before_unknown);
}

/// 15 — the legacy synchronous paths remain operational and unchanged
/// (the full fingerprint preservation is the double release-mode
/// selftest — the frozen Stage-0 pins over the untouched engine-core).
#[test]
fn legacy_synchronous_paths_remain_operational() {
    let dir = TestDir::new("legacy");
    let location = found_location(&dir, false);
    let (mut spine, mut director) = scenario_pair(150, &near_region_4());
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "legacy").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    let now = spine.clock();
    crossing
        .deactivate(&mut spine, &mut director, 4, now)
        .unwrap()
        .expect("the legacy deactivation journals");
    assert!(matches!(
        crossing.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    drop(crossing);
    let (_spine, _director, recovered) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "legacy").unwrap();
    drop(recovered);
    assert!(oracle_records_at(&location) >= 3);
}

// ------------------------------------------------------ the generation groundwork

/// 16 — the capture-validity generation (the journal flip count):
/// zero at open, incremented by each successful pointer flip — the
/// counter the orchestrated captures record and the submissions
/// compare (the orchestrator-level end-to-end capture→flip→submit
/// stale path lands with Checkpoint 4's maintenance integration,
/// where the orchestrator gains its rotation entry points under the
/// lease; here the crossing-level counter is proven over the
/// worker-backed instantiation).
#[test]
fn the_journal_flip_count_generation() {
    let dir = TestDir::new("flip-count");
    let location = found_location(&dir, false);
    let (spine, director) = scenario_pair(150, &near_region_4());
    let worker = IoWorker::spawn(&location).unwrap();
    let source = worker.source().unwrap();
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open_at(&source, journal, "pp-b4-flip").unwrap();
    assert_eq!(crossing.journal_flip_count(), 0);
    // The rotation requires a verified anchor: publish one first.
    crossing.publish_snapshot(&spine, &director).unwrap();
    crossing.rotate_journal().unwrap();
    assert_eq!(crossing.journal_flip_count(), 1);
    crossing.rotate_journal().unwrap();
    assert_eq!(crossing.journal_flip_count(), 2);
    drop(crossing);
    drop(source);
    worker.join().unwrap();
}

/// The barrier-based queued-poison supplement (the GO's replacement
/// for the timed queued-poison test): a publication admitted before
/// the poison but NOT YET STARTED is refused at the worker's
/// execution boundary with the DEFINITE typed error — the
/// deterministic synchronization barrier, no timing.
#[test]
fn the_barrier_queued_poison_refuses_admitted_work() {
    let dir = TestDir::new("barrier-queued-poison");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    let head_before = oracle_head_at(&location);
    let capture = session.capture_snapshot().unwrap();
    session.fault_control().set(WorkerFault::ParkNext);
    let handle = session.submit_publication(capture).unwrap();
    // The poison lands while the job is parked in the worker's hands
    // (admitted, not started).
    session.poison();
    session.fault_control().release_park();
    match session.wait_publication(handle).unwrap() {
        PublicationOutcome::Failed { error, .. } => {
            assert!(matches!(
                error,
                engine_persist::MediumError::WorkerUnavailable { .. }
            ));
        }
        other => panic!("the queued job is refused at the boundary: {other:?}"),
    }
    // No append: the head is unchanged, and the poisoned session
    // refuses further authoritative progression.
    assert_eq!(oracle_head_at(&location), head_before);
    assert!(session.tick(&far_presence()).is_err());
}

/// Poisoning during an ALREADY EXECUTING append: the job passed the
/// poison boundary and parks inside the append; the poison lands; the
/// release lets the append COMPLETE — the honest in-flight outcome
/// (Accepted, the head advanced), never a false refusal.
#[test]
fn poisoning_during_an_executing_append_completes_honestly() {
    let dir = TestDir::new("poison-in-flight");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    let capture = session.capture_snapshot().unwrap();
    session
        .fault_control()
        .set(WorkerFault::ParkExecutingAppend);
    let handle = session.submit_publication(capture).unwrap();
    // THE DETERMINISTIC PARK-ENTRY BARRIER (the final-validation
    // determinism audit): wait for the worker to actually enter the
    // executing-append park — the real event, never a guessed
    // duration.
    while !session.fault_control().park_entered() {
        std::hint::spin_loop();
    }
    session.poison();
    session.fault_control().release_park();
    match session.wait_publication(handle).unwrap() {
        PublicationOutcome::Accepted { revision, .. } => {
            assert_eq!(oracle_head_at(&location), revision, "the append ran");
        }
        other => panic!("the in-flight job completes: {other:?}"),
    }
    // The poisoned session refuses further progression.
    assert!(session.tick(&far_presence()).is_err());
}

// ----------------------------------------------- the C3 corrective regressions

/// The durable boundary routing (the pp_b3 rotation fixture's
/// proven shape), COLLECTING the journaled transaction identities —
/// the C3-F2 regression's classification evidence.
fn durable_boundary_receipts(
    crossing: &mut SessionCrossing,
    spine: &mut Spine<NpcWorld>,
    director: &mut RegionDirector,
    presence: &PresenceSample,
    journaled: &mut Vec<TransactionId>,
) {
    let now = spine.clock();
    for region in 0..REGION_COUNT {
        let current = director.levels()[region];
        let next = director.target_level(region, std::slice::from_ref(presence));
        if next == current {
            continue;
        }
        if next == LodLevel::Dormant {
            if let Some(receipt) = crossing
                .deactivate(spine, director, region, now)
                .expect("the deactivation commits")
            {
                journaled.push(receipt.journal.id);
            }
        } else if current == LodLevel::Dormant {
            crossing
                .activate(spine, director, region, next, now)
                .expect("the activation installs");
        } else {
            director.transition_region(spine, region, next, now);
        }
    }
}

fn run_ticks_near(
    spine: &mut Spine<NpcWorld>,
    director: &mut RegionDirector,
    presence: &PresenceSample,
    ticks: u64,
) {
    for _ in 0..ticks {
        director.apply(spine, std::slice::from_ref(presence));
        spine.run_tick();
    }
}

/// C3-F1 — the join eliminates the sender-lifetime deadlock: the
/// crossing (the pipeline's queue proxies) is released BEFORE the
/// join blocks, the worker drains every admitted job FIFO, and the
/// unpolled completion slots are harvested into the ledger with
/// their honest dispositions — never a "safely saved" claim.
#[test]
fn join_releases_the_crossing_and_harvests_unpolled_completions() {
    let dir = TestDir::new("join-harvest");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    // Worker-backed operations first (the crossing holds live queue
    // proxies through its pipeline).
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    // Pending optional work, deliberately unpolled: both captures
    // taken BEFORE either submission (a later capture's observe_head
    // would queue behind the first append and read the ADVANCED head
    // — a different record with the same content key, the
    // duplicate-key rejection) — two byte-identical records: the
    // first accepts, the second hits the advanced parent's CAS
    // rejection.
    let capture_a = session.capture_snapshot().unwrap();
    let capture_b = session.capture_snapshot().unwrap();
    let first = session.submit_publication(capture_a).unwrap();
    let second = session.submit_publication(capture_b).unwrap();
    assert_eq!(session.pending_publications(), 2);
    // THE JOIN: must RETURN (the pre-fix join blocked indefinitely —
    // the crossing's queue senders outlived the blocked thread join).
    let report = session.join();
    assert!(report.worker.is_ok(), "the worker joined cleanly");
    let first_record = report
        .completions
        .iter()
        .find(|record| record.id == first.id)
        .expect("the unpolled first publication is harvested");
    let second_record = report
        .completions
        .iter()
        .find(|record| record.id == second.id)
        .expect("the unpolled second publication is harvested");
    match &first_record.disposition {
        CompletionDisposition::Accepted { revision, .. } => {
            assert_eq!(*revision, oracle_head_at(&location));
        }
        other => panic!("the drained first publication: {other:?}"),
    }
    assert!(matches!(
        second_record.disposition,
        CompletionDisposition::Stale { .. }
    ));
    // The candidate identity and the captured frontier retained.
    assert_eq!(first_record.revision, oracle_head_at(&location));
    assert!(!first_record.coverage.materialized.is_empty());
}

/// C3-F1 — the real-child join bound: the child performs the full
/// worker-backed scenario with unpolled pending work and joins; the
/// parent asserts the child TERMINATES within a bound (the pre-fix
/// join deadlocked indefinitely — the crossing's queue senders
/// outlived the blocked join). The parent invocation without the
/// environment variable is a no-op pass.
#[test]
fn a_real_child_join_terminates_within_a_bound() {
    if let Ok(child_root) = std::env::var("S7_PP_B4_JOIN_CHILD_DIR") {
        // ==== the child: the join scenario in this process ====
        let root = PathBuf::from(child_root);
        std::fs::create_dir_all(&root).unwrap();
        let location = StoreLocation::Directory(root.join("store"));
        let mut store = SessionStore::open(&location).unwrap();
        if store.head().unwrap().is_none() {
            store
                .append(
                    &RevisionRecord::founding(AuthorityMode::Standalone, "pp-b4-join", 0),
                    &[],
                    None,
                )
                .unwrap();
        }
        drop(store);
        let (spine, director) = scenario_pair(150, &near_region_4());
        let journal = T1Journal::open(root.join("journal.t1")).unwrap();
        let mut session =
            OrchestratedSession::open(spine, director, &location, journal, "pp-b4-join").unwrap();
        session.tick(&far_presence()).unwrap();
        assert!(matches!(
            session.commit_canonical().unwrap(),
            CommitOutcome::Confirmed(_)
        ));
        let capture_a = session.capture_snapshot().unwrap();
        let capture_b = session.capture_snapshot().unwrap();
        let first = session.submit_publication(capture_a).unwrap();
        let second = session.submit_publication(capture_b).unwrap();
        let _ = (first, second);
        let report = session.join();
        assert!(report.worker.is_ok(), "the child's join is clean");
        assert_eq!(
            report.completions.len(),
            2,
            "both unpolled publications harvested"
        );
        eprintln!(
            "the child joined cleanly with {} harvested completions",
            report.completions.len()
        );
        return;
    }
    // ==== the parent: the bounded wait ====
    let dir = TestDir::new("join-child");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "a_real_child_join_terminates_within_a_bound",
            "--exact",
            "--nocapture",
        ])
        .env("S7_PP_B4_JOIN_CHILD_DIR", dir.0.display().to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        match child.try_wait().unwrap() {
            Some(status) => break status,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                panic!(
                    "the child's join did not terminate within the bound \
                     (the sender-lifetime deadlock)"
                );
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    };
    assert!(
        status.success(),
        "the child joined and exited cleanly: {status}"
    );
}

/// C3-F2 — the captured journal frontier follows the ACTIVE
/// generation: after a rotation with genuinely different
/// retained/retired sets and post-rotation T1 activity, the
/// recovery-resolved orchestrated session's capture carries the
/// ACTIVE journal's validated committed identities — never the
/// retired root's set (the pre-fix capture scanned the root and
/// carried the retired identities).
#[test]
fn the_captured_frontier_follows_the_active_journal_generation() {
    let dir = TestDir::new("frontier-active");
    let near = near_region_4();
    let far = far_from_region_4();
    let legacy = dir.journal();
    // The legacy pre-worker campaign (the rotation remains
    // Checkpoint 4's orchestrated scope — the fixture rotates through
    // the accepted crossing directly, the pp_b3 lifecycle's proven
    // shape).
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(&legacy).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "pp-b4-fa").unwrap();
    // S1 — the pre-checkpoint publication (the checkpoint's range).
    crossing.publish_snapshot(&spine, &director).unwrap();
    // T1 — the first far boundary: region 4's captures journaled,
    // unconfirmed.
    let mut journaled = Vec::new();
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &far,
        &mut journaled,
    );
    let t1 = journaled
        .last()
        .copied()
        .expect("the first far boundary journals");
    // Confirm + materialize: C1 confirms the captures; the near
    // boundary reactivates region 4 (the captures apply) and the
    // near run accrues the post-reactivation crossing work.
    assert!(matches!(
        crossing.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &near,
        &mut journaled,
    );
    run_ticks_near(&mut spine, &mut director, &near, 100);
    // THE ANCHOR — the checkpoint (the compaction's lane boundary +
    // SK+MK): T1 becomes confirmed-materialized-SUBSUMED.
    crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the checkpoint publishes the anchor");
    // THE COVERING PUBLICATION (the frontier guard's discipline, the
    // C protocol's manual form): the outstanding plans' obligations
    // are allocated by the boundary accruals — beyond the anchor
    // SK's frontier. The covering snapshot (a few ticks past the
    // anchor to differentiate the content hash, then the
    // publication) puts T2's identities within the recovered
    // frontier; T3 gets its own below.
    run_ticks_near(&mut spine, &mut director, &near, 5);
    crossing.publish_snapshot(&spine, &director).unwrap();
    // T2 — the second far boundary: the NEW outstanding unconfirmed
    // transaction over the post-reactivation accrual.
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &far,
        &mut journaled,
    );
    let t2 = journaled
        .last()
        .copied()
        .expect("the second far boundary journals");
    assert_ne!(t1, t2);
    // THE ROTATION: the subsumed T1 retires (it stays in the root,
    // byte-untouched); the outstanding T2 re-anchors into the new
    // ACTIVE generation.
    let rotation = crossing.rotate_journal().expect("the rotation completes");
    assert_eq!(rotation.retained, vec![t2], "the outstanding survives");
    assert!(rotation.retired.contains(&t1), "the subsumed retires");
    assert_eq!(active_journal_path(&legacy).unwrap(), rotation.new_path);
    // POST-ROTATION T1 ACTIVITY: the near boundary + accrual + the
    // third far boundary — T3 lands ONLY in the ACTIVE generation.
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &near,
        &mut journaled,
    );
    run_ticks_near(&mut spine, &mut director, &near, 100);
    // T3's covering publication (the same discipline).
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &far,
        &mut journaled,
    );
    let t3 = journaled
        .last()
        .copied()
        .expect("the third far boundary journals");
    let active_ids: BTreeSet<TransactionId> = T1Journal::scan_file(&rotation.new_path)
        .unwrap()
        .committed
        .iter()
        .map(|transaction| transaction.id)
        .collect();
    let root_ids: BTreeSet<TransactionId> = T1Journal::scan_file(&legacy)
        .unwrap()
        .committed
        .iter()
        .map(|transaction| transaction.id)
        .collect();
    assert!(active_ids.contains(&t2) && active_ids.contains(&t3));
    assert!(root_ids.contains(&t1) && !root_ids.contains(&t3));
    assert_ne!(root_ids, active_ids, "the sets genuinely differ");
    drop(crossing);
    // THE ORCHESTRATED RECOVERY over the campaign root: the pointer
    // resolves the ACTIVE generation and the outstanding plans (T2,
    // T3) replay — the coherent recovery.
    let location = StoreLocation::Directory(dir.store());
    let session = OrchestratedSession::recover(&location, &legacy, "pp-b4-fa").unwrap();
    assert_eq!(
        session.region_level(4),
        LodLevel::Dormant,
        "the outstanding plan replayed through the active generation"
    );
    // THE CAPTURE: the frontier equals the ACTIVE generation's
    // committed identities — not the root's (the pre-fix capture
    // scanned the root and carried the retired T1).
    let capture = session.capture_snapshot().unwrap();
    assert_eq!(capture.journal_frontier(), &active_ids);
    assert_ne!(capture.journal_frontier(), &root_ids);
}

/// C3-F3 — the retained completion dispositions: every outcome class
/// (Accepted with the actual tier, Stale with the observed parent,
/// Canceled, Failed with the definite detail, Unknown with the
/// reason) survives the caller's one-shot consumption in the
/// completion ledger, and an unpolled completed slot stays pending
/// with its live reply — recoverable by the future drain, never
/// silently discarded.
#[test]
fn completion_dispositions_survive_the_callers_consumption() {
    let dir = TestDir::new("dispositions");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    // ACCEPTED: consumed by the caller, retained with the tier.
    let handle = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    let (accepted_revision, accepted_tier) = match session.wait_publication(handle).unwrap() {
        PublicationOutcome::Accepted { revision, tier } => (revision, tier),
        other => panic!("the first publication: {other:?}"),
    };
    let record = session.completion_record(handle).unwrap();
    assert_eq!(record.revision, accepted_revision);
    match &record.disposition {
        CompletionDisposition::Accepted { revision, tier } => {
            assert_eq!(*revision, accepted_revision);
            assert_eq!(*tier, accepted_tier);
        }
        other => panic!("the accepted disposition: {other:?}"),
    }
    assert!(!record.coverage.materialized.is_empty());
    // STALE: the captured parent advanced past the confirmation.
    let stale_capture = session.capture_snapshot().unwrap();
    session.tick(&far_presence()).unwrap();
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    let advanced = oracle_head_at(&location);
    let handle = session.submit_publication(stale_capture).unwrap();
    match session.wait_publication(handle).unwrap() {
        PublicationOutcome::Stale { observed, .. } => assert_eq!(observed, Some(advanced)),
        other => panic!("the stale capture: {other:?}"),
    }
    let record = session.completion_record(handle).unwrap();
    match &record.disposition {
        CompletionDisposition::Stale { observed } => assert_eq!(*observed, Some(advanced)),
        other => panic!("the stale disposition: {other:?}"),
    }
    // CANCELED: before the job starts — a definite non-event with
    // the candidate identity still retained.
    let capture = session.capture_snapshot().unwrap();
    session.fault_control().set(WorkerFault::ParkNext);
    let handle = session.submit_publication(capture).unwrap();
    session.cancel_publication(handle);
    session.fault_control().release_park();
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Canceled { .. }
    ));
    let record = session.completion_record(handle).unwrap();
    assert!(matches!(
        record.disposition,
        CompletionDisposition::Canceled
    ));
    assert!(!record.coverage.materialized.is_empty());
    // FAILED: the definite pre-commit fault — nothing installed, the
    // detail retained WITHOUT implying an append committed.
    let head_before = oracle_head_at(&location);
    session
        .fault_control()
        .set(WorkerFault::FailAppendPreCommit);
    let handle = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Failed { .. }
    ));
    let record = session.completion_record(handle).unwrap();
    match &record.disposition {
        CompletionDisposition::Failed { detail } => assert!(!detail.is_empty()),
        other => panic!("the failed disposition: {other:?}"),
    }
    assert_eq!(oracle_head_at(&location), head_before, "nothing installed");
    // UNKNOWN: the lost append reply — identifiable as Unknown AFTER
    // the polling consumed it.
    session.fault_control().set(WorkerFault::LoseAppendReply);
    let handle = session
        .submit_publication(session.capture_snapshot().unwrap())
        .unwrap();
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Unknown { .. }
    ));
    let record = session.completion_record(handle).unwrap();
    assert!(matches!(
        record.disposition,
        CompletionDisposition::Unknown {
            reason: UnknownReason::WorkerReplyLost,
        }
    ));
    // UNPOLLED: a completed-but-unconsumed slot stays pending with
    // its live reply (the future drain's input) — never silently
    // discarded; the late poll recovers it. A FRESH content identity
    // first: the Unknown case's EXECUTED append already installed
    // this state's content key — a transition-free far tick advances
    // the clock (part of the serialized state), giving the two
    // captures a fresh key and keeping the deterministic
    // accept-then-stale pair.
    session.tick(&far_presence()).unwrap();
    let capture_a = session.capture_snapshot().unwrap();
    let capture_b = session.capture_snapshot().unwrap();
    let first = session.submit_publication(capture_a).unwrap();
    let second = session.submit_publication(capture_b).unwrap();
    // FIFO: waiting for the second proves the first already finished.
    assert!(matches!(
        session.wait_publication(second).unwrap(),
        PublicationOutcome::Stale { .. }
    ));
    assert_eq!(
        session.pending_publications(),
        1,
        "the unpolled first stays pending"
    );
    assert!(matches!(
        session.poll_publication(first).unwrap(),
        PublicationOutcome::Accepted { .. }
    ));
    assert_eq!(session.pending_publications(), 0);
    assert!(matches!(
        session.completion_record(first).unwrap().disposition,
        CompletionDisposition::Accepted { .. }
    ));
}

/// C3-F4 — the coherent activation refusal refuses the tick BEFORE
/// the clock: the dormant region's reactivation coherently refused
/// (the accepted director seam, armed pre-open — the encapsulated
/// director admits no mutable escape), the typed orchestration error
/// surfaces with the region safely dormant, the already-completed
/// transitions of the refused tick stand, the clock is unchanged,
/// and the one-shot retry succeeds WITHOUT double application (the
/// clean twin reaches the identical consequential state).
#[test]
fn a_coherent_activation_refusal_refuses_the_tick_before_the_clock() {
    let dir = TestDir::new("activation-refusal");
    let location = found_location(&dir, false);
    // The injection is armed BEFORE the open (the flag consults only
    // the named region's NEXT activation — the far boundary's
    // deactivation never consumes it) and is consumed by the refused
    // attempt (the retry is clean).
    let (spine, mut director) = scenario_pair(150, &near_region_4());
    director.debug_inject_apply_failure(4);
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut session =
        OrchestratedSession::open(spine, director, &location, journal, "pp-b4-orch").unwrap();
    // A genuinely dormant region with a valid checkpoint: the far
    // boundary's six-phase deactivation (the T1 journal transaction
    // carrying region 4's plan and checkpoint).
    session.tick(&far_presence()).unwrap();
    assert_eq!(session.region_level(4), LodLevel::Dormant);
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    assert_eq!(scan.committed.len(), 1);
    assert!(!scan.committed[0].plan.is_empty(), "the checkpoint is real");
    assert!(matches!(
        session.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    let clock_before = session.clock();
    // THE REFUSAL: the near boundary's reactivation coherently
    // refused — the typed orchestration error, BEFORE the clock.
    match session.tick(&near_presence()) {
        Err(TransitionError::ActivationRefused { region: 4 }) => {}
        other => panic!("the coherent refusal is the typed error: {other:?}"),
    }
    assert_eq!(
        session.region_level(4),
        LodLevel::Dormant,
        "nothing half-promoted — the region stays dormant"
    );
    assert_eq!(session.clock(), clock_before, "the clock did not advance");
    // The already-completed transitions of the refused tick stand
    // (region 3's activation preceded region 4 in the loop).
    assert_ne!(
        session.region_level(3),
        LodLevel::Dormant,
        "the completed transitions stand"
    );
    // THE RETRY: the injection is one-shot — the next near boundary
    // activates cleanly, exactly once.
    session.tick(&near_presence()).unwrap();
    assert_ne!(session.region_level(4), LodLevel::Dormant);
    assert_eq!(session.clock().as_u64(), clock_before.as_u64() + 1);
    // WITHOUT DOUBLE APPLICATION — the clean twin: the same scenario
    // with ONE successful near boundary reaches the IDENTICAL
    // consequential coverage, full serialized state, clock, and
    // levels (the refused attempt applied nothing; the retry applied
    // exactly once).
    let twin_dir = TestDir::new("activation-refusal-twin");
    let mut twin = orchestrated(&twin_dir);
    twin.tick(&far_presence()).unwrap();
    assert!(matches!(
        twin.commit_canonical().unwrap(),
        CommitOutcome::Confirmed(_)
    ));
    twin.tick(&near_presence()).unwrap();
    assert_ne!(twin.region_level(4), LodLevel::Dormant);
    assert_eq!(twin.clock().as_u64(), session.clock().as_u64());
    assert_eq!(twin.region_level(4), session.region_level(4));
    let session_capture = session.capture_snapshot().unwrap();
    let twin_capture = twin.capture_snapshot().unwrap();
    assert_eq!(
        session_capture.coverage(),
        twin_capture.coverage(),
        "the consequential coverage matches the clean twin"
    );
    assert_eq!(
        session_capture.content_identity(),
        twin_capture.content_identity(),
        "the full serialized state matches the clean twin (no double application)"
    );
}

/// §3 of the C1 final corrective (the unpolled accepted optional
/// publication): the optional snapshot of state S is submitted and
/// the worker ACCEPTS it, but its completion is deliberately never
/// consumed — the completion path's covering-track seeding never
/// runs. A nonempty consequential deactivation prepared from the
/// UNCHANGED state S must satisfy the C1 prerequisite through the
/// already-durable accepted snapshot — resolved against the chain
/// (the record's idempotency key, its single content reference, and
/// the strict session roundtrip), never the duplicate rejection
/// alone — and never by appending the same content twice. The
/// pending completion stays consumable and the publication
/// accounting is preserved exactly.
#[test]
fn an_unpolled_accepted_publication_covers_the_same_state_prerequisite() {
    let dir = TestDir::new("unpolled-covering");
    let location = found_location(&dir, false);
    let mut session = orchestrated(&dir);
    let founding = oracle_head_at(&location);
    // The optional snapshot of state S — submitted, never consumed.
    let capture = session.capture_snapshot().unwrap();
    let handle = session.submit_publication(capture).unwrap();
    assert_eq!(session.pending_publications(), 1);
    // The worker accepts it (the chain advances past the founding) —
    // the bounded read-only wait on the ORACLE chain, never the
    // session's own completion consumption.
    let deadline = Instant::now() + Duration::from_secs(30);
    while oracle_head_at(&location) == founding {
        assert!(
            Instant::now() < deadline,
            "the worker accepts the optional publication"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        session.pending_publications(),
        1,
        "the completion stays unconsumed"
    );
    // THE CROSSING from the UNCHANGED state S: the far-north-west
    // presence deactivates region 4 as the tick's FIRST transition —
    // every earlier region holds its level (regions 0-2 dormant-stay
    // beyond the event band, region 3 EventDriven-stay inside its
    // hysteresis), and the only other transition — region 5's
    // deactivation — comes later in the region order — so the
    // covering prerequisite serializes exactly the state S the
    // accepted publication carries.
    let north_far = [PresenceSample {
        position: (-150.0, 160.0),
        velocity: (0.0, 0.0),
    }];
    session.tick(&north_far).unwrap();
    assert_eq!(session.region_level(4), LodLevel::Dormant);
    assert_eq!(session.region_level(3), LodLevel::EventDriven);
    // No second append of the same content: the chain holds the
    // founding plus the accepted publication — the covering REUSED it
    // through the chain-verified resolution (the T1 is journal-side).
    let publication_head = oracle_head_at(&location);
    assert_eq!(
        oracle_records_at(&location),
        2,
        "the accepted publication IS the covering — no duplicate append"
    );
    assert_eq!(
        oracle_head_at(&location),
        publication_head,
        "the tick appended nothing to the chain — the prerequisite reused the accepted record"
    );
    {
        // The reused record is the accepted publication's: a
        // content-keyed session snapshot of exactly state S (the
        // pre-boundary state — region 4 still Full).
        let mut oracle = SessionStore::open(&location).unwrap();
        let record = oracle.read_record(&publication_head).unwrap();
        assert_eq!(record.tick, 150);
        let bytes = oracle.read_object(&record.content_refs[0]).unwrap();
        let (_spine, restored_director) = engine_slice::crossing::restore_session(&bytes)
            .expect("the reused covering round-trips");
        assert_eq!(
            restored_director.levels()[4],
            LodLevel::Full,
            "the reused snapshot is the PRE-boundary state S"
        );
    }
    // The pending completion stays consumable — the accounting
    // preserved (the accepted publication reports through its own
    // completion, exactly once).
    assert!(matches!(
        session.wait_publication(handle).unwrap(),
        PublicationOutcome::Accepted { .. }
    ));
    assert_eq!(session.pending_publications(), 0);
}
