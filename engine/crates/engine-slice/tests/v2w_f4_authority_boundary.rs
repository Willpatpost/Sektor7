//! (V2-W F4, the F4-R7..R10 authority-boundary corrective) The
//! publication capability's serialization proof and the pointer-
//! durability repair's authority discipline — the fault-injected
//! arc (this suite runs in its own test binary precisely because
//! the injected `POINTER_DIR_SYNC_FAULT` window must never overlap
//! another test's rotation): a bound campaign whose rotation leaves
//! the pointer durability unresolved fails closed for publication
//! (F4-R7's healthy-authority requirement); the repair's authority
//! probe PROPAGATES every verification failure — the corrupted
//! founding, the corrupted binding record, the corrupted binding
//! content — never downgrading the bound campaign to the legacy
//! journal open (F4-R10); the fully verified repair restores the
//! healthy authority and the publication completes; a subsequent
//! rotation cannot invalidate the published obligations (F4-R7's
//! serialization proof); the positively verified UNBOUND campaign
//! keeps the legitimate legacy repair (F4-R10's positive); and the
//! publication core's permit check refuses a permit held for any
//! other root (F4-R7's inter-crate boundary — without a permit the
//! core does not compile at the call site at all).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use engine_core::{LodLevel, PresenceSample, Spine};
use engine_persist::reclaim::{
    PlanPublicationOutcome, find_installed_journal_root_binding, find_installed_plan,
    founding_identity_medium, verify_bound_journal_authority,
};
use engine_persist::{
    CanonicalMedium, Hash, LocalFileMedium, MediumError, POINTER_DIR_SYNC_FAULT, T1Journal,
    active_journal_path,
};
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
            "s7-v2w-f4-auth-{name}-{}",
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

    fn other_root(&self) -> PathBuf {
        self.0.join("other-root.t1")
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

/// The campaign's installed binding: (founding, root_id, the binding
/// record's id, the binding object's hash).
fn installed_binding_parts(store: &Path) -> (Hash, Hash, Hash, Hash) {
    let mut medium = LocalFileMedium::open(store).unwrap();
    let founding = founding_identity_medium(&mut medium).unwrap();
    let (id, binding) = find_installed_journal_root_binding(&mut medium, &founding)
        .unwrap()
        .expect("the campaign is bound");
    let record = medium.read_record(&id).unwrap();
    let object = record.content_refs[0];
    (founding, binding.root_id, id, object)
}

// ------------------------------------------------------------- the matrix

#[test]
fn the_unresolved_durability_fails_closed_and_the_repair_never_downgrades_to_legacy() {
    // (F4-R7 + F4-R10) THE FAULT-INJECTED ARC: a bound campaign's
    // rotation leaves the pointer durability unresolved — the
    // crossing FAILS CLOSED for the plan publication (the healthy-
    // authority requirement); the repair's authority probe
    // PROPAGATES each verification failure (the corrupted founding
    // record, the corrupted binding record, the corrupted binding
    // content) — the bound campaign never reopens through the
    // legacy journal open, and the fail-closed state survives every
    // failed attempt; the fully verified repair restores the healthy
    // authority, the publication completes, and a subsequent
    // rotation cannot invalidate the published obligations. The
    // positively verified UNBOUND campaign keeps the legitimate
    // legacy repair (the authenticated absence of a binding).
    let dir = TestDir::new("fault-arc");
    let near = near_region_4();
    let far = far_from_region_4();
    let store = dir.store();
    let journal_root = dir.journal();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let mut crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "auth-campaign",
    )
    .unwrap();
    // The anchor + the checkpoint (the publication's MK) + the
    // covering publication for the rotation's outstanding window.
    crossing.publish_snapshot(&spine, &director).unwrap();
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    let report = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the checkpoint publishes");
    let mk = report.mk;
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    run_ticks_near(&mut spine, &mut director, &near, 5);
    crossing.publish_snapshot(&spine, &director).unwrap();
    // THE INJECTED UNRESOLVED DURABILITY: the rotation's pointer flip
    // succeeds but the directory sync fails — the crossing fails
    // closed.
    POINTER_DIR_SYNC_FAULT.store(true, Ordering::SeqCst);
    let detail = match crossing.rotate_journal() {
        Err(engine_slice::crossing::TransitionError::InstallIncompatible { detail, .. }) => detail,
        Err(other) => panic!("the unresolved durability refuses the rotation, got {other:?}"),
        Ok(_rotation) => panic!("an unresolved pointer durability is never a completed rotation"),
    };
    assert_eq!(detail, "rotation/pointer-durability");
    POINTER_DIR_SYNC_FAULT.store(false, Ordering::SeqCst);
    // (F4-R7) THE UNHEALTHY CROSSING REFUSES THE PUBLICATION (no
    // authoritative work precedes the established pointer).
    let error = crossing
        .publish_reclamation_plan_bound(&mk, 1024, 500)
        .unwrap_err();
    assert!(
        matches!(
            &error,
            engine_slice::crossing::TransitionError::InstallIncompatible { detail, .. } if *detail == "rotation/pointer-durability"
        ),
        "the durability-unresolved crossing refuses the publication: {error:?}"
    );
    // (F4-R10) THE THREE CORRUPTION FORMS — each a verification
    // FAILURE the repair must PROPAGATE (never the legacy
    // downgrade, never the healthy restore). The damaged bytes are
    // restored after each attempt (the corruption is the probe, not
    // the state under test).
    let (founding, _root_id, binding_record_id, binding_object) = installed_binding_parts(&store);
    let founding_file = LocalFileMedium::open(&store)
        .unwrap()
        .object_path_pub(&founding);
    let binding_record_file = LocalFileMedium::open(&store)
        .unwrap()
        .object_path_pub(&binding_record_id);
    let binding_object_file = LocalFileMedium::open(&store)
        .unwrap()
        .object_path_pub(&binding_object);
    let founding_bytes = std::fs::read(&founding_file).unwrap();
    let binding_record_bytes = std::fs::read(&binding_record_file).unwrap();
    let binding_object_bytes = std::fs::read(&binding_object_file).unwrap();
    for (label, file, bytes) in [
        (
            "the corrupted founding record",
            &founding_file,
            &founding_bytes,
        ),
        (
            "the corrupted binding record",
            &binding_record_file,
            &binding_record_bytes,
        ),
        (
            "the corrupted binding content",
            &binding_object_file,
            &binding_object_bytes,
        ),
    ] {
        std::fs::write(file, b"corrupted-authority-evidence").unwrap();
        let repair = crossing.reestablish_pointer_durability();
        assert!(
            repair.is_err(),
            "{label}: the failed authority probe propagates (never the legacy downgrade)"
        );
        // THE FAIL-CLOSED STATE SURVIVES the failed repair — the
        // healthy authority is never restored by an unsuccessful
        // attempt.
        let error = crossing
            .publish_reclamation_plan_bound(&mk, 1024, 501)
            .unwrap_err();
        assert!(
            matches!(
                &error,
                engine_slice::crossing::TransitionError::InstallIncompatible { detail, .. } if *detail == "rotation/pointer-durability"
            ),
            "{label}: the crossing stays fail-closed after the failed repair: {error:?}"
        );
        std::fs::write(file, bytes).unwrap();
    }
    // THE FULLY VERIFIED REPAIR: the healthy authority restored.
    crossing
        .reestablish_pointer_durability()
        .expect("the verified repair restores the authoritative progression");
    // (F4-R7) THE VALID CROSSING PUBLISHES (the rotated active — the
    // explicit root lock path).
    let (_plan_id, outcome) = crossing
        .publish_reclamation_plan_bound(&mk, 1024, 502)
        .expect("the repaired crossing publishes under the explicit root lock");
    assert!(
        matches!(outcome, PlanPublicationOutcome::Accepted { .. }),
        "the plan publishes: {outcome:?}"
    );
    // (F4-R7) A SUBSEQUENT ROTATION CANNOT INVALIDATE THE PUBLISHED
    // OBLIGATIONS: every obligation generation still exists, carries
    // the bound identities, and the plan remains the installed
    // authority (the rotation never deletes — the reclamation's
    // audited retirement owns that).
    crossing.rotate_journal().expect("the subsequent rotation");
    {
        let (founding, root_id, _binding_id, _object) = installed_binding_parts(&store);
        let mut medium = LocalFileMedium::open(&store).unwrap();
        let (_, plan) = find_installed_plan(&mut medium, &mk)
            .unwrap()
            .expect("the plan remains installed");
        for name in &plan.journal_files {
            engine_persist::journal::scan_bound_file(
                journal_root.with_file_name(name),
                &founding,
                &root_id,
            )
            .expect("every obligation generation remains present and bound-verified");
        }
    }
    drop(crossing);
    drop(spine);
    drop(director);
    // (F4-R10) THE LEGACY POSITIVE: a positively verified UNBOUND
    // campaign keeps the legitimate legacy repair (the authenticated
    // absence of a binding — the legacy journal open, unchanged).
    {
        let dir = TestDir::new("legacy-repair");
        let store = dir.store();
        let legacy = dir.journal();
        let (mut spine, mut director) = scenario_pair(150, &near);
        let journal = T1Journal::open(&legacy).unwrap();
        let mut crossing =
            SessionCrossing::open(&store, journal, "legacy-repair-campaign").unwrap();
        crossing.publish_snapshot(&spine, &director).unwrap();
        confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
        durable_boundary(&mut crossing, &mut spine, &mut director, &far);
        run_ticks_near(&mut spine, &mut director, &near, 5);
        crossing.publish_snapshot(&spine, &director).unwrap();
        POINTER_DIR_SYNC_FAULT.store(true, Ordering::SeqCst);
        let detail = match crossing.rotate_journal() {
            Err(engine_slice::crossing::TransitionError::InstallIncompatible {
                detail, ..
            }) => detail,
            Err(other) => panic!("the legacy rotation refuses under the fault, got {other:?}"),
            Ok(_rotation) => panic!("the faulted legacy rotation never completes"),
        };
        assert_eq!(detail, "rotation/pointer-durability");
        POINTER_DIR_SYNC_FAULT.store(false, Ordering::SeqCst);
        crossing
            .reestablish_pointer_durability()
            .expect("the unbound campaign's verified repair keeps the legacy path");
        assert_eq!(
            active_journal_path(&legacy).unwrap(),
            crossing.pipeline_journal_path().to_path_buf(),
            "the legacy repair rebuilt onto the pointer-resolved active journal"
        );
    }
}

