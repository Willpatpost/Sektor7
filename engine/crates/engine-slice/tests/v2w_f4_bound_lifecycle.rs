//! (V2-W F4, the F4-R1..F4-R6 corrective) The bound journal
//! lifecycle through the crossing's OWN methods — the §8
//! end-to-end: the trusted bootstrap, the crash-safe continuation,
//! the unrotated and twice-rotated plan publications through the
//! crossing-scoped route (F4-R2/R3 — never reacquiring the root
//! lock the crossing itself holds), the bound rotations preserving
//! the founding hash and root ID through every generation (F4-R1),
//! the restart recovery through the bound branch, the legacy and
//! mismatched-campaign refusals with the state preserved (F4-R5),
//! the crash states of the bootstrap resuming (the founding-only
//! re-entry, the orphan journal, the missing-journal fail-closed),
//! and the non-destruction proof (nothing deleted, no begin, no
//! completion marker — only the plan records added).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use engine_core::{LodLevel, PresenceSample, Spine};
use engine_persist::reclaim::{
    JournalRootBinding, PlanPublicationOutcome, find_installed_completion,
    find_installed_journal_root_binding, find_installed_plan, founding_identity_medium,
    journal_root_id,
};
use engine_persist::{
    AppendOutcome, AuthorityMode, CanonicalMedium, Hash, LocalFileMedium, RevisionRecord,
    T1Journal, active_journal_path,
};
use engine_slice::crossing::{SessionCrossing, TransitionError};
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
            "s7-v2w-f4-{name}-{}",
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

    /// The campaign's journal ROOT candidate (the file the bound
    /// bootstrap creates at, and the legacy open uses).
    fn journal(&self) -> PathBuf {
        self.0.join("journal.t1")
    }

    /// A second root path (the legacy campaign's bound attempt).
    fn bound_journal(&self) -> PathBuf {
        self.0.join("bound-journal.t1")
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

/// The durable boundary (the established suite fixture).
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

/// One confirmation cycle (the established crash-arc fixture): the
/// far boundary's captures, the canonical confirmation, the
/// reactivation, and the accrual run.
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

/// The campaign's installed binding, discovered from the store
/// alone: (founding, root_id).
fn installed_binding(store: &Path) -> (Hash, Hash) {
    let mut medium = LocalFileMedium::open(store).unwrap();
    let founding = founding_identity_medium(&mut medium).unwrap();
    let (_id, binding) = find_installed_journal_root_binding(&mut medium, &founding)
        .unwrap()
        .expect("the campaign is bound");
    (founding, binding.root_id)
}

/// Opens (WITHOUT locking) the journal root's sidecar lock file —
/// the try-lock probe handle for the serialization asserts.
fn root_lock_probe(journal_root: &Path) -> std::fs::File {
    let name = journal_root.file_name().unwrap().to_str().unwrap();
    std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(journal_root.with_file_name(format!("{name}.lock")))
        .expect("the sidecar lock file opens")
}

// ------------------------------------------------------------- the matrix

#[test]
fn the_bound_lifecycle_end_to_end() {
    // §8 THROUGH THE CROSSING'S OWN METHODS: the trusted bootstrap
    // on a truly fresh store; the crash-safe continuation (the
    // completed-bootstrap re-entry); the checkpointed campaign; the
    // UNROTATED plan publication through the crossing-scoped route;
    // two bound rotations (every generation carrying the SAME
    // founding hash and root ID); the restart recovery through the
    // bound branch; the TWICE-ROTATED publication with the two
    // authenticated stale generations as the plan's journal
    // obligations; and the non-destruction proof throughout.
    let dir = TestDir::new("e2e");
    let near = near_region_4();
    let far = far_from_region_4();
    let store = dir.store();
    let journal_root = dir.journal();
    let (mut spine, mut director) = scenario_pair(150, &near);
    // THE FRESH STORE (the bootstrap's precondition).
    {
        let mut probe = LocalFileMedium::open(&store).unwrap();
        assert!(probe.head().unwrap().is_none(), "the store is fresh");
    }
    // §8(a) THE TRUSTED BOOTSTRAP.
    let crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "e2e-campaign",
    )
    .expect("the bound bootstrap opens the fresh campaign");
    {
        let (founding, root_id) = installed_binding(&store);
        let mut probe = LocalFileMedium::open(&store).unwrap();
        let (_id, binding) = find_installed_journal_root_binding(&mut probe, &founding)
            .unwrap()
            .expect("the bootstrap installed the binding");
        assert_eq!(
            binding.location,
            JournalRootBinding::normalize_location(&journal_root)
        );
        assert_eq!(binding.root_id, root_id);
        engine_persist::journal::scan_bound_file(&journal_root, &founding, &root_id)
            .expect("the bound journal carries the bound identities");
    }
    // §8(b) THE CRASH-SAFE CONTINUATION: the completed-bootstrap
    // re-entry (the binding already installed — the resume).
    drop(crossing);
    let mut crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "e2e-campaign",
    )
    .expect("the re-entry resumes the completed bootstrap");
    assert_eq!(
        crossing.pipeline_journal_path(),
        journal_root.as_path(),
        "the unrotated crossing holds the ROOT journal"
    );
    // The campaign: S1 + the confirmed-materialized window +
    // CHECKPOINT 1 (the plan publication's MK).
    crossing.publish_snapshot(&spine, &director).unwrap();
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    let report1 = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the first checkpoint publishes");
    let mk1 = report1.mk;
    // §8(c) THE UNROTATED PUBLICATION through the crossing-scoped
    // route — the crossing's own active-journal handle holds the
    // root's sidecar lock, and the route does NOT reacquire it (the
    // completion of this call is the no-self-deadlock proof).
    let records_before = {
        let mut probe = LocalFileMedium::open(&store).unwrap();
        probe.validate().unwrap().records
    };
    let (plan_id, outcome) = crossing
        .publish_reclamation_plan_bound(&mk1, 1024, 200)
        .expect("the unrotated publication completes without reacquiring the root lock");
    assert!(
        matches!(&outcome, PlanPublicationOutcome::Accepted { id } if *id == plan_id),
        "the unrotated plan publishes: {outcome:?}"
    );
    {
        let mut probe = LocalFileMedium::open(&store).unwrap();
        let (_, plan) = find_installed_plan(&mut probe, &mk1)
            .unwrap()
            .expect("the plan is installed");
        assert!(
            plan.journal_files.is_empty(),
            "the unrotated campaign has no stale generations"
        );
        assert_eq!(
            probe.validate().unwrap().records,
            records_before + 1,
            "only the plan's record was added"
        );
        assert!(
            find_installed_completion(&mut probe, &mk1)
                .unwrap()
                .is_none(),
            "no completion marker"
        );
    }
    // The outstanding work for the rotation: the second far
    // boundary (an unconfirmed capture set) + the covering
    // publication (the frontier guard's discipline).
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    run_ticks_near(&mut spine, &mut director, &near, 5);
    crossing.publish_snapshot(&spine, &director).unwrap();
    // The root's bytes AFTER the outstanding work is journaled (the
    // rotation's untouched-stale proof — captured at the rotation
    // boundary itself).
    let root_bytes_before = std::fs::read(&journal_root).unwrap();
    // §8(d) ROTATION 1 (F4-R1: the bound successor generation).
    let rotation1 = crossing
        .rotate_journal()
        .expect("the first rotation completes");
    assert_ne!(rotation1.new_path, journal_root);
    assert_eq!(
        active_journal_path(&journal_root).unwrap(),
        rotation1.new_path,
        "the pointer lands at the campaign root"
    );
    {
        let (founding, root_id) = installed_binding(&store);
        engine_persist::journal::scan_bound_file(&journal_root, &founding, &root_id)
            .expect("the stale ROOT generation retains the bound identity");
        engine_persist::journal::scan_bound_file(&rotation1.new_path, &founding, &root_id)
            .expect("the first successor generation carries the SAME founding and root ID");
    }
    // The second window (post-rotation): S3 + CHECKPOINT 2.
    run_ticks_near(&mut spine, &mut director, &near, 20);
    crossing.publish_snapshot(&spine, &director).unwrap();
    run_ticks_near(&mut spine, &mut director, &near, 20);
    let report2 = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the second checkpoint publishes (through the rotated journal)");
    let mk2 = report2.mk;
    // §8(e) ROTATION 2 (the twice-rotated campaign).
    let rotation2 = crossing
        .rotate_journal()
        .expect("the second rotation completes");
    assert_ne!(rotation2.new_path, rotation1.new_path);
    assert_eq!(
        active_journal_path(&journal_root).unwrap(),
        rotation2.new_path
    );
    {
        let (founding, root_id) = installed_binding(&store);
        engine_persist::journal::scan_bound_file(&rotation2.new_path, &founding, &root_id)
            .expect("the second successor generation carries the SAME founding and root ID");
    }
    assert_eq!(
        std::fs::read(&journal_root).unwrap(),
        root_bytes_before,
        "the stale ROOT journal is byte-identical (rotation never deletes)"
    );
    // §8(f) THE RESTART RECOVERY through the bound branch: the
    // recovery follows the root pointer to the second successor.
    drop(crossing);
    let (recovered_spine, recovered_director, mut recovered) =
        SessionCrossing::recover(&store, &journal_root, "e2e-campaign")
            .expect("the recovery follows the bound root pointer");
    assert_eq!(
        recovered.pipeline_journal_path(),
        rotation2.new_path.as_path(),
        "the recovered crossing holds the ACTIVE (second successor) journal"
    );
    drop(recovered_spine);
    drop(recovered_director);
    // §8(g) THE TWICE-ROTATED PUBLICATION through the RECOVERED
    // crossing — the explicit root lock (the active is a rotated
    // generation), the obligations the TWO authenticated stale
    // generations.
    let records_before2 = {
        let mut probe = LocalFileMedium::open(&store).unwrap();
        probe.validate().unwrap().records
    };
    let (plan2_id, outcome2) = recovered
        .publish_reclamation_plan_bound(&mk2, 1024, 210)
        .expect("the twice-rotated publication completes under the explicit root lock");
    assert!(
        matches!(&outcome2, PlanPublicationOutcome::Accepted { id } if *id == plan2_id),
        "the twice-rotated plan publishes: {outcome2:?}"
    );
    {
        let (founding, root_id) = installed_binding(&store);
        let mut probe = LocalFileMedium::open(&store).unwrap();
        let (_, plan) = find_installed_plan(&mut probe, &mk2)
            .unwrap()
            .expect("the second plan is installed");
        let root_name = journal_root
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let gen1_name = rotation1
            .new_path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            plan.journal_files.len(),
            2,
            "the two stale generations are the obligations: {:?}",
            plan.journal_files
        );
        assert!(plan.journal_files.contains(&root_name));
        assert!(plan.journal_files.contains(&gen1_name));
        for name in &plan.journal_files {
            engine_persist::journal::scan_bound_file(
                journal_root.with_file_name(name),
                &founding,
                &root_id,
            )
            .expect("every obligation generation carries the bound identity");
        }
        assert_eq!(
            probe.validate().unwrap().records,
            records_before2 + 1,
            "only the second plan's record was added"
        );
        assert!(
            find_installed_completion(&mut probe, &mk2)
                .unwrap()
                .is_none(),
            "no completion marker"
        );
    }
}

