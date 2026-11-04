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
    AppendOutcome, AuthorityMode, CanonicalMedium, Hash, LocalFileMedium, MediumError,
    PipelineError, RevisionRecord, T1Journal, active_journal_path,
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
    // (§8, F4-R8/§7 — the JOURNAL-FIRST ordering's crash matrix)
    // through the real entry point: the crash after the journal
    // creation but before the founding append (the truly fresh
    // medium + the durable journal) reuses the journal and proceeds;
    // the crash after the founding append but before the binding
    // publication reuses the journal and publishes; the crash
    // between the founding-only chain and NO journal at the root is
    // REFUSED — the state indistinguishable from a legacy campaign
    // whose historical journal lives elsewhere (the §7 closure: no
    // automatic migration, no history discarded); the crash-after-
    // binding with the journal REMOVED is missing authoritative
    // evidence — fail closed, never recreated; and the PRE-BINDING
    // torn creation resolves (nothing could have been journaled —
    // F4-R9's positive).
    let campaign = "crash-campaign";
    // (a) THE JOURNAL-ONLY CRASH STATE: the durable bound journal
    // exists (the bootstrap's step 2 crashed after the creation),
    // the chain is EMPTY (the founding append never ran).
    {
        let dir = TestDir::new("crash-journal-only");
        let store = dir.store();
        let journal_root = dir.bound_journal();
        let founding_id = RevisionRecord::founding(AuthorityMode::Standalone, campaign, 0).id();
        let location = JournalRootBinding::normalize_location(&journal_root);
        let root_id = journal_root_id(&founding_id, &location);
        {
            let mut medium = LocalFileMedium::open(&store).unwrap();
            drop(
                engine_persist::reclaim::create_campaign_bound_journal(
                    &mut medium,
                    &journal_root,
                    &founding_id,
                )
                .expect("the interrupted bootstrap's journal is created"),
            );
        }
        let crossing = SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(store.clone()),
            &journal_root,
            campaign,
        )
        .expect("the journal-only crash state resumes (the truly fresh medium reuses the journal)");
        drop(crossing);
        let (installed_founding, installed_root) = installed_binding(&store);
        assert_eq!(installed_founding, founding_id);
        assert_eq!(installed_root, root_id);
    }
    // (b) THE JOURNAL + FOUNDING CRASH STATE: the crash after the
    // founding append but before the binding publication (the
    // not-fresh re-entry onto the pre-existing durable residue).
    {
        let dir = TestDir::new("crash-journal-founding");
        let store = dir.store();
        let journal_root = dir.bound_journal();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, campaign, 0);
        let founding_id = founding.id();
        let location = JournalRootBinding::normalize_location(&journal_root);
        let root_id = journal_root_id(&founding_id, &location);
        let mut medium = LocalFileMedium::open(&store).unwrap();
        drop(
            engine_persist::reclaim::create_campaign_bound_journal(
                &mut medium,
                &journal_root,
                &founding_id,
            )
            .expect("the interrupted bootstrap's journal is created"),
        );
        match medium.append(&founding, &[], None).unwrap() {
            AppendOutcome::Accepted { .. } => {}
            _ => panic!("the founding append accepts"),
        }
        drop(medium);
        let crossing = SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(store.clone()),
            &journal_root,
            campaign,
        )
        .expect("the journal+founding crash state resumes onto the pre-existing journal");
        drop(crossing);
        let (installed_founding, installed_root) = installed_binding(&store);
        assert_eq!(installed_founding, founding_id);
        assert_eq!(installed_root, root_id);
    }
    // (c) THE FOUNDING-ONLY, NO-JOURNAL STATE — the §7 closure: the
    // chain carries only its founding (a legacy campaign's exact
    // shape — its historical journal, with real unconfirmed work,
    // lives at a caller-supplied path this entry cannot see), and no
    // bound journal exists at the root. The bootstrap REFUSES the
    // fresh creation: no automatic migration, no history discarded.
    {
        let dir = TestDir::new("crash-founding-only");
        let store = dir.store();
        let journal_root = dir.bound_journal();
        let mut medium = LocalFileMedium::open(&store).unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, campaign, 0);
        match medium.append(&founding, &[], None).unwrap() {
            AppendOutcome::Accepted { .. } => {}
            _ => panic!("the founding append accepts"),
        }
        let head_before = medium.head().unwrap();
        drop(medium);
        let error = match SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(store.clone()),
            &journal_root,
            campaign,
        ) {
            Err(error) => error,
            Ok(_crossing) => panic!("the founding-only chain with no journal never bootstraps"),
        };
        assert!(
            matches!(
                &error,
                TransitionError::InstallIncompatible { detail, .. } if *detail == "journal-root/missing-bootstrap-evidence"
            ),
            "the missing durable bootstrap evidence refuses: {error:?}"
        );
        // THE STATE UNCHANGED: no journal created, no binding, the
        // chain stands at its founding.
        assert!(!journal_root.exists(), "no bound journal was created");
        {
            let mut probe = LocalFileMedium::open(&store).unwrap();
            assert_eq!(probe.head().unwrap(), head_before);
            assert!(
                find_installed_journal_root_binding(&mut probe, &founding.id())
                    .unwrap()
                    .is_none(),
                "no binding was installed"
            );
        }
    }
    // (d) THE MISSING-JOURNAL-AFTER-BINDING CRASH STATE (fail
    // closed, never recreated).
    {
        let dir = TestDir::new("crash-missing");
        let store = dir.store();
        let journal_root = dir.bound_journal();
        let founding_id = {
            let crossing = SessionCrossing::open_bound_bootstrap(
                &StoreLocation::Directory(store.clone()),
                &journal_root,
                campaign,
            )
            .expect("the bootstrap completes");
            drop(crossing);
            installed_binding(&store).0
        };
        std::fs::remove_file(&journal_root).unwrap();
        let error = match SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(store.clone()),
            &journal_root,
            campaign,
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
        assert!(installed_binding(&store).0 == founding_id);
    }
    // (e) THE PRE-BINDING TORN CREATION — the owner's Option B: the
    // conservative ambiguous-recovery policy. A torn bound-header
    // prefix (the crashed creation's residue) is PRESERVED and
    // refused: the automatic repair is prohibited, the damaged
    // bytes stand untouched, and NO canonical progress is installed
    // (no founding appended, no binding published — the chain stays
    // empty). A separately authorized recovery operation applies;
    // it is deliberately not implemented in this checkpoint.
    {
        let dir = TestDir::new("crash-torn-prebinding");
        let store = dir.store();
        let journal_root = dir.bound_journal();
        let founding_id = RevisionRecord::founding(AuthorityMode::Standalone, campaign, 0).id();
        {
            let mut medium = LocalFileMedium::open(&store).unwrap();
            drop(
                engine_persist::reclaim::create_campaign_bound_journal(
                    &mut medium,
                    &journal_root,
                    &founding_id,
                )
                .expect("the journal is created"),
            );
        }
        // The torn prefix: a strict prefix of the canonical bound
        // header (the crashed creation's residue).
        let complete = std::fs::read(&journal_root).unwrap();
        std::fs::write(&journal_root, &complete[..10]).unwrap();
        let torn_bytes = std::fs::read(&journal_root).unwrap();
        // THE CONSERVATIVE REFUSAL through the real entry point.
        let error = match SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(store.clone()),
            &journal_root,
            campaign,
        ) {
            Err(error) => error,
            Ok(_crossing) => panic!("the torn pre-binding creation is never auto-repaired"),
        };
        assert!(
            matches!(
                &error,
                TransitionError::InstallIncompatible { detail, .. } if *detail == "journal-root/residue-torn"
            ),
            "the conservative policy refuses the torn creation prefix: {error:?}"
        );
        // THE EVIDENCE PRESERVED byte-exact; NO canonical progress.
        assert_eq!(
            std::fs::read(&journal_root).unwrap(),
            torn_bytes,
            "the damaged creation evidence is preserved byte-exact"
        );
        {
            // The empty chain is itself the proof: no founding was
            // appended (and with no chain, no binding can exist).
            let mut medium = LocalFileMedium::open(&store).unwrap();
            assert!(
                medium.head().unwrap().is_none(),
                "no founding was appended — the chain stands empty, so no                  journal-root binding can exist"
            );
        }
        // The single creation operation refuses the same state
        // through its own typed admission.
        {
            let mut medium = LocalFileMedium::open(&store).unwrap();
            let error = match engine_persist::reclaim::create_campaign_bound_journal(
                &mut medium,
                &journal_root,
                &founding_id,
            ) {
                Err(engine_persist::reclaim::BoundCreationError::Journal(error)) => error,
                Err(other) => panic!("the torn admission refuses typed: {other:?}"),
                Ok(_journal) => panic!("the torn creation is never auto-repaired"),
            };
            assert!(
                matches!(error, engine_persist::JournalError::CorruptHeader { ref detail } if detail.contains("conservative recovery policy")),
                "the operation's own admission refuses the torn prefix: {error:?}"
            );
        }
        assert_eq!(
            std::fs::read(&journal_root).unwrap(),
            torn_bytes,
            "the damaged evidence still stands"
        );
    }
}

