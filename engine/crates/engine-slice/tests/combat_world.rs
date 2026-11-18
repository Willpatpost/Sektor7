//! Cycle 3 P6 integration battery — the combat encounter's
//! application-level guards. All headless: no SDL initialization, no
//! GPU, no process spawns — the deterministic scenarios, the mapping,
//! and the demo's public surfaces (the same discipline as the
//! Cycle-2 playable battery).
//!
//! The encounter drives SEAT B's reviewed adversary module (the
//! spine composition); the scenario expectations were RE-MEASURED at
//! the swap (the review-F1 honest re-measurement: the winnable keeps
//! its tick-147 resolution and its UNCHANGED player trace; the
//! failure's player trace is a NEW pinned value — the walk's stop
//! became player-only, cross-platform-safe).

use engine_core::StaticCollider;
use engine_shell::{InputSnapshot, MouseButton};
use engine_slice::combat_demo::{
    ACTIVATION_Z, ADVERSARY_SPAWN, ARENA_BOXES, ArenaRole, CombatDemo, EncounterPhase,
    PLAYER_SPAWN, adversary_body, arena_world, combat_config, map_combat_input,
    run_combat_failure_headless, run_combat_headless, run_winnable_script,
};

/// The layout literal's invariants (the F2 rule: an independent
/// expectation, never derived from the builder it checks — the
/// critical extents, the centerline-clear corridor the scripts rely
/// on, and the role census are hand-written here).
#[test]
fn arena_layout_pinned() {
    assert_eq!(ARENA_BOXES.len(), 12, "the layout census");
    let roles = |role: ArenaRole| {
        ARENA_BOXES
            .iter()
            .filter(|class_box| class_box.role == role)
            .count()
    };
    assert_eq!(roles(ArenaRole::Perimeter), 4);
    assert_eq!(roles(ArenaRole::CoverPillar), 4);
    assert_eq!(roles(ArenaRole::LowWall), 2);
    assert_eq!(roles(ArenaRole::ActivationPad), 1);
    assert_eq!(roles(ArenaRole::AdversaryMarker), 1);

    // The cover pillars stand above the eye (1.6 m) — genuine shot
    // blockers; the low walls stay below it — movement cover only.
    for class_box in ARENA_BOXES
        .iter()
        .filter(|b| b.role == ArenaRole::CoverPillar)
    {
        assert!(
            class_box.max[1] > 1.6,
            "a cover pillar must block the eye line"
        );
    }
    for class_box in ARENA_BOXES.iter().filter(|b| b.role == ArenaRole::LowWall) {
        assert!(
            class_box.max[1] < 1.6,
            "a low wall must never block the eye line"
        );
    }

    // The centerline corridor (x ∈ [−0.6, 0.6], z ∈ [−4, 6]) — the
    // scripted scenarios' shots, pursuit, and melee ride it, so no
    // SOLID box may intrude (the pads are visual-only).
    for class_box in ARENA_BOXES.iter().filter(|b| b.role.is_solid()) {
        let x_clear = class_box.max[0] < -0.6 || class_box.min[0] > 0.6;
        let z_clear = class_box.max[2] < -4.6 || class_box.min[2] > 6.0;
        assert!(
            x_clear || z_clear,
            "the centerline corridor must stay clear: {:?}",
            class_box
        );
    }

    // The spawn geometry: the player and the adversary's home ground
    // on the centerline, the activation line between them.
    assert_eq!(PLAYER_SPAWN, [0.0, 2.0, 6.0]);
    assert_eq!(ADVERSARY_SPAWN, [0.0, 0.0, -4.0]);
    assert_eq!(ACTIVATION_Z, 2.0);
    let (adversary_min, adversary_max) = adversary_body(ADVERSARY_SPAWN);
    assert_eq!(adversary_min, [-0.45, 0.0, -4.45]);
    assert_eq!(adversary_max, [0.45, 1.8, -3.55]);

    // The collision world carries exactly the solid boxes.
    let expected: Vec<StaticCollider> = ARENA_BOXES
        .iter()
        .filter(|class_box| class_box.role.is_solid())
        .map(|class_box| StaticCollider {
            min: class_box.min,
            max: class_box.max,
        })
        .collect();
    let world = arena_world();
    assert_eq!(world.colliders().len(), expected.len());
    for (index, collider) in world.colliders().iter().enumerate() {
        assert_eq!(collider.min, expected[index].min, "collider {index}");
        assert_eq!(collider.max, expected[index].max, "collider {index}");
    }
}

/// B's seeded circuit spawn (the adversary's position at
/// construction, per the composed `AdversaryConfig`: seed 0xA11CE,
/// the circuit of radius 2.5 around the home ground) — MEASURED and
/// pinned (a divergence is an investigation, never a silent re-pin;
/// the value feeds the placement literals below).
#[test]
fn adversary_circuit_spawn_pinned() {
    let demo = CombatDemo::new();
    let position = demo.spine.world().adversary(0).position;
    assert_eq!(
        position,
        [-1.2500001, 0.0, -1.8349366], // MEASURED at the swap (fail-first captured).
        "the seeded circuit spawn (measured at the swap)"
    );
}

