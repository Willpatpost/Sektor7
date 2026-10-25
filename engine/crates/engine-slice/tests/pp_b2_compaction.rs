//! PP-B2 — the consequence-lane extension and the compacted session
//! publication/recovery over the REAL B4 crossing machinery (the
//! GO §3/§5 acceptance list at the application level): the lane
//! eligibility matrix (pending and unconfirmed identities retained —
//! never aggregate totals alone; applied-and-confirmed identities
//! summarized under the boundary); the compacted publication's round
//! trip through the real codecs; the full-vs-compacted twin
//! continuation (the compacted publication + recovery is transparent
//! — the continued session reaches the control's state exactly); the
//! crash-after-confirmation arc replayed from a compacted anchor;
//! repeated recovery exact-once; the over-claiming summary failing
//! closed without damage (a compacted anchor whose summarized
//! coverage its campaign's chain does not confirm); and the director
//! codec's v4 gates (the discriminator, the lane disjointness, the
//! episode frontier).

use std::path::PathBuf;

use engine_core::{
    CompactionError, Decoder, Encoder, Fingerprinter, HistoryKind, LodLevel, PresenceSample, Spine,
    World,
};
use engine_persist::{
    AppendOutcome, AuthorityMode, CanonicalMedium, LocalFileMedium, RevisionRecord, StoredObject,
    T1Journal,
};
use engine_slice::crossing::{DeactivationPlan, SessionCrossing, TransitionError, save_session};
use engine_slice::npc::{NpcConfig, NpcWorld, REGION_COUNT, install_initial_schedules};
use engine_slice::region::{RegionConfig, RegionDirector};

// ------------------------------------------------------------- fixtures

/// A temporary campaign directory (the B4 test pattern, local to this
/// suite): a unique child of the system temp dir, removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-pp-b2-{name}-{}",
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

/// The real pair (the crossing needs the spine and the director as
/// separate mutable references), driven through the legacy per-tick
/// sequence: at 150 ticks under the near-presence, region 4 stays
/// live and five far regions sit dormant since tick 0 with their
/// (never-journaled) captures pending in the ledger.
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

/// The durable boundary (the B4 test pattern): every region whose
/// target under the presence differs routes — Dormant targets through
/// the six-phase deactivation, reactivations through the durable
/// activation, the rest through the legacy semantics.
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

fn world_fingerprint(spine: &Spine<NpcWorld>) -> u64 {
    let mut fp = Fingerprinter::new();
    spine.world().fingerprint(&mut fp);
    fp.finish()
}

/// The presence-held tick run (the B5 lifecycle pattern).
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

/// The recovery-refusal probe — the recovery tuple does not
/// implement Debug, so a match (the B1 expect_error discipline),
/// never `expect_err`.
fn expect_recovery_error(
    store: &std::path::Path,
    journal: &std::path::Path,
    campaign: &str,
) -> TransitionError {
    match SessionCrossing::recover(store, journal, campaign) {
        Err(error) => error,
        Ok(_) => panic!("the recovery must halt, never succeed"),
    }
}

