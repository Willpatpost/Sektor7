//! PP-B1 §6.1's end-to-end acceptance: a REAL campaign lives on the
//! legacy file medium (the B5 lifecycle through the crossing), the
//! operator runs the offline migration, and the SAME campaign
//! recovers through the SQLite provider — the migrated database plus
//! the UNTOUCHED T1 journal (D-PP-2) compose through the standard
//! recovery, and the continuation reaches the never-persisted twin
//! exactly. The gameplay eligibility is the operator's checklist
//! (D-PP-6); this test is that checklist's first item performed by
//! the machine.

use std::path::PathBuf;

use engine_core::{LodLevel, PresenceSample, Spine};
use engine_persist::{CommitOutcome, ConsequenceId, KeyPhase, T1Journal, migrate_local_to_sqlite};
use engine_slice::crossing::SessionCrossing;
use engine_slice::npc::{NpcConfig, NpcWorld, install_initial_schedules};
use engine_slice::region::{RegionConfig, RegionDirector, stage4_state_fingerprint};
use engine_slice::store::StoreLocation;

/// A temporary campaign directory: the legacy store, the new
/// database, and the journal all live inside it (the ratified
/// layout: the store directory, the database file, and the journal
/// as siblings — exactly what the migration's layout check
/// requires), removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-pp-b1-migration-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        TestDir(path)
    }

    /// The legacy file-medium store directory.
    fn store(&self) -> PathBuf {
        self.0.join("store")
    }

    /// The SQLite campaign database the migration produces.
    fn db(&self) -> PathBuf {
        self.0.join("store.db")
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
fn migrated_campaign_recovers_through_the_sqlite_provider() {
    let near = near_region_4();
    let far = far_from_region_4();
    // THE TWIN — the never-persisted reference run.
    let (mut twin_spine, mut twin_director) = scenario_pair(150, &near);
    run(&mut twin_spine, &mut twin_director, &near, 100);
    twin_director.apply(&mut twin_spine, &[far]);
    twin_director.apply(&mut twin_spine, &[near]);
    run(&mut twin_spine, &mut twin_director, &near, 100);
    twin_director.apply(&mut twin_spine, &[far]);
    run(&mut twin_spine, &mut twin_director, &near, 50);
    // THE LEGACY CAMPAIGN — the complete durable arc on the file
    // medium (the provider seam's Directory form), closed gracefully.
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("end-to-end");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "migration-campaign").unwrap();
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    drop(crossing);
    // THE OFFLINE MIGRATION (the operator's act, between sessions):
    // the layout is safe (the siblings), the copy carries the whole
    // chain, the parity holds, the source is sealed and archived.
    let report = migrate_local_to_sqlite(&dir.store(), &dir.db(), &dir.journal())
        .expect("the migration completes");
    assert!(
        report.copied >= 2,
        "the founding plus the published snapshots carried over"
    );
    assert_eq!(
        report.source_summary.as_ref().unwrap(),
        &report.target_summary,
        "the ChainSummary parity end to end"
    );
    assert!(report.archived_to.is_some());
    assert!(!dir.store().exists(), "the legacy root moved aside");
    // THE RESTART through the SQLite provider: the migrated database
    // plus the UNTOUCHED journal compose through the standard
    // recovery — the operator checklist's machine-performed item.
    let (mut spine, mut director, mut crossing) = SessionCrossing::recover_at(
        &StoreLocation::Database(dir.db()),
        &dir.journal(),
        "migration-campaign",
    )
    .expect("the migrated campaign recovers through the SQLite provider");
    // The deterministic continuation: the second deactivation, the
    // canonical confirmation, the remaining accrual — the twin's work.
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    let CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the post-migration continuation confirms canonically");
    };
    run(&mut spine, &mut director, &near, 50);
    // THE MATCH: the migrated-and-restarted campaign ends exactly
    // where the never-persisted twin does.
    assert_eq!(
        stage4_state_fingerprint(&spine, &director),
        stage4_state_fingerprint(&twin_spine, &twin_director),
        "the migrated campaign matches the never-persisted twin exactly"
    );
    assert_eq!(
        spine.world().milestones(),
        twin_spine.world().milestones(),
        "the milestones persisted through the migration and continued identically"
    );
    assert_eq!(
        spine.world().credits(),
        twin_spine.world().credits(),
        "the credits persisted through the migration and continued identically"
    );
    assert_eq!(
        crossing.key_phase(ConsequenceId::new(4, 2, 4)),
        Some(KeyPhase::InConfirmedChain),
        "the continuation's captures are canonically confirmed over the migrated chain"
    );
}