#[test]
fn the_bound_journal_corruption_after_binding_fails_closed() {
    // (F4-R9) The post-binding corruption separation: a valid bound
    // campaign, closed, its active journal's header TRUNCATED to a
    // torn prefix — the bootstrap re-entry, the restart recovery,
    // and the plan publication ALL refuse (the torn header is
    // DAMAGED EVIDENCE, never an inert empty scan, never repaired,
    // never replaced with an empty valid journal), the journal's
    // bytes are preserved untouched for diagnosis, and no
    // retirement authority installs.
    let dir = TestDir::new("r9-corrupt");
    let near = near_region_4();
    let far = far_from_region_4();
    let store = dir.store();
    let journal_root = dir.journal();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let mut crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "r9-campaign",
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
    // THE TORN HEADER: a strict prefix of the canonical bound header
    // (the damaged-evidence shape — indistinguishable from a crashed
    // creation by bytes alone, which is exactly why the installed
    // binding's presence decides the interpretation).
    let complete = std::fs::read(&journal_root).unwrap();
    std::fs::write(&journal_root, &complete[..10]).unwrap();
    let damaged_bytes = std::fs::read(&journal_root).unwrap();
    // (1) THE BOOTSTRAP RE-ENTRY refuses (the damaged evidence is
    // never repaired into an empty valid journal).
    let error = match SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "r9-campaign",
    ) {
        Err(error) => error,
        Ok(_crossing) => panic!("the corrupted journal never re-enters"),
    };
    assert!(
        matches!(
            &error,
            TransitionError::InstallIncompatible { detail, .. } if *detail == "journal-root/active-corrupt"
        ),
        "the torn active journal refuses the re-entry: {error:?}"
    );
    // (2) THE RESTART RECOVERY refuses.
    let error = match SessionCrossing::recover(&store, &journal_root, "r9-campaign") {
        Err(error) => error,
        Ok(_recovered) => panic!("the corrupted journal never recovers"),
    };
    assert!(
        matches!(
            &error,
            TransitionError::InstallIncompatible { detail, .. } if *detail == "journal-root/active-corrupt"
        ),
        "the torn active journal refuses the recovery: {error:?}"
    );
    // (3) THE PLAN PUBLICATION refuses (the collection's
    // identity-verified scan of the active).
    {
        let mut medium = LocalFileMedium::open(&store).unwrap();
        let error = engine_persist::reclaim::publish_reclamation_plan(
            &mut medium,
            &mk,
            1024,
            &journal_root,
            400,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                engine_persist::MediumError::Chain {
                    rule: "journal-root/active-identity"
                }
            ),
            "the torn active journal refuses the plan publication: {error:?}"
        );
        assert!(
            find_installed_plan(&mut medium, &mk).unwrap().is_none(),
            "no retirement authority installed"
        );
    }
    // THE DAMAGED EVIDENCE PRESERVED (nothing repaired, nothing
    // replaced — the bytes stand for diagnosis).
    assert_eq!(std::fs::read(&journal_root).unwrap(), damaged_bytes);
}