/// Asserts a compacted twin and its uncompacted control are
/// logically indistinguishable after identical continuations — the
/// cross-representation comparisons available at the application
/// level: authoritative counters, queue depths, both watermarks, the
/// held fingerprint (valid on both representations), the world
/// fingerprint, the director's levels and episode counters, the
/// pending lane (identical — referenced identities are retained),
/// and conservation. (The lane-hashing fingerprints are full-history
/// APIs by contract; the compacted-history domains compare
/// compacted-vs-compacted and are asserted where both sides carry
/// the same cut.)
fn assert_continuation_equivalent(
    control: &Spine<NpcWorld>,
    control_director: &RegionDirector,
    twin: &Spine<NpcWorld>,
    twin_director: &RegionDirector,
    context: &str,
) {
    assert_eq!(control.clock(), twin.clock(), "{context}: clock");
    assert_eq!(control.stats(), twin.stats(), "{context}: stats");
    assert_eq!(
        control.pending_count(),
        twin.pending_count(),
        "{context}: pending"
    );
    assert_eq!(
        control.blocked_count(),
        twin.blocked_count(),
        "{context}: blocked"
    );
    assert_eq!(control.held_count(), twin.held_count(), "{context}: held");
    assert_eq!(
        control.max_executed_due(),
        twin.max_executed_due(),
        "{context}: max executed due"
    );
    assert_eq!(
        control.processed_through(),
        twin.processed_through(),
        "{context}: watermark"
    );
    assert_eq!(
        control.held_fingerprint(),
        twin.held_fingerprint(),
        "{context}: held fingerprint"
    );
    assert_eq!(
        world_fingerprint(control),
        world_fingerprint(twin),
        "{context}: world fingerprint"
    );
    assert_eq!(
        control_director.levels(),
        twin_director.levels(),
        "{context}: levels"
    );
    for region in 0..REGION_COUNT {
        assert_eq!(
            control_director.states()[region].episode,
            twin_director.states()[region].episode,
            "{context}: episode frontier {region}"
        );
    }
    assert_eq!(
        control_director.ledger().pending_count(),
        twin_director.ledger().pending_count(),
        "{context}: pending lane count"
    );
    assert_eq!(
        control_director.ledger().pending_total(),
        twin_director.ledger().pending_total(),
        "{context}: pending lane total"
    );
    for spine in [control, twin] {
        let stats = spine.stats();
        let conserved = stats.executed
            + spine.pending_count() as u64
            + spine.blocked_count() as u64
            + spine.held_count() as u64;
        assert_eq!(stats.scheduled, conserved, "{context}: conservation");
    }
}

/// Runs the shared scenario prefix: the legacy setup (region 4 live,
/// five far regions dormant with unjournaled pending captures), the
/// durable far boundary (region 4's deactivation journaled and
/// installed), the canonical confirmation, and the near boundary
/// (region 4's reactivation — its confirmed captures apply).
/// Region 4's episode-1 identities are now applied AND confirmed;
/// the five legacy regions' captures stay pending and unconfirmed.
fn confirmed_reactivation_prefix(
    dir: &TestDir,
) -> (Spine<NpcWorld>, RegionDirector, SessionCrossing) {
    let far = far_from_region_4();
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "prefix-campaign").unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    crossing.commit_canonical().unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    (spine, director, crossing)
}

// ------------------------------------------------------------- the matrix

#[test]
fn lane_eligibility_preserves_referenced_identities() {
    // THE eligibility matrix, in two phases. Phase 1 —
    // pre-confirmation: the journaled captures are PENDING
    // (referenced — the dormant regions' outstanding held obligations
    // and the T1 intent), so the compaction must retain every one in
    // full and summarize NOTHING (no applied identities exist).
    // Phase 2 — post-confirmation-and-reactivation: the captures are
    // applied AND confirmed, so the contiguous eligible prefix
    // summarizes exactly them — never aggregate totals alone.
    let dir = TestDir::new("eligibility");
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing =
        SessionCrossing::open(&dir.store(), journal, "eligibility-campaign").unwrap();
    run_ticks_near(&mut spine, &mut director, &near, 100);
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);

    // Phase 1: the pending captures are referenced — retained in
    // full, nothing summarized (nothing is applied).
    let pending_before = director.ledger().pending_count();
    let pending_total_before = director.ledger().pending_total();
    assert!(pending_before > 0, "the journaled captures are pending");
    let phase1 = crossing
        .compact_session(&mut spine, &mut director, 10)
        .expect("phase 1 compacts");
    assert_eq!(
        phase1.summarized_identities, 0,
        "nothing is applied — nothing summarized"
    );
    assert_eq!(
        director.ledger().pending_count(),
        pending_before,
        "pending identities retained in full"
    );
    assert_eq!(
        director.ledger().pending_total(),
        pending_total_before,
        "pending amounts retained in full"
    );
    assert_eq!(director.ledger().applied_count(), 0);

    // Phase 2: confirm, reactivate (the captures apply), accrue spine
    // work — then the applied-and-confirmed set summarizes.
    let engine_persist::CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the captures confirm")
    };
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    run_ticks_near(&mut spine, &mut director, &near, 20);
    let applied_before = director.ledger().applied_count();
    assert!(
        applied_before > 0,
        "the reactivated regions' captures applied"
    );
    let phase2 = crossing
        .compact_session(&mut spine, &mut director, 10)
        .expect("phase 2 compacts");
    assert!(
        phase2.summarized_identities > 0,
        "the applied-and-confirmed identities summarized"
    );
    // The applied lane's conservation across the summarization:
    // every applied identity is either summarized or retained.
    assert_eq!(
        phase2.summarized_identities + director.ledger().applied_count() as u64,
        applied_before as u64,
        "the applied lane is conserved"
    );
    let summary = director.ledger().summarized().expect("the summary exists");
    assert_eq!(phase2.lane_boundary, summary.boundary);
    let carried: u64 = summary.applied_totals.iter().sum();
    assert_eq!(
        carried, phase2.summarized_amount,
        "the aggregates carry exactly the summarized amounts"
    );
    // The boundary is monotonic across the successive compactions.
    for region in 0..REGION_COUNT {
        assert!(
            phase2.lane_boundary[region] >= phase1.lane_boundary[region],
            "the boundary never regresses (region {region})"
        );
    }
}

