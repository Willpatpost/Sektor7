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

use std::path::PathBuf;

use engine_core::{LodLevel, PresenceSample, Spine};
use engine_persist::{
    AwardCheckpoint, CancellationReason, ClassifiedSpan, ConsequenceEnvelope, ConsequenceId,
    CounterValues, Coverage, DurabilityClass, FrontierEntry, OccurrenceSpan, SignalLane,
    SourceBaseline, SourcePosition, T1Journal, TransactionId, TransactionInput,
    active_journal_path,
};
use engine_slice::backup::{backup_campaign, verify_backup};
use engine_slice::crossing::SessionCrossing;
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
