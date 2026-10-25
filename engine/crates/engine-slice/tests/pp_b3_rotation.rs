//! PP-B3 — the durable T1 journal rotation over the REAL crossing
//! machinery (commit 3's acceptance surface): the §4.4 survival
//! classification against the CHAIN-VERIFIED anchor (the newest
//! session snapshot's lane-summary boundary, never the caller's
//! unpublished state) and the chain-derived confirmed set — a
//! transaction retires only when canceled or when every identity is
//! confirmed AND materialized-and-subsumed below the boundary;
//! everything outstanding survives. The mechanism's footprint: the
//! survivors re-anchored in a fresh journal (the identities
//! content-derived, so dedup and replay semantics hold), the legacy
//! journal byte-untouched, the atomic pointer flip, the crossing's
//! pipeline swapped to the new journal, and the recovery following
//! the pointer from the LEGACY path — the outstanding plan replaying
//! through the rotated active journal. And the backup accounting:
//! the capture locks and copies THE ACTIVE journal, never the stale
//! legacy.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;

use engine_core::{LodLevel, PresenceSample, Spine};
use engine_persist::{
    AwardCheckpoint, CancellationReason, ClassifiedSpan, ConsequenceEnvelope, ConsequenceId,
    CounterValues, Coverage, DurabilityClass, FrontierEntry, GENERATIONS_IO_FAULT, JournalError,
    MediumError, OccurrenceSpan, POINTER_DIR_SYNC_FAULT, PipelineError, PointerDurability,
    SignalLane, SourceBaseline, SourcePosition, T1Journal, TransactionId, TransactionInput,
    active_journal_path, read_journal_generations,
};
use engine_slice::backup::{backup_campaign, verify_backup};
use engine_slice::crossing::{ROTATION_SWAP_FAULT, SessionCrossing, TransitionError};
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
            "s7-pp-b3-rotation-{name}-{}",
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

    fn output(&self) -> PathBuf {
        self.0.join("backup")
    }

    /// The campaign root's active-path pointer (the single
    /// authoritative location — the review corrective F1).
    fn active_pointer(&self) -> PathBuf {
        let journal = self.journal();
        let name = journal.file_name().unwrap().to_str().unwrap();
        journal.with_file_name(format!("{name}.active"))
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

/// The real pair (the B2 fixture): 150 ticks under the near presence
/// — region 4 live, five far regions dormant since tick 0.
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

/// The durable boundary (the B2 fixture), COLLECTING the journaled
/// transaction identities — the rotation's classification evidence.
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

/// One foreign pre-seeded transaction (a far-region identity, an
/// empty plan — the recovery's plan-less claim class), canceled
/// before the crossing opens: the classifier's inert branch.
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

// ------------------------------------------------------------- the matrix

#[test]
fn the_rotation_retires_the_subsumed_and_preserves_the_outstanding() {
    // THE full lifecycle: a foreign canceled transaction (retired —
    // inert by construction), a confirmed-materialized-subsumed
    // transaction (retired — every identity confirmed on the chain
    // AND below the checkpoint's verified lane boundary), and an
    // outstanding unconfirmed transaction (retained). The rotation
    // re-anchors the survivor into a fresh journal, leaves the
    // legacy byte-identical, flips the pointer, swaps the crossing's
    // pipeline (new appends land in the new journal), and the
    // recovery — handed the LEGACY path — follows the pointer and
    // replays the outstanding plan.
    let dir = TestDir::new("lifecycle");
    let near = near_region_4();
    let far = far_from_region_4();
    // The pre-seeded journal: the foreign transaction + its
    // cancellation.
    let legacy = dir.journal();
    let foreign_id = {
        let mut journal = T1Journal::open(&legacy).unwrap();
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
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(&legacy).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "rotation-campaign").unwrap();
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
    // boundary reactivates region 4 — the captures apply; the near
    // run accrues the post-reactivation crossing work.
    let engine_persist::CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the captures confirm")
    };
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &near,
        &mut journaled,
    );
    run_ticks_near(&mut spine, &mut director, &near, 100);
    // THE ANCHOR: the checkpoint — the compaction summarizes the
    // applied-and-confirmed identities below the lane boundary, and
    // SK+MK publish the verified anchor on the chain.
    crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the checkpoint publishes the anchor");
    // T2 — the second far boundary: a NEW outstanding unconfirmed
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
    let legacy_bytes = std::fs::read(&legacy).unwrap();

    // THE ROTATION.
    let rotation = crossing.rotate_journal().expect("the rotation completes");

    // THE classification: the subsumed retires, the canceled retires,
    // the outstanding survives.
    assert_eq!(
        rotation.retained,
        vec![t2],
        "the outstanding transaction survives"
    );
    assert_eq!(
        rotation.retired.len(),
        2,
        "the subsumed and the canceled retire: {:?}",
        rotation.retired
    );
    assert!(
        rotation.retired.contains(&t1),
        "the subsumed transaction retires"
    );
    assert!(
        rotation.retired.contains(&foreign_id),
        "the canceled transaction retires"
    );
    // THE mechanism's footprint: the new journal carries exactly the
    // survivor; the legacy is byte-identical; the pointer flipped.
    let scan = T1Journal::scan_file(&rotation.new_path).unwrap();
    assert_eq!(scan.committed.len(), 1);
    assert_eq!(scan.committed[0].id, t2);
    assert!(scan.cancellations.is_empty());
    assert_eq!(
        std::fs::read(&legacy).unwrap(),
        legacy_bytes,
        "the legacy journal is untouched"
    );
    assert_eq!(active_journal_path(&legacy).unwrap(), rotation.new_path);
    // THE SWAP: post-rotation appends land in the NEW journal only.
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &near,
        &mut journaled,
    );
    run_ticks_near(&mut spine, &mut director, &near, 100);
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
    let scan = T1Journal::scan_file(&rotation.new_path).unwrap();
    assert_eq!(
        scan.committed.len(),
        2,
        "the new appends land in the new journal"
    );
    assert_eq!(scan.committed[1].id, t3);
    let legacy_scan = T1Journal::scan_file(&legacy).unwrap();
    assert_eq!(
        legacy_scan.committed.len(),
        3,
        "the legacy journal stands at its pre-rotation state"
    );
    drop(crossing);

    // THE RECOVERY follows the pointer: handed the LEGACY path, the
    // recovery resolves the active journal and replays the
    // outstanding plans (T2 and T3 — region 4's deactivations).
    let (recovered_spine, recovered_director, recovered_crossing) =
        SessionCrossing::recover(&dir.store(), &legacy, "rotation-campaign")
            .expect("the recovery follows the pointer");
    assert_eq!(
        recovered_director.levels()[4],
        LodLevel::Dormant,
        "the outstanding plan replayed through the rotated journal"
    );
    assert_eq!(
        recovered_crossing.pipeline_journal_path(),
        rotation.new_path,
        "the recovered crossing holds the ACTIVE journal"
    );
    drop(recovered_crossing);
    let _ = recovered_spine;
}

