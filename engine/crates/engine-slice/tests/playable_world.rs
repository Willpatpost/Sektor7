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
    let press = InputSnapshot {
        keys_pressed: vec![Keycode::E],
        ..InputSnapshot::default()
    };
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

/// The Cycle-2 R1 corrective: the INTERACTIVE progression path
/// (`advance_frame` — the exact shared gameplay-update mechanism the
/// live loop routes through) registers interactions at real logical
/// timestamps and goal-pad contact as completion, exactly as the
/// headless `tick` path does. Written fail-first against the defect
/// (the live loop calling `driver.advance` directly: event
/// timestamps frozen at zero, completion never registered).
#[test]
fn interactive_frame_progression_registers_interactions_and_completion() {
    let frame = std::time::Duration::from_nanos(16_666_666); // 1/60 s → one tick
    let idle = engine_core::MoveIntent::default();

    // The logical clock advances through live frames.
    let mut demo = PlayableDemo::new();
    for _ in 0..10 {
        demo.advance_frame(frame, idle, false);
    }
    assert_eq!(
        demo.tick_index(),
        10,
        "the frame path advances the logical clock"
    );

    // The switch press on the eleventh frame: the event's timestamp
    // is the live logical position — the defect froze it at zero.
    let mut demo_at_switch = PlayableDemo::new();
    let state = demo_at_switch.driver.state_mut();
    state.position = [0.0, 1.2 + engine_core::player::SKIN, -4.4];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    for _ in 0..10 {
        demo_at_switch.advance_frame(frame, idle, false);
    }
    demo_at_switch.advance_frame(frame, idle, true);
    assert!(demo_at_switch.gatehouse.switch_on, "the switch toggled");
    assert_eq!(
        demo_at_switch.gatehouse.events.len(),
        1,
        "one press, one event"
    );
    let (tick, line) = &demo_at_switch.gatehouse.events[0];
    assert!(
        line.contains("switch toggled"),
        "the interaction event is logged: {line}"
    );
    assert_eq!(
        *tick, 11,
        "the interaction timestamps the live logical position (the defect: 0)"
    );

    // Goal-pad contact through the frame path registers completion
    // (the defect never registered it).
    let mut demo_at_goal = PlayableDemo::new();
    let state = demo_at_goal.driver.state_mut();
    state.position = [0.8, 0.0, -21.4];
    state.velocity = [0.0, 0.0, 0.0];
    state.on_ground = true;
    for _ in 0..5 {
        demo_at_goal.advance_frame(frame, idle, false);
    }
    assert!(
        demo_at_goal.gatehouse.complete.is_some(),
        "goal-pad contact through the frame path registers completion (the defect: never)"
    );
    let (tick, line) = demo_at_goal
        .gatehouse
        .events
        .last()
        .expect("the GOAL event");
    assert!(
        line.contains("GOAL"),
        "the completion event is logged: {line}"
    );
    assert!(*tick > 0, "the completion timestamp is real: {tick}");
}