#[test]
fn the_live_crossing_publication_never_reacquires_the_root_lock() {
    // (F4-R2) The publication path's serialization boundary: a LIVE
    // unrotated crossing holds the root's sidecar lock through its
    // own active-journal handle for the whole session — the
    // crossing-scoped route publishes WITHOUT reacquiring it (the
    // same-process flock self-deadlock the review found), while the
    // standalone provider-neutral route genuinely BLOCKS against
    // that held lock (the cross-session serialization — no
    // interleaving) and completes once the crossing drops it. After
    // a rotation the crossing holds only the successor generation's
    // lock — the root lock is free at rest (proved by the probe) and
    // the crossing-scoped route takes it explicitly across the
    // collection and the append.
    let dir = TestDir::new("r2");
    let near = near_region_4();
    let far = far_from_region_4();
    let store = dir.store();
    let journal_root = dir.journal();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let mut crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "r2-campaign",
    )
    .unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    let report = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the checkpoint publishes");
    let mk = report.mk;
    // THE CROSSING HOLDS THE ROOT LOCK (the serialization boundary —
    // the probe's non-blocking acquire must refuse).
    {
        let probe = root_lock_probe(&journal_root);
        assert!(
            probe.try_lock().is_err(),
            "the live unrotated crossing holds the root's exclusive sidecar lock"
        );
    }
    // THE CROSSING-SCOPED PUBLICATION does not reacquire (the call
    // completing is the no-self-deadlock proof).
    let (_plan_id, outcome) = crossing
        .publish_reclamation_plan_bound(&mk, 1024, 300)
        .expect("the crossing-scoped publication completes against its own held root lock");
    assert!(
        matches!(outcome, PlanPublicationOutcome::Accepted { .. }),
        "the plan publishes: {outcome:?}"
    );
    // THE STANDALONE ROUTE SERIALIZES: a second publication (the
    // provider-neutral entry) blocks on the root lock the crossing
    // holds, and completes — the exact-content duplicate — once the
    // crossing drops.
    let reached = Arc::new(AtomicBool::new(false));
    let reached_thread = Arc::clone(&reached);
    let thread_store = store.clone();
    let thread_root = journal_root.clone();
    let thread = std::thread::spawn(move || {
        let mut medium = LocalFileMedium::open(&thread_store).unwrap();
        reached_thread.store(true, Ordering::SeqCst);
        engine_persist::reclaim::publish_reclamation_plan(&mut medium, &mk, 1024, &thread_root, 301)
    });
    while !reached.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        !thread.is_finished(),
        "the standalone publication blocks against the live crossing's root lock"
    );
    drop(crossing);
    let (_dup_id, dup_outcome) = thread
        .join()
        .expect("the publication thread completes")
        .expect("the standalone publication succeeds once the lock frees");
    assert!(
        matches!(
            dup_outcome,
            PlanPublicationOutcome::DuplicateVerified { .. }
        ),
        "the identical plan verifies once the lock frees: {dup_outcome:?}"
    );
    // THE ROTATED CROSSING: the re-entry resumes the bound campaign,
    // the rotation moves the active to the successor generation, and
    // the root lock is FREE at rest (the crossing holds only the
    // successor's lock).
    let mut crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "r2-campaign",
    )
    .unwrap();
    let rotation = crossing.rotate_journal().expect("the rotation completes");
    {
        let probe = root_lock_probe(&journal_root);
        assert!(
            probe.try_lock().is_ok(),
            "the rotated crossing does not hold the root lock at rest"
        );
    }
    // The second window's checkpoint (through the rotated journal).
    run_ticks_near(&mut spine, &mut director, &near, 20);
    crossing.publish_snapshot(&spine, &director).unwrap();
    run_ticks_near(&mut spine, &mut director, &near, 20);
    let report2 = crossing
        .publish_checkpoint(&mut spine, &mut director, 10)
        .expect("the post-rotation checkpoint publishes");
    // THE ROTATED PUBLICATION: the explicit root lock (the active is
    // the successor), the stale root generation the obligation.
    let (_plan2_id, outcome2) = crossing
        .publish_reclamation_plan_bound(&report2.mk, 1024, 302)
        .expect("the rotated publication completes under the explicit root lock");
    assert!(
        matches!(outcome2, PlanPublicationOutcome::Accepted { .. }),
        "the rotated plan publishes: {outcome2:?}"
    );
    {
        let mut probe = LocalFileMedium::open(&store).unwrap();
        let (_, plan) = find_installed_plan(&mut probe, &report2.mk)
            .unwrap()
            .expect("the rotated plan is installed");
        let root_name = journal_root
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(plan.journal_files, vec![root_name]);
        let _ = rotation;
    }
}

