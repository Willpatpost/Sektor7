//! Stage 4 sub-batch 2b: the region-LOD workload extensions — Legacy
//! behavior preserved exactly (pinned to the accepted baseline),
//! Lod-mode cadence scaling and cursor semantics, milestones as
//! real-execution-only consequences, position bounds, and the two
//! reconstruction primitives (replay exact, synthesis bounded and
//! labeled by the caller).

use engine_core::{Fingerprinter, LodLevel, Spine, Tick};
use engine_slice::npc::{
    MILESTONE_PERIOD, NPC_COUNT, NpcConfig, NpcWorld, REGION_GRID, install_initial_schedules,
    payload, routine_payload, routine_payload_parts,
};
use engine_slice::sim::{SimConfig, SimDriver};

fn lod_config() -> NpcConfig {
    NpcConfig {
        region_lods_enabled: true,
        ..NpcConfig::default()
    }
}

#[test]
fn legacy_mode_reproduces_the_accepted_stage3_baseline() {
    // The Legacy handler path must reproduce the accepted Stage 3
    // default run exactly: 45,717 executed activities over 300 ticks,
    // lane fingerprint 5591d3eb05643e78, state fingerprint
    // 9d90cb3bbd45b032 — the frozen-domain guarantee, pinned. New
    // Stage 4 state (cursor, milestones) stays untouched in Legacy
    // mode.
    let mut driver = SimDriver::new(NpcConfig::default(), SimConfig::default());
    driver.run_headless(300);
    let spine = driver.spine();
    assert_eq!(spine.world().total_steps(), 45_717);
    assert_eq!(spine.lane_fingerprint_hex(), "5591d3eb05643e78");
    assert_eq!(spine.lane_state_fingerprint_hex(), "9d90cb3bbd45b032");
    assert!(
        spine
            .world()
            .routine_cursor()
            .iter()
            .all(|cursor| *cursor == 0),
        "Legacy mode never touches the routine cursor"
    );
    assert!(
        spine
            .world()
            .milestones()
            .iter()
            .all(|milestone| *milestone == 0),
        "Legacy mode never records milestones"
    );
    assert!(
        spine
            .world()
            .pending_signals()
            .iter()
            .all(|signal| *signal == 0),
        "Legacy mode never accrues signal scratch"
    );
    assert!(
        spine.world().credits().iter().all(|credit| *credit == 0),
        "Legacy mode never commits credits"
    );
    assert!(
        spine
            .world()
            .represented_through()
            .iter()
            .all(|boundary| *boundary == Tick(0)),
        "Legacy mode never advances a represented-through boundary"
    );
}

#[test]
fn lod_mode_cadence_scales_execution_rates() {
    // The LOD ladder is measurable in work: Full executes at the natural
    // period, Simplified at period × simplify factor, EventDriven at
    // period × commitment window — the lower levels do meaningfully
    // less per tick. Absent reconstruction, the routine cursor equals
    // the actual-execution count.
    let config = lod_config();
    let mut spine = Spine::with_reservation(NpcWorld::new(config.clone()), 200, 60);
    spine.world_mut().set_region_lod(0, LodLevel::Full);
    spine.world_mut().set_region_lod(3, LodLevel::Simplified);
    spine.world_mut().set_region_lod(5, LodLevel::EventDriven);
    install_initial_schedules(&mut spine, &config);
    spine.run_ticks(300);
    let executed_in = |class: u16| {
        spine
            .ledger()
            .iter()
            .filter(|record| record.class.0 == class)
            .count()
    };
    let full = executed_in(0);
    let simplified = executed_in(3);
    let event = executed_in(5);
    assert!(
        full > 4_000,
        "Full executes at the natural cadence ({full})"
    );
    assert!(
        simplified * 3 < full,
        "Simplified executes meaningfully less ({simplified} vs {full})"
    );
    assert!(
        event * 2 < simplified,
        "EventDriven executes meaningfully less ({event} vs {simplified})"
    );
    assert_eq!(
        spine.world().routine_cursor(),
        spine.world().steps(),
        "cursor == steps absent reconstruction"
    );
}

#[test]
fn milestones_count_real_executions_only() {
    // Reviewer priority A: milestones — irreversible consequences —
    // arise only from actual executions. Reconstruction (either method)
    // never increments them, and never touches `steps`.
    let config = lod_config();
    let mut spine = Spine::with_reservation(NpcWorld::new(config.clone()), 200, 60);
    install_initial_schedules(&mut spine, &config);
    spine.run_ticks(300);
    {
        let world = spine.world();
        for npc in (0..NPC_COUNT).step_by(37) {
            assert_eq!(
                world.milestones()[npc],
                world.steps()[npc] / MILESTONE_PERIOD,
                "milestones == floor(steps / period) for npc {npc}"
            );
        }
    }
    let (before_steps, before_milestones, before_pending, before_credits) = {
        let world = spine.world();
        (
            world.steps().to_vec(),
            world.milestones().to_vec(),
            world.pending_signals().to_vec(),
            world.credits().to_vec(),
        )
    };
    let replayed = spine
        .world_mut()
        .replay_routine_span(0, Tick(10), Tick(500), 1);
    let synthesized = spine
        .world_mut()
        .synthesize_routine_span(1, Tick(10), Tick(500), 1);
    assert!(replayed > 0, "the replay span reconstructs activities");
    assert!(
        synthesized > 0,
        "the synthesis span reconstructs activities"
    );
    let world = spine.world();
    assert_eq!(
        world.steps(),
        before_steps.as_slice(),
        "steps untouched by reconstruction"
    );
    assert_eq!(
        world.milestones(),
        before_milestones.as_slice(),
        "milestones untouched by reconstruction"
    );
    assert_eq!(
        world.pending_signals(),
        before_pending.as_slice(),
        "signal scratch untouched by reconstruction"
    );
    assert_eq!(
        world.credits(),
        before_credits.as_slice(),
        "credits untouched by reconstruction — neither replay nor synthesis can fabricate a consequence"
    );
    assert!(
        world.routine_cursor()[0] > world.steps()[0],
        "the routine cursor advanced by labeled reconstruction"
    );
}