#[test]
fn compacted_publication_recovers_to_the_control_continuation() {
    // THE full-vs-compacted twin over the REAL machinery: the control
    // runs the continuation live; the twin publishes a COMPACTED
    // snapshot, crashes, recovers through the actual B4 recovery,
    // and continues — the compacted publication + recovery is
    // transparent, and the recovery's own composition and
    // conservation gates (inside recover_at) prove the summarized
    // coverage and the retained lanes coherent against the chain.
    let control_dir = TestDir::new("twin-control");
    let twin_dir = TestDir::new("twin");
    let near = near_region_4();
    let (mut control_spine, mut control_director, _control_crossing) =
        confirmed_reactivation_prefix(&control_dir);
    let (mut twin_spine, mut twin_director, mut twin_crossing) =
        confirmed_reactivation_prefix(&twin_dir);

    // The twin compacts and publishes; the control is never
    // compacted (the existing paths are untouched).
    let report = twin_crossing
        .compact_session(&mut twin_spine, &mut twin_director, 10)
        .expect("the twin compacts");
    assert!(matches!(report.history, HistoryKind::Compacted { .. }));
    assert!(
        report.summarized_identities > 0,
        "the twin's lane summary carries real summarized coverage"
    );
    twin_crossing
        .publish_snapshot(&twin_spine, &twin_director)
        .expect("the compacted publication");

    // The crash: everything dropped; the recovery reconstructs from
    // the durable state alone.
    drop(twin_crossing);
    let (mut recovered_spine, mut recovered_director, recovered_crossing) =
        SessionCrossing::recover(&twin_dir.store(), &twin_dir.journal(), "prefix-campaign")
            .expect("the compacted anchor recovers");
    // The recovered spine is compacted (the save/2 form round-tripped
    // through the real session-snapshot envelope) with the same cut.
    assert!(matches!(
        recovered_spine.history_kind(),
        HistoryKind::Compacted { .. }
    ));
    // Identical continuations.
    for _ in 0..40 {
        control_director.apply(&mut control_spine, std::slice::from_ref(&near));
        control_spine.run_tick();
        recovered_director.apply(&mut recovered_spine, std::slice::from_ref(&near));
        recovered_spine.run_tick();
    }
    assert_continuation_equivalent(
        &control_spine,
        &control_director,
        &recovered_spine,
        &recovered_director,
        "the compacted twin's continuation",
    );
    drop(recovered_crossing);
}

#[test]
fn repeated_recovery_from_compacted_anchor_is_exact_once() {
    // Repeated recovery over the compacted pair: the second recovery
    // reconstructs the identical session — no double application, no
    // state movement (the same-cut compacted domains agree, the
    // strongest available cross-instance comparison).
    let dir = TestDir::new("repeat");
    let (mut spine, mut director, mut crossing) = confirmed_reactivation_prefix(&dir);
    crossing
        .compact_session(&mut spine, &mut director, 10)
        .expect("the compaction");
    crossing
        .publish_snapshot(&spine, &director)
        .expect("the publication");
    drop(crossing);

    let (first_spine, first_director, first_crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "prefix-campaign")
            .expect("the first recovery");
    drop(first_crossing);
    let (second_spine, second_director, second_crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "prefix-campaign")
            .expect("the second recovery");
    drop(second_crossing);

    assert_eq!(
        first_spine.compacted_fingerprint(),
        second_spine.compacted_fingerprint(),
        "the same-cut compacted domains agree"
    );
    assert_eq!(
        first_spine.compacted_state_fingerprint(),
        second_spine.compacted_state_fingerprint()
    );
    assert_eq!(
        first_director.ledger().pending_total(),
        second_director.ledger().pending_total(),
        "no capture was re-applied"
    );
    assert_eq!(
        world_fingerprint(&first_spine),
        world_fingerprint(&second_spine),
        "the world state is identical — exact once"
    );
    assert_eq!(
        first_spine.stats(),
        second_spine.stats(),
        "the counters are identical"
    );
}

