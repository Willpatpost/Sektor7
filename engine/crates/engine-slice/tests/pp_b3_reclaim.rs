//! PP-B3 — the audited explicit two-phase reclamation over the REAL
//! crossing machinery (commit 4's acceptance surface): the retention
//! window (the recovery works from the newest SK — only a checkpoint
//! with newer ones above it is eligible); Phase A's verify-before-
//! delete discipline (the compacted walk validates, the SK/MK binding
//! verifies, the digest is RECOMPUTED against the still-present range
//! bytes, and the reclamation-ledger/1 audit revision is appended and
//! read back verified BEFORE any destructive action); Phase B's
//! idempotent whole-revision deletes (the indexed records and their
//! content objects together, a re-run deleting only what remains);
//! the post-conditions (the compacted walk validates over the gapped
//! chain under the skip tables, the membership universe carries the
//! reclaimed identities and keys, and the newest snapshot remains
//! discoverable); the wrong-deletion fail-closed case (a hand-deleted
//! range record with no installed audit is hard corruption, never
//! assumed reclamation); and the full stack — checkpoint, rotation,
//! reclamation — with the old journal file removed only after the
//! rotation's verified active journal stands, and the recovery
//! following the pointer through it all.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;

use engine_core::{Encoder, LodLevel, PresenceSample, Spine};
use engine_persist::local::ARM_RECLAIM_MID_UNIT;
use engine_persist::{
    AppendOutcome, CanonicalMedium, Hash, LocalFileMedium, MediumError, PipelineError,
    RevisionRecord, SqliteMedium, StoredObject, T1Journal,
};
use engine_slice::crossing::{ARM_RECLAIM_AFTER_AUDIT, SessionCrossing, TransitionError};
use engine_slice::npc::{NpcConfig, NpcWorld, REGION_COUNT, install_initial_schedules};
use engine_slice::region::{RegionConfig, RegionDirector};
use engine_slice::store::StoreLocation;

// ------------------------------------------------------------- fixtures

/// A temporary campaign directory (the established suite pattern):
/// a unique child of the system temp dir, removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-pp-b3-reclaim-{name}-{}",
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
        self.0.join("store.db")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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

/// The real pair (the B2 fixture).
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

