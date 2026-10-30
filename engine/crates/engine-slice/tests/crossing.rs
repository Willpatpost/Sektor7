//! Stage 5 Batch 4: the six-phase crossing's preparatory half — the
//! DeactivationPlan object and its journal codec. Read-only
//! preparation (phases 1–2 mutate nothing), byte-identical
//! round-trips, the legacy-path equivalence (the plan's checkpoint
//! entries and captures are EXACTLY the legacy deactivation's — the
//! shared gathering), and the fail-closed corruption corpus.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use engine_core::{Encoder, EventClass, LodLevel, PresenceSample, Spine};
use engine_persist::{
    AppendOutcome, AwardCheckpoint, AwardState, CancellationReason, CanonicalMedium,
    ClassifiedSpan, CommitOutcome, CommitPipeline, ConsequenceEnvelope, ConsequenceId,
    CounterValues, Coverage, DurabilityClass, EncounterJournal, FaultAction, FrontierEntry, Hash,
    JournalFaultControl, JournalFaultPoint, KeyPhase, LocalFileMedium, MediumError, OccurrenceSpan,
    PipelineError, Reconciliation, RecoveryError, RevisionRecord, SignalLane, SourceBaseline,
    SourcePosition, StoredObject, SyncTier, T1Journal, TransactionInput, UnknownReason,
};
use engine_slice::crossing::{
    DeactivationPlan, SessionCrossing, TransitionError, restore_session, save_session,
};
use engine_slice::npc::{
    NpcConfig, NpcWorld, POSITION_DISCARDED, REGION_COUNT, install_initial_schedules,
};
use engine_slice::region::{CaptureStatus, RegionConfig, RegionDirector, stage4_state_fingerprint};
use engine_slice::sim::{SimConfig, SimDriver};
use engine_slice::store::MediumSource;

/// A Lod-mode spine with real executed work (no director — the plan
/// object's unit surface; the director-driven equivalence is the
/// legacy test below).
fn lod_spine(ticks: u64) -> Spine<NpcWorld> {
    let config = NpcConfig {
        region_lods_enabled: true,
        ..NpcConfig::default()
    };
    let mut spine = Spine::with_reservation(NpcWorld::new(config.clone()), 200, 60);
    install_initial_schedules(&mut spine, &config);
    spine.run_ticks(ticks);
    spine
}

#[test]
fn preparation_is_read_only() {
    // Phases 1–2 (§5.5.1): collect_matching plus the shared gathering
    // build the complete plan WITHOUT mutation — no suspension, no
    // accounting, no fingerprint movement. A pre-durable abort costs
    // nothing and the pre-transition state stands.
    let spine = lod_spine(150);
    let engine_before = spine.engine_fingerprint();
    let lanes_before = spine.lane_state_fingerprint();
    let held_before = spine.held_fingerprint();
    let stats_before = spine.stats();
    let class = EventClass::new(4);
    let collected = spine.collect_matching(|view| view.class == class);
    assert!(
        !collected.is_empty(),
        "region 4 has queued work to plan over"
    );
    let plan = DeactivationPlan::prepare(
        4,
        LodLevel::Full,
        1,
        spine.clock(),
        spine.world(),
        &collected,
    );
    assert_eq!(plan.ids().len(), collected.len());
    assert_eq!(spine.engine_fingerprint(), engine_before);
    assert_eq!(spine.lane_state_fingerprint(), lanes_before);
    assert_eq!(spine.held_fingerprint(), held_before);
    assert_eq!(spine.stats().scheduled, stats_before.scheduled);
    assert_eq!(spine.held_count(), 0, "nothing was suspended");
}

#[test]
fn plan_round_trips_byte_identically() {
    let spine = lod_spine(150);
    let class = EventClass::new(4);
    let collected = spine.collect_matching(|view| view.class == class);
    let plan = DeactivationPlan::prepare(
        4,
        LodLevel::Full,
        1,
        spine.clock(),
        spine.world(),
        &collected,
    );
    assert!(
        !plan.captures.is_empty(),
        "the fixture accrued real consequences to capture"
    );
    let bytes = plan.canonical_bytes().expect("the plan encodes");
    let restored = DeactivationPlan::restore(&bytes).expect("the plan restores");
    assert_eq!(restored, plan);
    assert_eq!(
        restored.canonical_bytes().expect("re-encode"),
        bytes,
        "the restored plan re-encodes byte-identically"
    );
}

#[test]
fn prepared_plan_matches_the_legacy_deactivation() {
    // THE crossing equivalence: two identical scenario drivers — one
    // through the legacy director-driven deactivation (the
    // mutation-first Stage 4 path), one stopped just before it — the
    // read-only preparation's checkpoint entries and captures are
    // EXACTLY the legacy checkpoint's (the shared gathering; the
    // six-phase preparation is behaviorally identical).
    let near_region_4 = PresenceSample {
        position: (-100.0, 0.0),
        velocity: (0.0, 0.0),
    };
    let far_from_region_4 = PresenceSample {
        position: (100.0, 0.0),
        velocity: (0.0, 0.0),
    };
    let mut legacy = SimDriver::with_regions(
        NpcConfig::default(),
        SimConfig::default(),
        RegionConfig::default(),
        &[near_region_4],
    )
    .expect("the legacy driver constructs");
    legacy.run_headless(150);
    legacy.set_presence(vec![far_from_region_4]);
    legacy.run_headless(1);
    let director = legacy.director().expect("director");
    let state = &director.states()[4];
    assert_eq!(state.level, LodLevel::Dormant, "region 4 deactivated");
    let checkpoint = state.checkpoint.as_ref().expect("checkpoint retained");
    let episode = state.episode;

    // The identical pre-deactivation state, prepared read-only.
    let mut twin = SimDriver::with_regions(
        NpcConfig::default(),
        SimConfig::default(),
        RegionConfig::default(),
        &[near_region_4],
    )
    .expect("the twin driver constructs");
    twin.run_headless(150);
    let spine = twin.spine();
    let class = EventClass::new(4);
    let collected = spine.collect_matching(|view| view.class == class);
    let plan = DeactivationPlan::prepare(
        4,
        LodLevel::Full,
        episode,
        spine.clock(),
        spine.world(),
        &collected,
    );
    // The tick and the whole checkpoint match the legacy
    // deactivation's — same gathering, same order.
    assert_eq!(plan.tick, checkpoint.dormant_since);
    assert_eq!(plan.entries, checkpoint.entries);
    // The captures match: the twin's scratch (identical to the
    // legacy's pre-deactivation scratch) per claimed entry.
    let mut expected: Vec<(usize, u64)> = checkpoint
        .entries
        .iter()
        .filter(|entry| entry.has_signal)
        .map(|entry| (entry.npc, twin.spine().world().pending_signals()[entry.npc]))
        .collect();
    let mut actual = plan.captures.clone();
    expected.sort_by_key(|(npc, _)| *npc);
    actual.sort_by_key(|(npc, _)| *npc);
    assert_eq!(actual, expected);
    // The conservation witness: the plan's captured deltas are exactly
    // the ledger's pending total.
    assert_eq!(
        plan.captures.iter().map(|(_, delta)| delta).sum::<u64>(),
        director.ledger().pending_total()
    );
}

#[test]
fn plan_corruption_fails_closed() {
    // The plan blob's own gates, on real bytes: the region bound, the
    // level discriminant, the canonical (due, seq) obligation order —
    // plus trailing bytes and strided truncation (a short or padded
    // input never yields a plan).
    let spine = lod_spine(150);
    let class = EventClass::new(4);
    let collected = spine.collect_matching(|view| view.class == class);
    let plan = DeactivationPlan::prepare(
        4,
        LodLevel::Full,
        1,
        spine.clock(),
        spine.world(),
        &collected,
    );
    let bytes = plan.canonical_bytes().expect("the plan encodes");
    // The fixed-width prefix layout: the domain tag (a u64 length +
    // 32 bytes), the region, the from-level discriminant, then the
    // episode, tick, and counts. The scenario's routine chains are
    // dependency-free, so each obligation is exactly 37 bytes (seq,
    // due, class, payload, field, dependency count).
    const DOMAIN_LEN: usize = 8 + 32;
    const REGION_AT: usize = DOMAIN_LEN;
    const FROM_LEVEL_AT: usize = REGION_AT + 8;
    const OBLIGATIONS_AT: usize = FROM_LEVEL_AT + 1 + 8 + 8 + 8;
    const OBLIGATION_BYTES: usize = 8 + 8 + 4 + 8 + 1 + 8;
    assert!(bytes.len() >= OBLIGATIONS_AT + 2 * OBLIGATION_BYTES);

    let accepts = |bytes: &[u8]| DeactivationPlan::restore(bytes).is_ok();
    // The region bound: region 999 is not this workload's.
    let mut doctored = bytes.clone();
    doctored[REGION_AT..REGION_AT + 8].copy_from_slice(&999u64.to_le_bytes());
    assert!(!accepts(&doctored), "region 999 rejected");
    // The level discriminant.
    let mut doctored = bytes.clone();
    doctored[FROM_LEVEL_AT] = 9;
    assert!(!accepts(&doctored), "unknown level rejected");
    // The canonical (due, seq) order: swap the first two obligations
    // (each 37 bytes) — the reversed pair violates the strict order.
    let mut doctored = bytes.clone();
    let first = doctored[OBLIGATIONS_AT..OBLIGATIONS_AT + OBLIGATION_BYTES].to_vec();
    let second =
        doctored[OBLIGATIONS_AT + OBLIGATION_BYTES..OBLIGATIONS_AT + 2 * OBLIGATION_BYTES].to_vec();
    doctored[OBLIGATIONS_AT..OBLIGATIONS_AT + OBLIGATION_BYTES].copy_from_slice(&second);
    doctored[OBLIGATIONS_AT + OBLIGATION_BYTES..OBLIGATIONS_AT + 2 * OBLIGATION_BYTES]
        .copy_from_slice(&first);
    assert!(!accepts(&doctored), "out-of-order obligations rejected");
    // Trailing bytes after a complete plan.
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(!accepts(&trailing), "trailing bytes rejected");
    // Truncation: strided cuts and the final sixteen bytes.
    let mut cut = 0;
    while cut < bytes.len() {
        assert!(!accepts(&bytes[..cut]), "cut at {cut} rejected");
        cut += 89;
    }
    for cut in bytes.len() - 16..bytes.len() {
        assert!(!accepts(&bytes[..cut]), "cut at {cut} rejected");
    }
}

// ------------------------------------------- the durable six-phase driver

/// A temporary campaign directory (the B3 test pattern, local to this
/// suite): a unique child of the system temp dir, removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-b4-crossing-{name}-{}",
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

/// The real pair (no SimDriver — the crossing needs the spine and the
/// director as separate mutable references): the Lod-mode spine plus
/// the director, driven through the driver's own per-tick sequence.
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

/// The durable boundary: every region whose target under the presence
/// differs routes — Dormant targets through the six-phase
/// deactivation (journal + install), reactivations (Dormant → live)
/// through the durable activation (the legacy install + the live
/// application report), and the remaining active↔active
/// reclassifications through the legacy semantics — the same
/// transition set `RegionDirector::apply` would make.
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

