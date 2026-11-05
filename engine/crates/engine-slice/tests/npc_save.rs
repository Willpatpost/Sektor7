//! Stage 5 Batch 4: the NpcWorld authoritative-state codec
//! (`engine-slice/npc-world-save/1`) — the save/1 world section for
//! the real Stage 4 workload. Byte-identical round-trips, continuation
//! invisibility (the B1 discipline applied to the real world), the
//! dormant-discard sentinels' and captured consequences' survival
//! across the save boundary, and the fail-closed corruption corpus for
//! the section's own gates.

use engine_core::{Decoder, Diagnostics, Encoder, Level, PresenceSample, SaveWorld, Spine};
use engine_slice::npc::{
    NPC_COUNT, NpcConfig, NpcWorld, POSITION_DISCARDED, REGION_COUNT, install_initial_schedules,
};
use engine_slice::region::RegionConfig;
use engine_slice::sim::{SimConfig, SimDriver};

/// A Lod-mode spine with real executed work (no director — the world
/// codec's unit surface; the director-driven scenario is the sentinel
/// test below).
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

fn load(bytes: &[u8]) -> Spine<NpcWorld> {
    Spine::<NpcWorld>::restore(bytes, Diagnostics::new(Level::Off)).expect("the save restores")
}

#[test]
fn round_trip_is_byte_identical_and_state_equal() {
    // 150 ticks ≈ 25 activities per NPC — every NPC crosses its first
    // MILESTONE_PERIOD boundary, so the consequential arrays
    // (milestones, pending signals) are nonzero on the wire, not just
    // structurally present.
    let spine = lod_spine(150);
    assert!(
        spine.world().milestones().iter().any(|m| *m > 0),
        "the fixture crossed milestone boundaries"
    );
    let bytes = spine.save_bytes();
    let restored = load(&bytes);
    assert_eq!(restored.save_bytes(), bytes);
    assert_eq!(spine.engine_fingerprint(), restored.engine_fingerprint());
    assert_eq!(
        spine.lane_state_fingerprint(),
        restored.lane_state_fingerprint()
    );
    assert_eq!(spine.held_fingerprint(), restored.held_fingerprint());
    // The authoritative world state — per-identity, exact.
    assert_eq!(spine.world().steps(), restored.world().steps());
    assert_eq!(
        spine.world().routine_cursor(),
        restored.world().routine_cursor()
    );
    assert_eq!(spine.world().positions(), restored.world().positions());
    assert_eq!(
        spine.world().represented_through(),
        restored.world().represented_through()
    );
    assert_eq!(spine.world().milestones(), restored.world().milestones());
    assert_eq!(
        spine.world().pending_signals(),
        restored.world().pending_signals()
    );
    assert_eq!(spine.world().credits(), restored.world().credits());
    assert_eq!(spine.world().region_lods(), restored.world().region_lods());
}

#[test]
fn continuation_is_invisible() {
    // The B1 discipline on the real workload: a saved-then-restored
    // spine continues exactly like the never-saved control — no
    // observable divergence through the save boundary.
    let mut control = lod_spine(150);
    let saved = lod_spine(150);
    let bytes = saved.save_bytes();
    let mut restored = load(&bytes);
    control.run_ticks(30);
    restored.run_ticks(30);
    assert_eq!(control.engine_fingerprint(), restored.engine_fingerprint());
    assert_eq!(
        control.lane_state_fingerprint(),
        restored.lane_state_fingerprint()
    );
    assert_eq!(control.stats().executed, restored.stats().executed);
    assert_eq!(
        control.world().total_steps(),
        restored.world().total_steps()
    );
    assert_eq!(control.world().milestones(), restored.world().milestones());
}