#[test]
fn positions_stay_within_region_bounds() {
    // The region-local grid is a bounds invariant: live execution and
    // both reconstruction methods keep every NPC inside it, with
    // per-activity travel of at most one cell (reachability by
    // construction).
    let config = lod_config();
    let mut spine = Spine::with_reservation(NpcWorld::new(config.clone()), 200, 60);
    install_initial_schedules(&mut spine, &config);
    spine.run_ticks(300);
    spine
        .world_mut()
        .replay_routine_span(7, Tick(1), Tick(1000), 1);
    spine
        .world_mut()
        .synthesize_routine_span(9, Tick(1), Tick(1000), 1);
    for (npc, &(x, y)) in spine.world().positions().iter().enumerate() {
        assert!(
            x < REGION_GRID && y < REGION_GRID,
            "npc {npc} stays in bounds ({x},{y})"
        );
    }
}

#[test]
fn replay_is_exact_and_synthesis_is_bounded_drift() {
    // E-09 semantics for the demo workload: replay recomputes the exact
    // seeded jitter chain (cost proportional to activities); synthesis
    // is the closed-form count (O(1)). The two agree within the
    // jitter's bounded drift — synthesis is labeled by the caller and
    // never claims exactness.
    let build = |replay: bool| {
        let mut world = NpcWorld::new(lod_config());
        let count = if replay {
            world.replay_routine_span(3, Tick(10), Tick(1000), 4)
        } else {
            world.synthesize_routine_span(3, Tick(10), Tick(1000), 4)
        };
        (count, world.routine_cursor()[3], world.positions()[3])
    };
    let (replay_count, replay_cursor, replay_pos) = build(true);
    let (synth_count, synth_cursor, synth_pos) = build(false);
    // The span: 990 ticks at period 6 × cadence 4 = 24 per activity —
    // synthesis counts 990 / 24 = 41; replay's jittered delays (24 or
    // 25 per activity) land within a small drift of that.
    assert_eq!(synth_count, 41);
    assert!(
        (41i64 - replay_count as i64).abs() <= 2,
        "replay within jitter drift of the closed form ({replay_count})"
    );
    assert_eq!(
        replay_cursor, replay_count,
        "the cursor advances by exactly the replayed count"
    );
    assert_eq!(synth_cursor, synth_count);
    assert!(replay_pos.0 < REGION_GRID && replay_pos.1 < REGION_GRID);
    assert!(synth_pos.0 < REGION_GRID && synth_pos.1 < REGION_GRID);
}

#[test]
fn spatial_fingerprint_is_deterministic_and_sensitive() {
    // The new Stage 4 workload domain ("engine-slice/npc-spatial/1")
    // hashes the routine cursor, positions, milestones, and the LOD
    // view — deterministic across double runs, sensitive to different
    // progression. The frozen Stage 3 domain never sees any of it
    // (pinned by the Legacy baseline test).
    let hash_of = |ticks: u64| {
        let config = lod_config();
        let mut spine = Spine::with_reservation(NpcWorld::new(config.clone()), 200, 60);
        install_initial_schedules(&mut spine, &config);
        spine.run_ticks(ticks);
        let mut fp = Fingerprinter::new();
        spine.world().spatial_fingerprint(&mut fp);
        fp.finish()
    };
    assert_eq!(hash_of(100), hash_of(100), "double-run determinism");
    assert_ne!(
        hash_of(100),
        hash_of(200),
        "different progression hashes differently"
    );
}

#[test]
fn routine_payloads_are_explicitly_typed() {
    // Batch 2 corrective finding A / reviewer addition: Lod-mode
    // routine chains carry an explicit type tag — a payload is a
    // routine chain only when the tag says so. Foreign payloads,
    // including ones that would decode as an NPC under the Legacy
    // layout, stay foreign.
    let (npc, cursor) = routine_payload_parts(routine_payload(123, 456)).unwrap();
    assert_eq!((npc, cursor), (123, 456));
    assert_eq!(
        routine_payload_parts(routine_payload(7, 0xFFFF_FFFF)).unwrap(),
        (7, 0xFFFF_FFFF),
        "the full 32-bit cursor round-trips"
    );
    assert!(
        routine_payload_parts(routine_payload(NPC_COUNT, 0)).is_none(),
        "out-of-range identities are not routine chains"
    );
    assert!(
        routine_payload_parts(4).is_none(),
        "a Legacy-layout payload (would decode as npc 0) is foreign in Lod mode"
    );
    assert!(
        routine_payload_parts(0x8000_0000_0000_0001).is_none(),
        "an untagged high payload is foreign"
    );
    assert!(
        routine_payload_parts(payload(123, 456)).is_none(),
        "Legacy payloads are not Lod routine payloads"
    );
}