#[test]
fn the_backup_captures_the_active_journal_after_a_rotation() {
    // THE backup accounting (the GO's requirement): after a
    // rotation, the capture locks and copies THE ACTIVE journal —
    // the pointer's target — never the stale legacy; the manifest's
    // source-journal provenance names it; the set verifies and
    // restores (the scratch recovery reads the captured journal at
    // the legacy path with no pointer — the pre-rotation
    // compatibility).
    let dir = TestDir::new("backup");
    let near = near_region_4();
    let far = far_from_region_4();
    let db = dir.db();
    let legacy = dir.journal();
    // The pre-seeded canceled foreign transaction: the rotation must
    // drop it, so the ACTIVE journal's bytes genuinely differ from
    // the legacy (with nothing retired the re-anchored survivor set
    // is byte-identical — the deterministic header, chain root, and
    // sequences; the backup test needs the difference to be
    // observable).
    let _foreign_id = {
        let mut journal = T1Journal::open(&legacy).unwrap();
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
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(&legacy).unwrap();
    let mut crossing = SessionCrossing::open_at(
        &StoreLocation::Database(db.clone()),
        journal,
        "rotation-backup",
    )
    .unwrap();
    // The anchor (any published snapshot) + one outstanding
    // unconfirmed transaction.
    crossing.publish_snapshot(&spine, &director).unwrap();
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
        .expect("the far boundary journals");
    let rotation = crossing.rotate_journal().expect("the rotation completes");
    assert_eq!(
        rotation.retained,
        vec![t1],
        "nothing is subsumed without a lane boundary — the unconfirmed transaction survives"
    );
    drop(crossing);

    // THE BACKUP: the capture resolves the pointer and binds the
    // ACTIVE journal.
    let out = dir.output();
    backup_campaign(&db, &legacy, &out).expect("the backup captures the rotated campaign");
    let captured = std::fs::read(out.join("journal.t1")).unwrap();
    let active_bytes = std::fs::read(&rotation.new_path).unwrap();
    assert_eq!(
        captured, active_bytes,
        "the captured journal is the ACTIVE one"
    );
    assert_ne!(
        captured,
        std::fs::read(&legacy).unwrap(),
        "never the stale legacy"
    );
    verify_backup(&out).expect("the set verifies and restores");
}

// ---------------------------------------- the review corrective (F1)

#[test]
fn three_successive_rotations_keep_one_authoritative_active_path() {
    // THE REPEATED ROTATION (the review corrective F1): every
    // rotation publishes its pointer at THE CAMPAIGN ROOT — never
    // beside the rotated file — so no matter how many generations
    // rotate there is exactly ONE authoritative active-path
    // location, and the recovery (handed the root) resolves the
    // newest journal. Three rotations with pending work between
    // them — the outstanding survivors accumulate, every rotation
    // re-anchors them whole; the prior generations are retained
    // (rotation never deletes; the reclamation's audited retirement
    // owns that); then the process restart: the recovery follows
    // the root pointer to the THIRD generation, replays the
    // outstanding plans, accepts additional journal appends, and
    // the backup captures the third generation's bytes.
    let dir = TestDir::new("three-rotations");
    let near = near_region_4();
    let far = far_from_region_4();
    let db = dir.db();
    let legacy = dir.journal();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(&legacy).unwrap();
    let mut crossing = SessionCrossing::open_at(
        &StoreLocation::Database(db.clone()),
        journal,
        "three-rotations",
    )
    .unwrap();
    // THE ANCHOR + the first outstanding transaction.
    crossing.publish_snapshot(&spine, &director).unwrap();
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
        .expect("the far boundary journals");

    // ROTATION 1: the pointer lands at the ROOT.
    let rotation1 = crossing.rotate_journal().expect("the first rotation");
    assert_eq!(rotation1.retained, vec![t1]);
    assert_eq!(
        active_journal_path(&legacy).unwrap(),
        rotation1.new_path,
        "the pointer lands at the campaign root"
    );

    // PENDING WORK between the rotations: the reactivation, the
    // accrual, and a NEW far boundary — journaled into the first
    // generation (the active journal).
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &near,
        &mut journaled,
    );
    run_ticks_near(&mut spine, &mut director, &near, 100);
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
        .expect("the second boundary journals");
    assert_ne!(t1, t2);

    // ROTATION 2: the pointer RE-LANDS at the root (never beside
    // the rotated first generation); the outstanding survivors
    // accumulate — both re-anchor whole.
    let rotation2 = crossing.rotate_journal().expect("the second rotation");
    assert_eq!(rotation2.retained.len(), 2, "t1 and t2 both survive");
    assert!(rotation2.retained.contains(&t1));
    assert!(rotation2.retained.contains(&t2));
    assert_eq!(
        active_journal_path(&legacy).unwrap(),
        rotation2.new_path,
        "the root pointer names the second generation"
    );
    assert_ne!(rotation2.new_path, rotation1.new_path);

    // More pending work + ROTATION 3.
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &near,
        &mut journaled,
    );
    run_ticks_near(&mut spine, &mut director, &near, 100);
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
        .expect("the third boundary journals");
    let rotation3 = crossing.rotate_journal().expect("the third rotation");
    assert_eq!(rotation3.retained.len(), 3, "t1, t2, and t3 all survive");
    assert!(rotation3.retained.contains(&t3));
    assert_eq!(
        active_journal_path(&legacy).unwrap(),
        rotation3.new_path,
        "the root pointer names the third generation"
    );

    // THE PRIOR GENERATIONS retained — the root file and both
    // rotated generations stand.
    assert!(legacy.is_file());
    assert!(rotation1.new_path.is_file());
    assert!(rotation2.new_path.is_file());
    drop(crossing);

    // THE RESTART: handed the ROOT, the recovery resolves the THIRD
    // generation and replays the outstanding plans (region 4's
    // deactivations).
    let (mut recovered_spine, mut recovered_director, mut recovered_crossing) =
        SessionCrossing::recover_at(
            &StoreLocation::Database(db.clone()),
            &legacy,
            "three-rotations",
        )
        .expect("the recovery follows the root pointer through three generations");
    assert_eq!(
        recovered_director.levels()[4],
        LodLevel::Dormant,
        "the outstanding plans replayed through the third generation"
    );
    assert_eq!(
        recovered_crossing.pipeline_journal_path(),
        rotation3.new_path,
        "the recovered crossing holds the THIRD generation"
    );

    // THE POST-RESTART CYCLE: the reactivation installs over the
    // recovered checkpoint; the accrual runs; the far boundary
    // deactivates — an EMPTY-SCRATCH deactivation by the accepted B2
    // design (a post-crash boundary re-derives the LOD against the
    // snapshot; the region's captured content is already durably
    // protected in the outstanding transactions — the boundary never
    // re-journals what the journal already holds).
    let mut post_restart = Vec::new();
    durable_boundary_receipts(
        &mut recovered_crossing,
        &mut recovered_spine,
        &mut recovered_director,
        &near,
        &mut post_restart,
    );
    assert_eq!(
        recovered_director.levels()[4],
        LodLevel::Full,
        "the reactivation installs over the recovered checkpoint"
    );
    run_ticks_near(&mut recovered_spine, &mut recovered_director, &near, 100);
    durable_boundary_receipts(
        &mut recovered_crossing,
        &mut recovered_spine,
        &mut recovered_director,
        &far,
        &mut post_restart,
    );
    assert_eq!(
        recovered_director.levels()[4],
        LodLevel::Dormant,
        "the post-restart far boundary re-derives against the snapshot"
    );
    assert!(
        post_restart.is_empty(),
        "the post-restart boundaries are empty-scratch by the B2 design — the captured content is already durably journaled"
    );

    // THE POST-RESTART JOURNAL EVOLUTION: the fourth rotation through
    // the RECOVERED crossing — the outstanding survivors re-anchor
    // into a fresh journal (real journal content written by the
    // restarted session) and the pointer RE-LANDS at the campaign
    // root: the journal family advances, still ONE authoritative
    // location.
    let rotation4 = recovered_crossing
        .rotate_journal()
        .expect("the post-restart rotation completes");
    assert_eq!(
        rotation4.retained.len(),
        3,
        "the outstanding survivors re-anchor through the restarted session"
    );
    assert!(rotation4.retained.contains(&t1));
    assert_eq!(
        active_journal_path(&legacy).unwrap(),
        rotation4.new_path,
        "the root pointer names the fourth generation"
    );
    assert_ne!(rotation4.new_path, rotation3.new_path);
    let scan = T1Journal::scan_file(&rotation4.new_path).unwrap();
    assert_eq!(
        scan.committed.len(),
        3,
        "the re-anchored survivors land in the fourth generation"
    );
    drop(recovered_crossing);
    drop(recovered_spine);
    drop(recovered_director);

    // THE BACKUP captures the ACTIVE (fourth) generation's bytes.
    let out = dir.output();
    backup_campaign(&db, &legacy, &out).expect("the backup captures the rotated campaign");
    let captured = std::fs::read(out.join("journal.t1")).unwrap();
    assert_eq!(
        captured,
        std::fs::read(&rotation4.new_path).unwrap(),
        "the captured journal is the fourth generation — the newest active path"
    );
    verify_backup(&out).expect("the set verifies and restores");
}