#[test]
fn crash_after_publication_replays_post_snapshot_work_from_compacted_anchor() {
    // The B5 crash-after-confirmation arc over a COMPACTED anchor: the
    // compacted snapshot predates the first durable deactivation; the
    // crash lands after the confirmation with only the older boundary
    // on disk; the recovery replays the journaled work, and the
    // continuation redoes the lost in-memory work — converging to the
    // never-crashed twin (the legacy pipeline-free path) exactly, at
    // the compacted-safe comparison surface (the engine state through
    // the counters, watermarks, and held fingerprint; the world's
    // milestones and credits; the ledger totals; the journaled
    // region's level).
    let near = near_region_4();
    let far = far_from_region_4();
    // The never-crashed twin (the legacy path, no crossing).
    let (mut twin_spine, mut twin_director) = scenario_pair(150, &near);
    run_ticks_near(&mut twin_spine, &mut twin_director, &near, 100);
    twin_director.apply(&mut twin_spine, &[far]);
    twin_director.apply(&mut twin_spine, &[near]);
    run_ticks_near(&mut twin_spine, &mut twin_director, &near, 100);
    twin_director.apply(&mut twin_spine, &[far]);
    run_ticks_near(&mut twin_spine, &mut twin_director, &near, 50);

    // The crash arc.
    let dir = TestDir::new("crash-arc");
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "crash-campaign").unwrap();
    run_ticks_near(&mut spine, &mut director, &near, 100);
    // The compaction and the snapshot BEFORE the first deactivation —
    // the crash lands after the confirmation with only this older
    // (compacted) boundary on disk.
    let report = crossing
        .compact_session(&mut spine, &mut director, 10)
        .expect("the session compacts");
    assert!(matches!(report.history, HistoryKind::Compacted { .. }));
    crossing
        .publish_snapshot(&spine, &director)
        .expect("the compacted publication");
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    let engine_persist::CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the captures confirm before the crash")
    };
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    run_ticks_near(&mut spine, &mut director, &near, 100);
    // THE CRASH: the confirmation is on the chain, the newer work is
    // in-memory only — lost.
    drop(crossing);
    let (mut spine, mut director, mut crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "crash-campaign")
            .expect("the compacted-anchor crash recovery");
    // The recovered spine stays compacted (the save/2 form round-tripped
    // through the real session-snapshot envelope).
    assert!(matches!(
        spine.history_kind(),
        HistoryKind::Compacted { .. }
    ));
    assert_eq!(director.levels()[4], LodLevel::Dormant, "the plan replayed");
    // The continuation: the redo (the reactivation, the near run, the
    // second deactivation, the remaining ticks) — the twin's work.
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    assert_eq!(
        director.levels()[4],
        LodLevel::Full,
        "the reactivation installs over the recovered checkpoint"
    );
    run_ticks_near(&mut spine, &mut director, &near, 100);
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    run_ticks_near(&mut spine, &mut director, &near, 50);
    // THE CONVERGENCE (the compacted-safe surface).
    assert_eq!(spine.clock(), twin_spine.clock(), "the clock converges");
    assert_eq!(spine.stats(), twin_spine.stats(), "the stats converge");
    assert_eq!(
        spine.processed_through(),
        twin_spine.processed_through(),
        "the watermark converges"
    );
    assert_eq!(
        spine.max_executed_due(),
        twin_spine.max_executed_due(),
        "the max executed due converges"
    );
    assert_eq!(
        spine.held_fingerprint(),
        twin_spine.held_fingerprint(),
        "the held state converges"
    );
    assert_eq!(
        spine.world().milestones(),
        twin_spine.world().milestones(),
        "the milestones converge"
    );
    assert_eq!(
        spine.world().credits(),
        twin_spine.world().credits(),
        "the credits converge — nothing stranded, nothing duplicated"
    );
    assert_eq!(
        director.ledger().pending_total(),
        twin_director.ledger().pending_total(),
        "the pending lane converges"
    );
    assert_eq!(
        director.levels()[4],
        twin_director.levels()[4],
        "the journaled region's level converges"
    );
    assert_eq!(
        director.levels()[3],
        twin_director.levels()[3],
        "the presence-re-derivable region's level converges"
    );
}