#[test]
fn durable_deactivation_matches_the_legacy_state() {
    // THE six-phase composition proof: two identical scenario pairs —
    // one through the legacy director.apply far boundary, one through
    // the durable crossing (journal + install) — produce EXACTLY the
    // same authoritative state (the combined Stage 4 fingerprint),
    // and the journal holds one committed transaction per
    // deactivation, each carrying its plan blob and capture
    // envelopes whose identities are exactly the ledger's captured
    // keys.
    let far = far_from_region_4();
    let (mut legacy_spine, mut legacy_director) = scenario_pair(150, &near_region_4());
    legacy_director.apply(&mut legacy_spine, &[far]);

    let (mut spine, mut director) = scenario_pair(150, &near_region_4());
    let dir = TestDir::new("equivalence");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "equivalence-campaign")
        .expect("the crossing opens");
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);

    assert_eq!(
        stage4_state_fingerprint(&spine, &director),
        stage4_state_fingerprint(&legacy_spine, &legacy_director),
        "the durable six-phase boundary produces exactly the legacy state"
    );
    // The journal holds the deactivation transactions with the plan
    // blobs, and the envelope identities are exactly the ledger's
    // captured keys.
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    assert!(!scan.committed.is_empty(), "the transitions were journaled");
    let mut expected_ids = HashSet::new();
    for (region, state) in director.states().iter().enumerate() {
        if let Some(checkpoint) = &state.checkpoint {
            for entry in checkpoint.entries.iter().filter(|entry| entry.has_signal) {
                expected_ids.insert(ConsequenceId::new(
                    region as u32,
                    state.episode,
                    entry.npc as u32,
                ));
            }
        }
    }
    let journal_ids: HashSet<ConsequenceId> = scan
        .committed
        .iter()
        .flat_map(|transaction| transaction.envelopes.iter().map(|envelope| envelope.id))
        .collect();
    assert_eq!(journal_ids, expected_ids);
    for transaction in &scan.committed {
        assert!(
            !transaction.plan.is_empty(),
            "the plan blob is carried whole"
        );
    }
}

#[test]
fn pre_durable_failure_aborts_intact_and_retries_identically() {
    // §5.5.1 case 5: a persistence failure after only some capture
    // writes — the transaction never commits, phase 3/4 fails, the
    // deactivation ABORTS with a typed error: the pre-transition
    // state is intact (nothing was mutated — the fingerprint
    // unchanged, nothing suspended), the partial journal records are
    // inert, and the retry through a fresh clean journal commits with
    // identical keys (the episode frontier never advanced) and
    // produces the legacy state.
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near_region_4());
    let before = stage4_state_fingerprint(&spine, &director);
    // The scenario itself holds the far-side regions' obligations
    // (five regions went dormant at tick 0 under the near-presence);
    // the abort must leave exactly these, no more.
    let held_before = spine.held_count();
    assert_eq!(held_before, 625, "five far regions dormant since tick 0");
    let dir = TestDir::new("abort");
    let faulted = T1Journal::open_with(
        dir.journal(),
        Some(
            JournalFaultControl::new()
                .arm(JournalFaultPoint::CapturesWritten, FaultAction::InjectError),
        ),
    )
    .unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), faulted, "abort-campaign").unwrap();
    let now = spine.clock();
    let error = crossing
        .deactivate(&mut spine, &mut director, 4, now)
        .unwrap_err();
    assert!(
        matches!(error, TransitionError::JournalFailed(_)),
        "the journal failure is typed: {error:?}"
    );
    assert_eq!(
        stage4_state_fingerprint(&spine, &director),
        before,
        "the pre-transition state stands, byte-exactly"
    );
    assert_eq!(
        director.levels()[4],
        LodLevel::Full,
        "the region stays live"
    );
    assert_eq!(
        spine.held_count(),
        held_before,
        "nothing was suspended by the attempt"
    );
    // The retry: drop the faulted session FIRST — the medium's
    // HEAD.lock and the journal's sidecar lock are session-scoped,
    // so a same-process second open over the same campaign blocks
    // until the first holder releases (the B2/B3 lock discipline) —
    // then a fresh clean journal (the fault fires once; the clean
    // open sweeps the inert partial records): identical keys, the
    // legacy state.
    drop(crossing);
    let clean = T1Journal::open(dir.journal()).unwrap();
    let mut retry = SessionCrossing::open(&dir.store(), clean, "abort-campaign").unwrap();
    durable_boundary(&mut retry, &mut spine, &mut director, &far);
    let (mut legacy_spine, mut legacy_director) = scenario_pair(150, &near_region_4());
    legacy_director.apply(&mut legacy_spine, &[far]);
    assert_eq!(
        stage4_state_fingerprint(&spine, &director),
        stage4_state_fingerprint(&legacy_spine, &legacy_director),
        "the retry produces exactly the legacy state"
    );
}

#[test]
fn install_defensive_failure_is_the_halt_signal() {
    // §5.5.2: the staged-commit discipline's defensive validation —
    // a plan-vs-live incompatibility detected at phase 5 is the
    // fail-closed halt signal (a bug, never "continue with the region
    // unchanged"). The seam: the durability completes, then an
    // obligation is moved out from under the plan before the install
    // — the re-validation rejects with the typed halt while the
    // durable intent stands in the journal.
    let (mut spine, mut director) = scenario_pair(150, &near_region_4());
    let dir = TestDir::new("halt");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "halt-campaign").unwrap();
    let now = spine.clock();
    let error = crossing
        .debug_deactivate_around_install(&mut spine, &mut director, 4, now, |spine, _| {
            // Sabotage: hold one of region 4's obligations between
            // durability and installation.
            let class = EventClass::new(4);
            let Some(victim) = spine
                .collect_matching(|view| view.class == class)
                .first()
                .map(|view| view.id)
            else {
                panic!("region 4 has obligations");
            };
            spine.suspend_ids(std::slice::from_ref(&victim));
        })
        .unwrap_err();
    assert!(
        matches!(
            error,
            TransitionError::InstallIncompatible {
                region: 4,
                detail: "obligation-moved"
            }
        ),
        "the defensive validation halts: {error:?}"
    );
    // The durable intent stands; the install refused — the caller's
    // correct response is the session halt, which this test models by
    // simply not continuing.
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    assert_eq!(scan.committed.len(), 1, "the intent is durable");
}

#[test]
fn durable_reactivation_cycle_matches_the_legacy_state() {
    // The full durable cycle: accrual → the durable far boundary (the
    // award captures journaled + installed) → one tick → the durable
    // near boundary (the legacy activation installed + the live
    // application reported) — the combined state identical to the
    // legacy director.apply-driven cycle at every boundary, the
    // credits applied exactly once on both sides, and the per-key
    // phase progressing JournaledT1 → AppliedToLivePending (the
    // activation adds NO journal transactions — the captured→credits
    // transfer is the live application of the already-durable
    // captures, re-derivable from the ledger plus the activation).
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut legacy_spine, mut legacy_director) = scenario_pair(150, &near);
    legacy_director.apply(&mut legacy_spine, &[far]);
    legacy_spine.run_tick();
    legacy_director.apply(&mut legacy_spine, &[near]);

    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("cycle");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "cycle-campaign").unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    let sample = ConsequenceId::new(4, 1, 4);
    assert_eq!(
        crossing.key_phase(sample),
        Some(KeyPhase::JournaledT1),
        "the capture is durable, pending its live application"
    );
    spine.run_tick();
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);

    assert_eq!(
        stage4_state_fingerprint(&spine, &director),
        stage4_state_fingerprint(&legacy_spine, &legacy_director),
        "the full durable cycle produces exactly the legacy state"
    );
    let credits: u64 = spine.world().credits().iter().sum();
    assert!(
        credits > 0,
        "the reactivations applied the captured consequences"
    );
    assert_eq!(
        credits,
        legacy_spine.world().credits().iter().sum::<u64>(),
        "the credits match the legacy cycle exactly"
    );
    // The per-key phase progressed through the live application, and
    // the journal holds ONLY the award captures (one transaction —
    // the capture-bearing deactivation of region 4; the empty-scratch
    // deactivations and the activation journal nothing).
    assert_eq!(
        crossing.key_phase(sample),
        Some(KeyPhase::AppliedToLivePending),
        "the reactivation reported the live application"
    );
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    assert_eq!(
        scan.committed.len(),
        1,
        "only the capture-bearing deactivation journaled"
    );
    assert!(
        !scan.committed[0].envelopes.is_empty(),
        "the award captures are journaled"
    );
}

// ------------------------------------------------- the crash recovery

fn far_from_everything() -> PresenceSample {
    PresenceSample {
        position: (2000.0, 2000.0),
        velocity: (0.0, 0.0),
    }
}

#[test]
fn recovery_reinstalls_the_deactivation_after_a_crash() {
    // THE W2 window, honestly simulated: the session snapshot
    // published at the accrual peak; the durable deactivation
    // (journal + install) completes; then the in-memory state is
    // LOST — the session reconstructs from the SNAPSHOT alone (the
    // pre-deactivation state) while the journal retains the durable
    // intent — and the recovery re-installs the deactivation from
    // the plan. The recovered region 4 — the deactivation that
    // carried durable consequences — matches the lost state exactly
    // (the checkpoint, the ledger, the sentinels, the milestones);
    // the empty-scratch deactivations of regions 3 and 5 are
    // presence-re-derivable by design (the recovery leaves them
    // live; the next boundary re-derives them); the B3 reconcile
    // verified no double application inside the recovery; and the
    // re-drive confirms the captures canonically.
    let near = near_region_4();
    let far = far_from_everything();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("recovery");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "recovery-campaign").unwrap();
    // The verified snapshot boundary.
    crossing.publish_snapshot(&spine, &director).unwrap();
    // The durable far boundary: region 4 with captures, regions 3
    // and 5 empty-scratch.
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    // THE CRASH: the session is dropped (the in-memory crossing
    // lost; the spine/director pair below represents the lost
    // state, kept for comparison); the recovery reconstructs from
    // the snapshot + the journal.
    drop(crossing);
    let (recovered_spine, recovered_director, mut recovered_crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "recovery-campaign").unwrap();
    assert_eq!(recovered_director.levels()[4], LodLevel::Dormant);
    assert_eq!(
        recovered_director.states()[4].checkpoint,
        director.states()[4].checkpoint,
        "the plan-installed checkpoint matches the lost state's exactly"
    );
    assert_eq!(
        recovered_director.ledger().pending_total(),
        director.ledger().pending_total(),
        "the recovered ledger matches the lost state's exactly"
    );
    assert_eq!(
        recovered_spine.world().milestones(),
        spine.world().milestones(),
        "the crossings were in the snapshot — the milestones match exactly"
    );
    for npc in (4..1000).step_by(8).take(5) {
        assert_eq!(
            recovered_spine.world().positions()[npc],
            POSITION_DISCARDED,
            "the region-4 sentinels are installed"
        );
    }
    assert_eq!(
        recovered_director.levels()[3],
        LodLevel::Dormant,
        "the empty-scratch deactivation is captured by region 4's covering \
         snapshot (the C1 discipline: the covering is the state at the \
         consequential crossing — region 3's live install is inside it; \
         the pre-C1 shape left it presence-re-derivable because the only \
         snapshot predated every install)"
    );
    // The pending T1 re-drive: the canonical confirmation.
    let CommitOutcome::Confirmed(receipt) = recovered_crossing.commit_canonical().unwrap() else {
        panic!("the re-drive confirms the recovered captures");
    };
    assert!(receipt.keys.contains(&ConsequenceId::new(4, 1, 4)));
    assert_eq!(
        recovered_crossing.key_phase(ConsequenceId::new(4, 1, 4)),
        Some(KeyPhase::InConfirmedChain),
        "the recovered capture is canonically confirmed"
    );
}

