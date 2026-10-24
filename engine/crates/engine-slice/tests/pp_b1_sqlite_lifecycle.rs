//! PP-B1: the accepted B5 lifecycle arcs over the **SQLite provider**
//! (the GO's "run the accepted persistence, recovery, and exact-once
//! tests with SQLite as the canonical provider") — the complete arc
//! (startup, founding, publication, journal recovery, continuation,
//! confirmation, the graceful close with the ratified TRUNCATE
//! checkpoint, the restart), the crash-after-confirmation restart,
//! and the repeated-recovery idempotence, all through the SAME
//! `SessionCrossing`/`CommitPipeline` machinery (the provider seam's
//! `open_at`/`recover_at`), ending exactly where the never-persisted
//! twin does. The T1 journal remains the separate file medium (the
//! ratified D-PP-2); the canonical chain lives in the database.

use std::path::PathBuf;

use engine_core::{LodLevel, PresenceSample, Spine};
use engine_persist::{CommitOutcome, ConsequenceId, KeyPhase, T1Journal};
use engine_slice::crossing::SessionCrossing;
use engine_slice::npc::{NpcConfig, NpcWorld, install_initial_schedules};
use engine_slice::region::{RegionConfig, RegionDirector, stage4_state_fingerprint};
use engine_slice::store::StoreLocation;