/// The durable boundary (the B2 fixture).
fn durable_boundary(
    crossing: &mut SessionCrossing,
    spine: &mut Spine<NpcWorld>,
    director: &mut RegionDirector,
    presence: &PresenceSample,
) {
    let now = spine.clock();
    for region in 0..REGION_COUNT {
        let current = director.levels()[region];
        let next = director.target_level(region, std::slice::from_ref(presence));
        if next == current {
            continue;
        }
        if next == LodLevel::Dormant {
            let _journaled = crossing
                .deactivate(spine, director, region, now)
                .expect("the deactivation commits");
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

/// One confirmation cycle (the B2 crash-arc pattern): the far
/// boundary (the deactivation with its captures), the canonical
/// confirmation, the reactivation, and the accrual run — the HWM
/// guard means the NEXT cycle's captures need the post-reactivation
/// accrual.
fn confirm_cycle(
    crossing: &mut SessionCrossing,
    spine: &mut Spine<NpcWorld>,
    director: &mut RegionDirector,
    far: &PresenceSample,
    near: &PresenceSample,
) {
    durable_boundary(crossing, spine, director, far);
    let engine_persist::CommitOutcome::Confirmed(_) = crossing
        .commit_canonical()
        .expect("the cycle's confirmation")
    else {
        panic!("the cycle confirms")
    };
    durable_boundary(crossing, spine, director, near);
    run_ticks_near(spine, director, near, 100);
}

/// A well-formed domain-tagged content object (the reuse-append
/// probe's fresh payload).
fn publication_object(payload: &[u8]) -> StoredObject {
    let mut enc = Encoder::new();
    enc.write_str("test/publication");
    enc.write_bytes(payload);
    StoredObject::new(enc.into_bytes()).unwrap()
}

/// The canonical two-checkpoint campaign: F ← S1 ← C1 ← SK1 ← MK1 ←
/// S3 ← SK2 ← MK2 (the GO's multi-checkpoint example's shape, the
/// lean form): S1 the pre-checkpoint publication; T1 the far
/// boundary's journaled captures, confirmed by C1 and materialized
/// by the reactivation; checkpoint 1 the verified anchor; S3 the
/// second checkpoint's range member; checkpoint 2 the newer anchor.
fn two_checkpoint_campaign(
    dir: &TestDir,
    campaign: &str,
) -> (Spine<NpcWorld>, RegionDirector, SessionCrossing, Hash, Hash) {
    two_checkpoint_campaign_at(&dir.store(), &dir.journal(), campaign)
}

/// The path-based core: the real-child crash tests rebuild this
/// campaign in a spawned process from environment-provided paths,
/// never through the TestDir.
fn two_checkpoint_campaign_at(
    store: &Path,
    journal_path: &Path,
    campaign: &str,
) -> (Spine<NpcWorld>, RegionDirector, SessionCrossing, Hash, Hash) {
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(journal_path).unwrap();
    let mut crossing = SessionCrossing::open(store, journal, campaign).unwrap();
    // S1 — the pre-checkpoint publication.
    crossing.publish_snapshot(&spine, &director).unwrap();
    // T1: the far boundary's captures, then C1's confirmation, then
    // the reactivation materializing them, then the near accrual.
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    let engine_persist::CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the captures confirm")
    };
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    run_ticks_near(&mut spine, &mut director, &near, 100);
    // Checkpoint 1: SK1 (parent C1) + MK1, the range [S1].
    let report1 = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the first checkpoint publishes");
    let s1 = report1.range[0];
    // S3 (parent MK1) + the second window + checkpoint 2: SK2 (parent
    // S3) + MK2, the range [S3]. The tick run comes FIRST: the
    // session state at SK1 and S3 must differ — an identical state
    // derives the same snapshot content hash and the same
    // idempotency key (the duplicate rejection).
    run_ticks_near(&mut spine, &mut director, &near, 20);
    crossing.publish_snapshot(&spine, &director).unwrap();
    run_ticks_near(&mut spine, &mut director, &near, 20);
    crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the second checkpoint publishes");
    // report1.sk and the second checkpoint's identities are consumed
    // by the caller via the reclamation's own report; the caller
    // re-reads what it needs.
    (spine, director, crossing, s1, report1.mk)
}

// ------------------------------------------------------------- the matrix

#[test]
fn the_two_phase_reclamation_retires_the_oldest_range_under_the_window() {
    // THE full lifecycle: the oldest checkpoint's range (the S1
    // publication plus SK1's own retirement — the whole-revision
    // units) is reclaimed under the verify-before-delete discipline;
    // the compacted walk validates over the gapped chain under MK1's
    // skip tables; the membership universe carries the reclaimed
    // identities (S1's key reuse rejected) and epochs (a depends_on
    // resolving via the permanent index); the newest snapshot (SK2)
    // remains the recovery anchor; and the re-run is IDEMPOTENT (the
    // installed audit recognized, nothing left to delete).
    let dir = TestDir::new("lifecycle");
    let (spine, director, mut crossing, s1, mk1) =
        two_checkpoint_campaign(&dir, "reclaim-campaign");
    // S1's idempotency key, captured BEFORE the reclamation (the
    // reuse probe after).
    let s1_key = {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        medium.read_record(&s1).unwrap().idempotency_key.unwrap()
    };
    drop(spine);
    drop(director);

    // THE RECLAMATION (generations = 1: the newest checkpoint
    // remains — the recovery anchor).
    let reclamation = crossing
        .reclaim_checkpoint(&dir.journal(), 1)
        .expect("the two-phase reclamation completes");
    assert_eq!(reclamation.mk, mk1, "the OLDEST checkpoint is the target");
    assert_eq!(
        reclamation.eligible_records, 2,
        "S1 plus SK1's own retirement"
    );
    assert_eq!(
        reclamation.removed_objects, 4,
        "two record objects plus two content objects"
    );
    assert!(
        reclamation.journal_files.is_empty(),
        "no rotation preceded — the legacy journal IS the active one"
    );

    // THE COMPACTED WALK validates over the gapped chain; the
    // membership accounting is honest.
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let summary = medium.validate().unwrap();
        assert_eq!(summary.reclaimed_records, 2);
        assert_eq!(summary.reclaimed_keys, 2);
        // The records: the audit revision, MK2, SK2, S3, MK1, C1, and
        // the founding (S1 and SK1 gone).
        assert_eq!(summary.records, 7);
        // S1's revision is genuinely absent.
        assert!(medium.read_record(&s1).is_err());
    }
    // THE MEMBERSHIP UNIVERSE: S1's key reuse is rejected (the
    // permanent index carries it — the mandatory condition 2).
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let head_epoch = medium.read_record(&head).unwrap().epoch;
        let object = publication_object(b"reuse-payload");
        let record = RevisionRecord::regular(head, head_epoch, 9000)
            .with_idempotency_key(s1_key)
            .with_content_refs(vec![object.hash()]);
        let error = medium.append(&record, &[object], Some(head)).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "duplicate-idempotency-key"
                }
            ),
            "the reclaimed key's reuse is rejected: {error:?}"
        );
    }
    drop(crossing);

    // THE RECOVERY: the newest snapshot (SK2) is the anchor — the
    // retention window's whole point.
    let (recovered_spine, recovered_director, mut recovered_crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "reclaim-campaign")
            .expect("the recovery works from the retained anchor");
    assert_eq!(recovered_director.levels()[4], LodLevel::Full);
    drop(recovered_spine);

    // THE RE-RUN (the review corrective F3's semantics: a COMPLETED
    // reclamation is distinguished from restartable work — the next
    // call finds nothing eligible beyond the window and refuses,
    // the state unchanged): the idempotence outcome.
    let detail = match recovered_crossing.reclaim_checkpoint(&dir.journal(), 1) {
        Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
        Err(other) => panic!("the completed re-run refuses, got {other:?}"),
        Ok(_) => panic!("a completed reclamation never repeats"),
    };
    assert_eq!(detail, "reclaim/nothing-eligible");
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let summary = medium.validate().unwrap();
        assert_eq!(summary.reclaimed_records, 2, "nothing further was deleted");
    }
}