#[test]
fn an_interrupted_later_flip_selects_the_previous_authoritative_journal() {
    // THE INTERRUPTED LATER FLIP (the review corrective F1's crash
    // window): a death between the second rotation's journal
    // publication and its pointer flip leaves the durable state
    // pointing at the FIRST generation — the pre-flip state. The
    // recovery, handed the ROOT, resolves the first generation: a
    // SUPERSET of the second's survivors (rotation re-anchors
    // survivors, never removes them from the prior generation), so
    // the outstanding plans replay correctly and the second
    // generation's file is inert — orphaned, nothing resolves it. A
    // subsequent rotation re-points the root: one authoritative
    // location, always.
    let dir = TestDir::new("interrupted-flip");
    let near = near_region_4();
    let far = far_from_region_4();
    let legacy = dir.journal();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(&legacy).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "interrupted-flip").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
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
        .expect("the far boundary journals");
    // ROTATION 1.
    let rotation1 = crossing.rotate_journal().expect("the first rotation");
    assert_eq!(rotation1.retained, vec![t1]);
    assert_eq!(active_journal_path(&legacy).unwrap(), rotation1.new_path);
    // PENDING WORK into the first generation.
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &near,
        &mut journaled,
    );
    run_ticks_near(&mut spine, &mut director, &near, 100);
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
        .expect("the second boundary journals");
    // ROTATION 2 — completed in-process (its journal file stands on
    // disk, its survivors re-anchored).
    let rotation2 = crossing.rotate_journal().expect("the second rotation");
    assert_eq!(
        rotation2.retained.len(),
        2,
        "both outstanding plans re-anchor into the second generation"
    );
    assert!(rotation2.retained.contains(&t2));
    assert_eq!(
        active_journal_path(&legacy).unwrap(),
        rotation2.new_path,
        "the completed second rotation points the root at its journal"
    );
    // THE INTERRUPTED FLIP: the durable pre-flip state — the root
    // pointer still names the FIRST generation (the crash window
    // between the second journal's publication and its flip,
    // reconstructed by hand).
    let first_name = rotation1.new_path.file_name().unwrap().to_str().unwrap();
    std::fs::write(dir.active_pointer(), first_name).unwrap();
    assert_eq!(
        active_journal_path(&legacy).unwrap(),
        rotation1.new_path,
        "the hand-rewritten pointer reconstructs the pre-flip durable state"
    );
    drop(crossing);

    // THE RECOVERY resolves the FIRST generation — the superset: the
    // outstanding plans replay, and the second generation's file is
    // inert.
    let (recovered_spine, recovered_director, mut recovered_crossing) =
        SessionCrossing::recover(&dir.store(), &legacy, "interrupted-flip")
            .expect("the recovery resolves the previous authoritative journal");
    assert_eq!(
        recovered_director.levels()[4],
        LodLevel::Dormant,
        "the outstanding plans replay from the first generation"
    );
    assert_eq!(
        recovered_crossing.pipeline_journal_path(),
        rotation1.new_path,
        "the previous authoritative journal is selected"
    );
    assert!(
        rotation2.new_path.is_file(),
        "the orphaned second generation stands (inert)"
    );
    drop(recovered_spine);
    drop(recovered_director);

    // A SUBSEQUENT ROTATION re-points the root — the single
    // authoritative location continues.
    let rotation3 = recovered_crossing
        .rotate_journal()
        .expect("the post-recovery rotation completes");
    assert_eq!(
        active_journal_path(&legacy).unwrap(),
        rotation3.new_path,
        "the root pointer re-lands at the campaign root"
    );
    assert_ne!(rotation3.new_path, rotation2.new_path);
}