#[test]
fn recovery_from_an_older_anchor_restores_post_snapshot_crossings() {
    // The OLDER-anchor recovery (the GO's item): the snapshot at
    // tick 50 — BEFORE region 4's crossings (the scratch empty) —
    // the accrual to 150 crosses them, the durable deactivation
    // captures them, the crash loses the in-memory state; the
    // recovery restores the post-snapshot crossings (the capture
    // delta minus the snapshot's scratch — here the full delta)
    // exactly as they were at the deactivation, then installs the
    // plan: the recovered milestones INCLUDE the restored crossings
    // (without the restore they would be short by exactly the
    // captured count).
    let near = near_region_4();
    let far = far_from_everything();
    let (mut spine, mut director) = scenario_pair(50, &near);
    let scratch_at_snapshot: u64 = spine.world().pending_signals().iter().sum();
    assert_eq!(
        scratch_at_snapshot, 0,
        "the older snapshot predates the crossings"
    );
    let dir = TestDir::new("older");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "older-campaign").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    // Accrue to 150 — the post-snapshot crossings — then the
    // durable deactivation.
    for _ in 0..100 {
        director.apply(&mut spine, std::slice::from_ref(&near));
        spine.run_tick();
    }
    // THE COVERING PUBLICATION (the frontier guard's discipline, the
    // C protocol's manual form): the plan's obligations include the
    // accrual's follow-up identities — beyond the tick-50 anchor's
    // allocation frontier. The covering snapshot (after the accrual,
    // before the boundary) puts them within the recovered frontier;
    // the crossings-restore arithmetic below is exercised over the
    // covering snapshot's nonzero scratch (the same restore path the
    // zero-scratch anchor case exercised — the exact-match
    // assertions are base-independent).
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    // THE CRASH + the recovery.
    drop(crossing);
    let (recovered_spine, recovered_director, _recovered_crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "older-campaign").unwrap();
    assert_eq!(recovered_director.levels()[4], LodLevel::Dormant);
    assert_eq!(
        recovered_director.states()[4].checkpoint,
        director.states()[4].checkpoint,
        "the plan-installed checkpoint matches the lost state's exactly"
    );
    assert_eq!(
        recovered_director.ledger().pending_total(),
        director.ledger().pending_total(),
        "the recovered ledger matches the lost state's exactly"
    );
    assert_eq!(
        recovered_spine.world().milestones(),
        spine.world().milestones(),
        "the post-snapshot crossings are restored — the milestones match the lost state's exactly"
    );
    assert!(
        spine.world().milestones().iter().any(|m| *m > 0),
        "the fixture crossed milestones after the snapshot"
    );
}

// --------------------------------------------------- the demonstrations

#[test]
fn encounter_rollback_policy_against_real_simulation_state() {
    // The GO's item 7 — the minimal encounter demonstration against
    // REAL simulation state: an encounter over a region-4 NPC's live
    // consequential state (the counters read from the world at the
    // accrual peak), with the accepted rollback policy — the durable
    // promotions SURVIVE the failure; the provisional effects roll
    // back coherently (the eligibility restored — the legitimate
    // re-attempt re-earns them); the gameplay policy (which effects
    // are durable) stays outside the machinery.
    let near = near_region_4();
    let (spine, _director) = scenario_pair(150, &near);
    let world = spine.world();
    let npc = 4;
    let real_counters = CounterValues {
        milestones: world.milestones()[npc],
        pending_signals: world.pending_signals()[npc],
        captured: 0,
        credits: world.credits()[npc],
    };
    assert!(
        real_counters.milestones > 0,
        "the NPC crossed real milestones"
    );
    // The encounter: the pending lane (a crossing feeds the pending
    // lane at execution — the application's boundary choice), over a
    // demonstration identity in the region's own space.
    let id = ConsequenceId::new(4, 1000, npc as u32);
    let mut encounter = EncounterJournal::new(SignalLane::Pending);
    encounter
        .begin_participant(id, AwardState::new(), real_counters)
        .unwrap();
    // The effects: two occurrences recorded provisional; occurrence
    // 1 promoted durable mid-encounter (the explicit gameplay
    // decision — the machinery executes it).
    assert!(
        encounter
            .record_effect(id, 1, DurabilityClass::EncounterProvisional)
            .unwrap()
    );
    assert!(
        encounter
            .record_effect(id, 2, DurabilityClass::EncounterProvisional)
            .unwrap()
    );
    assert!(encounter.promote(id, 1).unwrap());
    let (award, counters) = encounter.participant_state(id).unwrap();
    assert!(
        award.durable.covers(1),
        "the promoted occurrence is durable"
    );
    assert!(award.provisional.covers(2), "the other is provisional");
    assert_eq!(
        counters.milestones,
        real_counters.milestones + 2,
        "both units recorded"
    );
    // THE FAILURE: the encounter rolls back — the provisional
    // discards (the eligibility restored), the durable promotion
    // SURVIVES with its counter.
    encounter.rollback().unwrap();
    let (award, counters) = encounter.participant_state(id).unwrap();
    assert!(
        award.durable.covers(1),
        "the durable promotion survives the failure"
    );
    assert!(
        !award.provisional.covers(2),
        "the provisional effect rolled back — the eligibility restored"
    );
    assert!(
        !award.provisional.covers(1),
        "the promoted occurrence never returns to the provisional lane"
    );
    assert_eq!(
        counters.milestones,
        real_counters.milestones + 1,
        "the durable unit's counter survives the rollback"
    );
    assert_eq!(
        counters.pending_signals,
        real_counters.pending_signals + 1,
        "the durable unit's signal survives in its lane"
    );
    assert!(counters.conservation_holds());
    // The legitimate re-attempt: a fresh encounter from the
    // rolled-back state — the discarded occurrence is earnable
    // again, the survived promotion is never re-awarded.
    let mut survived = AwardState::new();
    survived.award_durable(1);
    let mut reattempt = EncounterJournal::new(SignalLane::Pending);
    reattempt
        .begin_participant(id, survived, *counters)
        .unwrap();
    assert!(
        reattempt
            .record_effect(id, 2, DurabilityClass::EncounterProvisional)
            .unwrap(),
        "the rolled-back occurrence is earnable again — the reattempt eligibility preserved"
    );
    assert!(
        !reattempt
            .record_effect(id, 1, DurabilityClass::EncounterProvisional)
            .unwrap(),
        "the durable-covered occurrence is never a second award"
    );
}

#[test]
fn irreversible_operations_gate_on_canonical_confirmation() {
    // The GO's item 8: pending T1 effects are clearly distinct from
    // confirmed canonical progress — an irreversible operation
    // dependent on unconfirmed milestones stays GATED until the
    // canonical confirmation. The durable status surface
    // (`key_phase`) distinguishes every phase; the gate opens only
    // on InConfirmedChain (the demo's policy — the engine surfaces
    // the phase, the gameplay decides).
    let near = near_region_4();
    let far = far_from_everything();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("gating");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "gating-campaign").unwrap();
    let gate = |crossing: &SessionCrossing, id: ConsequenceId| {
        matches!(crossing.key_phase(id), Some(KeyPhase::InConfirmedChain))
    };
    let key = ConsequenceId::new(4, 1, 4);
    // Before the capture: unknown — gated.
    assert!(!gate(&crossing, key), "unknown progress never permits");
    // The durable deactivation: the capture is journaled — T1
    // pending is NOT confirmed canonical progress. Still gated.
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    assert_eq!(crossing.key_phase(key), Some(KeyPhase::JournaledT1));
    assert!(
        !gate(&crossing, key),
        "pending T1 stays gated — never conflated with confirmation"
    );
    // The reactivation: the live application — live effects exist,
    // the canonical confirmation does not. Still gated.
    spine.run_tick();
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    assert_eq!(
        crossing.key_phase(key),
        Some(KeyPhase::AppliedToLivePending)
    );
    assert!(
        !gate(&crossing, key),
        "live-applied pending stays gated — the pending-vs-confirmed distinction"
    );
    // The canonical confirmation: the gate opens.
    let CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the re-drive confirms");
    };
    assert_eq!(crossing.key_phase(key), Some(KeyPhase::InConfirmedChain));
    assert!(
        gate(&crossing, key),
        "the canonically confirmed progress permits the irreversible operation"
    );
}

// -------------------------- the Batch 4 review-corrective regressions

/// The double-apply reproduction (the review's first corrective): a
/// snapshot published BETWEEN two deactivation episodes — after
/// episode 1's capture and reactivation, before episode 2. Episode
/// 1's crossings are ALREADY materialized in the snapshot (its
/// ledger carries them applied, its credits hold them); the journal
/// holds both episodes' claims. The recovery must derive its
/// materialization evidence from the SNAPSHOT — episode 1 is
/// pre-snapshot by the director's own episode counter, so its
/// claimed crossings are NEVER re-applied — and restore only
/// episode 2's post-snapshot crossings.
#[test]
fn recovery_does_not_reapply_presnapshot_episode_crossings() {
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("no-reapply");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "no-reapply-campaign").unwrap();
    // Episode 1: the durable far boundary (region 4's captures
    // journaled), then the durable reactivation (the captures applied
    // to credits — the ledger's applied lane).
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    assert!(
        director.ledger().applied_count() > 0,
        "episode 1's captures are applied in the ledger"
    );
    assert!(
        spine.world().credits().iter().sum::<u64>() > 0,
        "the credits hold episode 1's effects"
    );
    // THE SNAPSHOT between the episodes: episode 1 is INSIDE it (the
    // director's episode counter, the applied ledger, the credits);
    // episode 2 happens after it.
    crossing.publish_snapshot(&spine, &director).unwrap();
    // Episode 2: further accrual, then the second durable far
    // boundary.
    for _ in 0..150 {
        director.apply(&mut spine, std::slice::from_ref(&near));
        spine.run_tick();
    }
    // THE COVERING PUBLICATION (the frontier guard's discipline, the
    // C protocol's manual form): episode 2's plan will carry
    // obligations allocated during the accrual — identities beyond
    // the mid-episodes snapshot's allocation frontier. Publishing
    // the covering snapshot here (after the accrual, before the
    // durable boundary) puts every identity the plan references
    // below the recovered frontier — the within-frontier class the
    // recovery can reconstruct coherently. WITHOUT it the recovery
    // is correctly refused (the typed insufficient-evidence class);
    // this is exactly the ordering the covering-publication design
    // mandates at the real boundary.
    crossing.publish_snapshot(&spine, &director).unwrap();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    // THE CRASH + the recovery from the mid-session snapshot.
    drop(crossing);
    let (recovered_spine, recovered_director, mut recovered_crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "no-reapply-campaign")
            .expect("the recovery succeeds without re-applying episode 1");
    // Episode 2 installed; episode 1 never re-applied.
    assert_eq!(recovered_director.levels()[4], LodLevel::Dormant);
    assert_eq!(
        recovered_director.states()[4].checkpoint,
        director.states()[4].checkpoint,
        "episode 2's plan-installed checkpoint matches the lost state's exactly"
    );
    // THE no-double-apply proof: the recovered milestones are the
    // snapshot's plus exactly episode 2's post-snapshot crossings —
    // episode 1's already-materialized crossings are never applied
    // again.
    assert_eq!(
        recovered_spine.world().milestones(),
        spine.world().milestones(),
        "the milestones match the lost state's exactly — no re-application"
    );
    assert_eq!(
        recovered_spine.world().credits(),
        spine.world().credits(),
        "the credits are the snapshot's — episode 1's application is inside it"
    );
    assert_eq!(
        recovered_director.ledger().pending_total(),
        director.ledger().pending_total(),
        "only episode 2's captures are pending"
    );
    assert_eq!(
        recovered_director.ledger().applied_count(),
        director.ledger().applied_count(),
        "episode 1's applied identities stand from the snapshot"
    );
    let CommitOutcome::Confirmed(receipt) = recovered_crossing.commit_canonical().unwrap() else {
        panic!("the re-drive confirms the recovered captures");
    };
    assert!(receipt.keys.contains(&ConsequenceId::new(4, 2, 4)));
}