#[test]
fn over_claiming_summary_fails_closed_without_damage() {
    // THE composition gate: a compacted anchor whose lane summary
    // claims coverage its campaign's chain does NOT confirm — here a
    // real compacted snapshot transplanted onto a fresh campaign
    // whose chain carries no confirming revisions. The recovery
    // reconstructs the anchor's coverage from the chain under the
    // boundary's attestation, finds the composition mismatch, and
    // HALTS — never a silent partial recovery, never a fabricated
    // application — and the store is undamaged (the chain's head and
    // the failed recovery's repeatability stand).
    let source_dir = TestDir::new("over-claim-source");
    let (mut spine, mut director, mut crossing) = confirmed_reactivation_prefix(&source_dir);
    crossing
        .compact_session(&mut spine, &mut director, 10)
        .expect("the compaction");
    let snapshot_revision = crossing
        .publish_snapshot(&spine, &director)
        .expect("the compacted publication");
    // Extract the snapshot object's exact bytes from the source
    // chain (the content-addressed object is portable).
    let mut source_medium = LocalFileMedium::open(source_dir.store()).unwrap();
    let record = source_medium.read_record(&snapshot_revision).unwrap();
    let object_hash = *record.content_refs.first().unwrap();
    let snapshot_bytes = source_medium.read_object(&object_hash).unwrap();
    assert!(
        !snapshot_bytes.is_empty() && director.ledger().summarized().is_some(),
        "the fixture is a real compacted snapshot"
    );
    drop(source_medium);
    drop(crossing);

    // The transplant campaign: a fresh store with the founding and
    // the transplanted snapshot object — and NO confirming
    // revisions.
    let target_dir = TestDir::new("over-claim-target");
    let mut medium = LocalFileMedium::open(target_dir.store()).unwrap();
    let founding = RevisionRecord::founding(AuthorityMode::Standalone, "transplant", 0);
    let founding_id = match medium.append(&founding, &[], None).unwrap() {
        AppendOutcome::Accepted { id, .. } => id,
        _ => panic!("the founding accepts"),
    };
    let object = StoredObject::new(snapshot_bytes).unwrap();
    let head = medium.head().unwrap().unwrap();
    let parent_epoch = medium.read_record(&head).unwrap().epoch;
    let snapshot_record = RevisionRecord::regular(head, parent_epoch, spine.clock().as_u64())
        .with_idempotency_key(object.hash().as_bytes().to_vec())
        .with_content_refs(vec![object.hash()]);
    medium
        .append(
            &snapshot_record,
            std::slice::from_ref(&object),
            Some(founding_id),
        )
        .unwrap();
    let head_before = medium.head().unwrap();
    drop(medium);

    // The recovery must fail closed on the composition gate.
    let error = expect_recovery_error(&target_dir.store(), &target_dir.journal(), "transplant");
    assert!(
        matches!(
            &error,
            TransitionError::InstallIncompatible { detail, .. }
                if *detail == "lane-summary-composition"
        ),
        "the composition gate fails closed: {error:?}"
    );
    // Without damage: the chain is unchanged, and the failed
    // recovery repeats identically (a deterministic refusal, never a
    // partial mutation).
    let mut after = LocalFileMedium::open(target_dir.store()).unwrap();
    assert_eq!(after.head().unwrap(), head_before, "the chain is intact");
    drop(after);
    let repeat = expect_recovery_error(&target_dir.store(), &target_dir.journal(), "transplant");
    assert_eq!(
        format!("{repeat:?}"),
        format!("{error:?}"),
        "the failure is deterministic"
    );
}

