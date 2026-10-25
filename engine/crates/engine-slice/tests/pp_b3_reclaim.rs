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

use std::path::PathBuf;

use engine_core::{Encoder, LodLevel, PresenceSample, Spine};
use engine_persist::{
    CanonicalMedium, Hash, LocalFileMedium, MediumError, PipelineError, RevisionRecord,
    StoredObject, T1Journal,
};
use engine_slice::crossing::{SessionCrossing, TransitionError};
use engine_slice::npc::{NpcConfig, NpcWorld, REGION_COUNT, install_initial_schedules};
use engine_slice::region::{RegionConfig, RegionDirector};

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
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, campaign).unwrap();
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

    // THE IDEMPOTENT RE-RUN: the installed audit recognized by the
    // pre-scan; Phase B deletes only what remains — nothing.
    let rerun = recovered_crossing
        .reclaim_checkpoint(&dir.journal(), 1)
        .expect("the re-run completes");
    assert_eq!(rerun.removed_objects, 0, "nothing remained to delete");
    assert_eq!(rerun.ledger, reclamation.ledger, "the same audit");
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let summary = medium.validate().unwrap();
        assert_eq!(summary.reclaimed_records, 2);
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
    // THE IDEMPOTENT RE-RUN.
    let rerun = recovered_crossing
        .reclaim_checkpoint(&dir.journal(), 1)
        .expect("the re-run completes");
    assert_eq!(rerun.removed_objects, 0);
    assert!(rerun.journal_files.is_empty(), "already removed");
}