#[test]
fn a_single_checkpoint_never_reclaims_its_own_anchor() {
    // THE retention window: with one checkpoint, its SK is the newest
    // snapshot — the recovery anchor — and is never reclaimed; the
    // reclamation refuses (the newest `generations` checkpoints must
    // remain).
    let dir = TestDir::new("window");
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "window-campaign").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    let engine_persist::CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the captures confirm")
    };
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    run_ticks_near(&mut spine, &mut director, &near, 100);
    crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the checkpoint publishes");
    let detail = match crossing.reclaim_checkpoint(&dir.journal(), 1) {
        Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
        Err(other) => panic!("the window refusal, got {other:?}"),
        Ok(_) => panic!("a single checkpoint must never reclaim its own anchor"),
    };
    assert_eq!(detail, "reclaim/retention-window");
    // And nothing was touched.
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let summary = medium.validate().unwrap();
        assert_eq!(summary.reclaimed_records, 0);
    }
}

#[test]
fn a_hand_deleted_range_record_without_an_audit_fails_closed() {
    // THE wrong-deletion fail-closed case: a range record removed by
    // hand — no installed audit, no reclamation — is hard corruption
    // (an uncovered absence in the chain), never assumed reclamation;
    // the operation refuses before anything else happens.
    let dir = TestDir::new("wrong-deletion");
    let (spine, director, mut crossing, s1, _mk1) =
        two_checkpoint_campaign(&dir, "wrong-deletion-campaign");
    drop(spine);
    drop(director);
    // The hand deletion: S1's whole-revision unit, no audit.
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let content = medium.read_record(&s1).unwrap().content_refs[0];
        let objects = dir.store().join("objects");
        std::fs::remove_file(objects.join(s1.hex())).unwrap();
        std::fs::remove_file(objects.join(content.hex())).unwrap();
    }
    match crossing.reclaim_checkpoint(&dir.journal(), 1) {
        Err(TransitionError::JournalFailed(PipelineError::Medium(
            MediumError::MissingObject { hash },
        ))) => assert_eq!(hash, s1),
        Err(other) => panic!("the uncovered absence fails closed, got {other:?}"),
        Ok(_) => panic!("the hand deletion must fail closed, never pass"),
    }
}

#[test]
fn the_full_stack_rotates_then_reclaims_the_range_and_the_old_journal() {
    // THE full stack: two checkpoints, an outstanding unconfirmed
    // transaction (T3 — the far boundary after checkpoint 2), the
    // ROTATION (T3 survives re-anchored; T1 retires as subsumed),
    // then the RECLAMATION — MK1's range AND the old journal file
    // (removed only after the rotation's verified active journal
    // stands), and the recovery following the pointer: the active
    // journal's outstanding plan replays, the newest snapshot (SK2)
    // anchors, and the re-run is idempotent.
    let dir = TestDir::new("full-stack");
    let far = far_from_region_4();
    let (mut spine, mut director, mut crossing, s1, mk1) =
        two_checkpoint_campaign(&dir, "full-stack-campaign");
    // T3 — the outstanding unconfirmed transaction (the far boundary
    // over the post-reactivation accrual).
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    // THE ROTATION: T3 survives; the subsumed T1 retires.
    let rotation = crossing.rotate_journal().expect("the rotation completes");
    assert_eq!(rotation.retained.len(), 1, "the outstanding T3 survives");
    // THE RECLAMATION: MK1's range plus the old journal file.
    let reclamation = crossing
        .reclaim_checkpoint(&dir.journal(), 1)
        .expect("the reclamation completes");
    assert_eq!(reclamation.mk, mk1);
    assert_eq!(reclamation.eligible_records, 2);
    assert_eq!(reclamation.removed_objects, 4);
    assert_eq!(
        reclamation.journal_files,
        vec![dir.journal()],
        "the old journal file is removed after the verified active journal stands"
    );
    assert!(!dir.journal().is_file(), "the legacy journal is gone");
    assert!(rotation.new_path.is_file(), "the active journal stands");
    // The compacted walk validates; the anchor remains.
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let summary = medium.validate().unwrap();
        assert_eq!(summary.reclaimed_records, 2);
        assert!(medium.read_record(&s1).is_err());
    }
    drop(crossing);
    drop(spine);
    drop(director);
    // THE RECOVERY follows the pointer: the active journal's T3 plan
    // replays (region 4 restored to Dormant), SK2 anchors.
    let (_spine, recovered_director, mut recovered_crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "full-stack-campaign")
            .expect("the recovery follows the pointer");
    assert_eq!(
        recovered_director.levels()[4],
        LodLevel::Dormant,
        "the outstanding plan replayed through the active journal"
    );
    // THE RE-RUN (the F3 semantics): the completed reclamation never
    // repeats — the typed nothing-eligible refusal, the state
    // unchanged (the journal file stays removed).
    let detail = match recovered_crossing.reclaim_checkpoint(&dir.journal(), 1) {
        Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
        Err(other) => panic!("the completed re-run refuses, got {other:?}"),
        Ok(_) => panic!("a completed reclamation never repeats"),
    };
    assert_eq!(detail, "reclaim/nothing-eligible");
    assert!(!dir.journal().is_file(), "the journal file stays removed");
}