/// The multi-episode recovery UNDER THE C1 COVERING DISCIPLINE (the
/// recovery-coverage checkpoint's restoration): ONE snapshot before
/// BOTH episodes, but each consequential episode's crossing now
/// publishes its own covering snapshot of its exact preparation
/// state BEFORE journaling — so the recovery finds the newest
/// covering (episode 2's), carries episode 1's installed state
/// through it (the episode filter skips episode 1's pre-snapshot
/// plan), and replays episode 2's plan coherently (within-frontier,
/// the exact-state covered). THE PREVIOUSLY LOST COVERAGE RESTORED:
/// the two-episode campaign now recovers durably — and reopens
/// repeatedly — where the pre-C1 (D-guard-only) form correctly
/// refused it as the incoherent class.
#[test]
fn recovery_restores_both_episodes_under_the_covering_discipline() {
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("two-episodes");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing =
        SessionCrossing::open(&dir.store(), journal, "two-episodes-campaign").unwrap();
    // THE older snapshot: before both episodes (the fixture's own
    // publication — the crossings publish their coverings themselves).
    crossing.publish_snapshot(&spine, &director).unwrap();
    // Episode 1: accrual beyond the snapshot, the durable far
    // boundary (the C1 covering publishes automatically), and the
    // durable reactivation.
    for _ in 0..80 {
        director.apply(&mut spine, std::slice::from_ref(&near));
        spine.run_tick();
    }
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    // Episode 2: accrual, then the second durable far boundary —
    // the accrual long enough for the threshold crossings to recur.
    for _ in 0..160 {
        director.apply(&mut spine, std::slice::from_ref(&near));
        spine.run_tick();
    }
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    // THE CRASH + the recovery: the newest covering (episode 2's)
    // carries episode 1's installed state; episode 2's plan replays.
    drop(crossing);
    let (recovered_spine, recovered_director, mut recovered_crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "two-episodes-campaign")
            .expect("the covered two-episode campaign recovers");
    assert_eq!(recovered_director.levels()[4], LodLevel::Dormant);
    assert_eq!(
        recovered_director.states()[4].checkpoint,
        director.states()[4].checkpoint,
        "episode 2's checkpoint matches the lost state's exactly"
    );
    assert_eq!(
        recovered_spine.world().milestones(),
        spine.world().milestones(),
        "the milestones match the lost state's exactly across both episodes"
    );
    assert_eq!(
        recovered_director.ledger().pending_total(),
        director.ledger().pending_total(),
        "episode 2's captures are pending — matching the lost state's exactly"
    );
    assert_eq!(
        recovered_director.ledger().applied_count(),
        director.ledger().applied_count(),
        "episode 1's applied identities match the lost state's exactly"
    );
    assert_eq!(
        recovered_spine.world().credits(),
        spine.world().credits(),
        "the credits match the lost state's exactly — the derived reactivation applied them"
    );
    // The re-drive confirms episode 2's recovered captures (episode
    // 1's are applied-and-carried by the covering snapshot).
    let CommitOutcome::Confirmed(receipt) = recovered_crossing.commit_canonical().unwrap() else {
        panic!("the re-drive confirms the recovered captures");
    };
    assert!(receipt.keys.contains(&ConsequenceId::new(4, 2, 4)));
    // THE REPEATED RECOVERY: the recovered state reopens coherently
    // (the A/B/C/D discipline's library-level form).
    drop(recovered_crossing);
    drop(recovered_spine);
    drop(recovered_director);
    let second = SessionCrossing::recover(&dir.store(), &dir.journal(), "two-episodes-campaign");
    assert!(second.is_ok(), "the repeated recovery is coherent");
}

/// The cancellation integration (the review's second corrective): a
/// durably CANCELED deactivation transaction is never replayed — its
/// plan never installs, its captures never restore, its keys never
/// re-drive. The region stays exactly as the verified snapshot left
/// it.
#[test]
fn recovery_skips_canceled_deactivation_transactions() {
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let milestones_at_snapshot = spine.world().milestones().to_vec();
    let dir = TestDir::new("canceled");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "canceled-campaign").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    // The durable deactivation — then the durable CANCELLATION of its
    // transaction (the journal's own record; the crossing is dropped
    // first — the journal's sidecar lock is session-scoped).
    let now = spine.clock();
    let receipt = crossing
        .deactivate(&mut spine, &mut director, 4, now)
        .expect("the deactivation commits")
        .expect("the scenario captured consequences");
    drop(crossing);
    let mut journal = T1Journal::open(dir.journal()).unwrap();
    journal
        .append_cancellation(
            receipt.journal.id,
            CancellationReason::ApplicationPolicy,
            b"corrective-test-cancellation",
        )
        .expect("the cancellation is recorded");
    drop(journal);
    // THE recovery: the canceled plan must not install.
    let (recovered_spine, recovered_director, mut recovered_crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "canceled-campaign")
            .expect("the recovery succeeds over the canceled intent");
    assert_eq!(
        recovered_director.levels()[4],
        LodLevel::Full,
        "the canceled deactivation never installs — the snapshot's level stands"
    );
    assert_eq!(
        recovered_director.ledger().pending_count(),
        0,
        "the canceled captures never enter the ledger"
    );
    assert_eq!(
        recovered_spine.world().milestones(),
        milestones_at_snapshot,
        "the canceled crossings never restore — the snapshot's state stands"
    );
    assert!(matches!(
        recovered_crossing.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    ));
}

/// The canonical-confirmation integration (the review's second
/// corrective, the other half): the recovery feeds the RE-OPENED
/// pipeline's real chain-walk confirmation evidence into the B3
/// reconcile — a canonically confirmed capture recovers as confirmed
/// (the exact-provenance check passes: the journal envelope IS the
/// canonical evidence's transaction and coverage), the key phase
/// surfaces InConfirmedChain, and the re-drive has nothing pending
/// (chain-confirmed keys are never re-batched).
#[test]
fn recovery_composes_canonical_confirmation_evidence() {
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("confirmed");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "confirmed-campaign").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    let now = spine.clock();
    crossing
        .deactivate(&mut spine, &mut director, 4, now)
        .unwrap()
        .expect("the scenario captured consequences");
    let CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the captures confirm canonically before the crash");
    };
    drop(crossing);
    let (mut recovered_spine, mut recovered_director, mut recovered_crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "confirmed-campaign")
            .expect("the recovery composes the confirmation evidence");
    assert_eq!(recovered_director.levels()[4], LodLevel::Dormant);
    assert_eq!(
        recovered_spine.world().milestones(),
        spine.world().milestones(),
        "the crossings were in the snapshot — the milestones match exactly"
    );
    assert_eq!(
        recovered_crossing.key_phase(ConsequenceId::new(4, 1, 4)),
        Some(KeyPhase::InConfirmedChain),
        "the recovered capture's phase is canonically confirmed"
    );
    assert!(matches!(
        recovered_crossing.commit_canonical().unwrap(),
        CommitOutcome::NothingPending
    ));
    // THE CONTINUATION (the third review corrective's frontier
    // proof): the recovered session's next legitimate transitions
    // derive FRESH identities — the reactivation applies the
    // recovered captures to credits (the legitimate lifecycle), the
    // accrual crosses fresh milestones, and the next deactivation
    // captures under episode 2: no previously issued canonical
    // identity is reused (the frontier is monotonic), no pending
    // reward is stranded, the authoritative state remains coherent.
    durable_boundary(
        &mut recovered_crossing,
        &mut recovered_spine,
        &mut recovered_director,
        &near,
    );
    assert_eq!(
        recovered_director.levels()[4],
        LodLevel::Full,
        "the reactivation installs over the recovered checkpoint"
    );
    assert!(
        recovered_spine.world().credits().iter().sum::<u64>() > 0,
        "the recovered pending rewards applied to credits — nothing stranded"
    );
    for _ in 0..160 {
        recovered_director.apply(&mut recovered_spine, std::slice::from_ref(&near));
        recovered_spine.run_tick();
    }
    durable_boundary(
        &mut recovered_crossing,
        &mut recovered_spine,
        &mut recovered_director,
        &far,
    );
    assert_eq!(
        recovered_director.states()[4].episode,
        2,
        "the next deactivation derives episode 2 — the frontier advanced monotonically"
    );
    assert!(
        matches!(
            recovered_director
                .ledger()
                .status(ConsequenceId::new(4, 2, 4)),
            CaptureStatus::Pending(_)
        ),
        "the fresh capture sits under the fresh episode-2 identity"
    );
    assert_eq!(
        recovered_director
            .ledger()
            .status(ConsequenceId::new(4, 1, 4)),
        CaptureStatus::Applied,
        "the confirmed episode-1 identity stands applied — no collision, no reissue"
    );
}

/// The confirmation-evidence discriminator: a DIVERGENT journal claim
/// against a canonically confirmed identity — the same identity, a
/// different transaction with a different occurrence span — must
/// fail the recovery CLOSED through the confirmed-provenance gate
/// (the journal envelope does not carry the canonical evidence's
/// exact transaction and coverage), never a silent suppression by
/// session-side coverage.
#[test]
fn recovery_fails_closed_on_divergent_claims_against_confirmation() {
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("divergent");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "divergent-campaign").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    let now = spine.clock();
    crossing
        .deactivate(&mut spine, &mut director, 4, now)
        .unwrap()
        .expect("the scenario captured consequences");
    let CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the captures confirm canonically");
    };
    drop(crossing);
    // The divergent claim: the same identity under a DIFFERENT
    // transaction — a span one occurrence beyond the confirmed
    // coverage (a different effect commitment; the journal accepts a
    // distinct transaction identity — the exact scenario the
    // confirmed-provenance gate exists for).
    let original = T1Journal::scan_file(dir.journal())
        .unwrap()
        .committed
        .iter()
        .flat_map(|transaction| transaction.envelopes.iter())
        .find(|envelope| envelope.id == ConsequenceId::new(4, 1, 4))
        .expect("the original claim is journaled")
        .delta
        .milestones;
    let mut journal = T1Journal::open(dir.journal()).unwrap();
    let envelope = ConsequenceEnvelope::capture(
        ConsequenceId::new(4, 1, 4),
        vec![ClassifiedSpan {
            span: OccurrenceSpan::new(0, original + 1).unwrap(),
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
            tick: now.as_u64(),
        },
    )
    .unwrap();
    journal
        .append_transaction(&TransactionInput {
            envelopes: vec![envelope],
            position: SourcePosition {
                head: None,
                tick: now.as_u64(),
            },
            classification: DurabilityClass::DurableEligible,
            dependencies: Vec::new(),
            frontier: Vec::new(),
            plan: Vec::new(),
        })
        .expect("the divergent claim is journaled");
    drop(journal);
    // THE recovery: fail closed through the confirmed-provenance gate.
    let error = match SessionCrossing::recover(&dir.store(), &dir.journal(), "divergent-campaign") {
        Err(error) => error,
        Ok(_) => panic!("the divergent claim fails the recovery closed"),
    };
    assert!(matches!(
        error,
        TransitionError::Recovery(RecoveryError::ConfirmedProvenanceMismatch { .. })
    ));
}