// ------------------------------------- the review corrective R4's windows

/// The child-process plumbing (the R4 fault-injection regressions):
/// the fault statics are process-global and the harness runs this
/// file's tests in parallel, so every fault-armed scenario runs in
/// its OWN spawned process — the parent verifies the durable
/// outcomes firsthand.
const ROTATION_CHILD_STORE_ENV: &str = "S7_PP_B3_ROTATION_CHILD_STORE";
const ROTATION_CHILD_JOURNAL_ENV: &str = "S7_PP_B3_ROTATION_CHILD_JOURNAL";

/// The R4 child (the pre-flip manifest I/O failure): the campaign,
/// the armed ownership-record fault, the refused rotation, and the
/// post-failure boundary — proving the caller stays operational on
/// the still-authoritative journal.
#[test]
fn pp_b3_rotation_child_manifest_fault() {
    let Ok(store) = std::env::var(ROTATION_CHILD_STORE_ENV) else {
        return;
    };
    let journal_path = std::env::var(ROTATION_CHILD_JOURNAL_ENV).unwrap();
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(&journal_path).unwrap();
    let mut crossing =
        SessionCrossing::open(Path::new(&store), journal, "child-manifest-fault").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    let mut journaled = Vec::new();
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &far,
        &mut journaled,
    );
    // THE INJECTED PRE-FLIP FAILURE: the ownership record's I/O
    // error — with the record maintained BEFORE the flip, the error
    // returns with the authority unchanged.
    GENERATIONS_IO_FAULT.store(true, Ordering::SeqCst);
    let error = crossing
        .rotate_journal()
        .expect_err("the ownership-record I/O failure refuses the rotation");
    assert!(
        matches!(
            error,
            TransitionError::JournalFailed(PipelineError::Journal(JournalError::Io { .. }))
        ),
        "the injected manifest I/O error surfaces: {error:?}"
    );
    // THE AUTHORITY UNCHANGED and the crossing still holds it; a
    // subsequent boundary journals into the still-authoritative
    // journal (never a stale-journal divergence).
    assert_eq!(
        active_journal_path(Path::new(&journal_path)).unwrap(),
        Path::new(&journal_path)
    );
    assert_eq!(crossing.pipeline_journal_path(), Path::new(&journal_path));
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &near,
        &mut journaled,
    );
    run_ticks_near(&mut spine, &mut director, &near, 100);
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &far,
        &mut journaled,
    );
}

