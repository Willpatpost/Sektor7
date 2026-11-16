//! Cycle 2 C2-P1 integration battery — the playable-world loop's
//! application-level guards. All headless: no SDL initialization, no
//! GPU, no process spawns — the deterministic scenario, the mapping,
//! and the demo's public surfaces (the same discipline as the
//! Cycle-1 batteries).

use engine_core::StaticCollider;
use engine_shell::{InputSnapshot, Keycode};
use engine_slice::playable_demo::{
    GATEHOUSE_BOXES, GOAL_CENTER, GatehouseRole, PICKUP_IDS, PICKUP_POSITIONS, PlayableDemo,
    SWITCH_ID, SWITCH_POSITION, gatehouse_interactables, gatehouse_world, map_playable_input,
    run_playable_headless,
};

/// The layout literal's invariants (the F2 rule: an independent
/// expectation, never derived from the builder it checks — the
/// critical extents, the staircase's climbable monotone rises, and
/// the role census are hand-written here).
#[test]
fn gatehouse_layout_pinned() {
    assert_eq!(GATEHOUSE_BOXES.len(), 21, "the layout census");
    let solid = GATEHOUSE_BOXES
        .iter()
        .filter(|class_box| class_box.role.is_solid())
        .count();
    assert_eq!(solid, 16, "solid boxes (the gate counts while closed)");
    let roles = |role: GatehouseRole| {
        GATEHOUSE_BOXES
            .iter()
            .filter(|class_box| class_box.role == role)
            .count()
    };
    assert_eq!(roles(GatehouseRole::Perimeter), 4);
    assert_eq!(roles(GatehouseRole::Step), 3);
    assert_eq!(roles(GatehouseRole::Platform), 1);
    assert_eq!(roles(GatehouseRole::SwitchPillar), 1);
    assert_eq!(roles(GatehouseRole::SwitchTop), 1);
    assert_eq!(roles(GatehouseRole::GateWall), 2);
    assert_eq!(roles(GatehouseRole::Gate), 1);
    assert_eq!(roles(GatehouseRole::Corridor), 2);
    assert_eq!(roles(GatehouseRole::Lintel), 1);
    assert_eq!(roles(GatehouseRole::Occluder), 1);
    assert_eq!(roles(GatehouseRole::Pickup), 3);
    assert_eq!(roles(GatehouseRole::GoalPad), 1);

    // The staircase: three steps rising 0.4 each (climbable without
    // jumping at the 0.55 step height), monotone toward the platform.
    let step_tops: Vec<f32> = GATEHOUSE_BOXES
        .iter()
        .filter(|class_box| class_box.role == GatehouseRole::Step)
        .map(|class_box| class_box.max[1])
        .collect();
    assert_eq!(step_tops, [0.4, 0.8, 1.2]);
    let platform = GATEHOUSE_BOXES
        .iter()
        .find(|class_box| class_box.role == GatehouseRole::Platform)
        .expect("the platform");
    assert_eq!(platform.max[1], 1.2, "the platform meets the last step");

    // The critical extents, exactly.
    let gate = GATEHOUSE_BOXES
        .iter()
        .find(|class_box| class_box.role == GatehouseRole::Gate)
        .expect("the gate");
    assert_eq!(gate.min, [-1.5, 0.0, -10.0]);
    assert_eq!(gate.max, [1.5, 3.0, -9.5]);
    let occluder = GATEHOUSE_BOXES
        .iter()
        .find(|class_box| class_box.role == GatehouseRole::Occluder)
        .expect("the occluder");
    assert_eq!(occluder.min, [0.2, 0.0, -17.5]);
    assert_eq!(occluder.max, [0.5, 3.0, -17.2]);
    let goal = GATEHOUSE_BOXES
        .iter()
        .find(|class_box| class_box.role == GatehouseRole::GoalPad)
        .expect("the goal pad");
    assert_eq!(goal.max[0] - goal.min[0], 1.6);
    assert_eq!((goal.min[0] + goal.max[0]) / 2.0, GOAL_CENTER[0]);
    assert_eq!((goal.min[2] + goal.max[2]) / 2.0, GOAL_CENTER[1]);

    // The interactables mirror the layout constants: the switch plus
    // the uncollected pickups, at the declared points.
    let targets = gatehouse_interactables(&[false, false, false]);
    assert_eq!(targets.len(), 4);
    assert_eq!(targets[0].id, SWITCH_ID);
    assert_eq!(targets[0].position, SWITCH_POSITION);
    for (index, target) in targets[1..].iter().enumerate() {
        assert_eq!(target.id, PICKUP_IDS[index]);
        assert_eq!(target.position, PICKUP_POSITIONS[index]);
    }
    assert_eq!(gatehouse_interactables(&[true, true, true]).len(), 1);
}