#[test]
fn the_multi_checkpoint_example_the_mandatory_retirement_chain() {
    // THE GO's multi-checkpoint example, VERBATIM: F ← S1 ← C1 ←
    // S2 ← S2′ ← C2 ← SK1 ← MK1 ← S3 ← C3 ← S4 ← C4 ← SK2 ← MK2
    // (S3.parent = MK1). The reclamation of MK1's range retires SK1
    // whole with its publications (S1, S2, S2′); SK2 remains the
    // newest discoverable snapshot; MK1/MK2 stand permanent; the
    // walk traverses the self-skip past SK1 and the anchors' skips
    // past the reclaimed publications; the range-1 key reuse is
    // rejected; and a depends_on naming the reclaimed S2′ resolves
    // via MK1's permanent index with the strictly-earlier epoch.
    let dir = TestDir::new("example");
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "example-campaign").unwrap();
    // S1.
    crossing.publish_snapshot(&spine, &director).unwrap();
    // C1 — the first confirmation cycle.
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    // S2, then S2′ (the tick run between them — the states must
    // differ, the snapshot content hash being the idempotency key).
    crossing.publish_snapshot(&spine, &director).unwrap();
    run_ticks_near(&mut spine, &mut director, &near, 20);
    crossing.publish_snapshot(&spine, &director).unwrap();
    // C2 — the second cycle.
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    // SK1 + MK1 — checkpoint 1, the range [S2′, S2, S1] in descent
    // order.
    let report1 = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the first checkpoint publishes");
    assert_eq!(
        report1.range.len(),
        3,
        "the range carries S1, S2, and S2′ (descent order)"
    );
    // S3 — its parent IS MK1 (the example's S3.parent = MK1).
    run_ticks_near(&mut spine, &mut director, &near, 20);
    let s3 = crossing.publish_snapshot(&spine, &director).unwrap();
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        assert_eq!(
            medium.read_record(&s3).unwrap().parent,
            Some(report1.mk),
            "S3.parent = MK1 (the example's shape)"
        );
    }
    // C3 — the third cycle; S4; C4 — the fourth.
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    run_ticks_near(&mut spine, &mut director, &near, 20);
    crossing.publish_snapshot(&spine, &director).unwrap();
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    // SK2 + MK2 — checkpoint 2, the range [S4, S3].
    let report2 = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the second checkpoint publishes");
    assert_eq!(report2.range.len(), 2, "the range carries S3 and S4");
    // The probes' inputs, captured BEFORE the reclamation: S2's
    // idempotency key (the reuse probe) and S2′'s identity (the
    // dependency probe).
    let (s2_key, s2_prime) = {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let s2_prime = report1.range[0];
        let s2 = report1.range[1];
        let key = medium.read_record(&s2).unwrap().idempotency_key.unwrap();
        (key, s2_prime)
    };

    // THE RECLAMATION of MK1's range (generations = 1: SK2 — the
    // newest snapshot — remains the recovery anchor).
    let reclamation = crossing
        .reclaim_checkpoint(&dir.journal(), 1)
        .expect("the example's reclamation completes");
    assert_eq!(reclamation.mk, report1.mk);
    assert_eq!(reclamation.sk, report1.sk);
    assert_eq!(
        reclamation.eligible_records, 4,
        "S1, S2, S2′, and SK1's own retirement entry"
    );
    assert_eq!(reclamation.removed_objects, 8, "the whole-revision units");

    // SK1 RETIRED WHOLE; MK1/MK2 PERMANENT with their metadata
    // objects; the walk validating through the self-skip past SK1
    // and the anchors' skips past the reclaimed publications.
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        assert!(
            medium.read_record(&report1.sk).is_err(),
            "SK1 retired whole"
        );
        let mk1 = medium.read_record(&report1.mk).unwrap();
        let mk2 = medium.read_record(&report2.mk).unwrap();
        medium
            .read_object(mk1.content_refs.first().unwrap())
            .unwrap();
        medium
            .read_object(mk2.content_refs.first().unwrap())
            .unwrap();
        let summary = medium.validate().unwrap();
        assert_eq!(summary.checkpoints, vec![report2.mk, report1.mk]);
        assert_eq!(summary.reclaimed_records, 4);
        assert_eq!(summary.reclaimed_keys, 4);
        assert_eq!(
            summary.records, 11,
            "F, C1, C2, MK1, S3, C3, S4, C4, SK2, MK2, and the audit"
        );
    }
    // THE RANGE-1 KEY REUSE REJECTED — S2's key, preserved by MK1's
    // permanent index (the mandatory condition 2).
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let head_epoch = medium.read_record(&head).unwrap().epoch;
        let object = publication_object(b"example-reuse");
        let record = RevisionRecord::regular(head, head_epoch, 9000)
            .with_idempotency_key(s2_key)
            .with_content_refs(vec![object.hash()]);
        let error = medium.append(&record, &[object], Some(head)).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "duplicate-idempotency-key"
                }
            ),
            "the range-1 key reuse is rejected: {error:?}"
        );
    }
    // THE RECLAIMED DEPENDENCY: a depends_on naming S2′ resolves via
    // MK1's index with the strictly-earlier epoch.
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let head_epoch = medium.read_record(&head).unwrap().epoch;
        let object = publication_object(b"example-dependency");
        let record = RevisionRecord::regular(head, head_epoch, 9001)
            .with_idempotency_key(b"example/dependency-key".to_vec())
            .with_content_refs(vec![object.hash()])
            .with_depends_on(vec![s2_prime]);
        match medium.append(&record, &[object], Some(head)).unwrap() {
            AppendOutcome::Accepted { .. } => {}
            other => panic!("the reclaimed dependency resolves: {other:?}"),
        }
    }
    drop(crossing);
    drop(spine);
    drop(director);
    // SK2 THE NEWEST DISCOVERABLE — the recovery works from it.
    let (_spine, _director, mut recovered_crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "example-campaign")
            .expect("the recovery works from SK2");
    // THE RE-RUN (the F3 semantics): the completed reclamation never
    // repeats — the typed nothing-eligible refusal.
    let detail = match recovered_crossing.reclaim_checkpoint(&dir.journal(), 1) {
        Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
        Err(other) => panic!("the completed re-run refuses, got {other:?}"),
        Ok(_) => panic!("a completed reclamation never repeats"),
    };
    assert_eq!(detail, "reclaim/nothing-eligible");
}