/// The invalid-newest-snapshot fail-closed (the review's third
/// corrective): a recognizable-but-invalid session object on TOP of
/// a valid snapshot — the newest authoritative session state is
/// corrupt, and the recovery must fail CLOSED, never silently fall
/// back to the older snapshot (which would resurrect stale
/// authoritative state). The future-version case too: a
/// session-snapshot/2 object is recognizable (the domain family) and
/// equally fails closed.
#[test]
fn recovery_fails_closed_on_invalid_newest_snapshot() {
    let near = near_region_4();
    let (spine, director) = scenario_pair(150, &near);
    let dir = TestDir::new("corrupt-snapshot");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "corrupt-campaign").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    drop(crossing);
    // A recognizable-but-invalid session object appended on top: the
    // domain tag is genuine, the bodies are garbage.
    let append_corrupt = |domain: &str| {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let mut enc = Encoder::new();
        enc.write_str(domain);
        enc.write_bytes(b"not-a-spine-save");
        enc.write_bytes(b"not-a-director-save");
        let object = StoredObject::new(enc.into_bytes()).unwrap();
        let record = RevisionRecord::regular(head, epoch, 999)
            .with_idempotency_key(object.hash().as_bytes().to_vec())
            .with_content_refs(vec![object.hash()]);
        medium
            .append(&record, std::slice::from_ref(&object), Some(head))
            .unwrap();
    };
    append_corrupt("engine-slice/session-snapshot/1");
    let error = match SessionCrossing::recover(&dir.store(), &dir.journal(), "corrupt-campaign") {
        Err(error) => error,
        Ok(_) => panic!("the invalid newest snapshot fails the recovery closed"),
    };
    assert!(matches!(error, TransitionError::CorruptSnapshot(_)));
    // The future-version case (K2-C1 corrected): a session-snapshot/2
    // object can no longer ENTER through the generic append at all —
    // the snapshot-family domain gate refuses it typed BEFORE any
    // mutation, so the recovery never meets an unadmitted /2 object
    // (the fail-closed moved EARLIER; the head is unchanged and the
    // recovery still succeeds on the last valid /1 snapshot).
    let gate_refusal = {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let mut enc = Encoder::new();
        enc.write_str("engine-slice/session-snapshot/2");
        enc.write_bytes(b"not-a-spine-save");
        enc.write_bytes(b"not-a-director-save");
        let object = StoredObject::new(enc.into_bytes()).unwrap();
        let record = RevisionRecord::regular(head, epoch, 999)
            .with_idempotency_key(object.hash().as_bytes().to_vec())
            .with_content_refs(vec![object.hash()]);
        medium
            .append(&record, std::slice::from_ref(&object), Some(head))
            .expect_err("the generic append refuses the /2 object at the domain gate")
    };
    assert!(matches!(
        gate_refusal,
        engine_persist::medium::MediumError::CompositeDomainGate { .. }
    ));
    // The chain's newest is still the CORRUPT /1 object (the /2 was
    // refused at the gate), so the recovery fails closed exactly as
    // the first half asserted — the gate prevented the /2 from ever
    // entering, and the existing fail-closed behavior is unchanged.
    let error = match SessionCrossing::recover(&dir.store(), &dir.journal(), "corrupt-campaign") {
        Err(error) => error,
        Ok(_) => panic!("the corrupt newest snapshot still fails the recovery closed"),
    };
    assert!(matches!(error, TransitionError::CorruptSnapshot(_)));
}

// ----------------- the second review-corrective regressions

/// The accepted B3 coherence case at the application level (the
/// review's second corrective): a canonically confirmed consequence
/// ALREADY materialized in the verified snapshot (the pending
/// capture in its ledger) with the historical T1 journal GONE — the
/// recovery succeeds with no duplicate and no false failure: the
/// chain and the snapshot carry the truth, the journal is not
/// required.
#[test]
fn recovery_recognizes_confirmed_pending_without_journal() {
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let milestones_at_snapshot = spine.world().milestones().to_vec();
    let dir = TestDir::new("no-journal-pending");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "no-journal-campaign").unwrap();
    // The durable deactivation, canonically confirmed, THEN the
    // snapshot (the ledger holds the pending captures; the world
    // holds the deactivation's state).
    let now = spine.clock();
    crossing
        .deactivate(&mut spine, &mut director, 4, now)
        .unwrap()
        .expect("the scenario captured consequences");
    let CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the captures confirm canonically");
    };
    crossing.publish_snapshot(&spine, &director).unwrap();
    let pending_count = director.ledger().pending_count();
    assert!(
        pending_count > 0,
        "the snapshot's ledger holds the pending captures"
    );
    // THE historical T1 journal is GONE (the accepted case: confirmed
    // + materialized needs no journal).
    drop(crossing);
    std::fs::remove_file(dir.journal()).unwrap();
    // THE recovery: no duplicate, no false failure.
    let (recovered_spine, recovered_director, mut recovered_crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "no-journal-campaign")
            .expect("the confirmed + materialized state recovers without the journal");
    assert_eq!(
        recovered_director.levels()[4],
        LodLevel::Dormant,
        "the snapshot's deactivation state stands"
    );
    assert_eq!(
        recovered_director.ledger().pending_count(),
        pending_count,
        "the pending captures stand from the snapshot — no duplicate"
    );
    assert_eq!(
        recovered_spine.world().milestones(),
        milestones_at_snapshot,
        "the milestones stand from the snapshot — no duplicate award"
    );
    assert!(
        matches!(
            recovered_crossing.commit_canonical().unwrap(),
            CommitOutcome::NothingPending
        ),
        "nothing re-batches — the confirmation is already on the chain"
    );
}

/// The same accepted case on the APPLIED lane: confirmed + applied
/// (the credits) + the journal gone — the recovery succeeds with the
/// credits and the applied ledger intact (the per-identity applied
/// AMOUNTS — the persisted v3 evidence — are the materialization
/// proof; never inferred from aggregate counters).
#[test]
fn recovery_recognizes_confirmed_applied_without_journal() {
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("no-journal-applied");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "no-journal-campaign").unwrap();
    // Episode 1: the durable deactivation, the durable reactivation
    // (the captures applied to credits), the canonical confirmation,
    // then the snapshot.
    let now = spine.clock();
    crossing
        .deactivate(&mut spine, &mut director, 4, now)
        .unwrap()
        .expect("the scenario captured consequences");
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    let CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the captures confirm canonically");
    };
    crossing.publish_snapshot(&spine, &director).unwrap();
    let applied_count = director.ledger().applied_count();
    let credits_at_snapshot = spine.world().credits().to_vec();
    assert!(
        applied_count > 0,
        "the snapshot's ledger holds the applied captures"
    );
    assert!(
        credits_at_snapshot.iter().sum::<u64>() > 0,
        "the snapshot's world holds the applied credits"
    );
    drop(crossing);
    std::fs::remove_file(dir.journal()).unwrap();
    let (recovered_spine, recovered_director, _recovered_crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "no-journal-campaign")
            .expect("the confirmed + applied state recovers without the journal");
    assert_eq!(
        recovered_director.ledger().applied_count(),
        applied_count,
        "the applied identities stand from the snapshot — no duplicate"
    );
    assert_eq!(
        recovered_spine.world().credits(),
        credits_at_snapshot,
        "the credits stand from the snapshot — no duplicate application"
    );
}

/// The mirror case: confirmed + NOT materialized in the snapshot +
/// no journal (the recovery evidence insufficient) — the fail-closed
/// halt (the B3 ConfirmedUnmaterialized), never a silent discard and
/// never a fabricated state.
#[test]
fn recovery_fails_closed_on_confirmed_unmaterialized_without_evidence() {
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("unmaterialized");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing =
        SessionCrossing::open(&dir.store(), journal, "unmaterialized-campaign").unwrap();
    // The snapshot BEFORE the deactivation (the ledger empty — the
    // confirmed captures are NOT materialized in it).
    crossing.publish_snapshot(&spine, &director).unwrap();
    let now = spine.clock();
    crossing
        .deactivate(&mut spine, &mut director, 4, now)
        .unwrap()
        .expect("the scenario captured consequences");
    let CommitOutcome::Confirmed(_) = crossing.commit_canonical().unwrap() else {
        panic!("the captures confirm canonically");
    };
    drop(crossing);
    std::fs::remove_file(dir.journal()).unwrap();
    // THE recovery: the confirmed captures are unmaterialized in the
    // snapshot and the journal (their only recovery evidence) is
    // gone — fail closed.
    let error =
        match SessionCrossing::recover(&dir.store(), &dir.journal(), "unmaterialized-campaign") {
            Err(error) => error,
            Ok(_) => panic!("the unmaterialized confirmed captures fail the recovery closed"),
        };
    assert!(matches!(
        error,
        TransitionError::Recovery(RecoveryError::ConfirmedUnmaterialized { .. })
    ));
}

/// The confirmed effect OUTSIDE the supported application plan path
/// (the third review corrective): a canonically confirmed, actionable
/// PLAN-LESS consequence has NO legitimate application lifecycle — no
/// deactivation plan, no checkpoint, no activation path, and its
/// foreign episode choice sits outside the director's frontier
/// arithmetic (the next ordinary deactivation would derive the same
/// episode and collide with the already-confirmed identity — the
/// pipeline's ConflictingReuse — stranding the session with a pending
/// reward no activation can reach). The application REJECTS the
/// unsupported plan-less form explicitly: the recovery fails CLOSED
/// — never a successful materialization whose continuation is
/// incoherent, and never a fabricated checkpoint or activation.
#[test]
fn recovery_fails_closed_on_planless_confirmed_effects() {
    let near = near_region_4();
    let (spine, director) = scenario_pair(150, &near);
    let dir = TestDir::new("plan-less");
    // The plan-less claim, staged and confirmed through the raw
    // pipeline. The campaign is FOUNDED through a crossing first (the
    // raw pipeline's canonical commit needs the founded chain; the
    // journal's sidecar lock is session-scoped — each holder is
    // dropped before the next opens).
    let journal = T1Journal::open(dir.journal()).unwrap();
    let founding_crossing =
        SessionCrossing::open(&dir.store(), journal, "plan-less-campaign").unwrap();
    drop(founding_crossing);
    let journal = T1Journal::open(dir.journal()).unwrap();
    let medium = LocalFileMedium::open(dir.store()).unwrap();
    let mut pipeline = CommitPipeline::open(medium, journal).unwrap();
    let envelope = ConsequenceEnvelope::capture(
        ConsequenceId::new(4, 1, 4),
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
            tick: 150,
        },
    )
    .unwrap();
    pipeline.stage(envelope).unwrap();
    pipeline
        .capture(
            SourcePosition {
                head: None,
                tick: 150,
            },
            DurabilityClass::DurableEligible,
            Vec::new(),
            vec![FrontierEntry {
                region: 4,
                episode: 1,
            }],
            Vec::new(),
        )
        .unwrap();
    let CommitOutcome::Confirmed(_) = pipeline.commit_canonical().unwrap() else {
        panic!("the plan-less claim confirms canonically");
    };
    drop(pipeline);
    // The snapshot AFTER the confirmation, from a world that never
    // materialized the claim (the crossing re-opens over the same
    // campaign — the journal's lock was released by the drop).
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "plan-less-campaign").unwrap();
    crossing.publish_snapshot(&spine, &director).unwrap();
    drop(crossing);
    // THE recovery: the plan-less claim is confirmed + unmaterialized
    // — the B3 reports it confirmed_recovered, and the application
    // REJECTS the unsupported form explicitly (fail closed — the
    // session is never stranded with a pending reward no activation
    // can reach and an identity the next deactivation would collide
    // with).
    let error = match SessionCrossing::recover(&dir.store(), &dir.journal(), "plan-less-campaign") {
        Err(error) => error,
        Ok(_) => panic!("the unsupported plan-less confirmed effect fails the recovery closed"),
    };
    assert!(matches!(
        error,
        TransitionError::InstallIncompatible {
            detail: "confirmed-recovered-planless-unsupported",
            ..
        }
    ));
}