/// A temporary campaign directory (the suite pattern): the SQLite
/// database and the T1 journal both live inside it, removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-pp-b1-sqlite-lifecycle-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        TestDir(path)
    }

    /// The SQLite campaign database (one database per campaign — the
    /// ratified layout: the store file alongside the journal).
    fn db(&self) -> PathBuf {
        self.0.join("store.db")
    }

    fn location(&self) -> StoreLocation {
        StoreLocation::Database(self.db())
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

/// The real scenario pair (the suite's fixture): the Lod-mode spine
/// plus the director, driven through the driver's own per-tick
/// sequence.
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

fn run(
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

/// The durable boundary (the suite's helper): every region whose
/// target under the presence differs routes through the crossing.
fn durable_boundary(
    crossing: &mut SessionCrossing,
    spine: &mut Spine<NpcWorld>,
    director: &mut RegionDirector,
    presence: &PresenceSample,
) {
    let now = spine.clock();
    for region in 0..engine_slice::npc::REGION_COUNT {
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

#[test]
fn pp_b1_sqlite_graceful_close_and_restart_matches_the_twin() {
    // The complete lifecycle arc over SQLite: the durable session
    // runs, snapshots at its graceful boundaries, CLOSES (the final
    // snapshot + the ratified TRUNCATE checkpoint + the drop), and
    // RESTARTS (the recovery from the newest snapshot + the journal)
    // — ending EXACTLY where the never-persisted twin does.
    let near = near_region_4();
    let far = far_from_region_4();
    // THE TWIN — the never-persisted legacy run (the reference).
    let (mut twin_spine, mut twin_director) = scenario_pair(150, &near);
    run(&mut twin_spine, &mut twin_director, &near, 100);
    twin_director.apply(&mut twin_spine, &[far]);
    twin_director.apply(&mut twin_spine, &[near]);
    run(&mut twin_spine, &mut twin_director, &near, 100);
    twin_director.apply(&mut twin_spine, &[far]);
    run(&mut twin_spine, &mut twin_director, &near, 50);
    // THE LIFECYCLE SESSION over the SQLite provider.
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("lifecycle");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing =
        SessionCrossing::open_at(&dir.location(), journal, "lifecycle-campaign").unwrap();
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    // THE GRACEFUL CLOSE: the final snapshot published, the ratified
    // TRUNCATE checkpoint taken, the session dropped.
    crossing.checkpoint().unwrap();
    drop(crossing);
    // THE RESTART: the recovery from the newest snapshot + the
    // journal — over the same database.
    let (mut spine, mut director, mut crossing) =
        SessionCrossing::recover_at(&dir.location(), &dir.journal(), "lifecycle-campaign")
            .expect("the restart recovers the closed session");
    // The deterministic continuation: the second deactivation, the
    // canonical confirmation, the remaining accrual.
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    let CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the continuation confirms canonically");
    };
    run(&mut spine, &mut director, &near, 50);
    // THE MATCH: the closed-and-restarted SQLite session ends exactly
    // where the never-persisted twin does.
    assert_eq!(
        stage4_state_fingerprint(&spine, &director),
        stage4_state_fingerprint(&twin_spine, &twin_director),
        "the SQLite graceful-close-and-restart session matches the twin exactly"
    );
    assert_eq!(
        spine.world().milestones(),
        twin_spine.world().milestones(),
        "the milestones persisted and continued identically"
    );
    assert_eq!(
        spine.world().credits(),
        twin_spine.world().credits(),
        "the credits persisted and continued identically"
    );
    assert_eq!(
        crossing.key_phase(ConsequenceId::new(4, 2, 4)),
        Some(KeyPhase::InConfirmedChain),
        "the continuation's captures are canonically confirmed"
    );
}

#[test]
fn pp_b1_sqlite_crash_after_confirmation_restarts_coherently() {
    // The crash AFTER the canonical confirmation with only the older
    // snapshot on disk: the SQLite restart recovers — the confirmed
    // plan re-installs, the confirmation suppresses the replay — and
    // the continuation reaches the twin's final state exactly.
    let near = near_region_4();
    let far = far_from_region_4();
    // THE TWIN with the intermediate coherence checkpoints.
    let (mut twin_spine, mut twin_director) = scenario_pair(150, &near);
    run(&mut twin_spine, &mut twin_director, &near, 100);
    twin_director.apply(&mut twin_spine, &[far]);
    let twin_e1_milestones = twin_spine.world().milestones().to_vec();
    let twin_e1_checkpoint = twin_director.states()[4].checkpoint.clone();
    let twin_e1_pending = twin_director.ledger().pending_total();
    twin_director.apply(&mut twin_spine, &[near]);
    run(&mut twin_spine, &mut twin_director, &near, 100);
    twin_director.apply(&mut twin_spine, &[far]);
    run(&mut twin_spine, &mut twin_director, &near, 50);
    let twin_final_engine = twin_spine.engine_fingerprint();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("crash-confirmed");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing =
        SessionCrossing::open_at(&dir.location(), journal, "lifecycle-campaign").unwrap();
    run(&mut spine, &mut director, &near, 100);
    // The snapshot BEFORE the first deactivation — the crash lands
    // after the confirmation with only this older boundary on disk.
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    let CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the captures confirm before the crash");
    };
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    run(&mut spine, &mut director, &near, 100);
    // THE CRASH: the confirmation is on the SQLite chain, the newer
    // work is in-memory only — lost (no checkpoint, the raw drop).
    drop(crossing);
    // THE RESTART over the same database.
    let (mut spine, mut director, mut crossing) =
        SessionCrossing::recover_at(&dir.location(), &dir.journal(), "lifecycle-campaign")
            .expect("the post-confirmation crash restarts coherently");
    assert_eq!(
        crossing.key_phase(ConsequenceId::new(4, 1, 4)),
        Some(KeyPhase::InConfirmedChain),
        "the confirmed captures stand confirmed through the SQLite restart"
    );
    // The post-restart region-4-scoped intermediates.
    assert_eq!(director.levels()[4], LodLevel::Dormant);
    assert_eq!(director.states()[4].episode, 1);
    assert_eq!(director.states()[4].checkpoint, twin_e1_checkpoint);
    assert_eq!(director.ledger().pending_total(), twin_e1_pending);
    assert_eq!(
        spine.world().milestones(),
        &twin_e1_milestones[..],
        "the recovery's milestones match the twin's episode-1 state"
    );
    // The continuation: the reactivation (the confirmed captures
    // applied to credits — the legitimate lifecycle), the accrual,
    // the second deactivation (which also re-derives the
    // presence-derived regions), the remaining ticks — the twin's
    // work.
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    assert_eq!(
        director.levels()[4],
        LodLevel::Full,
        "the reactivation installs over the recovered checkpoint"
    );
    assert!(
        spine.world().credits().iter().sum::<u64>() > 0,
        "the confirmed captures applied to credits — nothing stranded"
    );
    run(&mut spine, &mut director, &near, 100);
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    run(&mut spine, &mut director, &near, 50);
    // THE FINAL: the spine's engine state matches the twin's exactly,
    // the consequential authority matches, and the presence-derived
    // region reaches the twin's final level.
    assert_eq!(
        spine.engine_fingerprint(),
        twin_final_engine,
        "the SQLite crash-restart continuation converges to the twin's engine state"
    );
    assert_eq!(spine.world().milestones(), twin_spine.world().milestones());
    assert_eq!(spine.world().credits(), twin_spine.world().credits());
    assert_eq!(
        director.ledger().pending_total(),
        twin_director.ledger().pending_total()
    );
    assert_eq!(
        director.ledger().applied_count(),
        twin_director.ledger().applied_count()
    );
    assert_eq!(
        director.levels()[3],
        twin_director.levels()[3],
        "the presence-re-derivable region reaches the twin's final level"
    );
}

#[test]
fn pp_b1_sqlite_repeated_recovery_is_idempotent() {
    // The recovery is a pure function of the durable SQLite state:
    // recovering TWICE from the same database reconstructs the
    // identical session, and the re-drive after the second recovery
    // never double-counts.
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("repeated");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing =
        SessionCrossing::open_at(&dir.location(), journal, "recovery-campaign").unwrap();
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    let CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the captures confirm before the close")
    };
    crossing.publish_snapshot(&spine, &director).unwrap();
    drop(crossing);
    // THE FIRST RECOVERY.
    let (spine_a, director_a, mut crossing_a) =
        SessionCrossing::recover_at(&dir.location(), &dir.journal(), "recovery-campaign")
            .expect("the first recovery succeeds");
    let fingerprint_a = stage4_state_fingerprint(&spine_a, &director_a);
    let milestones_a = spine_a.world().milestones().to_vec();
    let _ = &mut crossing_a;
    drop((spine_a, director_a, crossing_a));
    // THE SECOND RECOVERY from the same durable state.
    let (spine_b, director_b, _crossing_b) =
        SessionCrossing::recover_at(&dir.location(), &dir.journal(), "recovery-campaign")
            .expect("the second recovery succeeds");
    assert_eq!(
        stage4_state_fingerprint(&spine_b, &director_b),
        fingerprint_a,
        "the repeated recovery reconstructs the identical session"
    );
    assert_eq!(
        spine_b.world().milestones(),
        &milestones_a[..],
        "the milestones never double-count through the repeated recovery"
    );
}