#[test]
fn the_open_at_entry_point_joins_bound_campaigns_only_through_their_journal() {
    // (F4 §6) The entry-point closure: an already-bound canonical
    // campaign joins ONLY through its bound journal — a foreign
    // legacy journal refuses before any authoritative progression or
    // rotation; a mismatched campaign name refuses at the founding's
    // persisted identity; the authorized bound journal continues
    // through the correct bound path with the crossing's journal
    // root at the BINDING's authoritative location.
    let dir = TestDir::new("entry-closure");
    let near = near_region_4();
    let far = far_from_region_4();
    let store = dir.store();
    let journal_root = dir.journal();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let mut crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "entry-campaign",
    )
    .unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    drop(crossing);
    drop(spine);
    drop(director);
    let (founding, root_id) = installed_binding(&store);
    // THE FOREIGN LEGACY JOURNAL refuses: a legacy journal at another
    // path never joins a bound campaign.
    {
        let foreign = dir.bound_journal();
        let journal = T1Journal::open(&foreign).unwrap();
        let error = match SessionCrossing::open_at(
            &StoreLocation::Directory(store.clone()),
            journal,
            "entry-campaign",
        ) {
            Err(error) => error,
            Ok(_crossing) => panic!("the foreign legacy journal never joins a bound campaign"),
        };
        assert!(
            matches!(
                &error,
                PipelineError::Medium(MediumError::Chain {
                    rule: "journal-root/foreign-journal"
                })
            ),
            "the foreign journal refuses the bound join: {error:?}"
        );
    }
    // THE MISMATCHED CAMPAIGN NAME refuses (the founding's persisted
    // identity).
    {
        let journal =
            T1Journal::open_bound(&journal_root, &founding, &root_id).expect("the bound journal");
        let error = match SessionCrossing::open_at(
            &StoreLocation::Directory(store.clone()),
            journal,
            "wrong-name",
        ) {
            Err(error) => error,
            Ok(_crossing) => panic!("the mismatched campaign name never opens"),
        };
        assert!(
            matches!(
                &error,
                PipelineError::Medium(MediumError::Chain {
                    rule: "journal-root/campaign-mismatch"
                })
            ),
            "the mismatched name refuses: {error:?}"
        );
    }
    // THE AUTHORIZED BOUND JOURNAL continues through the correct
    // bound path (the crossing's journal root at the binding's
    // authoritative location).
    {
        let journal =
            T1Journal::open_bound(&journal_root, &founding, &root_id).expect("the bound journal");
        let crossing = SessionCrossing::open_at(
            &StoreLocation::Directory(store.clone()),
            journal,
            "entry-campaign",
        )
        .expect("the authorized bound journal joins");
        assert_eq!(
            crossing.pipeline_journal_path(),
            journal_root.as_path(),
            "the bound join holds the bound journal"
        );
        assert_eq!(
            crossing.journal_root_path(),
            journal_root.as_path(),
            "the crossing's journal root is the BINDING's authoritative root"
        );
        drop(crossing);
    }
}

#[test]
fn the_bound_journal_recreation_bypass_fails_closed() {
    // (F4-R11) The low-level recreation bypass closed: a bound
    // campaign with REAL COMMITTED journal history, closed, its
    // active journal REMOVED, then separately TRUNCATED to a valid
    // header prefix — the PUBLIC low-level bound opening (the
    // ordinary open) refuses BOTH (never creating the missing file,
    // never repairing the torn header — no manufactured empty
    // authoritative history), and the crossing re-entry refuses the
    // same states; restoring the genuine evidence re-opens. The
    // legitimate creation paths — the fresh bootstrap, the
    // pre-binding torn resolution, the rotation's unique successor —
    // are proven by the adjacent suite members (the crash matrix,
    // the end-to-end rotations).
    let dir = TestDir::new("r11-bypass");
    let near = near_region_4();
    let far = far_from_region_4();
    let store = dir.store();
    let journal_root = dir.journal();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let mut crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "r11-campaign",
    )
    .unwrap();
    // The real committed journal history: a snapshot publication and
    // a journaled deactivation (a complete committed transaction).
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    drop(crossing);
    drop(spine);
    drop(director);
    let (founding, root_id) = installed_binding(&store);
    let intact_bytes = std::fs::read(&journal_root).unwrap();
    // (1) THE REMOVED JOURNAL: the public low-level bound open
    // refuses and NEVER CREATES the missing file.
    std::fs::remove_file(&journal_root).unwrap();
    let error = match T1Journal::open_bound(&journal_root, &founding, &root_id) {
        Err(error) => error,
        Ok(_journal) => panic!("the ordinary open never creates the missing bound journal"),
    };
    assert!(
        matches!(error, engine_persist::JournalError::CorruptHeader { ref detail } if detail.contains("absent")),
        "the ordinary open never creates the missing bound journal: {error:?}"
    );
    assert!(
        !journal_root.exists(),
        "no journal file was manufactured by the refused open"
    );
    // The crossing re-entry refuses (missing authoritative evidence).
    let error = match SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "r11-campaign",
    ) {
        Err(error) => error,
        Ok(_crossing) => panic!("the removed journal never re-enters"),
    };
    assert!(
        matches!(
            &error,
            TransitionError::InstallIncompatible { detail, .. } if *detail == "journal-root/missing-journal"
        ),
        "the re-entry fails closed on the removed journal: {error:?}"
    );
    assert!(
        !journal_root.exists(),
        "no journal file was manufactured by the refused re-entry"
    );
    // The single creation operation refuses: the installed binding
    // makes the current lifecycle verification — and with it every
    // public creation path — refuse for this campaign.
    {
        let mut medium = LocalFileMedium::open(&store).unwrap();
        let error = match engine_persist::reclaim::create_campaign_bound_journal(
            &mut medium,
            &journal_root,
            &founding,
        ) {
            Err(engine_persist::reclaim::BoundCreationError::Medium(error)) => error,
            Err(other) => {
                panic!("the bound campaign's creation refuses at the lifecycle: {other:?}")
            }
            Ok(_journal) => panic!("the bound campaign's journal is never recreated"),
        };
        assert!(
            matches!(
                error,
                engine_persist::MediumError::Chain {
                    rule: "journal-root/creation-authority-bound"
                }
            ),
            "the bound campaign never authorizes journal creation: {error:?}"
        );
    }
    // (2) THE TRUNCATED JOURNAL (a valid header prefix): the public
    // low-level open refuses without repairing (the damaged
    // evidence preserved).
    std::fs::write(&journal_root, &intact_bytes[..10]).unwrap();
    let damaged_bytes = std::fs::read(&journal_root).unwrap();
    let error = match T1Journal::open_bound(&journal_root, &founding, &root_id) {
        Err(error) => error,
        Ok(_journal) => panic!("the ordinary open never repairs the torn bound header"),
    };
    assert!(
        matches!(error, engine_persist::JournalError::CorruptHeader { ref detail } if detail.contains("torn")),
        "the ordinary open never repairs the torn bound header: {error:?}"
    );
    assert_eq!(
        std::fs::read(&journal_root).unwrap(),
        damaged_bytes,
        "the damaged evidence is preserved byte-exact by the refused open"
    );
    // The crossing re-entry refuses the damaged evidence.
    let error = match SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "r11-campaign",
    ) {
        Err(error) => error,
        Ok(_crossing) => panic!("the truncated journal never re-enters"),
    };
    assert!(
        matches!(
            &error,
            TransitionError::InstallIncompatible { detail, .. } if *detail == "journal-root/active-corrupt"
        ),
        "the re-entry fails closed on the damaged evidence: {error:?}"
    );
    assert_eq!(
        std::fs::read(&journal_root).unwrap(),
        damaged_bytes,
        "the damaged evidence is preserved byte-exact by the refused re-entry"
    );
    // (3) THE RESTORED GENUINE EVIDENCE re-opens (the strict open
    // over the intact committed history).
    std::fs::write(&journal_root, &intact_bytes).unwrap();
    let crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "r11-campaign",
    )
    .expect("the restored genuine evidence re-opens the bound campaign");
    drop(crossing);
}