/// The full WINNABLE scenario, headless and deterministic: the event
/// sequence in order — B's machine catches the walking player (the
/// first attack lands BEFORE the activation line: the adversary is
/// self-governing, perception-driven), the exchange, and the
/// completion — byte-identical across invocations.
#[test]
fn winnable_scenario_completes_headless() {
    let first = run_combat_headless();
    let second = run_combat_headless();
    assert_eq!(first, second, "the scenario double-runs identically");

    let expected_sequence = [
        "settled on the arena ground",
        "adversary attack — player takes 20 (80/100)",
        "encounter activated — the adversary stirs",
        "adversary attack — player takes 20 (60/100)",
        "fire 1: HIT — adversary 75/100",
        "fire 2: HIT — adversary 50/100",
        "adversary attack — player takes 20 (40/100)",
        "fire 3: HIT — adversary 25/100",
        "adversary attack — player takes 20 (20/100)",
        "fire 4: HIT — adversary 0/100",
        "ADVERSARY DEFEATED — encounter WON",
    ];
    let mut last = 0;
    for line in expected_sequence {
        let at = first
            .find(line)
            .unwrap_or_else(|| panic!("the scenario must log: {line}"));
        assert!(at >= last, "the events happen in order: {line}");
        last = at;
    }
    assert!(first.contains("outcome: WON"), "the win is reported");
    assert!(
        first.contains("attacks taken 4"),
        "the adversary meaningfully threatened the player (four landed attacks — \
         the R1xR2 combined semantics: B's canonical logical-tick cooldown \
         attacks on a ~26-tick cycle; the R1-era value was 2)"
    );
    assert!(first.contains("fires 4"), "exactly four fires");
    assert!(first.contains("refused=0"), "no refused movement steps");
    assert!(
        first.contains("player: hp 20/100"),
        "the player survives wounded (barely — a maximally threatened win; \
         the R1-era value was 60/100): {}",
        first
            .lines()
            .find(|l| l.starts_with("player:"))
            .unwrap_or_default()
    );
}

/// The reproducible FAILURE scenario: the player who does not fight
/// back loses — byte-identical across invocations.
#[test]
fn failure_scenario_reproduces_headless() {
    let first = run_combat_failure_headless();
    let second = run_combat_failure_headless();
    assert_eq!(first, second, "the failure double-runs identically");

    let expected_sequence = [
        "settled on the arena ground",
        "adversary attack — player takes 20 (80/100)",
        "encounter activated — the adversary stirs",
        "adversary attack — player takes 20 (60/100)",
        "adversary attack — player takes 20 (40/100)",
        "adversary attack — player takes 20 (20/100)",
        "adversary attack — player takes 20 (0/100)",
        "PLAYER DEFEATED — encounter LOST",
    ];
    let mut last = 0;
    for line in expected_sequence {
        let at = first
            .find(line)
            .unwrap_or_else(|| panic!("the failure must log: {line}"));
        assert!(at >= last, "the failure events happen in order: {line}");
        last = at;
    }
    assert!(first.contains("outcome: LOST"), "the loss is reported");
    assert!(first.contains("fires 0"), "the player never fired");
    assert!(first.contains("attacks taken 5"), "five attacks to defeat");
    assert!(
        first.contains("adversary: RECOVER · hp 100/100"),
        "the adversary survives the failure"
    );
}

/// The scenarios' player traces, pinned (the libm-free class: the
/// scripted courses never look, so yaw and pitch stay exactly zero —
/// frozen constants, cross-platform; the walk stops are PLAYER-ONLY
/// lines, so the traces cannot drift with the seeded circuit
/// arithmetic inside B's module).
#[test]
fn combat_scenario_trace_pinned() {
    let winnable = run_combat_headless();
    let trace = winnable
        .lines()
        .find_map(|line| line.strip_prefix("player-trace="))
        .expect("the winnable summary carries the trace fingerprint");
    assert_eq!(
        trace, "d2ef688081dbc659",
        "the winnable scenario trace — UNCHANGED through the B-swap \
         (the player-only walk; a divergence is an investigation, \
         never a silent re-pin)"
    );

    let failure = run_combat_failure_headless();
    let trace = failure
        .lines()
        .find_map(|line| line.strip_prefix("player-trace="))
        .expect("the failure summary carries the trace fingerprint");
    assert_eq!(
        trace, "0ddc141d7e3c53c1",
        "the failure scenario trace — the ONE COMBINED R1xR2 remeasure \
         (B's canonical logical-tick cooldown defeats the passive player \
         earlier — LOST at tick 162 vs 274 — so the run records fewer \
         idle movement ticks; the R1-era value was 18e578ef87eec296, \
         the pre-B-swap provisional 494ade50b21d43cc; both preserved as \
         evidence — a further divergence is an investigation, never a \
         silent re-pin)"
    );
}

