//! Stage 5 Batch 4: the six-phase crossing's preparatory half — the
//! DeactivationPlan object and its journal codec. Read-only
//! preparation (phases 1–2 mutate nothing), byte-identical
//! round-trips, the legacy-path equivalence (the plan's checkpoint
//! entries and captures are EXACTLY the legacy deactivation's — the
//! shared gathering), and the fail-closed corruption corpus.

use std::collections::HashSet;
use std::path::PathBuf;

use engine_core::{EventClass, LodLevel, PresenceSample, Spine};
use engine_persist::{
    ConsequenceId, FaultAction, JournalFaultControl, JournalFaultPoint, KeyPhase, T1Journal,
};
use engine_slice::crossing::{DeactivationPlan, SessionCrossing, TransitionError};
use engine_slice::npc::{NpcConfig, NpcWorld, REGION_COUNT, install_initial_schedules};
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