#[test]
fn the_bootstrap_refuses_pre_binding_journals_with_history() {
    // (F4-R12) The residue's authoritative-history refusal: an
    // existing bound journal at the root is adoptable as
    // interrupted-bootstrap residue ONLY when EMPTY — the trusted
    // ordering journals nothing before the binding. A matching
    // journal ALREADY carrying a committed transaction refuses the
    // EMPTY canonical store and the founding-only chain alike (no
    // founding appended, no binding installed, the bytes preserved);
    // a matching journal with an unresolved non-creation tail fails
    // closed without truncation; the genuine EMPTY matching residue
    // bootstraps.
    let dir = TestDir::new("r12-residue");
    let near = near_region_4();
    let far = far_from_region_4();
    let campaign = "r12-campaign";
    // THE HISTORY JOURNAL: a genuine bound campaign's journal with a
    // real committed transaction (a snapshot publication + a
    // journaled deactivation), left at the root after its crossing
    // closes. Its identities are the deterministic function of the
    // campaign name and the location, so a fresh store presenting
    // the same name and root path proposes EXACTLY this journal as
    // its residue.
    let history_root = dir.journal();
    {
        let store = dir.store();
        let (mut spine, mut director) = scenario_pair(150, &near);
        let mut crossing = SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(store.clone()),
            &history_root,
            campaign,
        )
        .unwrap();
        crossing.publish_snapshot(&spine, &director).unwrap();
        durable_boundary(&mut crossing, &mut spine, &mut director, &far);
        drop(crossing);
        drop(spine);
        drop(director);
    }
    let history_bytes = std::fs::read(&history_root).unwrap();
    // (1) THE EMPTY CANONICAL STORE + the history journal: the
    // residue classification refuses (no founding, no binding).
    {
        let fresh_store = dir.bound_journal().with_extension("store-fresh");
        std::fs::create_dir_all(&fresh_store).unwrap();
        let error = match SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(fresh_store.clone()),
            &history_root,
            campaign,
        ) {
            Err(error) => error,
            Ok(_crossing) => panic!("a journal with committed history is never adopted"),
        };
        assert!(
            matches!(
                &error,
                TransitionError::InstallIncompatible { detail, .. } if *detail == "journal-root/residue-history"
            ),
            "the exact authority boundary — the residue carries history: {error:?}"
        );
        let mut medium = LocalFileMedium::open(&fresh_store).unwrap();
        assert!(
            medium.head().unwrap().is_none(),
            "no founding was appended to the fresh store"
        );
        assert_eq!(
            std::fs::read(&history_root).unwrap(),
            history_bytes,
            "the history journal's bytes are preserved"
        );
    }
    // (2) THE FOUNDING-ONLY CHAIN + the history journal: the same
    // refusal (the founding's presence changes nothing — the
    // residue's history is the refusal's cause).
    {
        let founding_store = dir.bound_journal().with_extension("store-founding");
        std::fs::create_dir_all(&founding_store).unwrap();
        let mut medium = LocalFileMedium::open(&founding_store).unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, campaign, 0);
        match medium.append(&founding, &[], None).unwrap() {
            AppendOutcome::Accepted { .. } => {}
            _ => panic!("the founding append accepts"),
        }
        drop(medium);
        let error = match SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(founding_store.clone()),
            &history_root,
            campaign,
        ) {
            Err(error) => error,
            Ok(_crossing) => panic!("the founding-only chain never adopts the history journal"),
        };
        assert!(
            matches!(
                &error,
                TransitionError::InstallIncompatible { detail, .. } if *detail == "journal-root/residue-history"
            ),
            "the founding-only chain refuses the history residue: {error:?}"
        );
        let mut medium = LocalFileMedium::open(&founding_store).unwrap();
        assert_eq!(
            medium.head().unwrap(),
            Some(founding.id()),
            "the founding-only chain stands unchanged"
        );
        assert!(
            find_installed_journal_root_binding(&mut medium, &founding.id())
                .unwrap()
                .is_none(),
            "no binding was installed"
        );
    }
    // (3) THE UNRESOLVED NON-CREATION TAIL: a matching journal with
    // trailing garbage after the committed history fails closed
    // without truncation (the bytes preserved).
    {
        let mut tailed_bytes = history_bytes.clone();
        tailed_bytes.extend_from_slice(b"unresolved trailing damage");
        std::fs::write(&history_root, &tailed_bytes).unwrap();
        let tail_store = dir.bound_journal().with_extension("store-tail");
        std::fs::create_dir_all(&tail_store).unwrap();
        let error = match SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(tail_store.clone()),
            &history_root,
            campaign,
        ) {
            Err(error) => error,
            Ok(_crossing) => panic!("the unresolved non-creation tail never adopts"),
        };
        assert!(
            matches!(
                &error,
                TransitionError::InstallIncompatible { detail, .. } if *detail == "journal-root/residue-history"
            ),
            "the unresolved non-creation tail fails closed: {error:?}"
        );
        assert_eq!(
            std::fs::read(&history_root).unwrap(),
            tailed_bytes,
            "the damaged residue is preserved byte-exact (never truncated)"
        );
        std::fs::write(&history_root, &history_bytes).unwrap();
    }
    // (4) THE GENUINE EMPTY MATCHING RESIDUE: a fresh campaign whose
    // root carries an EMPTY bound journal with the exact expected
    // identities bootstraps (the interrupted bootstrap's true
    // residue).
    {
        let empty_root = dir.bound_journal().with_extension("empty-root");
        let empty_store = dir.bound_journal().with_extension("store-empty");
        std::fs::create_dir_all(&empty_store).unwrap();
        let founding_id = RevisionRecord::founding(AuthorityMode::Standalone, campaign, 0).id();
        let location = JournalRootBinding::normalize_location(&empty_root);
        let root_id = journal_root_id(&founding_id, &location);
        {
            let mut medium = LocalFileMedium::open(&empty_store).unwrap();
            drop(
                engine_persist::reclaim::create_campaign_bound_journal(
                    &mut medium,
                    &empty_root,
                    &founding_id,
                )
                .expect("the empty residue is created"),
            );
        }
        let crossing = SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(empty_store.clone()),
            &empty_root,
            campaign,
        )
        .expect("the genuine empty matching residue bootstraps");
        drop(crossing);
        let (installed_founding, installed_root) = installed_binding(&empty_store);
        assert_eq!(installed_founding, founding_id);
        assert_eq!(installed_root, root_id);
    }
}