/// The fire edge: one press, one resolution — at the mapping level
/// (a held button produces no fresh press) and at the demo level (a
/// single fire tick resolves once; further ticks without the edge do
/// nothing; the cooldown gates a too-fast second press).
#[test]
fn fire_edge_fires_once_per_press() {
    // Mapping level: the press frame maps fire; the held frame (no
    // mouse_pressed entry — the shell only reports edges) does not.
    let press = InputSnapshot {
        mouse_pressed: vec![MouseButton::Left],
        ..InputSnapshot::default()
    };
    assert!(map_combat_input(&press, false).fire_pressed);
    let held = InputSnapshot::default();
    assert!(!map_combat_input(&held, false).fire_pressed);
    // The focus-loss neutralization kills the fire edge too.
    let lost = InputSnapshot {
        mouse_pressed: vec![MouseButton::Left],
        focus_lost: true,
        ..InputSnapshot::default()
    };
    assert!(!map_combat_input(&lost, false).fire_pressed);

    // Demo level: teleport past the activation line onto the
    // adversary's spawn x-line (the zero-angle aim), activate, and
    // press once (the shot hits the patrolling adversary).
    let mut demo = CombatDemo::new();
    let adversary_x = demo.spine.world().adversary(0).position[0];
    let state = demo.driver.state_mut();
    state.position = [adversary_x, 0.0, 1.5];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    demo.tick(engine_core::MoveIntent::default(), false);
    assert_eq!(
        demo.encounter.phase,
        EncounterPhase::Active,
        "crossing the line activates the encounter's marker"
    );
    demo.tick(engine_core::MoveIntent::default(), true);
    assert_eq!(demo.encounter.fires, 1, "one press, one fire");
    assert_eq!(demo.encounter.hits, 1, "the dead-ahead press hits");
    for _ in 0..5 {
        demo.tick(engine_core::MoveIntent::default(), false);
    }
    assert_eq!(
        demo.encounter.fires, 1,
        "no further presses, no further fires"
    );

    // The cooldown gate: an immediate second press is refused with
    // feedback (one refusal event; a refused press is NOT a fire).
    demo.tick(engine_core::MoveIntent::default(), true);
    assert_eq!(
        demo.encounter.fires, 1,
        "the refused press does not count as a fire"
    );
    let last = demo.encounter.events.last().expect("an event").1.clone();
    assert!(
        last.starts_with("fire refused — the weapon is cooling"),
        "the cooldown refusal is logged: {last}"
    );
}

/// The fire refusals: out of range (with the measured distance), the
/// pillar occlusion (BLOCKED behind cover), and the low wall that
/// never blocks a standing eye-level shot (the honest cover model).
/// The placements ride the MEASURED circuit position (read from the
/// live world at setup — the pinned spawn test guards the value),
/// keeping the zero-angle aim discipline.
#[test]
fn fire_refused_out_of_range_and_occluded() {
    // Out of range: the shrunken-range config against the dead-ahead
    // geometry (the measured circuit spawn sits ~3.4 m from the z=2
    // placement; the 3.0 m range refuses the shot).
    let mut config = combat_config();
    config.weapon_range = 3.0;
    let mut demo = CombatDemo::new_with_config(config);
    let adversary_x = demo.spine.world().adversary(0).position[0];
    let state = demo.driver.state_mut();
    state.position = [adversary_x, 0.0, 2.0];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    demo.tick(engine_core::MoveIntent::default(), false);
    demo.tick(engine_core::MoveIntent::default(), true);
    assert_eq!(demo.encounter.out_of_range, 1, "the far shot is refused");
    let last = demo.encounter.events.last().expect("an event").1.clone();
    assert!(
        last.starts_with("fire 1: OUT OF RANGE") && last.contains("m > 3 m"),
        "the refusal names the distance: {last}"
    );

    // The pillar occlusion: the seed-32 circuit spawn (2.500,
    // −3.956) sits dead on the east (2.5, 3.0) pillar's line; the
    // player north of that pillar fires dead-ahead at tick 2 (the
    // seeded first patrol wake is later, so the spawn holds) — the
    // exact-slab query blocks the eye→entry segment through the
    // pillar. A blocked shot does no damage (B's machine never sees
    // it — the patrolling state is untouched).
    let mut config = combat_config();
    config.adversary_seed = 32;
    let mut demo = CombatDemo::new_with_config(config);
    assert_eq!(
        demo.spine.world().adversary(0).position,
        [2.4996192, 0.0, -3.956369],
        "the seed-32 circuit spawn (measured; the pinned spawn test guards \
         the default seed's value)"
    );
    let state = demo.driver.state_mut();
    state.position = [2.5, 0.0, 4.2];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    demo.tick(engine_core::MoveIntent::default(), false);
    demo.tick(engine_core::MoveIntent::default(), true);
    assert_eq!(demo.encounter.blocked, 1, "the covered shot is blocked");
    assert_eq!(demo.encounter.hits, 0, "no damage through cover");
    let last = demo.encounter.events.last().expect("an event").1.clone();
    assert!(
        last.starts_with("fire 1: BLOCKED"),
        "the occlusion refusal is logged: {last}"
    );
    assert_eq!(
        demo.spine.world().adversary(0).state,
        engine_core::adversary::AdversaryState::Patrol,
        "the blocked shot does not provoke (no damage received)"
    );

    // The low wall never blocks the standing shot: the dead-ahead
    // line over the left low wall (its top 1.1 m is below the eye).
    // The landed hit on the PATROLLING adversary provokes AGGRO
    // (B's wired rule: damage received forces AGGRO from any
    // non-dormant state).
    let mut demo = CombatDemo::new();
    let adversary_x = demo.spine.world().adversary(0).position[0];
    let state = demo.driver.state_mut();
    state.position = [adversary_x, 0.0, 4.2];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    demo.tick(engine_core::MoveIntent::default(), false);
    demo.tick(engine_core::MoveIntent::default(), true);
    assert_eq!(demo.encounter.hits, 1, "the shot passes over the low wall");
    assert_eq!(demo.encounter.blocked, 0, "the low wall does not block");
    assert_eq!(
        demo.spine.world().adversary(0).health.current,
        75.0,
        "the contract's Health transition applies the damage (through B's module)"
    );
    assert_eq!(
        demo.spine.world().adversary(0).state,
        engine_core::adversary::AdversaryState::Aggro,
        "damage received provokes AGGRO (B's wired rule)"
    );
    assert_eq!(
        demo.encounter.phase,
        EncounterPhase::Dormant,
        "the encounter's own marker stays at the line's authority"
    );
}

