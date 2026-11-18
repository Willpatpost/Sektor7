//! Cycle 3 P6 integration battery — the combat encounter's
//! application-level guards. All headless: no SDL initialization, no
//! GPU, no process spawns — the deterministic scenarios, the mapping,
//! and the demo's public surfaces (the same discipline as the
//! Cycle-2 playable battery).

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

    // The spawn geometry: the adversary dead-ahead of the player
    // (both on the centerline), the activation line between them.
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

/// The full WINNABLE scenario, headless and deterministic: the event
/// sequence in order, activation, the fire ladder, the one attack
/// taken, the defeat, and the completion — byte-identical across
/// invocations.
#[test]
fn winnable_scenario_completes_headless() {
    let first = run_combat_headless();
    let second = run_combat_headless();
    assert_eq!(first, second, "the scenario double-runs identically");

    let expected_sequence = [
        "settled on the arena ground",
        "encounter activated — the adversary wakes",
        "fire 1: HIT — adversary 75/100",
        "fire 2: HIT — adversary 50/100",
        "fire 3: HIT — adversary 25/100",
        "adversary attack — player takes 20",
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
        first.contains("attacks taken 1"),
        "the adversary meaningfully threatened the player (one landed attack)"
    );
    assert!(first.contains("fires 4"), "exactly four fires");
    assert!(first.contains("refused=0"), "no refused movement steps");
    assert!(
        first.contains("player: hp 80/100"),
        "the player survives wounded: {}",
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
        "encounter activated — the adversary wakes",
        "adversary attack — player takes 20 (80/100)",
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
        first.contains("adversary: ACTIVE · hp 100/100"),
        "the adversary survives the failure"
    );
}