#[test]
fn director_v4_gates_fail_closed() {
    // The director codec's compacted-lane gates, on real bytes: the
    // discriminator (v3 full lanes / v4 the compacted form / anything
    // else refused), the lane disjointness (no retained identity at
    // or below its region's boundary), and the episode frontier (the
    // boundary never exceeds a region's episode counter). The lane
    // offsets derive exactly from the fixed-width tail; the overlap
    // target is the journal's own plan region — the deactivated
    // region holding real pending captures.
    let dir = TestDir::new("v4-gates");
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "v4-campaign").unwrap();
    run_ticks_near(&mut spine, &mut director, &near, 100);
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    // The compaction over the PRE-CONFIRMATION state: the lane
    // summary is all-zero (nothing is applied — the deactivated
    // regions' captures are pending and referenced), the v4 form
    // with real pending entries.
    crossing
        .compact_session(&mut spine, &mut director, 10)
        .expect("the compaction");
    let mut enc = Encoder::new();
    director.save(&mut enc);
    let bytes = enc.into_bytes();

    // The ledger section's offsets derive exactly from the
    // fixed-width tail and the known lane counts (the director's own
    // pub surface): the per-NPC totals end the encoding, the boundary
    // precedes them, and the two lanes walk back by their known
    // entry counts (24 bytes each) — no forward walking over the
    // variable-length state sections.
    let summary = director.ledger().summarized().expect("the compacted lanes");
    let totals_bytes = summary.applied_totals.len() * 8;
    let boundary_at = bytes.len() - totals_bytes - 8 - REGION_COUNT * 8;
    let applied_count = director.ledger().applied_count();
    let pending_count = director.ledger().pending_count();
    let discriminator_at = boundary_at - applied_count * 24 - 8 - pending_count * 24 - 8 - 1;
    assert_eq!(
        bytes[discriminator_at], 4,
        "the compacted director writes v4"
    );
    assert!(pending_count > 0, "the fixture holds real pending entries");

    // A valid re-encode round-trips (the offsets found the fields).
    {
        let mut dec = Decoder::new(&bytes);
        let restored = RegionDirector::restore(&mut dec).expect("the compacted director restores");
        assert!(restored.ledger().summarized().is_some());
    }

    // The discriminator: an unknown schema version is refused.
    {
        let mut doctored = bytes.clone();
        doctored[discriminator_at] = 5;
        let mut dec = Decoder::new(&doctored);
        assert!(RegionDirector::restore(&mut dec).is_err());
    }

    // The journal's plan regions — exactly the deactivated regions
    // holding pending captures (the overlap target), and the
    // complement set for the frontier target.
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    let plan_regions: Vec<usize> = scan
        .committed
        .iter()
        .filter_map(|transaction| {
            DeactivationPlan::restore(&transaction.plan)
                .ok()
                .map(|plan| plan.region)
        })
        .collect();
    assert!(
        !plan_regions.is_empty(),
        "the far boundary journaled real deactivations"
    );

    // The lane disjointness: raising a plan region's boundary to its
    // own current episode puts its retained pending captures at the
    // boundary: refused.
    {
        let plan_region = plan_regions[0];
        let episode = director.states()[plan_region].episode;
        let mut doctored = bytes.clone();
        let slot = boundary_at + plan_region * 8;
        doctored[slot..slot + 8].copy_from_slice(&episode.to_le_bytes());
        let mut dec = Decoder::new(&doctored);
        assert!(RegionDirector::restore(&mut dec).is_err());
    }

    // The episode frontier: a boundary above a region's episode
    // counter — a dormant checkpoint region with NO lane entries (a
    // legacy dormant region that never journaled), so only the
    // frontier gate is violated: refused.
    {
        let empty_region = (0..REGION_COUNT)
            .find(|&region| {
                !plan_regions.contains(&region)
                    && director.levels()[region] == LodLevel::Dormant
                    && director.states()[region].checkpoint.is_some()
            })
            .expect("a dormant region with no lane entries");
        let episode = director.states()[empty_region].episode;
        let mut doctored = bytes.clone();
        let slot = boundary_at + empty_region * 8;
        doctored[slot..slot + 8].copy_from_slice(&(episode + 5).to_le_bytes());
        let mut dec = Decoder::new(&doctored);
        assert!(RegionDirector::restore(&mut dec).is_err());
    }
}