#[test]
fn dormant_capture_and_sentinels_round_trip() {
    // The director-driven scenario: active accrual (milestone
    // crossings → pending scratch), a far-presence deactivation
    // (capture-before-discard — the scratch captured into the
    // director's ledger, the reconstructible representation discarded
    // to sentinels), the save boundary in the DORMANT mid-state, then
    // a reactivation (the captured consequences applied exactly once →
    // credits) and the boundary again with committed credits on the
    // wire.
    let near = PresenceSample {
        position: (100.0, 0.0),
        velocity: (0.0, 0.0),
    };
    let far = PresenceSample {
        position: (2000.0, 2000.0),
        velocity: (0.0, 0.0),
    };
    let mut driver = SimDriver::with_regions(
        NpcConfig::default(),
        SimConfig::default(),
        RegionConfig::default(),
        &[near],
    )
    .expect("the driver constructs");
    // Active accrual in the player-proximate regions (the others went
    // dormant at construction — their sentinels are present from the
    // start).
    driver.run_headless(150);
    let pending_before: u64 = driver.spine().world().pending_signals().iter().sum();
    assert!(
        pending_before > 0,
        "the active regions accrued pending signals"
    );
    // Deactivate everything: capture, then discard to sentinels.
    driver.set_presence(vec![far]);
    driver.run_headless(5);
    let dormant = driver.spine();
    assert_eq!(
        dormant.world().pending_signals().iter().sum::<u64>(),
        0,
        "the scratch was captured and cleared"
    );
    let dormant_bytes = dormant.save_bytes();
    let restored_dormant = load(&dormant_bytes);
    assert_eq!(restored_dormant.save_bytes(), dormant_bytes);
    let sentinels = |spine: &Spine<NpcWorld>| {
        spine
            .world()
            .positions()
            .iter()
            .filter(|position| **position == POSITION_DISCARDED)
            .count()
    };
    assert!(sentinels(dormant) > 0, "distant regions went dormant");
    assert_eq!(sentinels(&restored_dormant), sentinels(dormant));
    // Reactivate the player-proximate regions: the captured
    // consequences apply exactly once → credits; the boundary again.
    driver.set_presence(vec![near]);
    driver.run_headless(5);
    let credits: u64 = driver.spine().world().credits().iter().sum();
    assert!(
        credits > 0,
        "the reactivation applied the captured signals exactly once"
    );
    let bytes = driver.spine().save_bytes();
    let restored_active = load(&bytes);
    assert_eq!(restored_active.save_bytes(), bytes);
    assert_eq!(
        restored_active.world().credits(),
        driver.spine().world().credits()
    );
}