/// The scenarios' player traces, pinned (the libm-free class: the
/// scripted courses never look, so yaw and pitch stay exactly zero —
/// frozen constants, cross-platform; measured at the scaffold
/// delivery and pinned here).
#[test]
fn combat_scenario_trace_pinned() {
    let winnable = run_combat_headless();
    let trace = winnable
        .lines()
        .find_map(|line| line.strip_prefix("player-trace="))
        .expect("the winnable summary carries the trace fingerprint");
    assert_eq!(
        trace, "d2ef688081dbc659",
        "the winnable scenario trace (a divergence is an investigation, \
         never a silent re-pin)"
    );

    let failure = run_combat_failure_headless();
    let trace = failure
        .lines()
        .find_map(|line| line.strip_prefix("player-trace="))
        .expect("the failure summary carries the trace fingerprint");
    assert_eq!(
        trace, "494ade50b21d43cc",
        "the failure scenario trace (a divergence is an investigation, \
         never a silent re-pin)"
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

    // Demo level: teleport past the activation line, activate, and
    // press once (the adversary is dead-ahead — the shot hits).
    let mut demo = CombatDemo::new();
    let state = demo.driver.state_mut();
    state.position = [0.0, 0.0, 1.5];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    demo.tick(engine_core::MoveIntent::default(), false);
    assert_eq!(
        demo.encounter.phase,
        EncounterPhase::Active,
        "crossing the line activates the encounter"
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
#[test]
fn fire_refused_out_of_range_and_occluded() {
    // Out of range: the shrunken-range config against the dead-ahead
    // spawn geometry (the entry distance ~5.55 m > the 5.0 m range).
    let mut config = combat_config();
    config.weapon_range = 5.0;
    let mut demo = CombatDemo::new_with_config(config);
    let state = demo.driver.state_mut();
    state.position = [0.0, 0.0, 2.0];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    demo.tick(engine_core::MoveIntent::default(), false);
    demo.tick(engine_core::MoveIntent::default(), true);
    assert_eq!(demo.encounter.out_of_range, 1, "the far shot is refused");
    let last = demo.encounter.events.last().expect("an event").1.clone();
    assert!(
        last.starts_with("fire 1: OUT OF RANGE") && last.contains("m > 5 m"),
        "the refusal names the distance: {last}"
    );

    // The pillar occlusion: the adversary behind the (3.5, −1.0)
    // pillar, the player dead-ahead of it on the same line — the
    // exact-slab query blocks the eye→entry segment. A blocked shot
    // does no damage, so the DORMANT adversary stays dormant (the
    // provocation rule requires damage).
    let mut demo = CombatDemo::new();
    demo.encounter.adversary.position = [3.5, 0.0, -4.0];
    let state = demo.driver.state_mut();
    state.position = [3.5, 0.0, 3.5];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    demo.tick(engine_core::MoveIntent::default(), false);
    demo.tick(engine_core::MoveIntent::default(), true);
    assert_eq!(demo.encounter.blocked, 1, "the covered shot is blocked");
    let last = demo.encounter.events.last().expect("an event").1.clone();
    assert!(
        last.starts_with("fire 1: BLOCKED"),
        "the occlusion refusal is logged: {last}"
    );
    assert_eq!(
        demo.encounter.phase,
        EncounterPhase::Dormant,
        "a blocked shot does not provoke"
    );

    // The low wall never blocks the standing shot: the dead-ahead
    // line over the left low wall (its top 1.1 m is below the eye);
    // the player stands at z = 4.2, clear of the (−2.5, 3.0) pillar's
    // footprint. The landed hit damages the DORMANT adversary WITHOUT
    // waking it — SEAT B's wired DORMANT rule: activation is the
    // encounter line's authority, not damage.
    let mut demo = CombatDemo::new();
    demo.encounter.adversary.position = [-3.0, 0.0, -4.0];
    let state = demo.driver.state_mut();
    state.position = [-3.0, 0.0, 4.2];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    demo.tick(engine_core::MoveIntent::default(), false);
    demo.tick(engine_core::MoveIntent::default(), true);
    assert_eq!(demo.encounter.hits, 1, "the shot passes over the low wall");
    assert_eq!(demo.encounter.blocked, 0, "the low wall does not block");
    assert_eq!(
        demo.encounter.adversary.health.current, 75.0,
        "the contract's Health transition applies the damage"
    );
    assert_eq!(
        demo.encounter.phase,
        EncounterPhase::Dormant,
        "damage does not wake the DORMANT adversary (B's wired rule: \
         the activation line is the authority)"
    );
    assert!(
        !demo.encounter.adversary.active,
        "the adversary itself stays dormant"
    );
}

/// The adversary's threat contract: the melee attack is range-gated
/// and line-of-sight-gated (cover protects the player), and the
/// cooldown spaces the attacks.
#[test]
fn adversary_attack_respects_range_and_los() {
    // In range but occluded: the adversary adjacent to the pillar
    // with the player on its far side — the attack is refused.
    let mut demo = CombatDemo::new();
    demo.encounter.adversary.activate();
    demo.encounter.phase = EncounterPhase::Active;
    demo.encounter.adversary.position = [3.1, 0.0, -0.4];
    let state = demo.driver.state_mut();
    state.position = [4.6, 0.0, -1.4];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    // The pillar (x ∈ [3.1, 3.9], z ∈ [−1.4, −0.6]) sits between the
    // adversary eye (3.1, 1.6, −0.4) and the player eye (4.6, 1.6, −1.4).
    for _ in 0..30 {
        demo.tick(engine_core::MoveIntent::default(), false);
    }
    assert!(
        demo.encounter
            .events
            .iter()
            .any(|(_, line)| line.contains("adversary attack BLOCKED")),
        "the occluded attack is refused with feedback"
    );
    assert_eq!(demo.encounter.attacks_taken, 0, "no damage through cover");
    assert!(
        demo.encounter.player.current > 99.0,
        "the player is unhurt behind cover"
    );

    // The clear line: the same adjacency without the pillar between
    // lands cooldown-spaced attacks (the pursuit closes the gap, so
    // the exact tick spacing is behavior, not a pinned constant).
    let mut demo = CombatDemo::new();
    demo.encounter.adversary.activate();
    demo.encounter.phase = EncounterPhase::Active;
    demo.encounter.adversary.position = [0.0, 0.0, -3.0];
    let state = demo.driver.state_mut();
    state.position = [0.0, 0.0, -1.5];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    for _ in 0..30 {
        demo.tick(engine_core::MoveIntent::default(), false);
    }
    assert!(
        demo.encounter.attacks_taken >= 1,
        "the clear-line attack lands"
    );
    assert!(
        demo.encounter.player.current < 100.0,
        "the player takes damage"
    );
    let attack_ticks: Vec<u64> = demo
        .encounter
        .events
        .iter()
        .filter(|(_, line)| line.starts_with("adversary attack —"))
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
        let state = demo.driver.state_mut();
        state.position = [0.0, 0.0, 2.0];
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
            demo.encounter.adversary.health.current,
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
        demo.encounter.adversary.health.current, 100.0,
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
/// colors (the adversary by phase and hit-flash, the pad by phase,
/// the player proxy in third person).
#[test]
fn arena_render_feedback_pinned() {
    // Dormant, first person: 12 arena boxes + the gray adversary.
    let demo = CombatDemo::new();
    let boxes = demo.render_boxes();
    assert_eq!(boxes.len(), 13, "12 arena boxes + the adversary body");
    let adversary = boxes.last().expect("the adversary body");
    assert_eq!(adversary.2, [0.50, 0.50, 0.55], "the dormant gray");

    // Active: the adversary turns red; the pad turns green (the
    // activation recolor, read through the box list).
    let mut demo = CombatDemo::new();
    let state = demo.driver.state_mut();
    state.position = [0.0, 0.0, ACTIVATION_Z];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    demo.tick(engine_core::MoveIntent::default(), false);
    assert_eq!(demo.encounter.phase, EncounterPhase::Active);
    let boxes = demo.render_boxes();
    let adversary = boxes.last().expect("the adversary body");
    assert_eq!(adversary.2, [0.75, 0.25, 0.20], "the active red");
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

    // The flash decays back to the active red after the window.
    for _ in 0..7 {
        demo.tick(engine_core::MoveIntent::default(), false);
    }
    let boxes = demo.render_boxes();
    let adversary = boxes.last().expect("the adversary body");
    assert_eq!(adversary.2, [0.75, 0.25, 0.20], "the flash decays");

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
    //    loop maps the mouse press).
    for _ in 0..3_000 {
        if demo.encounter.phase != EncounterPhase::Active {
            break;
        }
        let fire = demo.weapon_ready();
        demo.advance_frame(frame, idle, fire);
    }
    assert_eq!(
        demo.encounter.phase,
        EncounterPhase::Won {
            tick: demo.tick_index()
        },
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
