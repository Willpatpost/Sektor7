//! Stage 5 Batch 5: the end-to-end session lifecycle (the GO's first
//! item) — the complete arc over the REAL simulation: startup,
//! campaign opening (the founding), verified snapshot publication and
//! loading, journal recovery, deterministic continuation, milestone
//! persistence, canonical confirmation, the graceful save/close, and
//! the restart. The closed-and-restarted session ends EXACTLY where
//! the never-persisted twin does (the combined Stage 4 fingerprint),
//! the crash-after-confirmation restart continues coherently to the
//! same twin, and the repeated recovery is idempotent.

use std::path::PathBuf;

use engine_core::{LodLevel, PresenceSample, Spine};
use engine_persist::{CommitOutcome, ConsequenceId, KeyPhase, T1Journal};
use engine_slice::crossing::SessionCrossing;
use engine_slice::npc::{NpcConfig, NpcWorld, install_initial_schedules};
use engine_slice::region::{RegionConfig, RegionDirector, stage4_state_fingerprint};

/// A temporary campaign directory (the suite pattern, local to this
/// file): a unique child of the system temp dir, removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-b5-lifecycle-{name}-{}",
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

/// The real scenario pair (the crossing suite's fixture, local to
/// this file): the Lod-mode spine plus the director, driven through
/// the driver's own per-tick sequence.
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

/// Runs the session forward under one presence (the deterministic
/// accrual).
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