#[test]
fn the_creation_operation_is_current_and_source_bound() {
    // (F4-R11-A) The creation authorization is the CURRENT verified
    // lifecycle of the canonical campaign, verified and consumed
    // indivisibly — no detachable authority exists (the token type
    // is gone from the API; the operation IS the authority). (1) A
    // completed binding makes the operation refuse the campaign's
    // OWN medium for both the removed and the truncated journal
    // (the lifecycle check fires before any file work; the damaged
    // state is preserved, nothing manufactured); (2) an UNRELATED
    // EMPTY medium aimed at the established campaign's root cannot
    // restore the missing journal — the root's prior journal life
    // (the sidecar lock, created once and never unlinked) refuses
    // the fresh-creation branch: a root that had a journal is never
    // a fresh bootstrap's target.
    let dir = TestDir::new("r11a-current");
    let near = near_region_4();
    let far = far_from_region_4();
    let store = dir.store();
    let journal_root = dir.journal();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let mut crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "r11a-campaign",
    )
    .unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    drop(crossing);
    drop(spine);
    drop(director);
    let (founding, _root_id) = installed_binding(&store);
    let intact_bytes = std::fs::read(&journal_root).unwrap();
    // (1a) THE REMOVED JOURNAL + the campaign's OWN medium: the
    // operation refuses at the current lifecycle (the binding IS
    // installed) — never reaching the file work.
    std::fs::remove_file(&journal_root).unwrap();
    {
        let mut medium = LocalFileMedium::open(&store).unwrap();
        let error = match engine_persist::reclaim::create_campaign_bound_journal(
            &mut medium,
            &journal_root,
            &founding,
        ) {
            Err(engine_persist::reclaim::BoundCreationError::Medium(error)) => error,
            Err(other) => panic!("the bound lifecycle refuses the creation: {other:?}"),
            Ok(_journal) => panic!("the removed journal is never recreated"),
        };
        assert!(
            matches!(
                error,
                engine_persist::MediumError::Chain {
                    rule: "journal-root/creation-authority-bound"
                }
            ),
            "the current lifecycle refuses the bound campaign's creation: {error:?}"
        );
        assert!(!journal_root.exists(), "no journal file was manufactured");
    }
    // (1b) THE TRUNCATED JOURNAL + the campaign's OWN medium: the
    // same lifecycle refusal, the damaged bytes preserved.
    std::fs::write(&journal_root, &intact_bytes[..10]).unwrap();
    let damaged_bytes = std::fs::read(&journal_root).unwrap();
    {
        let mut medium = LocalFileMedium::open(&store).unwrap();
        let error = match engine_persist::reclaim::create_campaign_bound_journal(
            &mut medium,
            &journal_root,
            &founding,
        ) {
            Err(engine_persist::reclaim::BoundCreationError::Medium(error)) => error,
            Err(other) => panic!("the bound lifecycle refuses the repair: {other:?}"),
            Ok(_journal) => panic!("the truncated journal is never repaired"),
        };
        assert!(
            matches!(
                error,
                engine_persist::MediumError::Chain {
                    rule: "journal-root/creation-authority-bound"
                }
            ),
            "the current lifecycle refuses the bound campaign's repair: {error:?}"
        );
        assert_eq!(
            std::fs::read(&journal_root).unwrap(),
            damaged_bytes,
            "the damaged evidence is preserved byte-exact"
        );
    }
    // (2) THE UNRELATED EMPTY MEDIUM aimed at the established root:
    // the journal is removed again (the sidecar lock — created once,
    // never unlinked — remains as the root's prior-journal-life
    // evidence), and a SEPARATE fresh empty medium with the
    // established campaign's founding cannot create there.
    std::fs::remove_file(&journal_root).unwrap();
    {
        let unrelated_store = dir.bound_journal().with_extension("store-unrelated");
        std::fs::create_dir_all(&unrelated_store).unwrap();
        let mut unrelated = LocalFileMedium::open(&unrelated_store).unwrap();
        assert!(
            unrelated.head().unwrap().is_none(),
            "the unrelated medium is genuinely empty"
        );
        let error = match engine_persist::reclaim::create_campaign_bound_journal(
            &mut unrelated,
            &journal_root,
            &founding,
        ) {
            Err(engine_persist::reclaim::BoundCreationError::Journal(error)) => error,
            Err(other) => panic!("the unrelated-medium aim refuses at the root: {other:?}"),
            Ok(_journal) => {
                panic!("an unrelated empty medium never restores another campaign's journal")
            }
        };
        assert!(
            matches!(
                error,
                engine_persist::JournalError::CorruptHeader { ref detail } if detail.contains("prior journal life")
            ),
            "the established root's prior journal life refuses the fresh creation: {error:?}"
        );
        assert!(
            !journal_root.exists(),
            "the missing journal was not restored"
        );
    }
    // The genuine evidence restored re-opens (the strict path).
    std::fs::write(&journal_root, &intact_bytes).unwrap();
    let crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "r11a-campaign",
    )
    .expect("the restored genuine evidence re-opens");
    drop(crossing);
}