#[test]
fn the_bound_bootstrap_refuses_legacy_and_mismatched_campaigns() {
    // (F4-R5) The bootstrap's campaign admission: an existing LEGACY
    // campaign (real chain history, a real legacy journal, no
    // binding) never acquires a bound journal through the bootstrap
    // — the refusal is typed, precedes any journal creation, and
    // leaves the campaign's state untouched (the chain, the legacy
    // journal, the absent binding, the uncreated bound path); a
    // mismatched campaign name refuses at the founding's persisted
    // identity.
    let dir = TestDir::new("r5");
    let near = near_region_4();
    let far = far_from_region_4();
    let store = dir.store();
    let legacy = dir.journal();
    let bound_path = dir.bound_journal();
    // THE LEGACY CAMPAIGN with real history: a snapshot, a journaled
    // deactivation (a real transaction in the legacy journal), and
    // the canonical confirmation.
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(&legacy).unwrap();
    let mut crossing = SessionCrossing::open(&store, journal, "legacy-campaign").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    let engine_persist::CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the legacy window confirms")
    };
    drop(crossing);
    drop(spine);
    drop(director);
    // The state capture (the preservation proof).
    let (head_before, records_before, legacy_bytes) = {
        let mut probe = LocalFileMedium::open(&store).unwrap();
        (
            probe.head().unwrap(),
            probe.validate().unwrap().records,
            std::fs::read(&legacy).unwrap(),
        )
    };
    // THE MISMATCHED CAMPAIGN NAME (the founding's persisted
    // identity — refuses before any journal work).
    let error = match SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &bound_path,
        "wrong-name",
    ) {
        Err(error) => error,
        Ok(_crossing) => panic!("the mismatched campaign name never opens"),
    };
    assert!(
        matches!(
            &error,
            TransitionError::InstallIncompatible { detail, .. } if *detail == "journal-root/campaign-mismatch"
        ),
        "the mismatched campaign name refuses: {error:?}"
    );
    // THE LEGACY REFUSAL (the correct name, real history, no
    // binding).
    let error = match SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &bound_path,
        "legacy-campaign",
    ) {
        Err(error) => error,
        Ok(_crossing) => panic!("the legacy campaign never acquires the bound journal"),
    };
    assert!(
        matches!(
            &error,
            TransitionError::InstallIncompatible { detail, .. } if *detail == "journal-root/legacy-unbound"
        ),
        "the legacy campaign refuses the automatic binding: {error:?}"
    );
    // THE STATE PRESERVED: the chain, the legacy journal, the absent
    // binding, the uncreated bound path.
    {
        let mut probe = LocalFileMedium::open(&store).unwrap();
        assert_eq!(probe.head().unwrap(), head_before);
        assert_eq!(probe.validate().unwrap().records, records_before);
        let founding = founding_identity_medium(&mut probe).unwrap();
        assert!(
            find_installed_journal_root_binding(&mut probe, &founding)
                .unwrap()
                .is_none(),
            "no binding was installed"
        );
    }
    assert_eq!(std::fs::read(&legacy).unwrap(), legacy_bytes);
    assert!(
        !bound_path.exists(),
        "the bound journal path was never created"
    );
}