/// B's surfaced-attack application (the Health-ownership seam): the
/// resolved attacks apply to the player's `Health` with F's damage
/// policy — the failure shape in miniature (idle in the adversary's
/// ground, never fire): the health ladder descends by the policy
/// damage per surfaced Hit, the attacks are cadence-spaced, and the
/// engine's Defeated declaration decides the loss.
#[test]
fn surfaced_attacks_apply_player_damage() {
    let mut demo = CombatDemo::new();
    // The failure scenario's proven stop: the centerline inside the
    // adversary's aggro ground (the player-only z ≤ 1.0 line).
    let state = demo.driver.state_mut();
    state.position = [0.0, 0.0, 1.0];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;

    // The miniature: idle until the encounter resolves (the player
    // who does not fight back loses; the loop breaks only on a
    // TERMINAL phase — the Dormant marker at entry must not stop it).
    for _ in 0..3_000 {
        if demo.encounter.phase.is_terminal() {
            break;
        }
        demo.tick(engine_core::MoveIntent::default(), false);
    }
    assert!(
        matches!(demo.encounter.phase, EncounterPhase::Lost { .. }),
        "the idle player is defeated"
    );
    assert_eq!(demo.encounter.fires, 0, "the player never fired");
    assert_eq!(
        demo.encounter.attacks_taken, 5,
        "exactly five landed attacks (100 hp / 20 damage)"
    );
    assert_eq!(demo.encounter.player.current, 0.0);
    // The ladder: 80/60/40/20/0 in order.
    let ladder: Vec<&str> = demo
        .encounter
        .events
        .iter()
        .filter(|(_, line)| line.starts_with("adversary attack — player takes"))
        .map(|(_, line)| line.as_str())
        .collect();
    let expected = [
        "adversary attack — player takes 20 (80/100)",
        "adversary attack — player takes 20 (60/100)",
        "adversary attack — player takes 20 (40/100)",
        "adversary attack — player takes 20 (20/100)",
        "adversary attack — player takes 20 (0/100)",
    ];
    assert_eq!(ladder, expected, "the health ladder, in order");
    // The cadence spacing (B's wake-cycle: the cooldown decrements
    // per wake-up, the recover window, then the re-aggro — the
    // measured 54-tick cycle at these placeholders).
    let attack_ticks: Vec<u64> = demo
        .encounter
        .events
        .iter()
        .filter(|(_, line)| line.starts_with("adversary attack — player takes"))
        .map(|(tick, _)| *tick)
        .collect();
    for pair in attack_ticks.windows(2) {
        assert!(
            pair[1] - pair[0] >= 24,
            "the attacks are cooldown-spaced: {attack_ticks:?}"
        );
    }
}

/// FP/TP compatibility: the aim derives from (eye, look_direction),
/// never the camera — firing hits identically in both camera modes.
#[test]
fn fire_works_in_both_camera_modes() {
    for third_person in [false, true] {
        let mut demo = CombatDemo::new();
        let adversary_x = demo.spine.world().adversary(0).position[0];
        let state = demo.driver.state_mut();
        state.position = [adversary_x, 0.0, 2.0];
        state.velocity = [0.0, 0.0, 0.0];
        state.on_ground = true;
        if third_person {
            state.toggle_camera_mode();
        }
        demo.tick(engine_core::MoveIntent::default(), false);
        demo.tick(engine_core::MoveIntent::default(), true);
        assert_eq!(
            demo.encounter.hits,
            1,
            "the fire hits in {} person",
            if third_person { "third" } else { "first" }
        );
        assert_eq!(
            demo.spine.world().adversary(0).health.current,
            75.0,
            "the damage applies in {} person",
            if third_person { "third" } else { "first" }
        );
    }
}