#[test]
fn the_generation_creation_is_rotation_only() {
    // (F4-R11-B) The generation construction is CRATE-PRIVATE to the
    // trusted rotation (an ordinary external caller cannot even name
    // it — the compile-time boundary; rotation and the journal
    // implementation both reside in engine-persist). A bound campaign
    // that loses its active ROTATED generation cannot manufacture an
    // empty replacement through any public API: the strict open
    // refuses the missing generation, the creation operation refuses
    // the bound lifecycle — while the genuine production rotation
    // still creates a new identity-matching successor and the
    // restart recovery follows it.
    let dir = TestDir::new("r11b-rotation-only");
    let near = near_region_4();
    let far = far_from_region_4();
    let store = dir.store();
    let journal_root = dir.journal();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let mut crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "r11b-campaign",
    )
    .unwrap();
    // The anchor + the outstanding window for the rotation.
    crossing.publish_snapshot(&spine, &director).unwrap();
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    run_ticks_near(&mut spine, &mut director, &near, 5);
    crossing.publish_snapshot(&spine, &director).unwrap();
    let rotation = crossing.rotate_journal().expect("the rotation completes");
    let (founding, root_id) = installed_binding(&store);
    let active = rotation.new_path.clone();
    let active_bytes = std::fs::read(&active).unwrap();
    drop(crossing);
    drop(spine);
    drop(director);
    // THE ACTIVE ROTATED GENERATION REMOVED: no public path
    // manufactures a replacement.
    std::fs::remove_file(&active).unwrap();
    {
        // The strict open refuses the missing generation.
        let error = match T1Journal::open_bound(&active, &founding, &root_id) {
            Err(error) => error,
            Ok(_journal) => panic!("the missing generation is never created by an open"),
        };
        assert!(
            matches!(error, engine_persist::JournalError::CorruptHeader { ref detail } if detail.contains("absent")),
            "the strict open refuses the missing generation: {error:?}"
        );
        assert!(!active.exists(), "no generation file was manufactured");
        // The creation operation refuses the bound lifecycle.
        let mut medium = LocalFileMedium::open(&store).unwrap();
        let error = match engine_persist::reclaim::create_campaign_bound_journal(
            &mut medium,
            &active,
            &founding,
        ) {
            Err(engine_persist::reclaim::BoundCreationError::Medium(error)) => error,
            Err(other) => panic!("the bound lifecycle refuses: {other:?}"),
            Ok(_journal) => panic!("the missing generation is never manufactured"),
        };
        assert!(
            matches!(
                error,
                engine_persist::MediumError::Chain {
                    rule: "journal-root/creation-authority-bound"
                }
            ),
            "the bound campaign never authorizes generation creation: {error:?}"
        );
        assert!(
            !active.exists(),
            "still no generation file was manufactured"
        );
        // The re-entry refuses the missing active (fail closed).
        let error = match SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(store.clone()),
            &journal_root,
            "r11b-campaign",
        ) {
            Err(error) => error,
            Ok(_crossing) => panic!("the missing active generation never re-enters"),
        };
        assert!(
            matches!(
                &error,
                TransitionError::InstallIncompatible { detail, .. } if *detail == "journal-root/missing-journal"
            ),
            "the re-entry fails closed on the missing active: {error:?}"
        );
    }
    // THE GENUINE PRODUCTION ROTATION still creates the successor:
    // the restored active (the strict path) re-opens the campaign,
    // the rotation creates a new identity-matching generation, and
    // the restart recovery follows it.
    std::fs::write(&active, &active_bytes).unwrap();
    let mut crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "r11b-campaign",
    )
    .expect("the restored active re-opens the campaign");
    // The genuine rotation over the CHAIN's existing anchor (the
    // first session's covering publication — no new snapshot needed;
    // the outstanding transaction re-anchors whole into the
    // successor, preserving the frontier-coherent recovery state).
    let rotation2 = crossing
        .rotate_journal()
        .expect("the second rotation completes");
    drop(crossing);
    engine_persist::journal::scan_bound_file(&rotation2.new_path, &founding, &root_id)
        .expect("the genuine rotation's successor carries the bound identities");
    let (_recovered_spine, _recovered_director, recovered) =
        SessionCrossing::recover(&store, &journal_root, "r11b-campaign")
            .expect("the restart recovery follows the genuine successor");
    assert_eq!(
        recovered.pipeline_journal_path(),
        rotation2.new_path.as_path(),
        "the recovery holds the rotated active"
    );
    drop(recovered);
}

#[test]
fn the_rotation_entry_is_session_bound() {
    // (F4-R11-B) The rotation wrappers' closure: the raw mechanisms
    // (rotate_journal_with and the legacy form) are CRATE-PRIVATE —
    // an external caller cannot even name them (the compile-time
    // boundary; the legacy wrapper is removed outright), so no
    // caller-invented retention evidence and no caller-selected
    // legacy arm for a bound root exist in the public API at all.
    // The single public entry rotate_session_journal derives the
    // bound identities by probing the pipeline's own medium (the arm
    // selection is never an input) and derives the classification
    // from the pipeline's live journal and confirmed set. The
    // regressions here: the bound campaign's rotation through the
    // crossing produces the identity-inheriting successor (the arm
    // probed, never chosen); the legacy campaign's rotation keeps
    // the legacy arm (the authenticated absence); the active and the
    // historical generations remain complete across the restart
    // recovery; and the serialization contract is the accepted
    // root-lock discipline (proven by the R2 and fault-arc suites).
    let dir = TestDir::new("r11b-entry");
    let near = near_region_4();
    let far = far_from_region_4();
    // THE BOUND CAMPAIGN's rotation: the successor carries the SAME
    // founding hash and root ID (the probed bound arm).
    let store = dir.store();
    let journal_root = dir.journal();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let mut crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "r11b-entry-campaign",
    )
    .unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    run_ticks_near(&mut spine, &mut director, &near, 5);
    crossing.publish_snapshot(&spine, &director).unwrap();
    let rotation = crossing
        .rotate_journal()
        .expect("the bound rotation succeeds");
    drop(crossing);
    drop(spine);
    drop(director);
    let (founding, root_id) = installed_binding(&store);
    engine_persist::journal::scan_bound_file(&rotation.new_path, &founding, &root_id)
        .expect("the probed bound arm produced the identity-inheriting successor");
    // THE RESTART COMPLETENESS: the active and the historical
    // generations stand across the recovery.
    let (_spine, _director, recovered) =
        SessionCrossing::recover(&store, &journal_root, "r11b-entry-campaign")
            .expect("the restart recovery follows the successor");
    assert_eq!(
        recovered.pipeline_journal_path(),
        rotation.new_path.as_path(),
        "the recovery holds the rotated active"
    );
    engine_persist::journal::scan_bound_file(&journal_root, &founding, &root_id)
        .expect("the historical root generation remains complete");
    drop(recovered);
    // THE LEGACY CAMPAIGN's rotation: the authenticated absence
    // keeps the legacy arm (the successor carries the legacy header
    // form — scan_file, not the bound scanner).
    let legacy_dir = TestDir::new("r11b-entry-legacy");
    let legacy_store = legacy_dir.store();
    let legacy_root = legacy_dir.journal();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(&legacy_root).unwrap();
    let mut crossing = SessionCrossing::open(&legacy_store, journal, "r11b-entry-legacy").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    run_ticks_near(&mut spine, &mut director, &near, 5);
    crossing.publish_snapshot(&spine, &director).unwrap();
    let legacy_rotation = crossing
        .rotate_journal()
        .expect("the legacy rotation succeeds through the same entry");
    drop(crossing);
    drop(spine);
    drop(director);
    let legacy_scan = T1Journal::scan_file(&legacy_rotation.new_path)
        .expect("the authenticated absence kept the legacy arm (the legacy header form)");
    assert_eq!(
        legacy_scan.tail,
        engine_persist::TailState::Clean,
        "the legacy successor is a clean legacy journal"
    );
}