// ---------------------------------------- the review corrective (F2–F4)

/// The child-process plumbing (the F4 real-death evidence): the
/// spawned child rebuilds the campaign at environment-provided
/// paths — never through this process's TestDir — and dies at an
/// armed seam inside the real Phase B.
const RECLAIM_CHILD_STORE_ENV: &str = "S7_PP_B3_RECLAIM_CHILD_STORE";
const RECLAIM_CHILD_JOURNAL_ENV: &str = "S7_PP_B3_RECLAIM_CHILD_JOURNAL";

/// The recursive store size (the F3 non-accumulation measurement).
fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let child = entry.path();
            if child.is_dir() {
                total += dir_size(&child);
            } else {
                total += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// The objects directory's file-name set (the F4 convergence proof:
/// the crashed-and-resumed store against the cleanly reclaimed
/// twin).
fn object_names(store: &Path) -> BTreeSet<String> {
    std::fs::read_dir(store.join("objects"))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn unsafe_retention_windows_are_refused_before_any_audit_or_deletion() {
    // THE zero-generation window (the review corrective F2): it
    // names no protected checkpoint — eventually the newest SK, the
    // only recovery anchor, becomes eligible and the store loses
    // every recoverable snapshot. The refusal is FIRST — before the
    // audit revision, before any destructive action, never a
    // post-deletion check — and the refused request leaves the
    // canonical and journal state unchanged.
    let dir = TestDir::new("unsafe-window");
    let (spine, director, mut crossing, s1, _mk1) = two_checkpoint_campaign(&dir, "unsafe-window");
    drop(spine);
    drop(director);
    let detail = match crossing.reclaim_checkpoint(&dir.journal(), 0) {
        Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
        Err(other) => panic!("the zero window refuses, got {other:?}"),
        Ok(_) => panic!("a zero-generation window must never be accepted"),
    };
    assert_eq!(detail, "reclaim/invalid-window");
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let summary = medium.validate().unwrap();
        assert_eq!(summary.reclaimed_records, 0);
        assert_eq!(
            summary.records, 8,
            "F, S1, C1, SK1, MK1, S3, SK2, MK2 — no audit was appended"
        );
        assert!(
            medium.read_record(&s1).is_ok(),
            "nothing was deleted by the refused request"
        );
    }
    assert!(
        dir.journal().is_file(),
        "the journal is untouched by the refused request"
    );
}

#[test]
fn four_checkpoints_reclaim_sequentially_across_generations() {
    // THE PROGRESSION (the review corrective F3): MK records are
    // permanent — a completed reclamation stays in the summary
    // forever — so the target selection must distinguish a FULLY
    // RECLAIMED range (skip it; never a no-op repeat) from an
    // incomplete one (resume it) and otherwise pick the OLDEST
    // ELIGIBLE UNRECLAIMED range beyond the window. Four
    // checkpoints; three sequential reclamation generations (a
    // two-generation window first, then the minimal one); the
    // permanent MK metadata, the confirming anchors, and the
    // historical key/hash/epoch universe intact across every
    // generation; and the storage measurably NOT accumulating the
    // retired publication snapshots.
    let dir = TestDir::new("sequential");
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "sequential").unwrap();
    // THE CHAIN: F ← S1 ← C1 ← S2 ← C2 ← SK1 ← MK1 ← S3 ← C3 ←
    // SK2 ← MK2 ← S4 ← C4 ← SK3 ← MK3 ← S5 ← C5 ← SK4 ← MK4.
    crossing.publish_snapshot(&spine, &director).unwrap();
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    crossing.publish_snapshot(&spine, &director).unwrap();
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    let report1 = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the first checkpoint publishes");
    run_ticks_near(&mut spine, &mut director, &near, 20);
    crossing.publish_snapshot(&spine, &director).unwrap();
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    let report2 = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the second checkpoint publishes");
    run_ticks_near(&mut spine, &mut director, &near, 20);
    crossing.publish_snapshot(&spine, &director).unwrap();
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    let report3 = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the third checkpoint publishes");
    run_ticks_near(&mut spine, &mut director, &near, 20);
    crossing.publish_snapshot(&spine, &director).unwrap();
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    let report4 = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the fourth checkpoint publishes");
    // MK1's range keys (the reuse probe after the generations) and
    // S3's identity (the dependency probe), captured BEFORE any
    // reclamation.
    let range_keys: Vec<Vec<u8>> = {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        report1
            .range
            .iter()
            .map(|hash| {
                medium
                    .read_record(hash)
                    .unwrap()
                    .idempotency_key
                    .clone()
                    .unwrap()
            })
            .collect()
    };
    let s3 = report2.range[0];
    drop(spine);
    drop(director);

    // GENERATION 1 — the two-generation window (MK4 and MK3
    // protected): the oldest eligible unreclaimed range is MK1's,
    // [S1, S2, and SK1's own retirement entry].
    let size_before = dir_size(&dir.store());
    let r1 = crossing
        .reclaim_checkpoint(&dir.journal(), 2)
        .expect("the first generation reclaims");
    assert_eq!(r1.mk, report1.mk, "the oldest eligible range is MK1's");
    assert_eq!(r1.eligible_records, 3, "S1, S2, and SK1's own entry");
    assert_eq!(r1.removed_objects, 6, "three records plus three objects");
    let size_after_r1 = dir_size(&dir.store());
    assert!(
        size_after_r1 < size_before,
        "the first generation actually reclaims storage"
    );

    // GENERATION 2 — the minimal window (MK4 protected): MK1 is
    // fully reclaimed and SKIPPED, never repeated; the reclamation
    // PROGRESSES to MK2's range.
    let r2 = crossing
        .reclaim_checkpoint(&dir.journal(), 1)
        .expect("the second generation reclaims");
    assert_eq!(
        r2.mk, report2.mk,
        "the reclamation progresses past the completed MK1"
    );
    assert_eq!(r2.eligible_records, 2, "S3 and SK2's own entry");
    assert_eq!(r2.removed_objects, 4);

    // GENERATION 3 — MK3's range.
    let r3 = crossing
        .reclaim_checkpoint(&dir.journal(), 1)
        .expect("the third generation reclaims");
    assert_eq!(r3.mk, report3.mk);
    assert_eq!(r3.eligible_records, 2, "S4 and SK3's own entry");
    assert_eq!(r3.removed_objects, 4);
    let size_after_r3 = dir_size(&dir.store());
    assert!(
        size_after_r3 < size_after_r1,
        "the storage does not accumulate the retired publication snapshots"
    );

    // THE EXHAUSTED RE-RUN: every range beyond the window is fully
    // reclaimed — the typed refusal, the state unchanged.
    let detail = match crossing.reclaim_checkpoint(&dir.journal(), 1) {
        Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
        Err(other) => panic!("the exhausted re-run refuses, got {other:?}"),
        Ok(_) => panic!("the protected window is never reclaimed"),
    };
    assert_eq!(detail, "reclaim/nothing-eligible");

    // THE PERMANENT EVIDENCE across generations: every MK record and
    // its metadata object readable, the walk validating through the
    // chained skip tables, the checkpoint summary and the reclaimed
    // accounting exact.
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        for report in [&report1, &report2, &report3, &report4] {
            let mk = medium.read_record(&report.mk).unwrap();
            medium
                .read_object(mk.content_refs.first().unwrap())
                .unwrap();
        }
        let summary = medium.validate().unwrap();
        assert_eq!(
            summary.checkpoints,
            vec![report4.mk, report3.mk, report2.mk, report1.mk]
        );
        assert_eq!(
            summary.reclaimed_records,
            r1.eligible_records + r2.eligible_records + r3.eligible_records
        );
        // The walk: the three audits, MK4, SK4, C5, S5, MK3, C4,
        // MK2, C3, MK1, C2, C1, and the founding.
        assert_eq!(summary.records, 15);
    }
    // THE HISTORICAL UNIVERSE across generations: a key from MK1's
    // range (reclaimed at generation 1) still rejected after
    // generation 3; a depends_on naming MK2's range member
    // (reclaimed at generation 2) still resolved via the permanent
    // index with the strictly-earlier epoch.
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let head_epoch = medium.read_record(&head).unwrap().epoch;
        let reuse_object = publication_object(b"sequential-reuse");
        let reuse_record = RevisionRecord::regular(head, head_epoch, 9000)
            .with_idempotency_key(range_keys[0].clone())
            .with_content_refs(vec![reuse_object.hash()]);
        let error = medium
            .append(&reuse_record, &[reuse_object], Some(head))
            .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "duplicate-idempotency-key"
                }
            ),
            "MK1's reclaimed key stays rejected across generations: {error:?}"
        );
        let dep_object = publication_object(b"sequential-dependency");
        let dep_record = RevisionRecord::regular(head, head_epoch, 9001)
            .with_idempotency_key(b"sequential/dependency-key".to_vec())
            .with_content_refs(vec![dep_object.hash()])
            .with_depends_on(vec![s3]);
        match medium
            .append(&dep_record, &[dep_object], Some(head))
            .unwrap()
        {
            AppendOutcome::Accepted { .. } => {}
            other => panic!("MK2's reclaimed dependency resolves: {other:?}"),
        }
    }
    drop(crossing);
    // THE RECOVERY from the retained anchor (SK4 — MK4's range is
    // the protected window).
    let (recovered_spine, recovered_director, _crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "sequential")
            .expect("the recovery works across three reclaimed generations");
    assert_eq!(recovered_director.levels()[4], LodLevel::Full);
    drop(recovered_spine);
}