// ---------------------------------------- the C1 §6 crash-window matrix

/// The §6 injection seam: a medium source whose appends can be
/// scripted at the COVERING boundary — the stale-parent race, the
/// worker-death unknown (installed and not), and the healthy
/// delegation. The crossing is generic over `MediumSource` (PP-B4's
/// ownership seam), so the matrix drives the REAL crossing over the
/// REAL file medium with only the one append's outcome scripted;
/// every other operation delegates untouched. The script is one-shot
/// — the next append (the retry, the confirmation, a later
/// publication) observes the healthy medium again.
#[derive(Clone)]
struct CoveringRaceSource {
    root: PathBuf,
    script: Arc<Mutex<RaceScript>>,
    /// The reconcile_unknown injection (§5: the unresolved
    /// reconciliation errors) — an independent one-shot from the
    /// append script, so a covering append that reports Unknown can
    /// exercise the reconciliation's OWN failure modes.
    reconcile: Arc<Mutex<Option<ReconcileScript>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RaceScript {
    /// The plain delegation (the healthy medium).
    Healthy,
    /// The worker died BEFORE the append ran: nothing installed, the
    /// reply lost — the uncertain class; the reconcile must find
    /// NotInstalled and the crossing must refuse with the candidate
    /// preserved.
    UnknownNotInstalled,
    /// The worker died AFTER the append ran: the record IS installed,
    /// the reply lost — the reconcile must find Installed and the
    /// covering track must follow the installed identity.
    UnknownInstalled,
    /// A concurrent publication wins the CAS between the covering's
    /// head read and its append: the stale parent observed once, the
    /// single retry lands above the winner.
    StaleOnce,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconcileScript {
    /// The reconciliation returns the unverifiable disposition — the
    /// uncertainty retained, never converted into a confirmation.
    Unverifiable,
    /// The reconciliation itself fails (the transport, the lock) —
    /// never a raw escape, never a success: the typed refusal with
    /// the attempted candidate preserved.
    TransportFailure,
}

impl MediumSource for CoveringRaceSource {
    type Medium = CoveringRaceMedium;

    fn open_medium(&self) -> Result<CoveringRaceMedium, MediumError> {
        Ok(CoveringRaceMedium {
            inner: LocalFileMedium::open(self.root.clone())?,
            script: self.script.clone(),
            reconcile: self.reconcile.clone(),
        })
    }
}

struct CoveringRaceMedium {
    inner: LocalFileMedium,
    script: Arc<Mutex<RaceScript>>,
    reconcile: Arc<Mutex<Option<ReconcileScript>>>,
}

impl CanonicalMedium for CoveringRaceMedium {
    fn head(&mut self) -> Result<Option<Hash>, MediumError> {
        self.inner.head()
    }

    fn append_admitted(
        &mut self,
        admitted: engine_persist::medium::AdmittedAppend,
        expected_parent: Option<Hash>,
    ) -> Result<AppendOutcome, MediumError> {
        let (record, objects, _composite) = admitted.parts_for_tests();
        let record = record.clone();
        let objects = objects.to_vec();
        let scripted = {
            let mut guard = self.script.lock().unwrap();
            let scripted = *guard;
            *guard = RaceScript::Healthy;
            scripted
        };
        match scripted {
            RaceScript::Healthy => self.inner.append(&record, &objects, expected_parent),
            RaceScript::UnknownNotInstalled => Ok(AppendOutcome::Unknown {
                reason: UnknownReason::WorkerReplyLost,
            }),
            RaceScript::UnknownInstalled => {
                let _ = self.inner.append(&record, &objects, expected_parent)?;
                Ok(AppendOutcome::Unknown {
                    reason: UnknownReason::WorkerReplyLost,
                })
            }
            RaceScript::StaleOnce => {
                // The concurrent winner: a foreign regular record at
                // the CURRENT head, moving it before the covering's
                // own append runs against its now-stale expected
                // parent.
                let head = self.inner.head()?.expect("the store is founded");
                let head_epoch = self.inner.read_record(&head)?.epoch;
                let object = StoredObject::new({
                    let mut enc = Encoder::new();
                    enc.write_str("test/publication");
                    enc.write_bytes(b"covering-race-foreign");
                    enc.into_bytes()
                })
                .expect("the foreign object encodes");
                let foreign = RevisionRecord::regular(head, head_epoch, record.tick)
                    .with_idempotency_key(b"covering/race-foreign".to_vec())
                    .with_content_refs(vec![object.hash()]);
                let AppendOutcome::Accepted { id: foreign_id, .. } =
                    self.inner
                        .append(&foreign, std::slice::from_ref(&object), Some(head))?
                else {
                    panic!("the foreign winner lands at the current head");
                };
                let observed = self.inner.append(&record, &objects, expected_parent);
                match observed {
                    Ok(AppendOutcome::RejectedStaleParent {
                        observed: Some(observed),
                    }) if observed == foreign_id => Ok(AppendOutcome::RejectedStaleParent {
                        observed: Some(observed),
                    }),
                    other => panic!("the stale observation names the foreign winner: {other:?}"),
                }
            }
        }
    }

    fn read_record(&mut self, id: &Hash) -> Result<RevisionRecord, MediumError> {
        self.inner.read_record(id)
    }

    fn read_object(&mut self, hash: &Hash) -> Result<Vec<u8>, MediumError> {
        self.inner.read_object(hash)
    }

    fn validate(&mut self) -> Result<engine_persist::ChainSummary, MediumError> {
        self.inner.validate()
    }

    fn reconcile_unknown(&mut self, candidate: &Hash) -> Result<Reconciliation, MediumError> {
        let scripted = self.reconcile.lock().unwrap().take();
        match scripted {
            Some(ReconcileScript::Unverifiable) => Ok(Reconciliation::Unverifiable {
                reason: "scripted-unverifiable".into(),
            }),
            Some(ReconcileScript::TransportFailure) => Err(MediumError::Io {
                op: "fault-injected",
                path: "reconcile".to_owned(),
                source: std::io::Error::other("fault-injected"),
            }),
            None => self.inner.reconcile_unknown(candidate),
        }
    }

    fn durability_tier(&self) -> SyncTier {
        self.inner.durability_tier()
    }

    fn reclaim(&mut self, units: &[engine_persist::ReclaimUnit]) -> Result<u64, MediumError> {
        self.inner.reclaim(units)
    }

    fn reclaim_pending(
        &mut self,
        units: &[engine_persist::ReclaimUnit],
    ) -> Result<bool, MediumError> {
        self.inner.reclaim_pending(units)
    }

    fn checkpoint(&mut self) -> Result<(), MediumError> {
        self.inner.checkpoint()
    }
}

/// §6 case 1 + the uncertain refusal: the covering's append lost its
/// worker BEFORE anything installed — the reconcile finds
/// NotInstalled and the crossing REFUSES with the uncertain
/// candidate's identity preserved, BEFORE the T1 (nothing journaled,
/// nothing suspended, the region and the chain untouched) — and the
/// same deactivate through the healthy medium succeeds (the refusal
/// preserves exact retryability).
#[test]
fn covering_unknown_refusal_preserves_the_pre_t1_state_and_retries() {
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("covering-unknown-refused");
    let script = Arc::new(Mutex::new(RaceScript::Healthy));
    let source = CoveringRaceSource {
        root: dir.store(),
        script: script.clone(),
        reconcile: Arc::new(Mutex::new(None)),
    };
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing =
        SessionCrossing::open_at(&source, journal, "covering-unknown-refused").unwrap();
    // The scenario's own baseline holds (five far regions dormant
    // since tick 0) — the refusal must leave exactly these, no more.
    let held_before = spine.held_count();
    // The arm: the NEXT append (the covering's — the deactivate's
    // first medium operation) reports the lost worker with nothing
    // installed.
    *script.lock().unwrap() = RaceScript::UnknownNotInstalled;
    let now = spine.clock();
    let refusal = crossing.deactivate(&mut spine, &mut director, 4, now);
    let (detail, candidate) = match refusal {
        Err(TransitionError::CoveringRefused { detail, candidate }) => (detail, candidate),
        Err(other) => panic!("the uncertain covering refuses the crossing, got {other:?}"),
        Ok(_) => panic!("an unresolved covering uncertainty never proceeds to the T1"),
    };
    assert_eq!(detail, "covering/unknown-not-installed");
    let candidate = candidate.expect("the uncertain candidate's identity is preserved");
    // NOTHING journaled, nothing suspended, the region and the chain
    // exactly as they stood.
    {
        let scan = T1Journal::scan_file(dir.journal()).unwrap();
        assert!(scan.committed.is_empty(), "no T1 precedes the covering");
        assert!(scan.cancellations.is_empty());
    }
    assert_eq!(
        spine.held_count(),
        held_before,
        "nothing was suspended by the refused attempt"
    );
    assert_eq!(
        director.levels()[4],
        LodLevel::Full,
        "the region stands exactly at its pre-transition level"
    );
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let summary = medium.validate().unwrap();
        assert_eq!(summary.records, 1, "only the founding stands");
        assert!(
            medium.read_record(&candidate).is_err(),
            "the uncertain candidate was never installed by the refused attempt"
        );
    }
    // THE RETRY: the same deactivate through the healthy medium —
    // the refusal never moved the state, so the identical plan
    // prepares again and succeeds.
    assert_eq!(*script.lock().unwrap(), RaceScript::Healthy);
    let receipt = crossing
        .deactivate(&mut spine, &mut director, 4, now)
        .expect("the retried deactivation commits")
        .expect("the scenario captured consequences");
    assert_eq!(director.levels()[4], LodLevel::Dormant);
    {
        let scan = T1Journal::scan_file(dir.journal()).unwrap();
        assert_eq!(scan.committed.len(), 1, "the retried boundary journaled");
        assert_eq!(scan.committed[0].id, receipt.journal.id);
    }
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let summary = medium.validate().unwrap();
        assert_eq!(summary.records, 2, "the founding plus the covering");
    }
    let _ = receipt;
}