/// The R2 closeout's obligation B: the CONNECTED live-frame route —
/// a SINGLE PlayableDemo instance walked through the essential
/// gatehouse loop entirely via `advance_frame()` (the actual shared
/// progression code `run_playable_demo()` uses), with genuine player
/// movement (no state teleports): settle, the staircase climb, the
/// switch press opening the gate, the corridor walk, the pickups,
/// and the goal pad — with real timestamps, no refused steps, and a
/// loud bounded failure if any phase stalls. The route intentionally
/// mirrors the scripted scenario's checkpoints but drives ONLY
/// frames: 1/60 s per frame, the movement accumulator's own
/// catch-up, and E edges on single frames.
#[test]
fn connected_live_frame_route_completes_the_gatehouse() {
    let mut demo = PlayableDemo::new();
    let frame = std::time::Duration::from_nanos(16_666_666); // 1/60 s → one tick
    let idle = engine_core::MoveIntent::default();
    let forward = engine_core::MoveIntent { forward: 1, ..idle };
    let strafe_left = engine_core::MoveIntent { strafe: -1, ..idle };
    let strafe_right = engine_core::MoveIntent { strafe: 1, ..idle };

    // The frame-driven equivalent of the scripted runner's bounded
    // phase loop: advance frames until the condition holds; a phase
    // that stalls fails the test loudly (never a silent skip).
    fn frames_until(
        demo: &mut PlayableDemo,
        frame: std::time::Duration,
        intent: engine_core::MoveIntent,
        interact: bool,
        condition: impl Fn(&PlayableDemo) -> bool,
        label: &str,
    ) {
        for _ in 0..3_000 {
            if condition(demo) {
                return;
            }
            demo.advance_frame(frame, intent, interact);
        }
        panic!("the connected live-frame route stalled at '{label}'");
    }

    // 1. Settle to the courtyard ground.
    frames_until(
        &mut demo,
        frame,
        idle,
        false,
        |d| d.driver.state().on_ground,
        "settle",
    );

    // 2. Climb the staircase and cross the platform to the switch
    //    (the step-up carries genuine movement over the terrain).
    frames_until(
        &mut demo,
        frame,
        forward,
        false,
        |d| d.driver.state().position[2] <= -4.4,
        "approach the switch",
    );
    assert!(
        demo.driver.state().position[1] > 1.0,
        "the staircase was genuinely climbed: y = {}",
        demo.driver.state().position[1]
    );

    // 3. The switch press (one E edge): the gate opens.
    demo.advance_frame(frame, idle, true);
    assert!(demo.gatehouse.switch_on, "the switch toggled");
    assert!(demo.gatehouse.gate_open(), "the gate opened");
    let switch_timestamp = demo.gatehouse.events.last().expect("the switch event").0;
    assert!(
        switch_timestamp > 0 && switch_timestamp <= demo.tick_index(),
        "the switch timestamp is a real logical position: {switch_timestamp}"
    );

    // 4. Third person, detour left past the switch pillar, off the
    //    platform edge, and land.
    demo.driver.state_mut().toggle_camera_mode();
    frames_until(
        &mut demo,
        frame,
        strafe_left,
        false,
        |d| d.driver.state().position[0] <= -0.5,
        "detour past the switch pillar",
    );
    frames_until(
        &mut demo,
        frame,
        forward,
        false,
        |d| d.driver.state().position[2] <= -7.2,
        "reach the platform edge",
    );
    frames_until(
        &mut demo,
        frame,
        idle,
        false,
        |d| d.driver.state().on_ground,
        "land",
    );

    // 5. Through the opened gate and the corridor to pickup 1.
    frames_until(
        &mut demo,
        frame,
        forward,
        false,
        |d| d.driver.state().position[2] <= -15.5,
        "reach pickup 1",
    );
    demo.advance_frame(frame, idle, true);
    assert!(demo.gatehouse.pickups[0], "pickup 1 collected");

    // 6. The occluder strafe, then pickups 2 and 3.
    frames_until(
        &mut demo,
        frame,
        forward,
        false,
        |d| d.driver.state().position[2] <= -16.0,
        "close on the occluder",
    );
    frames_until(
        &mut demo,
        frame,
        strafe_right,
        false,
        |d| d.driver.state().position[0] >= 0.8,
        "strafe past the occluder",
    );
    frames_until(
        &mut demo,
        frame,
        forward,
        false,
        |d| d.driver.state().position[2] <= -16.4,
        "reach pickup 2",
    );
    demo.advance_frame(frame, idle, true);
    assert!(demo.gatehouse.pickups[1], "pickup 2 collected");
    frames_until(
        &mut demo,
        frame,
        forward,
        false,
        |d| d.driver.state().position[2] <= -17.9,
        "reach pickup 3",
    );
    demo.advance_frame(frame, idle, true);
    assert!(demo.gatehouse.pickups[2], "pickup 3 collected");

    // 7. The goal pad: completion through the connected route.
    frames_until(
        &mut demo,
        frame,
        forward,
        false,
        |d| d.gatehouse.complete.is_some(),
        "reach the goal",
    );
    assert!(demo.gatehouse.complete.is_some(), "the route completed");
    let events = &demo.gatehouse.events;
    let (_, goal_line) = events.last().expect("the GOAL event");
    assert!(
        goal_line.contains("GOAL"),
        "the completion line: {goal_line}"
    );
    assert_eq!(
        demo.gatehouse.collected_count(),
        3,
        "all pickups collected on the connected route"
    );

    // 8. The invariants: real advancing timestamps, no refused
    //    steps, monotone event order.
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
    let mut last_tick = 0;
    for (tick, _) in events {
        assert!(*tick >= last_tick, "the events are timestamped in order");
        last_tick = *tick;
    }
    assert_eq!(
        events.len(),
        5,
        "settle-free route: switch, 3 pickups, GOAL"
    );
}