#[test]
fn corruption_fails_closed() {
    // The section's own gates, on real bytes: the fixed identity
    // count, the position bounds (with the sanctioned sentinel), the
    // LOD discriminants, the near-mask width — plus truncation at
    // every header cut and strided cuts across the fixed-width arrays
    // (a short input never yields a world).
    let spine = lod_spine(150);
    let mut enc = Encoder::new();
    spine.world().save(&mut enc);
    let section = enc.into_bytes();
    // The fixed-width layout: the config (5×u64 + u8), the count,
    // then the dense arrays, then the region-LOD view.
    const CONFIG_LEN: usize = 8 * 5 + 1;
    const COUNT_AT: usize = CONFIG_LEN;
    const STEPS_AT: usize = COUNT_AT + 8;
    const CURSOR_AT: usize = STEPS_AT + NPC_COUNT * 8;
    const POSITIONS_AT: usize = CURSOR_AT + NPC_COUNT * 8;
    const BOUNDARY_AT: usize = POSITIONS_AT + NPC_COUNT * 2;
    const MILESTONES_AT: usize = BOUNDARY_AT + NPC_COUNT * 8;
    const PENDING_AT: usize = MILESTONES_AT + NPC_COUNT * 8;
    const CREDITS_AT: usize = PENDING_AT + NPC_COUNT * 8;
    const LODS_AT: usize = CREDITS_AT + NPC_COUNT * 8;
    assert_eq!(section.len(), LODS_AT + REGION_COUNT);

    let accepts = |bytes: &[u8]| NpcWorld::restore(&mut Decoder::new(bytes)).is_ok();
    // The identity-count gate: 999 is an incompatible workload, 1001
    // exceeds the documented maximum.
    for value in [999u64, 1001] {
        let mut doctored = section.clone();
        doctored[COUNT_AT..COUNT_AT + 8].copy_from_slice(&value.to_le_bytes());
        assert!(!accepts(&doctored), "npc count {value} rejected");
    }
    // The position-bounds gate: (200, y) is off-grid and not the
    // sentinel; the sanctioned sentinel pair passes.
    let mut doctored = section.clone();
    doctored[POSITIONS_AT] = 200;
    assert!(!accepts(&doctored), "off-grid position rejected");
    let mut sentinel = section.clone();
    sentinel[POSITIONS_AT] = u8::MAX;
    sentinel[POSITIONS_AT + 1] = u8::MAX;
    assert!(
        accepts(&sentinel),
        "the dormant-discard sentinel is sanctioned"
    );
    // The LOD discriminant gate.
    let mut doctored = section.clone();
    doctored[LODS_AT] = 9;
    assert!(!accepts(&doctored), "unknown LOD discriminant rejected");
    // The near-mask width gate: mask bit 8 is past REGION_COUNT (the
    // mask occupies section bytes 8..16).
    let mut doctored = section.clone();
    doctored[9] |= 0x01;
    assert!(!accepts(&doctored), "wide near mask rejected");
    // Truncation: every cut through the header and count, strided
    // cuts across the arrays, and the final sixteen bytes.
    for cut in 0..STEPS_AT {
        assert!(!accepts(&section[..cut]), "cut at {cut} rejected");
    }
    let mut cut = STEPS_AT;
    while cut < section.len() {
        assert!(!accepts(&section[..cut]), "cut at {cut} rejected");
        cut += 997;
    }
    for cut in section.len() - 16..section.len() {
        assert!(!accepts(&section[..cut]), "cut at {cut} rejected");
    }
}

/// The world's save section as the shared engine-core data (built
/// from the live world's accessors — the assembly the anchor's fixed
/// world holds after a restore).
fn world_section(world: &NpcWorld) -> engine_core::NpcWorldSection {
    let mut near_mask = 0u64;
    for (index, near) in world.config().near_regions.iter().enumerate() {
        if *near {
            near_mask |= 1 << index;
        }
    }
    engine_core::NpcWorldSection {
        seed: world.config().seed,
        near_mask,
        activity_period: world.config().activity_period,
        simplify_factor: world.config().simplify_factor,
        commitment_window: world.config().commitment_window,
        region_lods_enabled: world.config().region_lods_enabled,
        steps: world.steps().to_vec(),
        routine_cursor: world.routine_cursor().to_vec(),
        positions: world.positions().to_vec(),
        represented_through: world.represented_through().to_vec(),
        milestones: world.milestones().to_vec(),
        pending_signals: world.pending_signals().to_vec(),
        credits: world.credits().to_vec(),
        region_lods: world.region_lods(),
    }
}

#[test]
fn the_shared_section_codec_restores_the_production_save_bytes() {
    // (F4-R15) THE PARITY, ON THE WIRE: the production encoder's
    // bytes (`NpcWorld::save` — unchanged, the pinned save path)
    // decode through the SHARED engine-core section codec with the
    // exact per-identity state — the anchor's fixed world and the
    // production restore read ONE interpretation, never two.
    let spine = lod_spine(150);
    let mut enc = Encoder::new();
    spine.world().save(&mut enc);
    let bytes = enc.into_bytes();
    let mut dec = Decoder::new(&bytes);
    let section = engine_core::NpcWorldSection::restore(&mut dec).unwrap();
    dec.finish().expect("the section consumed exactly");
    let world = spine.world();
    assert_eq!(section.seed, world.config().seed);
    assert_eq!(section.steps, world.steps());
    assert_eq!(section.routine_cursor, world.routine_cursor());
    assert_eq!(section.positions, world.positions());
    assert_eq!(section.represented_through, world.represented_through());
    assert_eq!(section.milestones, world.milestones());
    assert_eq!(section.pending_signals, world.pending_signals());
    assert_eq!(section.credits, world.credits());
    assert_eq!(section.region_lods, world.region_lods());
}