#[test]
fn the_publication_core_requires_the_permit_for_this_root() {
    // (F4-R7) The inter-crate capability boundary: the publication
    // core takes the NONFORGEABLE, lifetime-bound publication permit
    // — without a held permit the core does not even compile at the
    // call site (the permit type is nonconstructible outside
    // engine-persist), and a permit held for ANY OTHER root refuses
    // before any journal evidence is read (the serialized
    // publication of THIS root cannot be reached through a
    // different root's lock).
    let dir = TestDir::new("permit-boundary");
    let near = near_region_4();
    let far = far_from_region_4();
    let store = dir.store();
    let journal_root = dir.journal();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let mut crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "permit-campaign",
    )
    .unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    let report = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the checkpoint publishes");
    let mk = report.mk;
    drop(crossing);
    drop(spine);
    drop(director);
    let mut medium = LocalFileMedium::open(&store).unwrap();
    let authority = verify_bound_journal_authority(&mut medium, &journal_root).unwrap();
    // THE PERMIT FOR A DIFFERENT ROOT: the lock is genuinely held
    // (the other root's sidecar lock), and the core still refuses —
    // the path agreement is the publication's own root.
    let other_root = dir.other_root();
    let guard = engine_persist::journal::hold_publication_lock(&other_root).unwrap();
    let error = engine_persist::reclaim::publish_reclamation_plan_authorized(
        &mut medium,
        &mk,
        1024,
        &authority,
        &guard.permit(),
        &journal_root,
        503,
    )
    .unwrap_err();
    assert!(
        matches!(
            error,
            MediumError::Chain {
                rule: "reclamation-plan/permit-root-mismatch"
            }
        ),
        "a permit for another root never authorizes this publication: {error:?}"
    );
    assert!(
        find_installed_plan(&mut medium, &mk).unwrap().is_none(),
        "no plan installed through the foreign permit"
    );
    drop(guard);
    // THE CORRECT PERMIT publishes (the standalone policy: the
    // explicitly held root lock).
    let (_id, outcome) = engine_persist::reclaim::publish_reclamation_plan(
        &mut medium,
        &mk,
        1024,
        &journal_root,
        504,
    )
    .unwrap();
    assert!(
        matches!(outcome, PlanPublicationOutcome::Accepted { .. }),
        "the correctly permitted standalone publication completes: {outcome:?}"
    );
}