#[test]
fn the_unrelated_medium_cannot_repair_a_truncated_journal() {
    // (R11-A, Option B) The canonical-source ownership gap CLOSED:
    // a genuine bound campaign with committed journal history, its
    // journal truncated to a valid bound-header prefix, and an
    // UNRELATED EMPTY canonical medium armed with the genuine
    // founding identity — the single creation operation REFUSES
    // (the conservative policy: the torn prefix is preserved, never
    // repaired, regardless of the medium), the damaged bytes remain
    // identical, and no binding or journal authority was
    // manufactured on either medium.
    let dir = TestDir::new("r11a-optionb");
    let near = near_region_4();
    let far = far_from_region_4();
    let store = dir.store();
    let journal_root = dir.journal();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let mut crossing = SessionCrossing::open_bound_bootstrap(
        &StoreLocation::Directory(store.clone()),
        &journal_root,
        "r11a-optionb-campaign",
    )
    .unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    drop(crossing);
    drop(spine);
    drop(director);
    let (founding, _root_id) = installed_binding(&store);
    let intact = std::fs::read(&journal_root).unwrap();
    std::fs::write(&journal_root, &intact[..10]).unwrap();
    let damaged = std::fs::read(&journal_root).unwrap();
    // THE UNRELATED EMPTY MEDIUM with the genuine founding and the
    // damaged journal path: the typed refusal.
    let unrelated_store = dir.bound_journal().with_extension("store-unrelated");
    std::fs::create_dir_all(&unrelated_store).unwrap();
    let mut unrelated = LocalFileMedium::open(&unrelated_store).unwrap();
    assert!(
        unrelated.head().unwrap().is_none(),
        "the unrelated medium is genuinely empty"
    );
    let error = match engine_persist::reclaim::create_campaign_bound_journal(
        &mut unrelated,
        &journal_root,
        &founding,
    ) {
        Err(engine_persist::reclaim::BoundCreationError::Journal(error)) => error,
        Err(other) => panic!("the torn admission refuses typed: {other:?}"),
        Ok(_journal) => panic!("an unrelated medium never repairs another campaign's journal"),
    };
    assert!(
        matches!(error, engine_persist::JournalError::CorruptHeader { ref detail } if detail.contains("conservative recovery policy")),
        "the truncated journal is preserved under the conservative policy: {error:?}"
    );
    // THE DAMAGED BYTES identical; NO authority manufactured.
    assert_eq!(
        std::fs::read(&journal_root).unwrap(),
        damaged,
        "the damaged evidence is preserved byte-exact"
    );
    assert!(
        unrelated.head().unwrap().is_none(),
        "the unrelated medium's chain stands empty — no founding, no binding"
    );
    {
        let mut medium = LocalFileMedium::open(&store).unwrap();
        let (_id, binding) = find_installed_journal_root_binding(&mut medium, &founding)
            .unwrap()
            .unwrap();
        assert_eq!(
            binding.founding, founding,
            "the genuine campaign's binding stands unchanged"
        );
    }
}

#[test]
fn the_concurrent_creation_is_race_safe() {
    // (Option B §3) THE CREATION RACES: two concurrent creation
    // attempts at the same new root — exactly ONE initialization
    // obtains the creation ownership (the atomic create-new); the
    // competing attempt NEVER truncates or replaces the first
    // journal (it adopts the winner's COMPLETE empty journal through
    // the existing-file admission, the sidecar lock serializing its
    // read against the winner's initialization); and the binding
    // publication's CAS and duplicate/conflict discipline is
    // unchanged (one Accepted, the second the exact-duplicate
    // convergence).
    let dir = TestDir::new("race-creation");
    let store = dir.store();
    let journal_root = dir.journal();
    let campaign = "race-campaign";
    let founding = RevisionRecord::founding(AuthorityMode::Standalone, campaign, 0);
    let founding_id = founding.id();
    let location = JournalRootBinding::normalize_location(&journal_root);
    let root_id = journal_root_id(&founding_id, &location);
    // The founding-only chain (the racers create and adopt on top of
    // it — the fresh shape holds; no binding is published until both
    // attempts have completed, so both lifecycle verifications see
    // the authenticated absence).
    {
        let mut medium = LocalFileMedium::open(&store).unwrap();
        match medium.append(&founding, &[], None).unwrap() {
            AppendOutcome::Accepted { .. } => {}
            _ => panic!("the founding append accepts"),
        }
    }
    let root_for_a = journal_root.clone();
    let store_for_a = store.clone();
    let root_for_b = journal_root.clone();
    let store_for_b = store.clone();
    let handle_a = std::thread::spawn(move || {
        let mut medium = LocalFileMedium::open(&store_for_a).unwrap();
        let journal = engine_persist::reclaim::create_campaign_bound_journal(
            &mut medium,
            &root_for_a,
            &founding_id,
        )
        .expect("the first attempt creates or adopts");
        // The handle drops at the thread's end — releasing the root
        // lock for the competing attempt's serialized read.
        std::thread::sleep(std::time::Duration::from_millis(10));
        drop(journal);
    });
    let handle_b = std::thread::spawn(move || {
        // A small stagger: the typical interleaving has the first
        // attempt holding the creation ownership while this one
        // competes; the asserts hold for every interleaving.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let mut medium = LocalFileMedium::open(&store_for_b).unwrap();
        let journal = engine_persist::reclaim::create_campaign_bound_journal(
            &mut medium,
            &root_for_b,
            &founding_id,
        )
        .expect("the competing attempt adopts the winner's complete journal");
        drop(journal);
    });
    handle_a.join().unwrap();
    handle_b.join().unwrap();
    // EXACTLY ONE INITIALIZATION: the file carries precisely one
    // bound header and nothing else (never truncated, never
    // doubled) — verified against the deterministic identities.
    let scan = engine_persist::journal::scan_bound_file(&journal_root, &founding_id, &root_id)
        .expect("the raced root carries exactly one valid bound journal");
    assert!(scan.committed.is_empty(), "the journal is empty");
    assert_eq!(scan.tail, engine_persist::TailState::Clean, "a clean tail");
    assert_eq!(
        std::fs::read(&journal_root).unwrap().len(),
        engine_persist::journal::bound_header_bytes(&founding_id, &root_id).len(),
        "exactly one header — never doubled, never truncated"
    );
    // THE CAS/DUPLICATE DISCIPLINE (unchanged): the first
    // publication Accepts; the identical second converges as the
    // exact duplicate.
    {
        let mut medium = LocalFileMedium::open(&store).unwrap();
        let (_id, outcome) = engine_persist::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding_id,
            &root_id,
            &location,
            1,
        )
        .unwrap();
        assert!(
            matches!(
                outcome,
                engine_persist::reclaim::JournalRootBindingOutcome::Accepted { .. }
            ),
            "the first publication accepts: {outcome:?}"
        );
        let (_id, outcome) = engine_persist::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding_id,
            &root_id,
            &location,
            2,
        )
        .unwrap();
        assert!(
            matches!(
                outcome,
                engine_persist::reclaim::JournalRootBindingOutcome::DuplicateVerified { .. }
            ),
            "the identical publication converges as the exact duplicate: {outcome:?}"
        );
    }
    // The installed binding confirms the raced campaign's authority.
    let (founding, root) = installed_binding(&store);
    assert_eq!(founding, founding_id);
    assert_eq!(root, root_id);
}