#[test]
fn the_crash_states_of_the_bound_bootstrap_resume() {
    // (§8, F4-R5/R6) The bootstrap's crash matrix through the real
    // entry point: the crash between the founding append and the
    // binding publication (the founding-only chain — the state
    // indistinguishable from fresh) RESUMES and converges; the crash
    // between the bound-journal creation and the binding (the orphan
    // journal) resumes through the strict bound-identity reuse; and
    // the crash-after-binding with the journal REMOVED is missing
    // authoritative evidence — fail closed, never recreated.
    // (a) THE FOUNDING-ONLY CRASH STATE.
    {
        let dir = TestDir::new("crash-founding");
        let store = dir.store();
        let journal_root = dir.bound_journal();
        let mut medium = LocalFileMedium::open(&store).unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "crash-campaign", 0);
        match medium.append(&founding, &[], None).unwrap() {
            AppendOutcome::Accepted { .. } => {}
            _ => panic!("the founding append accepts"),
        }
        drop(medium);
        // No binding, no journal — the crash between the founding
        // append and the binding publication.
        let crossing = SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(store.clone()),
            &journal_root,
            "crash-campaign",
        )
        .expect("the founding-only crash state resumes the bootstrap");
        drop(crossing);
        let (founding_id, root_id) = installed_binding(&store);
        assert_eq!(founding_id, founding.id());
        engine_persist::journal::scan_bound_file(&journal_root, &founding_id, &root_id)
            .expect("the resumed bootstrap created the bound journal");
    }
    // (b) THE ORPHAN-JOURNAL CRASH STATE (the durable bound journal
    // exists, the binding was never published).
    {
        let dir = TestDir::new("crash-orphan");
        let store = dir.store();
        let journal_root = dir.bound_journal();
        let mut medium = LocalFileMedium::open(&store).unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "crash-campaign", 0);
        match medium.append(&founding, &[], None).unwrap() {
            AppendOutcome::Accepted { .. } => {}
            _ => panic!("the founding append accepts"),
        }
        drop(medium);
        let founding_id = founding.id();
        let location = JournalRootBinding::normalize_location(&journal_root);
        let root_id = journal_root_id(&founding_id, &location);
        drop(
            T1Journal::open_bound(&journal_root, &founding_id, &root_id)
                .expect("the orphan bound journal is created"),
        );
        let crossing = SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(store.clone()),
            &journal_root,
            "crash-campaign",
        )
        .expect("the orphan-journal crash state resumes through the strict identity reuse");
        drop(crossing);
        let (installed_founding, installed_root) = installed_binding(&store);
        assert_eq!(installed_founding, founding_id);
        assert_eq!(installed_root, root_id);
    }
    // (c) THE MISSING-JOURNAL CRASH STATE (the binding installed,
    // the journal removed — missing authoritative evidence).
    {
        let dir = TestDir::new("crash-missing");
        let store = dir.store();
        let journal_root = dir.bound_journal();
        let mut medium = LocalFileMedium::open(&store).unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "crash-campaign", 0);
        match medium.append(&founding, &[], None).unwrap() {
            AppendOutcome::Accepted { .. } => {}
            _ => panic!("the founding append accepts"),
        }
        drop(medium);
        {
            let crossing = SessionCrossing::open_bound_bootstrap(
                &StoreLocation::Directory(store.clone()),
                &journal_root,
                "crash-campaign",
            )
            .expect("the bootstrap completes");
            drop(crossing);
        }
        std::fs::remove_file(&journal_root).unwrap();
        let error = match SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(store.clone()),
            &journal_root,
            "crash-campaign",
        ) {
            Err(error) => error,
            Ok(_crossing) => panic!("the missing journal never opens"),
        };
        assert!(
            matches!(
                &error,
                TransitionError::InstallIncompatible { detail, .. } if *detail == "journal-root/missing-journal"
            ),
            "the missing journal after an established binding fails closed: {error:?}"
        );
        // The binding itself is untouched (the fail-closed state —
        // never a silent rebinding elsewhere).
        assert!(installed_binding(&store).0 == founding.id());
    }
}