/// The transient reset: after a terminal phase, reset returns the
/// demo to a bit-fresh DORMANT round and the scenario re-runs — the
/// honest TRANSIENT round reset (no persistence, no checkpoints).
#[test]
fn encounter_reset_is_transient_and_repeatable() {
    let mut demo = CombatDemo::new();
    run_winnable_script(&mut demo);
    assert!(
        demo.encounter.phase.is_terminal(),
        "the first round resolved"
    );
    let first_trace = demo.driver.trace_fingerprint_hex();
    demo.reset();
    assert_eq!(demo.encounter.phase, EncounterPhase::Dormant);
    assert_eq!(demo.encounter.rounds, 1, "the round counter advanced");
    assert_eq!(
        demo.encounter.player.current, 100.0,
        "the player is restored"
    );
    assert_eq!(
        demo.spine.world().adversary(0).health.current,
        100.0,
        "the adversary is restored"
    );
    assert_eq!(demo.encounter.fires, 0, "the counters are restored");
    assert_eq!(demo.tick_index(), 0, "the logical clock restarts");
    run_winnable_script(&mut demo);
    assert!(
        matches!(demo.encounter.phase, EncounterPhase::Won { .. }),
        "the second round is winnable again"
    );
    assert_eq!(
        first_trace,
        {
            // A fresh demo's scripted trace (the reset rebuilds the
            // driver at the spawn, so the traces must agree).
            let mut fresh = CombatDemo::new();
            run_winnable_script(&mut fresh);
            fresh.driver.trace_fingerprint_hex()
        },
        "the reset reproduces the original trace bit-for-bit"
    );
}

/// The visible feedback: the render census and the state-dependent
/// colors (the adversary by B's live state and the hit-flash, the
/// pad by phase, the player proxy in third person).
#[test]
fn arena_render_feedback_pinned() {
    // Fresh (B's adversary patrols — the non-threatening gray), first
    // person: 12 arena boxes + the adversary body.
    let demo = CombatDemo::new();
    let boxes = demo.render_boxes();
    assert_eq!(boxes.len(), 13, "12 arena boxes + the adversary body");
    let adversary = boxes.last().expect("the adversary body");
    assert_eq!(adversary.2, [0.50, 0.50, 0.55], "the patrolling gray");

    // The activation recolors the pad (read through the box list).
    let mut demo = CombatDemo::new();
    let adversary_x = demo.spine.world().adversary(0).position[0];
    let state = demo.driver.state_mut();
    state.position = [adversary_x, 0.0, ACTIVATION_Z];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    demo.tick(engine_core::MoveIntent::default(), false);
    assert_eq!(demo.encounter.phase, EncounterPhase::Active);
    let boxes = demo.render_boxes();
    let pad = boxes
        .iter()
        .find(|(_, max, _)| *max == [1.5, 0.05, 2.2])
        .expect("the activation pad");
    assert_eq!(pad.2, [0.20, 0.80, 0.40], "the pad recolors on activation");

    // The hit flash: one landed fire turns the body white.
    demo.tick(engine_core::MoveIntent::default(), true);
    let boxes = demo.render_boxes();
    let adversary = boxes.last().expect("the adversary body");
    assert_eq!(adversary.2, [0.95, 0.95, 0.95], "the hit-flash white");

    // The flash decays back to the threatening red once B's machine
    // aggros (the landed hit provoked it), after the flash window.
    for _ in 0..12 {
        demo.tick(engine_core::MoveIntent::default(), false);
    }
    let boxes = demo.render_boxes();
    let adversary = boxes.last().expect("the adversary body");
    assert_eq!(
        adversary.2,
        [0.75, 0.25, 0.20],
        "the flash decays to the threatening red (provoked AGGRO)"
    );

    // Defeated: the dark body; third person adds the player proxy.
    let mut demo = CombatDemo::new();
    run_winnable_script(&mut demo);
    demo.driver.state_mut().toggle_camera_mode();
    let boxes = demo.render_boxes();
    assert_eq!(boxes.len(), 14, "13 + the third-person proxy");
    let adversary = boxes[12];
    assert_eq!(adversary.2, [0.20, 0.20, 0.25], "the defeated dark");
    let proxy = boxes.last().expect("the player proxy");
    assert_eq!(proxy.2, [0.20, 0.80, 0.35]);
}