/// §6 case 6 (the worker death with the record installed): the
/// covering's append DID install before the worker died — the
/// reconcile finds Installed, the covering track follows the
/// installed identity, and the deactivation proceeds through its T1
/// and install exactly as the healthy path does.
#[test]
fn covering_unknown_reconciles_installed_and_carries_the_track() {
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("covering-unknown-installed");
    let script = Arc::new(Mutex::new(RaceScript::Healthy));
    let source = CoveringRaceSource {
        root: dir.store(),
        script: script.clone(),
        reconcile: Arc::new(Mutex::new(None)),
    };
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing =
        SessionCrossing::open_at(&source, journal, "covering-unknown-installed").unwrap();
    *script.lock().unwrap() = RaceScript::UnknownInstalled;
    let now = spine.clock();
    let pre_deactivation_fingerprint = spine.engine_fingerprint();
    crossing
        .deactivate(&mut spine, &mut director, 4, now)
        .expect("the reconciled covering proceeds to the deactivation")
        .expect("the scenario captured consequences");
    assert_eq!(director.levels()[4], LodLevel::Dormant);
    let covering = crossing
        .covering_revision()
        .expect("the track follows the installed covering");
    {
        let scan = T1Journal::scan_file(dir.journal()).unwrap();
        assert_eq!(scan.committed.len(), 1, "the T1 followed the covering");
    }
    {
        // The installed covering IS on the chain — the identity the
        // reconcile returned, reachable from the head.
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let summary = medium.validate().unwrap();
        assert_eq!(summary.records, 2, "the founding plus the covering");
        let record = medium.read_record(&covering).unwrap();
        let content = record.content_refs[0];
        let bytes = medium.read_object(&content).unwrap();
        let (restored_spine, restored_director) = restore_session(&bytes).unwrap();
        assert_eq!(
            restored_director.levels()[4],
            LodLevel::Full,
            "the covering captured the PRE-deactivation state"
        );
        assert_eq!(
            restored_spine.engine_fingerprint(),
            pre_deactivation_fingerprint,
            "the covering's spine is the exact pre-plan state"
        );
    }
}

/// §6 case 7 (the stale parent and the single retry): a concurrent
/// publication wins the CAS between the covering's head read and its
/// append — the crossing observes the winner, retries ONCE against
/// it, and the covering lands above the winner; the chain carries
/// both and validates.
#[test]
fn covering_stale_parent_retries_once_against_the_observed_head() {
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("covering-stale-retry");
    let script = Arc::new(Mutex::new(RaceScript::Healthy));
    let source = CoveringRaceSource {
        root: dir.store(),
        script: script.clone(),
        reconcile: Arc::new(Mutex::new(None)),
    };
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open_at(&source, journal, "covering-stale-retry").unwrap();
    *script.lock().unwrap() = RaceScript::StaleOnce;
    let now = spine.clock();
    crossing
        .deactivate(&mut spine, &mut director, 4, now)
        .expect("the stale-retried covering proceeds to the deactivation")
        .expect("the scenario captured consequences");
    assert_eq!(director.levels()[4], LodLevel::Dormant);
    let covering = crossing
        .covering_revision()
        .expect("the retry established the covering");
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let summary = medium.validate().unwrap();
        assert_eq!(
            summary.records, 3,
            "the founding, the concurrent winner, and the covering above it"
        );
        // The retry landed AGAINST THE OBSERVED WINNER: the covering's
        // parent is the foreign record, never the stale head it first
        // read.
        let record = medium.read_record(&covering).unwrap();
        let parent = record.parent.expect("the covering has a parent");
        let parent_record = medium.read_record(&parent).unwrap();
        assert_eq!(
            parent_record.idempotency_key.as_deref(),
            Some(b"covering/race-foreign".as_slice()),
            "the covering's parent is the concurrent winner"
        );
    }
    {
        let scan = T1Journal::scan_file(dir.journal()).unwrap();
        assert_eq!(scan.committed.len(), 1, "the T1 followed the covering");
    }
}

/// §6 case 8 (multiple deactivations at one campaign tick): the
/// boundary deactivates every live region at the SAME tick — each
/// capture-bearing plan binds its OWN exact state (a later covering
/// captures the earlier deactivation's install; an empty-scratch
/// plan publishes nothing and journals nothing), and the chain plus
/// the recovery stay coherent over the whole set.
#[test]
fn one_tick_deactivations_each_bind_their_own_exact_state() {
    // A presence inside region 4's full band (22.4 units of its
    // center) while inside region 3's simplified band (57 units —
    // region 4 accrues its standard full-rate capture scratch, region
    // 3 whatever the reduced rate accrues, plus the event-driven ring
    // around them); the far presence deactivates them all at one
    // campaign tick.
    let between = PresenceSample {
        position: (-90.0, 20.0),
        velocity: (0.0, 0.0),
    };
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &between);
    let dir = TestDir::new("one-tick-coverings");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "one-tick-coverings").unwrap();
    let boundary_tick = spine.clock();
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    // The boundary completed: every region the far presence deactivates
    // is dormant.
    for region in 0..REGION_COUNT {
        let target = director.target_level(region, std::slice::from_ref(&far));
        assert_eq!(
            director.levels()[region],
            target,
            "region {region} reached its far-presence target"
        );
    }
    // The coverings on the chain: every session-snapshot record at the
    // boundary tick (this campaign publishes nothing else — no app
    // snapshot, no checkpoint), collected newest-first.
    let coverings = {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let mut collected = Vec::new();
        let mut cursor = medium.head().unwrap();
        while let Some(hash) = cursor {
            let record = medium.read_record(&hash).unwrap();
            if record.tick == boundary_tick.as_u64() && !record.content_refs.is_empty() {
                let bytes = medium.read_object(&record.content_refs[0]).unwrap();
                if restore_session(&bytes).is_ok() {
                    collected.push(hash);
                }
            }
            cursor = record.parent;
        }
        collected
    };
    assert!(
        !coverings.is_empty(),
        "at least one capture-bearing deactivation published its covering"
    );
    // THE EXACT-STATE BINDING: consecutive coverings (newest first)
    // each captured the state AFTER the previous deactivation's
    // install — a later covering's restored director shows the
    // earlier deactivated region already dormant while its own
    // region still stands live.
    for pair in coverings.windows(2) {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let newer_content = medium.read_record(&pair[0]).unwrap().content_refs[0];
        let newer_bytes = medium.read_object(&newer_content).unwrap();
        let older_content = medium.read_record(&pair[1]).unwrap().content_refs[0];
        let older_bytes = medium.read_object(&older_content).unwrap();
        let (_newer_spine, newer_director) = restore_session(&newer_bytes).unwrap();
        let (_older_spine, older_director) = restore_session(&older_bytes).unwrap();
        let transitioned: Vec<usize> = (0..REGION_COUNT)
            .filter(|&region| {
                older_director.levels()[region] != LodLevel::Dormant
                    && newer_director.levels()[region] == LodLevel::Dormant
            })
            .collect();
        assert!(
            !transitioned.is_empty(),
            "the later covering captured the earlier deactivation's install"
        );
    }
    // Every covering sits at the SAME campaign tick (one boundary, one
    // tick — the records' informational tick).
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        for covering in &coverings {
            let record = medium.read_record(covering).unwrap();
            assert_eq!(record.tick, boundary_tick.as_u64());
        }
        let summary = medium.validate().unwrap();
        assert_eq!(
            summary.records as usize,
            coverings.len() + 1,
            "the founding plus exactly the coverings"
        );
    }
    // The journal: one committed transaction per capture-bearing
    // deactivation — never one for an empty-scratch plan.
    {
        let scan = T1Journal::scan_file(dir.journal()).unwrap();
        assert_eq!(
            scan.committed.len(),
            coverings.len(),
            "one T1 per covering — the empty-scratch plans journaled nothing"
        );
    }
    // The ordinary post-boundary publication (the fixture discipline):
    // the app's snapshot carries the boundary's FINAL state — the
    // trailing activations (after the last covering) and every
    // install — making the whole one-tick set durable for the
    // recovery.
    crossing.publish_snapshot(&spine, &director).unwrap();
    // THE RECOVERY: the whole one-tick set reconstructs coherently.
    drop(crossing);
    let (recovered_spine, recovered_director, _crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "one-tick-coverings")
            .expect("the one-tick boundary set recovers");
    assert_eq!(
        stage4_state_fingerprint(&recovered_spine, &recovered_director),
        stage4_state_fingerprint(&spine, &director),
        "the recovery reconstructs the boundary's final state exactly"
    );
}

/// §6 case 11 (the compacted history): the covering's prerequisite
/// and its roundtrip validation hold over the EXPLICIT compacted
/// session form (the spine-history/2 cut) exactly as over the
/// un-compacted form — the deactivation publishes its covering over
/// the compacted state, and the recovery restores through it.
#[test]
fn the_covering_binds_the_compacted_history_form() {
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("covering-compacted");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "covering-compacted").unwrap();
    // The explicit compaction (PP-B2): the lane summarization plus the
    // spine-history cut, BEFORE the boundary.
    crossing
        .compact_session(&mut spine, &mut director, 20)
        .expect("the compaction applies");
    let now = spine.clock();
    let pre_deactivation = (
        spine.clock(),
        spine.stats(),
        spine.held_count(),
        spine.held_fingerprint(),
        spine.world().milestones().to_vec(),
        director.levels(),
    );
    crossing
        .deactivate(&mut spine, &mut director, 4, now)
        .expect("the compacted-state covering publishes")
        .expect("the scenario captured consequences");
    assert_eq!(director.levels()[4], LodLevel::Dormant);
    let covering = crossing.covering_revision().expect("the covering stands");
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let record = medium.read_record(&covering).unwrap();
        let bytes = medium.read_object(&record.content_refs[0]).unwrap();
        let (restored_spine, restored_director) =
            restore_session(&bytes).expect("the compacted covering round-trips");
        assert_eq!(
            restored_director.levels()[4],
            LodLevel::Full,
            "the covering captured the PRE-deactivation compacted state"
        );
        // The compact-safe comparison domain (the compaction suite's
        // discipline — the full-history fingerprints are unavailable
        // by contract on a compacted spine).
        assert_eq!(restored_spine.clock(), pre_deactivation.0);
        assert_eq!(restored_spine.stats(), pre_deactivation.1);
        assert_eq!(restored_spine.held_count(), pre_deactivation.2);
        assert_eq!(restored_spine.held_fingerprint(), pre_deactivation.3);
        assert_eq!(restored_spine.world().milestones(), pre_deactivation.4);
        assert_eq!(restored_director.levels(), pre_deactivation.5);
    }
    // THE RECOVERY through the compacted covering — the same
    // compact-safe comparison domain against the post-boundary live
    // state.
    drop(crossing);
    let (recovered_spine, recovered_director, _crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "covering-compacted")
            .expect("the compacted-history recovery works");
    assert_eq!(recovered_spine.clock(), spine.clock());
    assert_eq!(recovered_spine.stats(), spine.stats());
    assert_eq!(recovered_spine.held_count(), spine.held_count());
    assert_eq!(recovered_spine.held_fingerprint(), spine.held_fingerprint());
    assert_eq!(
        recovered_spine.world().milestones(),
        spine.world().milestones()
    );
    assert_eq!(recovered_director.levels(), director.levels());
    assert_eq!(recovered_director.levels()[4], LodLevel::Dormant);
}