#[test]
fn the_rotation_entry_verifies_the_root_agreement() {
    // (R11-B) The journal/root agreement: the pipeline's journal
    // must BE the pointer-resolved active at the requested root —
    // an arbitrary pipeline constructed with a journal from ANOTHER
    // root refuses before any successor is created, and a STALE
    // journal (the pre-rotation root after a rotation) equally
    // refuses; the bound agreements (the binding's location vs the
    // root, the active's bound identities vs the installed binding)
    // hold for the genuine rotation.
    let dir = TestDir::new("r11b-agreement");
    let near = near_region_4();
    let far = far_from_region_4();
    // TWO bound campaigns at different roots.
    let make_campaign = |label: &str| {
        let store = dir.bound_journal().with_extension(format!("store-{label}"));
        std::fs::create_dir_all(&store).unwrap();
        let root = dir.bound_journal().with_extension(format!("root-{label}"));
        let (mut spine, mut director) = scenario_pair(150, &near);
        let mut crossing = SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(store.clone()),
            &root,
            label,
        )
        .unwrap();
        crossing.publish_snapshot(&spine, &director).unwrap();
        confirm_cycle(&mut crossing, &mut spine, &mut director, &far, &near);
        durable_boundary(&mut crossing, &mut spine, &mut director, &far);
        run_ticks_near(&mut spine, &mut director, &near, 5);
        crossing.publish_snapshot(&spine, &director).unwrap();
        drop(crossing);
        drop(spine);
        drop(director);
        (store, root)
    };
    let (store_a, root_a) = make_campaign("a");
    let (store_b, root_b) = make_campaign("b");
    let (founding_b, root_id_b) = installed_binding(&store_b);
    // THE ARBITRARY PIPELINE from another root: a pipeline over
    // campaign B's journal aimed at campaign A's root refuses before
    // any successor is created.
    {
        let active_b = active_journal_path(&root_b).unwrap();
        let journal_b =
            T1Journal::open_bound(&active_b, &founding_b, &root_id_b).expect("B's active");
        let medium_b = LocalFileMedium::open(&store_b).unwrap();
        let mut pipeline_b = engine_persist::CommitPipeline::open(medium_b, journal_b).unwrap();
        let error = match engine_persist::rotate_session_journal(&mut pipeline_b, &root_a, None) {
            Err(engine_persist::SessionRotationError::Journal(error)) => error,
            Err(other) => panic!("the cross-root aim refuses typed: {other:?}"),
            Ok(_rotation) => panic!("a pipeline from another root never rotates this root"),
        };
        assert!(
            matches!(error, engine_persist::JournalError::CorruptHeader { ref detail } if detail.contains("not the pointer-resolved active")),
            "the journal/root agreement refuses the cross-root pipeline: {error:?}"
        );
        assert!(
            active_journal_path(&root_a).unwrap() == root_a,
            "no successor was created at A's root — the pointer stands at the root"
        );
    }
    // THE STALE JOURNAL: campaign A rotated once (the successor is
    // the active); a pipeline over the STALE root journal aimed at
    // A's root refuses.
    {
        let mut crossing = SessionCrossing::open_bound_bootstrap(
            &StoreLocation::Directory(store_a.clone()),
            &root_a,
            "a",
        )
        .unwrap();
        // The chain's existing anchor (make_campaign's covering
        // publication) suffices — no new snapshot needed.
        let rotation = crossing.rotate_journal().expect("A rotates genuinely");
        drop(crossing);
        let (founding_a, root_id_a) = installed_binding(&store_a);
        let journal_stale =
            T1Journal::open_bound(&root_a, &founding_a, &root_id_a).expect("the stale root");
        let medium_a = LocalFileMedium::open(&store_a).unwrap();
        let mut pipeline_stale =
            engine_persist::CommitPipeline::open(medium_a, journal_stale).unwrap();
        let error = match engine_persist::rotate_session_journal(&mut pipeline_stale, &root_a, None)
        {
            Err(engine_persist::SessionRotationError::Journal(error)) => error,
            Err(other) => panic!("the stale-journal aim refuses typed: {other:?}"),
            Ok(_rotation) => panic!("a stale journal never rotates"),
        };
        assert!(
            matches!(error, engine_persist::JournalError::CorruptHeader { ref detail } if detail.contains("not the pointer-resolved active")),
            "the journal/root agreement refuses the stale journal: {error:?}"
        );
        assert_eq!(
            active_journal_path(&root_a).unwrap(),
            rotation.new_path,
            "the pointer still names the genuine successor — no unauthorized flip"
        );
    }
}