#[test]
fn a_manifest_io_failure_before_the_flip_leaves_the_authority_unchanged() {
    // THE R4 PRE-FLIP FAILURE (the review corrective R4): a real
    // I/O error at the ownership-manifest boundary — injected
    // deterministically in a spawned child (the fault statics are
    // process-global; the harness runs this file's tests in
    // parallel) — returns BEFORE the authoritative pointer changes:
    // the child's post-failure boundary lands in the
    // still-authoritative journal, and the parent verifies the
    // pointer unchanged, the recovery resolving the same journal
    // with the plans replayed exactly once, and a retry rotation
    // succeeding.
    let dir = TestDir::new("manifest-io-failure");
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "pp_b3_rotation_child_manifest_fault"])
        .env(ROTATION_CHILD_STORE_ENV, dir.store())
        .env(ROTATION_CHILD_JOURNAL_ENV, dir.journal())
        .status()
        .expect("the child spawns");
    assert_eq!(
        status.code(),
        Some(0),
        "the child's assertion chain passed: the injected manifest I/O failure refused the rotation with the authority unchanged, the crossing still operational on the authoritative journal"
    );
    // THE AUTHORITY UNCHANGED: no pointer — the root IS the active
    // journal — and the child's post-failure T1 evidence landed in
    // it.
    assert_eq!(active_journal_path(&dir.journal()).unwrap(), dir.journal());
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    assert!(
        scan.committed.len() >= 2,
        "the far boundary plus the post-failure boundary journaled into the still-authoritative journal"
    );
    // THE RECOVERY resolves the same journal; the plans replay
    // exactly once; a retry rotation succeeds.
    let (_spine, recovered_director, mut crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "child-manifest-fault")
            .expect("the recovery resolves the unchanged authority");
    assert_eq!(
        recovered_director.levels()[4],
        LodLevel::Dormant,
        "the outstanding plans replayed exactly once"
    );
    let rotation = crossing
        .rotate_journal()
        .expect("the retry rotation succeeds once the fault clears");
    assert_eq!(
        active_journal_path(&dir.journal()).unwrap(),
        rotation.new_path
    );
    assert_eq!(
        rotation.pointer_durability,
        PointerDurability::Established,
        "the retry rotation's flip is durably established"
    );
}