/// The full scripted scenario, headless and deterministic: the event
/// sequence in order, the occlusion flip, the pulled-in camera
/// sample, and completion — byte-identical across invocations.
#[test]
fn scripted_scenario_completes_headless() {
    let first = run_playable_headless();
    let second = run_playable_headless();
    assert_eq!(first, second, "the scenario double-runs identically");

    let expected_sequence = [
        "settled on the courtyard ground",
        "switch toggled — the gate opens",
        "camera mode: third person",
        "pickup 1 collected (1/3)",
        "target 3 occluded — line of sight blocked",
        "target 3 line of sight clear",
        "pickup 2 collected (2/3)",
        "pickup 3 collected (3/3)",
        "GOAL — scenario complete",
    ];
    let mut last = 0;
    for line in expected_sequence {
        let at = first
            .find(line)
            .unwrap_or_else(|| panic!("the scenario must log: {line}"));
        assert!(at >= last, "the events happen in order: {line}");
        last = at;
    }
    assert!(first.contains("goal: COMPLETE"), "completion is reported");
    assert!(first.contains("pickups 3/3"), "all pickups collected");
    assert!(
        first.contains("(PULLED IN)"),
        "the corridor camera sample shows the obstruction pull-in"
    );
    assert!(first.contains("refused 0"), "no refused steps");
}

/// The scenario's player trace, pinned (the libm-free class: the
/// scripted course never looks, so yaw and pitch stay exactly zero —
/// one frozen constant, cross-platform).
#[test]
fn scenario_trace_pinned() {
    let summary = run_playable_headless();
    let trace = summary
        .lines()
        .find_map(|line| line.strip_prefix("player-trace="))
        .expect("the summary carries the trace fingerprint");
    assert_eq!(
        trace, "8f6b490648326d26",
        "the gatehouse scenario trace (a divergence is an investigation, \
         never a silent re-pin)"
    );
}

/// The E edge: one press, one interaction — at the mapping level (a
/// held key produces no fresh press) and at the demo level (a single
/// interact tick fires once; further ticks without the edge do
/// nothing).
#[test]
fn interaction_events_fire_once_per_press() {
    // Mapping level: the press frame maps interact; the held frame
    // (no keys_pressed entry — the shell only reports edges) does not.
    let mut press = InputSnapshot::default();
    press.keys_pressed = vec![Keycode::E];
    assert!(map_playable_input(&press, false).interact_pressed);
    let held = InputSnapshot::default();
    assert!(!map_playable_input(&held, false).interact_pressed);

    // Demo level: teleport to the switch approach, press once.
    let mut demo = PlayableDemo::new();
    let state = demo.driver.state_mut();
    state.position = [0.0, 1.2 + engine_core::player::SKIN, -4.4];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    demo.tick(engine_core::MoveIntent::default(), true);
    assert_eq!(
        demo.gatehouse.events.len(),
        1,
        "one press, one switch event"
    );
    assert!(demo.gatehouse.switch_on, "the gate opened");
    for _ in 0..5 {
        demo.tick(engine_core::MoveIntent::default(), false);
    }
    assert_eq!(
        demo.gatehouse.events.len(),
        1,
        "no further presses, no further events"
    );
}

/// No target offered when out of range, and none when the in-cone
/// target's line of sight is blocked (the exact-slab filter) — the
/// corridor centerline, where the occluder pillar hides pickup 2.
#[test]
fn no_target_when_occluded_or_out_of_range() {
    // Out of range: at spawn, thirteen meters from the switch.
    let demo = PlayableDemo::new();
    assert!(
        demo.current_target().is_none(),
        "nothing offered at spawn range"
    );

    // The corridor centerline: the in-cone, in-range target is
    // pickup 2 (id 3), and its line of sight is blocked by the
    // occluder — the with-los probe shows the occlusion; the offered
    // target surface filters it out.
    let mut demo = PlayableDemo::new();
    let state = demo.driver.state_mut();
    state.position = [-0.5, 0.0, -16.0];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    let (id, _position, clear) = demo
        .current_target_with_los()
        .expect("pickup 2 is selected from the centerline");
    assert_eq!(id, 3, "the in-cone in-range target is pickup 2");
    assert!(!clear, "the occluder blocks the line of sight");
    assert!(
        demo.current_target().is_none(),
        "an occluded target is not offered"
    );
}