#[test]
fn the_section_save_round_trips_through_the_production_world() {
    // (F4-R15) THE DUAL-ENCODER PARITY: the section's own encoder
    // (`NpcWorldSection::save` — the fixed world's save side) emits
    // bytes the PRODUCTION `NpcWorld::restore` accepts with the
    // identical state — the two encoders of one wire format cannot
    // drift apart silently, and the shared walk's refusals are the
    // production walk's refusals (the equivalence sweep).
    let spine = lod_spine(150);
    let section = world_section(spine.world());
    let mut enc = Encoder::new();
    section.save(&mut enc);
    let bytes = enc.into_bytes();
    let mut dec = Decoder::new(&bytes);
    let restored = NpcWorld::restore(&mut dec).unwrap();
    dec.finish().expect("consumed exactly");
    assert_eq!(restored.steps(), spine.world().steps());
    assert_eq!(restored.routine_cursor(), spine.world().routine_cursor());
    assert_eq!(restored.positions(), spine.world().positions());
    assert_eq!(
        restored.represented_through(),
        spine.world().represented_through()
    );
    assert_eq!(restored.milestones(), spine.world().milestones());
    assert_eq!(restored.pending_signals(), spine.world().pending_signals());
    assert_eq!(restored.credits(), spine.world().credits());
    assert_eq!(restored.region_lods(), spine.world().region_lods());

    // THE EQUIVALENCE SWEEP: every corrupted variant the production
    // restore refuses, the shared section codec refuses identically
    // (and the one sanctioned sentinel form both accept) — the
    // acceptance sets are equal because the walk is one function.
    let mut enc = Encoder::new();
    spine.world().save(&mut enc);
    let section_bytes = enc.into_bytes();
    const CONFIG_LEN: usize = 8 * 5 + 1;
    const COUNT_AT: usize = CONFIG_LEN;
    const STEPS_AT: usize = COUNT_AT + 8;
    const POSITIONS_AT: usize = STEPS_AT + NPC_COUNT * 8 + NPC_COUNT * 8;
    const LODS_AT: usize = POSITIONS_AT
        + NPC_COUNT * 2
        + NPC_COUNT * 8
        + NPC_COUNT * 8
        + NPC_COUNT * 8
        + NPC_COUNT * 8;
    let mut variants: Vec<Vec<u8>> = Vec::new();
    for value in [999u64, 1001] {
        let mut doctored = section_bytes.clone();
        doctored[COUNT_AT..COUNT_AT + 8].copy_from_slice(&value.to_le_bytes());
        variants.push(doctored);
    }
    let mut doctored = section_bytes.clone();
    doctored[POSITIONS_AT] = 200;
    variants.push(doctored);
    let mut doctored = section_bytes.clone();
    doctored[LODS_AT] = 9;
    variants.push(doctored);
    let mut doctored = section_bytes.clone();
    doctored[9] |= 0x01;
    variants.push(doctored);
    for cut in [
        0,
        1,
        CONFIG_LEN,
        STEPS_AT,
        POSITIONS_AT,
        section_bytes.len() - 8,
    ] {
        variants.push(section_bytes[..cut].to_vec());
    }
    for variant in &variants {
        let production = NpcWorld::restore(&mut Decoder::new(variant)).is_ok();
        let shared = engine_core::NpcWorldSection::restore(&mut Decoder::new(variant)).is_ok();
        assert_eq!(
            production,
            shared,
            "the production walk and the shared walk agree on {} bytes",
            variant.len()
        );
        assert!(!production, "the corrupted variant is refused");
    }
    let mut sentinel = section_bytes.clone();
    sentinel[POSITIONS_AT] = u8::MAX;
    sentinel[POSITIONS_AT + 1] = u8::MAX;
    assert!(NpcWorld::restore(&mut Decoder::new(&sentinel)).is_ok());
    assert!(engine_core::NpcWorldSection::restore(&mut Decoder::new(&sentinel)).is_ok());
}