/// The R5 child (the post-flip pointer-durability failure): the
/// campaign, the armed pointer-dir-sync fault, the refused rotation
/// with the crossing FAILING CLOSED, the failing repair and the
/// armed-fault recovery both refusing, and the disarmed repair
/// re-opening the crossing — proving no authoritative T1 write
/// precedes a durably established pointer.
#[test]
fn pp_b3_rotation_child_dir_sync_fault() {
    let Ok(store) = std::env::var(ROTATION_CHILD_STORE_ENV) else {
        return;
    };
    let journal_path = std::env::var(ROTATION_CHILD_JOURNAL_ENV).unwrap();
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(&journal_path).unwrap();
    let mut crossing =
        SessionCrossing::open(Path::new(&store), journal, "child-dir-sync-fault").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    let mut journaled = Vec::new();
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &far,
        &mut journaled,
    );
    // THE INJECTED POST-FLIP DURABILITY FAILURE: the pointer flip
    // succeeds (the live authority IS the new journal) but the
    // directory sync fails — an UNRESOLVED durability is never a
    // durably completed transition; the rotation refuses and the
    // crossing FAILS CLOSED.
    POINTER_DIR_SYNC_FAULT.store(true, Ordering::SeqCst);
    let detail = match crossing.rotate_journal() {
        Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
        Err(other) => panic!("the unresolved durability refuses the rotation, got {other:?}"),
        Ok(_) => panic!("an unresolved pointer durability is never a completed rotation"),
    };
    assert_eq!(detail, "rotation/pointer-durability");
    // THE LIVE AUTHORITY CHANGED: the pointer names the new journal.
    let active = active_journal_path(Path::new(&journal_path)).unwrap();
    assert_ne!(active, Path::new(&journal_path));
    // THE FAIL-CLOSED STATE: no new authoritative T1 transaction may
    // commit while the pointer durability remains unresolved.
    for detail in [
        match crossing.publish_snapshot(&spine, &director) {
            Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
            Err(other) => panic!("the closed publication refuses, got {other:?}"),
            Ok(_) => panic!("no authoritative write precedes the established pointer"),
        },
        match crossing.commit_canonical() {
            Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
            Err(other) => panic!("the closed commit refuses, got {other:?}"),
            Ok(_) => panic!("no authoritative write precedes the established pointer"),
        },
    ] {
        assert_eq!(detail, "rotation/pointer-durability");
    }
    let now = spine.clock();
    match crossing.deactivate(&mut spine, &mut director, 4, now) {
        Err(TransitionError::InstallIncompatible { detail, .. }) => {
            assert_eq!(detail, "rotation/pointer-durability");
        }
        Err(other) => panic!("the closed deactivation refuses, got {other:?}"),
        Ok(_) => panic!("no authoritative T1 capture precedes the established pointer"),
    }
    // THE FAILING REPAIR (the fault still armed): the
    // re-establishment refuses, the fail-closed state intact.
    let error = crossing
        .reestablish_pointer_durability()
        .expect_err("the armed fault refuses the re-establishment");
    assert!(
        matches!(
            error,
            TransitionError::JournalFailed(PipelineError::Journal(JournalError::Io { .. }))
        ),
        "the injected re-sync failure surfaces: {error:?}"
    );
    match crossing.publish_snapshot(&spine, &director) {
        Err(TransitionError::InstallIncompatible { detail, .. }) => {
            assert_eq!(detail, "rotation/pointer-durability");
        }
        Err(other) => panic!("the still-closed publication refuses, got {other:?}"),
        Ok(_) => panic!("the failed repair leaves the crossing closed"),
    }
    // THE ARMED-FAULT RECOVERY REFUSES: a fresh recovery cannot
    // silently clear the unresolved condition merely by reading the
    // visible pointer — the durability verification fails closed.
    let error = match SessionCrossing::recover(
        Path::new(&store),
        Path::new(&journal_path),
        "child-dir-sync-fault",
    ) {
        Err(error) => error,
        Ok(_) => panic!("the armed fault refuses the recovery's durability verification"),
    };
    assert!(
        matches!(
            error,
            TransitionError::JournalFailed(PipelineError::Journal(JournalError::Io { .. }))
        ),
        "the recovery fails closed on the unresolved durability: {error:?}"
    );
    // THE SUCCESSFUL REPAIR (the fault disarmed): the durability
    // re-established, the crossing re-opened — and the new T1
    // write lands in the new authoritative generation.
    POINTER_DIR_SYNC_FAULT.store(false, Ordering::SeqCst);
    crossing
        .reestablish_pointer_durability()
        .expect("the repair re-establishes the pointer durability");
    assert_eq!(crossing.pipeline_journal_path(), active);
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &near,
        &mut journaled,
    );
    run_ticks_near(&mut spine, &mut director, &near, 100);
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &far,
        &mut journaled,
    );
}