/// The F4 child (the real process death after the audit): the
/// campaign at environment-provided paths, then the armed seam —
/// the process dies between the audit revision's durable
/// installation and Phase B's first deletion.
#[test]
fn pp_b3_reclaim_child_after_audit() {
    let Ok(store) = std::env::var(RECLAIM_CHILD_STORE_ENV) else {
        return;
    };
    let journal = std::env::var(RECLAIM_CHILD_JOURNAL_ENV).unwrap();
    let (spine, director, mut crossing, _s1, _mk1) =
        two_checkpoint_campaign_at(Path::new(&store), Path::new(&journal), "child-after-audit");
    drop(spine);
    drop(director);
    ARM_RECLAIM_AFTER_AUDIT.store(true, Ordering::SeqCst);
    crossing
        .reclaim_checkpoint(Path::new(&journal), 1)
        .expect("the child reaches the armed seam");
    panic!("the armed seam must terminate the child before this point");
}

#[test]
fn a_real_death_after_the_audit_resumes_and_completes_to_convergence() {
    // THE REAL DEATH (the review corrective F4): a genuine child
    // process dies AFTER the audit revision is durably on the chain
    // and BEFORE any destructive Phase B action. The durable state:
    // the audit installed, nothing reclaimed, the authoritative
    // recovery intact. THE RESUME: the recovery plus the same retry
    // — the installed audit is recognized by its deterministic key
    // bound to the reclaimed MK, the deletes run from the audited
    // manifest, and the state converges on the intended reclamation
    // with no second audit. THE IDEMPOTENCE: the re-run after
    // convergence refuses — nothing eligible, nothing further
    // deleted.
    let dir = TestDir::new("death-after-audit");
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "pp_b3_reclaim_child_after_audit"])
        .env(RECLAIM_CHILD_STORE_ENV, dir.store())
        .env(RECLAIM_CHILD_JOURNAL_ENV, dir.journal())
        .status()
        .expect("the child spawns");
    assert_eq!(
        status.code(),
        Some(9),
        "the child died at the after-audit seam"
    );

    // THE DURABLE POST-DEATH STATE: the audit revision installed,
    // nothing yet reclaimed, the authoritative state still
    // validating.
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let summary = medium.validate().unwrap();
        assert_eq!(summary.reclaimed_records, 0);
        assert_eq!(
            summary.records, 9,
            "F, S1, C1, SK1, MK1, S3, SK2, MK2, and the audit"
        );
    }

    // THE RESUME: the recovery from the retained anchor (SK2), then
    // the same reclamation call.
    let (_spine, _director, mut crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "child-after-audit")
            .expect("the post-crash recovery works from SK2");
    let reclamation = crossing
        .reclaim_checkpoint(&dir.journal(), 1)
        .expect("the re-run resumes from the installed audit");
    assert_eq!(reclamation.eligible_records, 2);
    assert_eq!(
        reclamation.removed_objects, 4,
        "the whole-revision units: two records plus two content objects"
    );
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let summary = medium.validate().unwrap();
        assert_eq!(summary.reclaimed_records, 2);
        assert_eq!(
            summary.records, 7,
            "F, C1, MK1, S3, SK2, MK2, and the audit — no second audit"
        );
    }

    // THE CONVERGED IDEMPOTENCE: the next call refuses.
    let detail = match crossing.reclaim_checkpoint(&dir.journal(), 1) {
        Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
        Err(other) => panic!("the converged re-run refuses, got {other:?}"),
        Ok(_) => panic!("a completed reclamation never repeats"),
    };
    assert_eq!(detail, "reclaim/nothing-eligible");
}