/// The CONNECTED live-frame route — a SINGLE CombatDemo instance
/// walked through the winnable encounter entirely via
/// `advance_frame()` (the actual shared progression code
/// `run_combat_demo()` uses), with genuine player movement (no state
/// teleports): settle, the walk to the activation line, the fire
/// exchange on cooldown, and the win — with real timestamps, no
/// refused steps, and a loud bounded failure if any phase stalls.
#[test]
fn connected_live_frame_route_wins_the_encounter() {
    let mut demo = CombatDemo::new();
    let frame = std::time::Duration::from_nanos(16_666_666); // 1/60 s → one tick
    let idle = engine_core::MoveIntent::default();
    let forward = engine_core::MoveIntent { forward: 1, ..idle };

    fn frames_until(
        demo: &mut CombatDemo,
        frame: std::time::Duration,
        intent: engine_core::MoveIntent,
        fire: bool,
        condition: impl Fn(&CombatDemo) -> bool,
        label: &str,
    ) {
        for _ in 0..3_000 {
            if condition(demo) {
                return;
            }
            demo.advance_frame(frame, intent, fire);
        }
        panic!("the connected live-frame route stalled at '{label}'");
    }

    // 1. Settle to the arena floor.
    frames_until(
        &mut demo,
        frame,
        idle,
        false,
        |d| d.driver.state().on_ground,
        "settle",
    );

    // 2. Walk forward across the activation line (genuine movement —
    //    the player starts at z = 6 and the line sits at z = 2).
    frames_until(
        &mut demo,
        frame,
        forward,
        false,
        |d| d.encounter.phase == EncounterPhase::Active,
        "activate the encounter",
    );
    assert!(
        demo.driver.state().position[2] < 6.0 && demo.driver.state().position[2] <= ACTIVATION_Z,
        "the activation came from real movement: z = {}",
        demo.driver.state().position[2]
    );

    // 3. Hold position and fire on cooldown through the frame path
    //    (the fire edge on single frames, exactly as the interactive
    //    loop maps the mouse press — only when the adversary crosses
    //    the dead-ahead aim line, the zero-angle discipline).
    for _ in 0..3_000 {
        if demo.encounter.phase != EncounterPhase::Active {
            break;
        }
        let on_line = demo.spine.world().adversary(0).position[0].abs() <= 0.4;
        let fire = demo.weapon_ready() && on_line;
        demo.advance_frame(frame, idle, fire);
    }
    assert!(
        matches!(demo.encounter.phase, EncounterPhase::Won { .. }),
        "the connected route wins the encounter"
    );

    // 4. The invariants: real advancing timestamps, no refused
    //    steps, monotone event order, the fire ladder intact.
    assert!(
        demo.tick_index() > 0,
        "the logical clock advanced: {}",
        demo.tick_index()
    );
    assert_eq!(
        demo.driver.refused_steps(),
        0,
        "no refused movement steps on the connected route"
    );
    let events = &demo.encounter.events;
    let mut last_tick = 0;
    for (tick, _) in events {
        assert!(*tick >= last_tick, "the events are timestamped in order");
        last_tick = *tick;
    }
    assert_eq!(demo.encounter.hits, 4, "four hits through the frame path");
    assert!(
        demo.encounter.attacks_taken >= 1,
        "the adversary threatened through the frame path"
    );
    assert!(demo.encounter.player.current > 0.0, "the player survives");
    let (_, won_line) = events.last().expect("the WON event");
    assert!(
        won_line.contains("ADVERSARY DEFEATED — encounter WON"),
        "the completion line: {won_line}"
    );
}

/// The review-F2 pacing regression: the combat transition advances
/// PER EXECUTED LOGICAL TICK, decoupled from the frame cadence —
/// (a) three 2-tick frames and six 1-tick frames reach BIT-IDENTICAL
/// encounter state (the same logical ticks, the same cooldown and
/// adversary state, whatever the frame rate); (b) a fire press on a
/// 0-tick frame (frames outpacing the 60 Hz tick rate) is BUFFERED
/// to the next executed tick — input never dropped, exactly one fire
/// at the correct logical index; (c) a fire edge on a multi-tick
/// frame lands ONCE, on the first executed tick.
#[test]
fn combat_pacing_advances_per_executed_tick_not_per_frame() {
    let idle = engine_core::MoveIntent::default();
    let tick_frame = std::time::Duration::from_nanos(16_666_666); // 1/60 s → 1 tick
    let two_tick_frame = std::time::Duration::from_nanos(33_333_333); // 2/60 s → 2 ticks
    let zero_tick_frame = std::time::Duration::from_nanos(8_000_000); // 8 ms → 0 ticks at 60 Hz

    // (a) The cadence equivalence: 3×2-tick frames == 6×1-tick
    //     frames. B's spine advances on MY transition calls — the
    //     same six run_ticks in the same order reach the identical
    //     adversary state, whatever the frame cadence.
    let mut coarse = CombatDemo::new();
    let mut fine = CombatDemo::new();
    for demo in [&mut coarse, &mut fine] {
        let state = demo.driver.state_mut();
        state.position = [0.0, 0.0, 1.5];
        state.velocity = [0.0, 0.0, 0.0];
        state.on_ground = true;
        demo.tick(idle, false);
    }
    for _ in 0..3 {
        coarse.advance_frame(two_tick_frame, idle, false);
    }
    for _ in 0..6 {
        fine.advance_frame(tick_frame, idle, false);
    }
    assert_eq!(
        coarse.tick_index(),
        7,
        "3×2-tick frames execute 6 ticks (+1 activation tick)"
    );
    assert_eq!(
        fine.tick_index(),
        7,
        "6×1-tick frames execute 6 ticks (+1 activation tick)"
    );
    assert_eq!(
        coarse.spine.world().adversary(0).position.map(f32::to_bits),
        fine.spine.world().adversary(0).position.map(f32::to_bits),
        "the adversary advances per logical tick — bit-identical across cadences"
    );
    assert_eq!(
        coarse.encounter.events, fine.encounter.events,
        "the event log (timestamps included) is cadence-independent"
    );

    // (b) The buffered press: fire on 0-tick frames until a tick
    //     executes — exactly ONE fire lands, at the correct index.
    let mut demo = CombatDemo::new();
    let state = demo.driver.state_mut();
    state.position = [0.0, 0.0, 1.5];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    demo.tick(idle, false); // tick 1: the activation
    // 8 ms frames: no tick executes on any single frame — the press
    // buffers; two consecutive frames accumulate 16 ms < 16.67 ms
    // (still 0 ticks); the cadence eventually reaches the boundary.
    demo.advance_frame(zero_tick_frame, idle, true);
    assert_eq!(demo.encounter.fires, 0, "a 0-tick frame runs no transition");
    demo.advance_frame(zero_tick_frame, idle, false);
    assert_eq!(
        demo.encounter.fires, 0,
        "the buffered press waits for a tick"
    );
    let mut executed = 0;
    for _ in 0..10 {
        let step = demo.advance_frame(zero_tick_frame, idle, false);
        executed += u64::from(step.executed_ticks);
        if executed > 0 {
            break;
        }
    }
    assert!(
        executed >= 1,
        "an 8 ms frame cadence eventually executes a tick"
    );
    assert_eq!(
        demo.encounter.fires, 1,
        "the buffered press resolves exactly once on the executed tick"
    );
    let (fire_tick, _) = demo
        .encounter
        .events
        .iter()
        .find(|(_, line)| line.starts_with("fire "))
        .expect("the fire event");
    assert_eq!(
        *fire_tick,
        demo.tick_index(),
        "the fire logs against the tick index that executed it (no repeated index)"
    );

    // (c) A fire edge on a 2-tick frame lands ONCE (the first
    //     executed tick).
    let mut demo = CombatDemo::new();
    let state = demo.driver.state_mut();
    state.position = [0.0, 0.0, 1.5];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    demo.tick(idle, false);
    demo.advance_frame(two_tick_frame, idle, true);
    assert_eq!(
        demo.encounter.fires, 1,
        "one press, one fire — even on a multi-tick frame"
    );
    assert_eq!(demo.tick_index(), 3, "the frame executed both ticks");
    let (fire_tick, _) = demo
        .encounter
        .events
        .iter()
        .find(|(_, line)| line.starts_with("fire "))
        .expect("the fire event");
    assert_eq!(*fire_tick, 2, "the edge lands on the FIRST executed tick");
}