#[test]
fn rejected_compound_compaction_leaves_the_session_unchanged() {
    // F1's regression (the review corrective): a REJECTED compound
    // compaction leaves BOTH session components byte-unchanged — the
    // spine-window preflight decides the compound's typed refusals
    // read-only, BEFORE the lane summarization can mutate anything.
    // The fixture holds genuinely eligible applied-and-confirmed
    // consequences (the reactivated regions' sets), so the lane half
    // WOULD summarize if the compound proceeded — the byte-identical
    // session after every refusal proves it never did (on the
    // pre-corrective ordering this assertion fails: the lane
    // summarization ran before the rejected spine cut).
    let dir = TestDir::new("f1-atomicity");
    let (mut spine, mut director, mut crossing) = confirmed_reactivation_prefix(&dir);

    // The degenerate windows on the fresh session (no existing
    // summarized records): both typed refusals, each leaving the
    // COMPLETE session envelope byte-identical (the spine history
    // representation, the active state, the world, the director
    // lanes, the frontiers — everything save_session carries).
    let ledger_len = spine.ledger_retained().len() as u64;
    assert!(ledger_len > 0, "the fixture holds a real history");
    let bytes_before = save_session(&spine, &director).unwrap();
    for (retain, expected) in [
        (
            ledger_len + 5,
            CompactionError::RetainExceedsLedger {
                ledger_len,
                retain: ledger_len + 5,
            },
        ),
        (
            ledger_len,
            CompactionError::NothingToSummarize {
                ledger_len,
                retain: ledger_len,
            },
        ),
    ] {
        let error = crossing
            .compact_session(&mut spine, &mut director, retain)
            .unwrap_err();
        let TransitionError::Compaction(refusal) = error else {
            panic!("the typed compaction refusal surfaces (retain {retain}): {error:?}")
        };
        assert_eq!(
            refusal, expected,
            "the exact typed refusal (retain {retain})"
        );
        assert_eq!(
            save_session(&spine, &director).unwrap(),
            bytes_before,
            "the rejected compound left the session byte-unchanged (retain {retain})"
        );
    }

    // The valid compaction succeeds — the eligibility was real.
    let report = crossing
        .compact_session(&mut spine, &mut director, 10)
        .unwrap();
    assert!(
        report.summarized_identities > 0,
        "the fixture was genuinely eligible"
    );
    assert!(matches!(report.history, HistoryKind::Compacted { .. }));

    // The failed repeats after the prior success: the same refusals
    // against the compacted session, again byte-unchanged.
    let bytes_after_success = save_session(&spine, &director).unwrap();
    let compacted_len = spine.ledger_retained().len() as u64;
    assert_eq!(compacted_len, 10, "the retained window is exact");
    for (retain, expected) in [
        (
            compacted_len,
            CompactionError::NothingToSummarize {
                ledger_len: compacted_len,
                retain: compacted_len,
            },
        ),
        (
            compacted_len + 1,
            CompactionError::RetainExceedsLedger {
                ledger_len: compacted_len,
                retain: compacted_len + 1,
            },
        ),
    ] {
        let error = crossing
            .compact_session(&mut spine, &mut director, retain)
            .unwrap_err();
        let TransitionError::Compaction(refusal) = error else {
            panic!("the repeat refusal surfaces (retain {retain}): {error:?}")
        };
        assert_eq!(
            refusal, expected,
            "the exact repeat refusal (retain {retain})"
        );
        assert_eq!(
            save_session(&spine, &director).unwrap(),
            bytes_after_success,
            "the rejected repeat left the session byte-unchanged (retain {retain})"
        );
    }

    // The subsequent valid compaction, publication, and the real B4
    // recovery remain successful.
    let repeat = crossing
        .compact_session(&mut spine, &mut director, 5)
        .unwrap();
    assert!(matches!(repeat.history, HistoryKind::Compacted { .. }));
    crossing.publish_snapshot(&spine, &director).unwrap();
    drop(crossing);
    let (recovered_spine, recovered_director, recovered_crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "prefix-campaign").unwrap();
    assert!(matches!(
        recovered_spine.history_kind(),
        HistoryKind::Compacted { .. }
    ));
    assert!(
        recovered_director.ledger().summarized().is_some(),
        "the lane summary survived the round trip"
    );
    drop(recovered_crossing);
}