/// §4 of the C1 final corrective (the validation provenance): a
/// chain record keyed by the exact content hash — the shape the
/// duplicate resolution consults — never qualifies as a covering
/// through identity alone. Two crafted forms: a key-matched record
/// referencing a FOREIGN object (the single-content-reference
/// check), and a key-matched record whose referenced object was
/// replaced BY HAND with non-session bytes after acceptance (the
/// CAS content verification and the strict session roundtrip).
/// Each refuses the crossing BEFORE the T1 with the attempted
/// candidate preserved — never a success, never the raw duplicate
/// rejection as acceptance.
#[test]
fn a_key_matched_record_never_qualifies_without_verified_content() {
    let near = near_region_4();
    // FORM 1 — the foreign content reference.
    {
        let (mut spine, mut director) = scenario_pair(150, &near);
        let dir = TestDir::new("covering-foreign-ref");
        let journal = T1Journal::open(dir.journal()).unwrap();
        let mut crossing =
            SessionCrossing::open(&dir.store(), journal, "covering-foreign-ref").unwrap();
        let bytes = save_session(&spine, &director).unwrap();
        let content_hash = StoredObject::new(bytes.clone()).unwrap().hash();
        // The crafted record: keyed by the content hash, referencing
        // a foreign object — a legitimate append the chain accepts.
        let foreign = StoredObject::new({
            let mut enc = Encoder::new();
            enc.write_str("test/publication");
            enc.write_bytes(b"covering-foreign-ref-payload");
            enc.into_bytes()
        })
        .unwrap();
        {
            let mut medium = LocalFileMedium::open(dir.store()).unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = RevisionRecord::regular(head, epoch, spine.clock().as_u64())
                .with_idempotency_key(content_hash.as_bytes().to_vec())
                .with_content_refs(vec![foreign.hash()]);
            match medium
                .append(&record, std::slice::from_ref(&foreign), Some(head))
                .unwrap()
            {
                AppendOutcome::Accepted { .. } => {}
                other => panic!("the crafted key-matched record lands: {other:?}"),
            }
        }
        // THE CROSSING: the covering for the same exact state hits the
        // duplicate key — the resolution must REFUSE (the record's
        // reference is foreign), before the T1.
        let now = spine.clock();
        let refusal = crossing.deactivate(&mut spine, &mut director, 4, now);
        let detail = match refusal {
            Err(TransitionError::CoveringRefused { detail, candidate }) => {
                assert!(candidate.is_some(), "the attempted candidate preserved");
                detail
            }
            Err(other) => panic!("the foreign-referenced key match refuses, got {other:?}"),
            Ok(_) => panic!("an unverified key match never covers the prerequisite"),
        };
        assert_eq!(detail, "covering/duplicate-unresolved");
        assert_eq!(director.levels()[4], LodLevel::Full, "the region unchanged");
        let scan = T1Journal::scan_file(dir.journal()).unwrap();
        assert!(scan.committed.is_empty(), "no T1 precedes the covering");
    }
    // FORM 2 — the accepted snapshot's object replaced by hand with
    // non-session bytes.
    {
        let (mut spine, mut director) = scenario_pair(150, &near);
        let dir = TestDir::new("covering-corrupted-bytes");
        let journal = T1Journal::open(dir.journal()).unwrap();
        let mut crossing =
            SessionCrossing::open(&dir.store(), journal, "covering-corrupted-bytes").unwrap();
        let bytes = save_session(&spine, &director).unwrap();
        let object = StoredObject::new(bytes).unwrap();
        let content_hash = object.hash();
        // The crafted record: keyed by the content hash, referencing
        // the REAL session object — a legitimate accepted publication
        // of the exact state.
        {
            let mut medium = LocalFileMedium::open(dir.store()).unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = RevisionRecord::regular(head, epoch, spine.clock().as_u64())
                .with_idempotency_key(content_hash.as_bytes().to_vec())
                .with_content_refs(vec![content_hash]);
            match medium
                .append(&record, std::slice::from_ref(&object), Some(head))
                .unwrap()
            {
                AppendOutcome::Accepted { .. } => {}
                other => panic!("the crafted publication lands: {other:?}"),
            }
        }
        // The hand corruption: the referenced object's bytes replaced
        // with foreign content (the previously accepted snapshot is
        // no longer what its identity claims).
        std::fs::write(
            dir.store().join("objects").join(content_hash.hex()),
            b"covering-corrupted-bytes-payload",
        )
        .unwrap();
        // THE CROSSING: the corruption is caught BEFORE any covering
        // logic — the medium's own open-time chain walk verifies
        // every referenced object's CAS identity (the previously
        // accepted snapshot is no longer what its identity claims),
        // and the crossing refuses with the typed corruption class —
        // never a success over unverified identity, never the T1.
        let now = spine.clock();
        let refusal = crossing.deactivate(&mut spine, &mut director, 4, now);
        match refusal {
            Err(TransitionError::JournalFailed(PipelineError::Medium(
                MediumError::ReferencedObjectCorrupt { hash },
            ))) => {
                assert_eq!(
                    hash, content_hash,
                    "the corrupted snapshot's identity named exactly"
                );
            }
            Err(other) => panic!("the corrupted-bytes key match refuses, got {other:?}"),
            Ok(_) => panic!("a corrupted snapshot never covers the prerequisite"),
        }
        assert_eq!(director.levels()[4], LodLevel::Full, "the region unchanged");
        let scan = T1Journal::scan_file(dir.journal()).unwrap();
        assert!(scan.committed.is_empty(), "no T1 precedes the covering");
    }
}

/// Corrective A of the C1 closeout (the exact covering-record shape):
/// the required covering publication is a canonical
/// SINGLE-snapshot-object record. A legitimate, chain-accepted record
/// whose idempotency key and FIRST content reference match the
/// expected snapshot but whose references carry a SECOND valid object
/// is a different record shape and never qualifies as covering
/// evidence — the crossing refuses before the T1 with the attempted
/// candidate preserved and the region unchanged.
#[test]
fn an_extra_reference_key_match_never_qualifies_as_the_covering() {
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("covering-extra-ref");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing = SessionCrossing::open(&dir.store(), journal, "covering-extra-ref").unwrap();
    let bytes = save_session(&spine, &director).unwrap();
    let object = StoredObject::new(bytes).unwrap();
    let content_hash = object.hash();
    // The second, genuinely valid object — the crafted record carries
    // BOTH: the snapshot's own content plus an extra reference, a
    // legitimate multi-object publication the chain accepts.
    let extra = StoredObject::new({
        let mut enc = Encoder::new();
        enc.write_str("test/publication");
        enc.write_bytes(b"covering-extra-ref-second-object");
        enc.into_bytes()
    })
    .unwrap();
    {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let record = RevisionRecord::regular(head, epoch, spine.clock().as_u64())
            .with_idempotency_key(content_hash.as_bytes().to_vec())
            .with_content_refs(vec![content_hash, extra.hash()]);
        match medium
            .append(&record, &[object.clone(), extra], Some(head))
            .unwrap()
        {
            AppendOutcome::Accepted { .. } => {}
            other => panic!("the crafted multi-reference record lands: {other:?}"),
        }
    }
    // THE CROSSING: the covering for the same exact state hits the
    // duplicate key — the resolution must REFUSE: the key-matched
    // record's shape is not the single-object covering publication,
    // however valid both its objects are.
    let now = spine.clock();
    let refusal = crossing.deactivate(&mut spine, &mut director, 4, now);
    match refusal {
        Err(TransitionError::CoveringRefused { detail, candidate }) => {
            assert_eq!(detail, "covering/duplicate-unresolved");
            assert!(candidate.is_some(), "the attempted candidate preserved");
        }
        Err(other) => panic!("the extra-reference key match refuses, got {other:?}"),
        Ok(_) => panic!("a multi-reference record never covers the prerequisite"),
    }
    assert_eq!(director.levels()[4], LodLevel::Full, "the region unchanged");
    assert_eq!(spine.held_count(), 625, "nothing suspended");
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    assert!(scan.committed.is_empty(), "no T1 precedes the covering");
}

/// §5 of the C1 final corrective (the unresolved reconciliation
/// errors) — the unverifiable disposition: the uncertain covering
/// append's reconciliation returns Unverifiable (the uncertainty
/// retained, never converted into a confirmation): the crossing
/// refuses BEFORE the T1 with the attempted candidate preserved.
#[test]
fn covering_unknown_reconciliation_unverifiable_refuses_with_the_candidate() {
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("covering-reconcile-unverifiable");
    let script = Arc::new(Mutex::new(RaceScript::Healthy));
    let reconcile = Arc::new(Mutex::new(Some(ReconcileScript::Unverifiable)));
    let source = CoveringRaceSource {
        root: dir.store(),
        script: script.clone(),
        reconcile: reconcile.clone(),
    };
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing =
        SessionCrossing::open_at(&source, journal, "covering-reconcile-unverifiable").unwrap();
    let held_before = spine.held_count();
    *script.lock().unwrap() = RaceScript::UnknownNotInstalled;
    let now = spine.clock();
    let refusal = crossing.deactivate(&mut spine, &mut director, 4, now);
    match refusal {
        Err(TransitionError::CoveringRefused { detail, candidate }) => {
            assert_eq!(
                detail,
                "covering/unknown-unverifiable: scripted-unverifiable"
            );
            assert!(candidate.is_some(), "the attempted candidate preserved");
        }
        Err(other) => panic!("the unverifiable reconciliation refuses, got {other:?}"),
        Ok(_) => panic!("an unverifiable reconciliation never covers the prerequisite"),
    }
    assert_eq!(director.levels()[4], LodLevel::Full, "the region unchanged");
    assert_eq!(spine.held_count(), held_before, "nothing suspended");
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    assert!(scan.committed.is_empty(), "no T1 precedes the covering");
}

/// §5 of the C1 final corrective (the unresolved reconciliation
/// errors) — the transport failure: the uncertain covering append's
/// reconciliation ITSELF fails (the I/O, the lock): the crossing
/// refuses BEFORE the T1 with the attempted candidate preserved —
/// never a raw error escape, never a success over an unverified
/// candidate.
#[test]
fn covering_unknown_reconciliation_failure_refuses_with_the_candidate() {
    let near = near_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("covering-reconcile-failure");
    let script = Arc::new(Mutex::new(RaceScript::Healthy));
    let reconcile = Arc::new(Mutex::new(Some(ReconcileScript::TransportFailure)));
    let source = CoveringRaceSource {
        root: dir.store(),
        script: script.clone(),
        reconcile: reconcile.clone(),
    };
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing =
        SessionCrossing::open_at(&source, journal, "covering-reconcile-failure").unwrap();
    let held_before = spine.held_count();
    *script.lock().unwrap() = RaceScript::UnknownNotInstalled;
    let now = spine.clock();
    let refusal = crossing.deactivate(&mut spine, &mut director, 4, now);
    match refusal {
        Err(TransitionError::CoveringRefused { detail, candidate }) => {
            assert!(
                detail.starts_with("covering/reconcile-unavailable:"),
                "the reconciliation failure's typed refusal: {detail}"
            );
            assert!(candidate.is_some(), "the attempted candidate preserved");
        }
        Err(other) => panic!("the failed reconciliation refuses, got {other:?}"),
        Ok(_) => panic!("a failed reconciliation never covers the prerequisite"),
    }
    assert_eq!(director.levels()[4], LodLevel::Full, "the region unchanged");
    assert_eq!(spine.held_count(), held_before, "nothing suspended");
    let scan = T1Journal::scan_file(dir.journal()).unwrap();
    assert!(scan.committed.is_empty(), "no T1 precedes the covering");
}