/// The interactive input mapping keeps the corrected mouse-look
/// behavior (the playable mapping is reused verbatim — only the fire
/// edge is added), so the combat camera turns exactly as the
/// playable camera does.
#[test]
fn combat_mapping_preserves_mouse_look() {
    use engine_core::player::look_direction;

    // Mouse-RIGHT through map_combat_input + advance_frame turns the
    // look direction toward world +X (the H2 hardening, unchanged).
    let snapshot = InputSnapshot {
        look_delta: (100.0, 0.0),
        ..InputSnapshot::default()
    };
    let mapped = map_combat_input(&snapshot, false);
    let mut demo = CombatDemo::new();
    for _ in 0..3 {
        demo.advance_frame(
            std::time::Duration::from_nanos(16_666_666),
            mapped.intent,
            false,
        );
    }
    let state = demo.driver.state();
    let look = look_direction(state.yaw, state.pitch);
    assert!(
        look[0] > 0.2,
        "mouse-right turns the combat camera toward world +X: {look:?}"
    );

    // The same snapshot carries no fire edge without a mouse press.
    assert!(!mapped.fire_pressed);
}

/// The C3-R1 ordering regression (fail-first at the defective
/// candidate `3a31627`): TWO single-tick frames with held movement
/// must agree with ONE double-tick frame carrying the same intent —
/// the combat transition observes EACH logical tick's own resulting
/// player state, never the frame's final state. The staged boundary:
/// the player starts at z = 2.1 walking −Z at full speed — one tick
/// lands at 2.0333 (NOT past the activation line), the second at
/// 1.9667 (past it): the activation must flip on the SECOND logical
/// tick, and everything downstream (the event log, the adversary's
/// per-tick samples, the final state) must match the single-tick
/// cadence exactly.
#[test]
fn ordering_two_single_tick_frames_match_one_double_tick_frame() {
    let forward = engine_core::MoveIntent {
        forward: 1,
        ..engine_core::MoveIntent::default()
    };
    let tick_frame = std::time::Duration::from_nanos(16_666_666); // 1/60 s → 1 tick
    let double_frame = std::time::Duration::from_nanos(33_333_333); // 2/60 s → 2 ticks

    fn staged_demo() -> CombatDemo {
        let mut demo = CombatDemo::new();
        let state = demo.driver.state_mut();
        state.position = [0.0, 0.0, 2.1];
        state.velocity = [0.0, 0.0, -4.0]; // At full walk speed: 0.0667 m/tick.
        state.on_ground = true;
        demo
    }

    // Two single-tick frames: the transition of tick 1 sees z ≈
    // 2.0333 (no activation), tick 2 sees z ≈ 1.9667 (activation).
    let mut fine = staged_demo();
    fine.advance_frame(tick_frame, forward, false);
    assert_eq!(
        fine.encounter.phase,
        EncounterPhase::Dormant,
        "tick 1 does not cross the line (z = {:.4})",
        fine.driver.state().position[2]
    );
    fine.advance_frame(tick_frame, forward, false);
    assert_eq!(
        fine.encounter.phase,
        EncounterPhase::Active,
        "tick 2 crosses the line (z = {:.4})",
        fine.driver.state().position[2]
    );

    // One double-tick frame: the SAME logical ticks, the SAME
    // result — the activation still on the SECOND tick, the event
    // log identical, the adversary state identical.
    let mut coarse = staged_demo();
    coarse.advance_frame(double_frame, forward, false);
    assert_eq!(
        coarse.tick_index(),
        fine.tick_index(),
        "the same logical ticks"
    );
    assert_eq!(
        coarse.encounter.phase,
        EncounterPhase::Active,
        "the double-tick frame activates — on its SECOND tick, not the first"
    );
    let fine_activation = fine
        .encounter
        .events
        .iter()
        .find(|(_, line)| line.starts_with("encounter activated"))
        .expect("the fine activation");
    let coarse_activation = coarse
        .encounter
        .events
        .iter()
        .find(|(_, line)| line.starts_with("encounter activated"))
        .expect("the coarse activation");
    assert_eq!(
        fine_activation.0, coarse_activation.0,
        "the activation lands on the same logical tick in both cadences"
    );
    assert_eq!(fine_activation.0, 2, "the crossing is on the SECOND tick");
    assert_eq!(
        coarse.encounter.events, fine.encounter.events,
        "the event log (timestamps included) is cadence-independent"
    );
    assert_eq!(
        coarse.spine.world().adversary(0).position.map(f32::to_bits),
        fine.spine.world().adversary(0).position.map(f32::to_bits),
        "the adversary saw each tick's own player sample"
    );
    assert_eq!(
        coarse.driver.state().position,
        fine.driver.state().position,
        "the player's final state agrees"
    );
}