/// The F4 child (the real orphaning death mid-unit): the campaign,
/// then the armed seam inside the local medium's deletion loop —
/// the process dies between the first unit's RECORD deletion and
/// its referenced-object deletions, leaving content only the
/// audited manifest can still identify.
#[test]
fn pp_b3_reclaim_child_mid_unit() {
    let Ok(store) = std::env::var(RECLAIM_CHILD_STORE_ENV) else {
        return;
    };
    let journal = std::env::var(RECLAIM_CHILD_JOURNAL_ENV).unwrap();
    let (spine, director, mut crossing, _s1, _mk1) =
        two_checkpoint_campaign_at(Path::new(&store), Path::new(&journal), "child-mid-unit");
    drop(spine);
    drop(director);
    ARM_RECLAIM_MID_UNIT.store(true, Ordering::SeqCst);
    crossing
        .reclaim_checkpoint(Path::new(&journal), 1)
        .expect("the child reaches the armed seam");
    panic!("the armed seam must terminate the child before this point");
}

#[test]
fn a_real_death_mid_unit_converges_from_the_audited_manifest() {
    // THE REAL ORPHANING DEATH (the review corrective F4's central
    // case): a genuine child process dies AFTER the audit and AFTER
    // the first deletion unit's record object is removed but BEFORE
    // its referenced content objects — the exact window where the
    // removed record can no longer identify its own content. THE
    // CONVERGENCE PROOF: a TWIN campaign — the same deterministic
    // fixture reclaimed cleanly — defines the intended storage
    // state; the recovery plus the same retry, deleting from the
    // AUDITED MANIFEST (including the copies the next open's orphan
    // sweep quarantined), converges the crashed store onto the
    // twin's exactly: the same object set, the same head bytes, an
    // empty quarantine — the intended storage state, not merely
    // quarantined orphans.
    let dir = TestDir::new("death-mid-unit");
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "pp_b3_reclaim_child_mid_unit"])
        .env(RECLAIM_CHILD_STORE_ENV, dir.store())
        .env(RECLAIM_CHILD_JOURNAL_ENV, dir.journal())
        .status()
        .expect("the child spawns");
    assert_eq!(
        status.code(),
        Some(10),
        "the child died between the record deletion and its object deletions"
    );

    // THE TWIN: the same campaign, reclaimed cleanly in-process —
    // the intended final storage state.
    let twin = TestDir::new("death-mid-unit-twin");
    let (spine, director, mut twin_crossing, _s1, _mk1) =
        two_checkpoint_campaign(&twin, "child-mid-unit");
    drop(spine);
    drop(director);
    let twin_reclaim = twin_crossing
        .reclaim_checkpoint(&twin.journal(), 1)
        .expect("the twin's reclamation completes");
    assert_eq!(twin_reclaim.eligible_records, 2);
    drop(twin_crossing);

    // THE DURABLE POST-DEATH STATE: the audit installed, the first
    // unit's record gone, its content objects orphaned — and the
    // authoritative state still validating (the walk skips exactly
    // what the audit covers; the still-present SK1 is walked, only
    // S1 is a gap).
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let summary = medium.validate().unwrap();
        assert_eq!(summary.reclaimed_records, 1, "S1 alone is reclaimed so far");
        assert_eq!(
            summary.records, 8,
            "the walk: F, C1, SK1, MK1, S3, SK2, MK2, and the audit"
        );
    }

    // THE RESUME: the recovery plus the same retry — the deletes
    // come from the audited manifest, never a re-read of the removed
    // record; the orphaned copies (wherever the sweep moved them)
    // are removed too.
    let (_spine, _director, mut crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "child-mid-unit")
            .expect("the post-crash recovery works");
    let reclamation = crossing
        .reclaim_checkpoint(&dir.journal(), 1)
        .expect("the re-run resumes from the audited manifest");
    assert_eq!(reclamation.eligible_records, 2);
    assert!(
        reclamation.removed_objects >= 3,
        "the resumed deletes complete the interrupted unit: SK1's record, SK1's object, and S1's orphaned object"
    );

    // THE CONVERGENCE: the object set and the audit head identical
    // to the twin's completed reclamation; the quarantine drained.
    assert_eq!(
        object_names(&dir.store()),
        object_names(&twin.store()),
        "the same retry converges on the intended storage state"
    );
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let mut twin_medium = LocalFileMedium::open(twin.store()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let twin_head = twin_medium.head().unwrap().unwrap();
        assert_eq!(head, twin_head);
        assert_eq!(
            medium.read_record(&head).unwrap().canonical_bytes(),
            twin_medium
                .read_record(&twin_head)
                .unwrap()
                .canonical_bytes(),
            "the audit revision is byte-identical to the twin's"
        );
        let summary = medium.validate().unwrap();
        assert_eq!(summary.reclaimed_records, 2);
        assert_eq!(summary.records, 7);
    }
    let remaining = std::fs::read_dir(dir.store().join("quarantine"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(
        remaining, 0,
        "the orphaned copies are removed, not retained"
    );

    // THE CONVERGED IDEMPOTENCE: the next call refuses.
    let detail = match crossing.reclaim_checkpoint(&dir.journal(), 1) {
        Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
        Err(other) => panic!("the converged re-run refuses, got {other:?}"),
        Ok(_) => panic!("a completed reclamation never repeats"),
    };
    assert_eq!(detail, "reclaim/nothing-eligible");
}

#[test]
fn the_sqlite_provider_reclaims_with_transactional_atomicity() {
    // THE SQLite provider's own atomicity model (the review
    // corrective F4: the two providers differ — the local medium's
    // per-file deletes are restartable via the audited manifest;
    // SQLite's Phase B is ONE transaction, all-or-nothing, so an
    // interrupted process leaves either the pre-transaction state
    // or the completed deletion — never a partial unit, never an
    // orphaned row): the same two-checkpoint reclamation over
    // StoreLocation::Database, the same eligibility and permanent
    // accounting, the same recovery and idempotence.
    let dir = TestDir::new("sqlite-reclaim");
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open_at(
        &StoreLocation::Database(dir.db()),
        journal,
        "sqlite-reclaim",
    )
    .unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the first checkpoint publishes");
    run_ticks_near(&mut spine, &mut director, &near, 20);
    crossing.publish_snapshot(&spine, &director).unwrap();
    run_ticks_near(&mut spine, &mut director, &near, 20);
    crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the second checkpoint publishes");
    drop(spine);
    drop(director);
    let reclamation = crossing
        .reclaim_checkpoint(&dir.journal(), 1)
        .expect("the sqlite reclamation completes");
    assert_eq!(reclamation.eligible_records, 2);
    assert_eq!(
        reclamation.removed_objects, 4,
        "two record rows plus two object rows in one transaction"
    );
    {
        let mut medium = SqliteMedium::open(dir.db()).unwrap();
        let summary = medium.validate().unwrap();
        assert_eq!(summary.reclaimed_records, 2);
        assert_eq!(summary.records, 7);
    }
    drop(crossing);
    let (_spine, _director, mut recovered) = SessionCrossing::recover_at(
        &StoreLocation::Database(dir.db()),
        &dir.journal(),
        "sqlite-reclaim",
    )
    .expect("the sqlite recovery works from the retained anchor");
    let detail = match recovered.reclaim_checkpoint(&dir.journal(), 1) {
        Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
        Err(other) => panic!("the completed sqlite re-run refuses, got {other:?}"),
        Ok(_) => panic!("a completed reclamation never repeats"),
    };
    assert_eq!(detail, "reclaim/nothing-eligible");
}
