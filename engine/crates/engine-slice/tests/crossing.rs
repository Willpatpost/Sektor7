//! Stage 5 Batch 4: the six-phase crossing's preparatory half — the
//! DeactivationPlan object and its journal codec. Read-only
//! preparation (phases 1–2 mutate nothing), byte-identical
//! round-trips, the legacy-path equivalence (the plan's checkpoint
//! entries and captures are EXACTLY the legacy deactivation's — the
//! shared gathering), and the fail-closed corruption corpus.

use engine_core::{EventClass, LodLevel, PresenceSample, Spine};
use engine_slice::crossing::DeactivationPlan;
use engine_slice::npc::{NpcConfig, NpcWorld, install_initial_schedules};
use engine_slice::region::RegionConfig;
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