/// The C3-R1 ordering regression, the pursuit/attack outcome: a
/// MOVING player during multi-tick frames — the adversary's
/// perception samples and pursuit path must observe each tick's own
/// position (the single-tick cadence), producing the same combat
/// outcome (the same attack events at the same logical ticks).
/// At the defective candidate, the double-tick frame's early
/// transitions sample the frame-final position — the pursuit chases
/// a jumped target and the combat outcome (the attack timing/log)
/// diverges.
#[test]
fn ordering_pursuit_samples_each_tick_during_multi_tick_frames() {
    let idle = engine_core::MoveIntent::default();
    let strafe_left = engine_core::MoveIntent { strafe: -1, ..idle };
    let tick_frame = std::time::Duration::from_nanos(16_666_666); // 1/60 s → 1 tick
    let double_frame = std::time::Duration::from_nanos(33_333_333); // 2/60 s → 2 ticks

    // The staged start: the player inside the adversary's aggro
    // ground, the adversary woken to AGGRO (the same deterministic
    // setup in both cadences).
    fn staged_demo() -> CombatDemo {
        let mut demo = CombatDemo::new();
        let state = demo.driver.state_mut();
        state.position = [0.0, 0.0, 1.0];
        state.velocity = [0.0, 0.0, 0.0];
        state.on_ground = true;
        for _ in 0..200 {
            if matches!(
                demo.spine.world().adversary(0).state,
                engine_core::adversary::AdversaryState::Aggro
                    | engine_core::adversary::AdversaryState::Attack
                    | engine_core::adversary::AdversaryState::Recover
            ) {
                break;
            }
            demo.tick(idle_like(), false);
        }
        demo
    }
    // A local idle (the closure above cannot capture the outer idle
    // before its declaration).
    fn idle_like() -> engine_core::MoveIntent {
        engine_core::MoveIntent::default()
    }

    // The fine cadence: seven single-tick frames of strafing left,
    // then idle until an attack lands.
    let mut fine = staged_demo();
    for _ in 0..7 {
        fine.advance_frame(tick_frame, strafe_left, false);
    }
    for _ in 0..600 {
        if fine
            .encounter
            .events
            .iter()
            .any(|(_, line)| line.starts_with("adversary attack — player takes"))
        {
            break;
        }
        fine.tick(idle, false);
    }

    // The coarse cadence: ONE single-tick frame (aligning the frame
    // parity so the adversary's wake-ups land on the double frames'
    // FIRST ticks — where the premature final-position sample
    // actually diverges) then three double-tick frames — the SAME
    // seven logical ticks, the same held strafe — then the same idle.
    let mut coarse = staged_demo();
    coarse.advance_frame(tick_frame, strafe_left, false);
    for _ in 0..3 {
        coarse.advance_frame(double_frame, strafe_left, false);
    }
    for _ in 0..600 {
        if coarse
            .encounter
            .events
            .iter()
            .any(|(_, line)| line.starts_with("adversary attack — player takes"))
        {
            break;
        }
        coarse.tick(idle, false);
    }

    // The outcomes must agree: the attack landed in both, at the
    // same logical tick, with the same event log and the same
    // adversary state.
    let fine_attack = fine
        .encounter
        .events
        .iter()
        .find(|(_, line)| line.starts_with("adversary attack — player takes"))
        .map(|(tick, _)| *tick);
    let coarse_attack = coarse
        .encounter
        .events
        .iter()
        .find(|(_, line)| line.starts_with("adversary attack — player takes"))
        .map(|(tick, _)| *tick);
    assert!(
        fine_attack.is_some(),
        "the adversary catches the strafing player (the fine cadence)"
    );
    assert_eq!(
        coarse_attack, fine_attack,
        "the attack lands at the same logical tick in both cadences"
    );
    assert_eq!(
        coarse.encounter.events, fine.encounter.events,
        "the combat outcome (the event log) is cadence-independent"
    );
    assert_eq!(
        coarse.spine.world().adversary(0).position.map(f32::to_bits),
        fine.spine.world().adversary(0).position.map(f32::to_bits),
        "the pursuit observed each tick's own sample"
    );
}
