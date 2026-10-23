//! Stage 5 Batch 4: the six-phase crossing's preparatory half — the
//! DeactivationPlan object and its journal codec. Read-only
//! preparation (phases 1–2 mutate nothing), byte-identical
//! round-trips, the legacy-path equivalence (the plan's checkpoint
//! entries and captures are EXACTLY the legacy deactivation's — the
//! shared gathering), and the fail-closed corruption corpus.

use std::collections::HashSet;
use std::path::PathBuf;

use engine_core::{Encoder, EventClass, LodLevel, PresenceSample, Spine};
use engine_persist::{
    AwardCheckpoint, AwardState, CancellationReason, CanonicalMedium, ClassifiedSpan,
    CommitOutcome, ConsequenceEnvelope, ConsequenceId, CounterValues, Coverage, DurabilityClass,
    EncounterJournal, FaultAction, JournalFaultControl, JournalFaultPoint, KeyPhase,
    LocalFileMedium, OccurrenceSpan, RecoveryError, RevisionRecord, SignalLane, SourceBaseline,
    SourcePosition, StoredObject, T1Journal, TransactionInput,
};
use engine_slice::crossing::{DeactivationPlan, SessionCrossing, TransitionError};
use engine_slice::npc::{
    NpcConfig, NpcWorld, POSITION_DISCARDED, REGION_COUNT, install_initial_schedules,
};
use engine_slice::region::{RegionConfig, RegionDirector, stage4_state_fingerprint};
use engine_slice::sim::{SimConfig, SimDriver};

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
        LodLevel::EventDriven,
        "the empty-scratch deactivation is presence-re-derivable, never recovered"
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

/// The multi-episode interleave (the same corrective's other half):
/// ONE snapshot before BOTH episodes. The recovery must interleave
/// per transaction — episode 1's restore (the claimed delta minus the
/// snapshot's scratch), its install (capturing and zeroing the
/// scratch), then episode 2's restore (the FULL delta — the prior
/// install rebased the scratch exactly as the live capture did) and
/// its install. Per-envelope arithmetic against the snapshot's stale
/// scratch without the interleave under-restores the second episode
/// and fails its install's scratch re-validation.
#[test]
fn recovery_restores_both_episodes_from_an_older_snapshot() {
    let near = near_region_4();
    let far = far_from_region_4();
    let (mut spine, mut director) = scenario_pair(150, &near);
    let dir = TestDir::new("two-episodes");
    let journal = T1Journal::open(dir.journal()).unwrap();
    let mut crossing =
        SessionCrossing::open(&dir.store(), journal, "two-episodes-campaign").unwrap();
    // THE older snapshot: before both episodes.
    crossing.publish_snapshot(&spine, &director).unwrap();
    // Episode 1: accrual beyond the snapshot, the durable far
    // boundary, and the durable reactivation.
    for _ in 0..80 {
        director.apply(&mut spine, std::slice::from_ref(&near));
        spine.run_tick();
    }
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    durable_boundary(&mut crossing, &mut spine, &mut director, &near);
    // Episode 2: accrual, then the second durable far boundary —
    // the accrual long enough for the threshold crossings to recur
    // (under the spine's budget a Full region executes ~0.15
    // activities per NPC per tick, so a MILESTONE_PERIOD crossing
    // needs ~a hundred ticks; a short accrual would make episode 2's
    // deactivation empty-scratch — unjournaled,
    // presence-re-derivable — and the multi-episode interleave
    // would never be exercised).
    for _ in 0..160 {
        director.apply(&mut spine, std::slice::from_ref(&near));
        spine.run_tick();
    }
    durable_boundary(&mut crossing, &mut spine, &mut director, &far);
    // THE CRASH + the recovery from the pre-episode snapshot.
    drop(crossing);
    let (recovered_spine, recovered_director, mut recovered_crossing) =
        SessionCrossing::recover(&dir.store(), &dir.journal(), "two-episodes-campaign")
            .expect("the interleaved recovery succeeds");
    assert_eq!(recovered_director.levels()[4], LodLevel::Dormant);
    assert_eq!(
        recovered_director.states()[4].checkpoint,
        director.states()[4].checkpoint,
        "episode 2's checkpoint matches the lost state's exactly"
    );
    // The milestones: the snapshot's plus BOTH episodes'
    // post-snapshot crossings — exactly the lost state's.
    assert_eq!(
        recovered_spine.world().milestones(),
        spine.world().milestones(),
        "the milestones match the lost state's exactly across both episodes"
    );
    // The durable lanes: episode 1's captures are APPLIED (the
    // derived reactivation replayed the lost session's application —
    // the credits match the lost state's exactly), episode 2's are
    // pending.
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
    let CommitOutcome::Confirmed(receipt) = recovered_crossing.commit_canonical().unwrap() else {
        panic!("the re-drive confirms both episodes' recovered captures");
    };
    assert!(receipt.keys.contains(&ConsequenceId::new(4, 1, 4)));
    assert!(receipt.keys.contains(&ConsequenceId::new(4, 2, 4)));
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
    let (recovered_spine, recovered_director, mut recovered_crossing) =
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
    // The future-version case: a session-snapshot/2 object is still
    // recognizable (the domain family) — this build fails closed
    // rather than silently using the older snapshot.
    append_corrupt("engine-slice/session-snapshot/2");
    let error = match SessionCrossing::recover(&dir.store(), &dir.journal(), "corrupt-campaign") {
        Err(error) => error,
        Ok(_) => panic!("the future-version snapshot also fails closed"),
    };
    assert!(matches!(error, TransitionError::CorruptSnapshot(_)));
}