/// The gate world swap never teleports or resets the player: the
/// switch interaction swaps the collision world (the gate collider
/// leaves) with the player state bit-identical across the swap.
#[test]
fn gate_world_swap_preserves_player_state() {
    let mut demo = PlayableDemo::new();
    let state = demo.driver.state_mut();
    state.position = [0.0, 1.2 + engine_core::player::SKIN, -4.4];
    state.velocity = [0.5, 0.0, -1.0];
    state.yaw = 0.0;
    state.on_ground = true;
    let bits = |demo: &PlayableDemo| {
        let state = demo.driver.state();
        (
            state.position.map(f32::to_bits),
            state.velocity.map(f32::to_bits),
            state.yaw.to_bits(),
            state.pitch.to_bits(),
            state.on_ground,
        )
    };
    let before = bits(&demo);
    // The gate is closed: the corridor mouth is blocked.
    assert!(
        demo.driver
            .world()
            .segment_hit([0.0, 1.5, -9.0], [0.0, 1.5, -10.5], 0.0)
            .is_some(),
        "the gate blocks the corridor mouth"
    );
    demo.try_interact(0);
    assert!(demo.gatehouse.switch_on, "the switch toggled");
    assert!(demo.gatehouse.gate_open(), "the gate is open");
    assert_eq!(bits(&demo), before, "no teleport, no reset");
    assert!(
        demo.driver
            .world()
            .segment_hit([0.0, 1.5, -9.0], [0.0, 1.5, -10.5], 0.0)
            .is_none(),
        "the gate collider left the world"
    );
}

/// Collide-what-you-see: the collision world carries exactly the
/// solid layout boxes (the gate only while closed), and the render
/// boxes mirror the layout with the state-dependent visibility — the
/// gate vanishes when open, collected pickups vanish, and the player
/// proxy appears in third person.
#[test]
fn gatehouse_collide_what_you_see() {
    let expected_colliders = |gate_open: bool| -> Vec<StaticCollider> {
        GATEHOUSE_BOXES
            .iter()
            .filter(|class_box| class_box.role.is_solid())
            .filter(|class_box| !(class_box.role == GatehouseRole::Gate && gate_open))
            .map(|class_box| StaticCollider {
                min: class_box.min,
                max: class_box.max,
            })
            .collect()
    };
    for gate_open in [false, true] {
        let world = gatehouse_world(gate_open);
        let expected = expected_colliders(gate_open);
        assert_eq!(world.colliders().len(), expected.len());
        for (index, collider) in world.colliders().iter().enumerate() {
            assert_eq!(collider.min, expected[index].min, "collider {index}");
            assert_eq!(collider.max, expected[index].max, "collider {index}");
        }
    }
    assert_eq!(
        gatehouse_world(false).colliders().len(),
        gatehouse_world(true).colliders().len() + 1,
        "the open world is exactly one gate shorter"
    );

    // The render surface: the fresh demo draws everything (21 boxes,
    // first person — no proxy); after the switch and two pickups in
    // third person, the gate and those pickups vanish and the proxy
    // appears.
    let demo = PlayableDemo::new();
    assert_eq!(demo.render_boxes().len(), 21);
    let mut demo = PlayableDemo::new();
    demo.gatehouse.switch_on = true;
    demo.gatehouse.pickups = [true, true, false];
    demo.driver.state_mut().toggle_camera_mode();
    let boxes = demo.render_boxes();
    assert_eq!(
        boxes.len(),
        21 - 1 /* gate */ - 2 /* collected pickups */ + 1, /* the proxy */
        "state-dependent visibility"
    );
    assert!(
        boxes
            .iter()
            .all(|(min, max, _)| *min != [-1.5, 0.0, -10.0] || *max != [1.5, 3.0, -9.5]),
        "the open gate is not drawn"
    );
}