#[test]
fn a_pointer_dir_sync_failure_after_the_flip_fails_closed_until_reestablished() {
    // THE R5 POST-FLIP DURABILITY FAILURE (the review corrective
    // R5): a real failure of the pointer directory sync after the
    // rename — injected deterministically in a spawned child — is
    // NEVER a durably completed transition: the child's crossing
    // fails closed (no authoritative T1 write while the pointer
    // durability remains unresolved), the armed repair and the
    // armed-fault recovery both refuse, and only the disarmed
    // repair re-opens the crossing — the child's post-repair T1
    // evidence landing in the new authoritative generation. The
    // parent verifies the durable state, the recovery (the
    // durability verified for real) resolving the same journal with
    // the plans replayed exactly once, and the repeat rotation
    // durably established.
    let dir = TestDir::new("dir-sync-failure");
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "pp_b3_rotation_child_dir_sync_fault"])
        .env(ROTATION_CHILD_STORE_ENV, dir.store())
        .env(ROTATION_CHILD_JOURNAL_ENV, dir.journal())
        .status()
        .expect("the child spawns");
    assert_eq!(
        status.code(),
        Some(0),
        "the child's assertion chain passed: the unresolved durability failed closed, the armed repair and recovery refused, the disarmed repair re-opened the crossing with the post-repair T1 evidence in the new generation"
    );
    // THE PARENT'S DURABLE VERIFICATION: the pointer names the new
    // journal; the child's post-repair T1 evidence is in the new
    // generation and NOT in the stale one.
    let active = active_journal_path(&dir.journal()).unwrap();
    assert_ne!(active, dir.journal());
    let new_scan = T1Journal::scan_file(&active).unwrap();
    assert!(
        new_scan.committed.len() >= 2,
        "the survivor plus the post-repair boundary journaled into the new authoritative generation"
    );
    let old_scan = T1Journal::scan_file(dir.journal()).unwrap();
    let newest = new_scan.committed.last().unwrap().id;
    assert!(
        old_scan.committed.iter().all(|entry| entry.id != newest),
        "the stale journal receives nothing after the flip"
    );
    // THE RECOVERY (the durability verified for real — the fault
    // died with the child): the same journal resolved, the plans
    // replayed exactly once; the repeat rotation durably
    // established.
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director, mut crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "child-dir-sync-fault")
            .expect("the recovery verifies the durability and resolves the new authority");
    assert_eq!(
        director.levels()[4],
        LodLevel::Dormant,
        "the outstanding plans replayed exactly once"
    );
    assert_eq!(crossing.pipeline_journal_path(), active);
    let mut journaled = Vec::new();
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &near,
        &mut journaled,
    );
    run_ticks_near(&mut spine, &mut director, &near, 100);
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &far,
        &mut journaled,
    );
    let rotation = crossing
        .rotate_journal()
        .expect("the repeat rotation succeeds after the verified recovery");
    assert_eq!(
        active_journal_path(&dir.journal()).unwrap(),
        rotation.new_path
    );
    assert_eq!(
        rotation.pointer_durability,
        PointerDurability::Established,
        "the repeat rotation's flip is durably established"
    );
}