/// The durable boundary (the crossing suite's helper, local to this
/// file): every region whose target under the presence differs
/// routes — Dormant targets through the six-phase deactivation,
/// reactivations through the durable activation, the remaining
/// reclassifications through the legacy semantics.
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
fn session_lifecycle_graceful_close_and_restart_matches_the_twin() {
    // THE complete lifecycle arc: the durable session runs, snapshots
    // at its graceful boundaries, CLOSES (the final snapshot + the
    // drop), RESTARTS (the recovery from the snapshot + the journal),
    // and continues deterministically — ending EXACTLY where the
    // never-persisted twin (the pure legacy run, no crossing, no
    // journal, no snapshots) ends: the combined Stage 4 fingerprint,
    // the milestones, the credits — the closed-and-restarted durable
    // session is the legacy run.
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
    // THE LIFECYCLE SESSION — the durable arc with the graceful close
    // and the restart.
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("lifecycle");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "lifecycle-campaign").unwrap();
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    // THE GRACEFUL CLOSE: the final snapshot published, the session
    // dropped — everything durable.
    drop(crossing);
    // THE RESTART: the recovery from the newest snapshot + the
    // journal.
    let (mut spine, mut director, mut crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "lifecycle-campaign")
            .expect("the restart recovers the closed session");
    // The deterministic continuation: the second deactivation, the
    // canonical confirmation, the remaining accrual — the same work
    // as the twin's.
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    let CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the continuation confirms canonically");
    };
    run(&mut spine, &mut director, &near, 50);
    // THE MATCH: the closed-and-restarted session ends exactly where
    // the never-persisted twin does.
    assert_eq!(
        stage4_state_fingerprint(&spine, &director),
        stage4_state_fingerprint(&twin_spine, &twin_director),
        "the graceful-close-and-restart session matches the never-persisted twin exactly"
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
fn lifecycle_crash_after_confirmation_restarts_coherently() {
    // The crash AFTER the canonical confirmation (before any newer
    // snapshot): the restart recovers from the OLDER snapshot + the
    // journal — the confirmed captures' plan re-installs, the
    // confirmation suppresses the replay — and the continuation
    // reaches the twin's final state exactly.
    let near = near_region_4();
    let far = far_from_region_4();
    // THE TWIN with the intermediate captures — the coherence
    // checkpoints the restart's continuation must reproduce one by
    // one.
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
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "lifecycle-campaign").unwrap();
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
    // THE CRASH: the confirmation is on the chain, the newer work is
    // in-memory only — lost.
    drop(crossing);
    // THE RESTART: the recovery from the older snapshot + the journal
    // (the confirmed plan re-installs; the confirmed identities
    // suppress the replay).
    let (mut spine, mut director, mut crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "lifecycle-campaign")
            .expect("the post-confirmation crash restarts coherently");
    assert_eq!(
        crossing.key_phase(ConsequenceId::new(4, 1, 4)),
        Some(KeyPhase::InConfirmedChain),
        "the confirmed captures stand confirmed through the restart"
    );
    // The post-restart checkpoint — REGION-4-SCOPED (the recovery's
    // target). The presence-re-derivable lanes (regions 3/5's
    // empty-scratch deactivations, unjournaled by design) legitimately
    // differ at this point: the twin's live far boundary deactivated
    // them at its tick; the recovery leaves them at the snapshot's
    // state — the continuation's next far boundary re-derives them.
    assert_eq!(
        director.levels()[4],
        LodLevel::Dormant,
        "the confirmed plan re-installed — region 4 dormant"
    );
    assert_eq!(
        director.states()[4].episode,
        1,
        "the episode frontier preserved"
    );
    assert_eq!(
        director.states()[4].checkpoint,
        twin_e1_checkpoint,
        "the plan-installed checkpoint matches the twin's exactly"
    );
    assert_eq!(
        director.ledger().pending_total(),
        twin_e1_pending,
        "the confirmed captures pending in the ledger"
    );
    assert_eq!(
        spine.world().milestones(),
        twin_e1_milestones,
        "the milestones match the twin's post-deactivation state exactly"
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
    // THE FINAL: the spine's engine state matches the twin's exactly
    // (the deterministic continuation — the same executions, the same
    // follow-up chains, the same ledger and statistics), and the
    // consequential authority matches; the presence-derived regions'
    // dormant_since legitimately differs (the re-derivation happened
    // at the continuation's boundary, not the twin's — the documented
    // design), so the director's checkpoint timing is not compared.
    assert_eq!(
        spine.engine_fingerprint(),
        twin_final_engine,
        "the engine state reaches the twin's exactly through the restart and continuation"
    );
    assert_eq!(
        spine.world().milestones(),
        twin_spine.world().milestones(),
        "the milestones reach the twin's exactly"
    );
    assert_eq!(
        spine.world().credits(),
        twin_spine.world().credits(),
        "the confirmed captures applied to credits through the continuation"
    );
    assert_eq!(
        director.ledger().pending_total(),
        twin_director.ledger().pending_total(),
        "the pending lane reaches the twin's exactly"
    );
    assert_eq!(
        director.ledger().applied_count(),
        twin_director.ledger().applied_count(),
        "the applied lane reaches the twin's exactly"
    );
    assert_eq!(
        director.levels()[3],
        twin_director.levels()[3],
        "the presence-derived region reaches the twin's final level — re-derived at the continuation's boundary and re-reactivated by the near-presence run, identically to the twin"
    );
}

#[test]
fn repeated_recovery_is_idempotent() {
    // The recovery is a pure function of the durable state: recovering
    // TWICE from the same disk (the snapshot + the unconfirmed
    // journal) reconstructs the IDENTICAL session — the second
    // recovery neither duplicates nor drifts.
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("repeated");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "lifecycle-campaign").unwrap();
    run(&mut spine, &mut director, &near, 100);
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    // THE CRASH: the unconfirmed durable intent stands in the journal.
    drop(crossing);
    // THE FIRST recovery.
    let (spine_a, director_a, crossing_a) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "lifecycle-campaign")
            .expect("the first recovery succeeds");
    let fingerprint_a = stage4_state_fingerprint(&spine_a, &director_a);
    let milestones_a = spine_a.world().milestones().to_vec();
    let pending_a = director_a.ledger().pending_total();
    drop(crossing_a);
    // THE SECOND recovery — from the same disk, untouched between.
    let (spine_b, director_b, mut crossing_b) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "lifecycle-campaign")
            .expect("the second recovery succeeds");
    assert_eq!(
        stage4_state_fingerprint(&spine_b, &director_b),
        fingerprint_a,
        "the repeated recovery reconstructs the identical session"
    );
    assert_eq!(spine_b.world().milestones(), milestones_a);
    assert_eq!(director_b.ledger().pending_total(), pending_a);
    // The re-drive after the second recovery confirms exactly once.
    let CommitOutcome::Confirmed(receipt) = crossing_b.commit_canonical().unwrap() else {
        panic!("the re-drive after the repeated recovery confirms");
    };
    assert!(receipt.keys.contains(&ConsequenceId::new(4, 1, 4)));
}