/// The R4 child (the post-flip swap failure): the campaign, the
/// armed pipeline-rebuild fault, the refused swap, and the poison —
/// proving every further authoritative operation refuses.
#[test]
fn pp_b3_rotation_child_swap_fault() {
    let Ok(store) = std::env::var(ROTATION_CHILD_STORE_ENV) else {
        return;
    };
    let journal_path = std::env::var(ROTATION_CHILD_JOURNAL_ENV).unwrap();
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(&journal_path).unwrap();
    let mut crossing =
        SessionCrossing::open(Path::new(&store), journal, "child-swap-fault").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    let mut journaled = Vec::new();
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &far,
        &mut journaled,
    );
    // THE INJECTED POST-FLIP SWAP FAILURE.
    ROTATION_SWAP_FAULT.store(true, Ordering::SeqCst);
    let error = crossing
        .rotate_journal()
        .expect_err("the injected pipeline-rebuild failure refuses the swap");
    assert!(
        matches!(
            error,
            TransitionError::JournalFailed(PipelineError::Medium(MediumError::Io { .. }))
        ),
        "the injected swap failure surfaces: {error:?}"
    );
    // THE AUTHORITY HAS CHANGED: the pointer names the new journal.
    assert_ne!(
        active_journal_path(Path::new(&journal_path)).unwrap(),
        Path::new(&journal_path)
    );
    // THE POISON: every further authoritative operation refuses —
    // no T1 write may silently continue through the stale journal.
    for detail in [
        match crossing.publish_snapshot(&spine, &director) {
            Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
            Err(other) => panic!("the poisoned publication refuses, got {other:?}"),
            Ok(_) => panic!("a poisoned crossing never publishes"),
        },
        match crossing.commit_canonical() {
            Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
            Err(other) => panic!("the poisoned commit refuses, got {other:?}"),
            Ok(_) => panic!("a poisoned crossing never confirms"),
        },
        match crossing.reclaim_checkpoint(Path::new(&journal_path), 1) {
            Err(TransitionError::InstallIncompatible { detail, .. }) => detail,
            Err(other) => panic!("the poisoned reclamation refuses, got {other:?}"),
            Ok(_) => panic!("a poisoned crossing never reclaims"),
        },
    ] {
        assert_eq!(detail, "rotation/authority-lost");
    }
    let now = spine.clock();
    match crossing.deactivate(&mut spine, &mut director, 4, now) {
        Err(TransitionError::InstallIncompatible { detail, .. }) => {
            assert_eq!(detail, "rotation/authority-lost");
        }
        Err(other) => panic!("the poisoned deactivation refuses, got {other:?}"),
        Ok(_) => panic!("a poisoned crossing never journals a capture"),
    }
}

#[test]
fn a_pipeline_rebuild_failure_after_the_flip_poisons_the_crossing() {
    // THE R4 POST-FLIP POISON (the review corrective R4): a real
    // failure of the post-flip pipeline reconstruction — injected
    // deterministically in a spawned child — returns the error AND
    // leaves the crossing EXPLICITLY UNUSABLE for further
    // authoritative progression (every mutation refuses; the
    // in-memory writer never continues through the journal the
    // campaign root no longer selects), while the parent's recovery
    // resolves the new authoritative journal, replays the
    // outstanding plan exactly once, and a repeat rotation keeps
    // the lineage coherent.
    let dir = TestDir::new("swap-failure");
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "pp_b3_rotation_child_swap_fault"])
        .env(ROTATION_CHILD_STORE_ENV, dir.store())
        .env(ROTATION_CHILD_JOURNAL_ENV, dir.journal())
        .status()
        .expect("the child spawns");
    assert_eq!(
        status.code(),
        Some(0),
        "the child's assertion chain passed: the injected swap failure refused, the poison refused every further authoritative operation, the pointer named the new journal"
    );
    // THE AUTHORITY HAS CHANGED: the pointer names the new journal.
    let active = active_journal_path(&dir.journal()).unwrap();
    assert_ne!(active, dir.journal());
    // THE RECOVERY resolves the NEW authoritative journal; the plan
    // replays exactly once.
    let (mut spine, mut director, mut crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "child-swap-fault")
            .expect("the recovery resolves the new authority");
    assert_eq!(
        director.levels()[4],
        LodLevel::Dormant,
        "the outstanding plan replayed exactly once through the new authority"
    );
    assert_eq!(crossing.pipeline_journal_path(), active);
    // THE REPEAT ROTATION after the recovery: the lineage coherent.
    let near = near_region_4();
    let far = far_from_region_4();
    let mut journaled = Vec::new();
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &near,
        &mut journaled,
    );
    run_ticks_near(&mut spine, &mut director, &near, 100);
    durable_boundary_receipts(
        &mut crossing,
        &mut spine,
        &mut director,
        &far,
        &mut journaled,
    );
    let rotation = crossing
        .rotate_journal()
        .expect("the repeat rotation succeeds after the recovery");
    assert_eq!(
        active_journal_path(&dir.journal()).unwrap(),
        rotation.new_path
    );
    {
        let lineage = read_journal_generations(&dir.journal()).unwrap();
        let root_name = dir
            .journal()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let active_name = active.file_name().unwrap().to_str().unwrap().to_owned();
        let new_name = rotation
            .new_path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(lineage.contains(&root_name));
        assert!(lineage.contains(&active_name));
        assert!(lineage.contains(&new_name));
    }
}
