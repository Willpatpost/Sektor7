//! Cycle 1 P3 integration battery — the player prototype's
//! application-level guards. All headless: no SDL initialization, no
//! GPU, no process spawns — plain library calls over the public
//! surfaces (the same discipline the sim/persist suites follow).

use engine_core::StaticCollider;
use engine_renderer::scene::{SceneVertex, build_scene};
use engine_shell::{InputSnapshot, Keycode};
use engine_slice::player_demo::{
    DEMO_LOOK_SENSITIVITY, HeadlessScript, demo_collision_world, map_intent, run_player_headless,
};

/// The collide-what-you-see contract, HAND-WRITTEN — never derived
/// from the builders it checks (the F2 rule: an independent truth so
/// drift on either side fails loudly). The scene's 5×5 cube field
/// (spacing 3.0, footprint 0.9, heights 0.6 + |gx + gz|·0.45) plus
/// its three depth demonstrators, in the demo world's construction
/// order. The heights are FROZEN at the scene's computed f32 values
/// in their shortest round-trip spellings (measured, like every pin:
/// `1.95` parses to a different f32 than the rendered geometry's
/// `0.6 + 3·0.45` rounding chain, one ulp away, while `1.05` happens
/// to be the exact shortest form of the n=1 chain).
const DEMO_BOXES: [([f32; 3], f32, f32); 28] = [
    ([-6.0, 0.0, -6.0], 0.9, 2.4),
    ([-6.0, 0.0, -3.0], 0.9, 1.9499999),
    ([-6.0, 0.0, 0.0], 0.9, 1.5),
    ([-6.0, 0.0, 3.0], 0.9, 1.05),
    ([-6.0, 0.0, 6.0], 0.9, 0.6),
    ([-3.0, 0.0, -6.0], 0.9, 1.9499999),
    ([-3.0, 0.0, -3.0], 0.9, 1.5),
    ([-3.0, 0.0, 0.0], 0.9, 1.05),
    ([-3.0, 0.0, 3.0], 0.9, 0.6),
    ([-3.0, 0.0, 6.0], 0.9, 1.05),
    ([0.0, 0.0, -6.0], 0.9, 1.5),
    ([0.0, 0.0, -3.0], 0.9, 1.05),
    ([0.0, 0.0, 0.0], 0.9, 0.6),
    ([0.0, 0.0, 3.0], 0.9, 1.05),
    ([0.0, 0.0, 6.0], 0.9, 1.5),
    ([3.0, 0.0, -6.0], 0.9, 1.05),
    ([3.0, 0.0, -3.0], 0.9, 0.6),
    ([3.0, 0.0, 0.0], 0.9, 1.05),
    ([3.0, 0.0, 3.0], 0.9, 1.5),
    ([3.0, 0.0, 6.0], 0.9, 1.9499999),
    ([6.0, 0.0, -6.0], 0.9, 0.6),
    ([6.0, 0.0, -3.0], 0.9, 1.05),
    ([6.0, 0.0, 0.0], 0.9, 1.5),
    ([6.0, 0.0, 3.0], 0.9, 1.9499999),
    ([6.0, 0.0, 6.0], 0.9, 2.4),
    ([0.0, 0.0, -2.0], 1.4, 1.4),
    ([0.6, 0.3, 0.8], 1.4, 1.4),
    ([-0.5, 2.2, -0.6], 0.8, 0.8),
];

#[test]
fn demo_collision_world_matches_visual_scene() {
    let world = demo_collision_world();
    let colliders = world.colliders();
    assert_eq!(colliders.len(), DEMO_BOXES.len());
    for (index, (base, footprint, height)) in DEMO_BOXES.iter().enumerate() {
        let expected = StaticCollider::from_base(*base, *footprint, *height);
        assert_eq!(colliders[index].min, expected.min, "box {index} min face");
        assert_eq!(colliders[index].max, expected.max, "box {index} max face");
    }

    // Collide-what-you-see, checked against the RENDERED geometry: every
    // collision box's four top-face corners must exist as vertices in the
    // static scene the renderer draws.
    let (vertices, _indices) = build_scene();
    for (base, footprint, height) in DEMO_BOXES {
        let half = footprint / 2.0;
        let top = base[1] + height;
        for (dx, dz) in [(-1.0f32, -1.0), (-1.0, 1.0), (1.0, -1.0), (1.0, 1.0)] {
            let corner = [base[0] + dx * half, top, base[2] + dz * half];
            assert!(
                vertices.iter().any(|vertex| vertex.pos == corner),
                "the rendered scene must draw box {base:?} top corner {corner:?}"
            );
        }
    }
}

#[test]
fn rendered_scene_byte_facts_pinned() {
    // The F2 pin, arithmetic-verified at the baseline: 43 quads (the
    // ground plate + 21×2 grid lines) × 4 verts + 28 cubes × 24 =
    // 844 vertices; 43×6 + 28×36 = 1,266 indices; 24 bytes per
    // SceneVertex. Any drift in the rendered scene or the collision
    // mirror's assumptions fails here, loudly.
    let (vertices, indices) = build_scene();
    assert_eq!(vertices.len(), 844);
    assert_eq!(indices.len(), 1266);
    assert_eq!(std::mem::size_of::<SceneVertex>(), 24);
    assert_eq!(vertices.len() * std::mem::size_of::<SceneVertex>(), 20_256);
    assert_eq!(indices.len() * std::mem::size_of::<u32>(), 5_064);
}

/// The F1 pinned trace: the `jump` script keeps yaw and pitch at
/// exactly zero (single-axis movement, no look input) — sin/cos
/// return their exact values in every libm and only correctly-rounded
/// IEEE arithmetic remains, so ONE frozen constant is valid on every
/// platform. Measured once, then frozen (never silently re-measured).
const PINNED_WALK_JUMP_TRACE_HEX: &str = "8513d40792ea88f0";

#[test]
fn scripted_walk_jump_trace_pinned() {
    let summary = run_player_headless(240, HeadlessScript::WalkJump);
    let trace = summary
        .lines()
        .find_map(|line| line.strip_prefix("player-trace="))
        .expect("the summary carries the trace fingerprint");
    assert_eq!(
        trace, PINNED_WALK_JUMP_TRACE_HEX,
        "the pinned libm-free script's trace (a divergence is an investigation, \
         never a silent re-pin)"
    );
}

#[test]
fn headless_runner_double_run_identical() {
    for script in [
        HeadlessScript::Settle,
        HeadlessScript::Walk,
        HeadlessScript::WalkJump,
        HeadlessScript::Tour,
    ] {
        let first = run_player_headless(240, script);
        let second = run_player_headless(240, script);
        assert_eq!(
            first,
            second,
            "script {} must double-run byte-identically",
            script.name()
        );
    }
}

#[test]
fn snapshot_to_intent_mapping() {
    // Axes pass through (the shell cancels opposite keys; the mapping
    // trusts the snapshot). The look expectation carries the
    // post-orientation hardening's corrected horizontal sign: mouse-X
    // is NEGATED at the mapping (+yaw turns toward world-left, so
    // rightward mouse motion must yaw negatively); the vertical
    // contribution is unchanged (upward mouse motion = negative y
    // delta pitches up). The pre-hardening pin (0.25, 0.125) held the
    // INVERTED horizontal direction — corrected with the measured
    // fail-first evidence in both directions (the new
    // mouse_look_direction test failed at the baseline; this pin
    // failed at the fixed tree — the two failures are the same
    // defect seen from either side).
    let snapshot = InputSnapshot {
        move_forward: 1,
        move_strafe: -1,
        look_delta: (100.0, -50.0),
        ..InputSnapshot::default()
    };
    let mapped = map_intent(&snapshot, false);
    assert_eq!(mapped.intent.forward, 1);
    assert_eq!(mapped.intent.strafe, -1);
    assert_eq!(
        mapped.intent.look,
        (-100.0 * DEMO_LOOK_SENSITIVITY, 50.0 * DEMO_LOOK_SENSITIVITY)
    );
    assert!(!mapped.toggle_camera);

    // Jump is a key edge; the toggle is a different key's edge.
    let snapshot = InputSnapshot {
        keys_pressed: vec![Keycode::Space],
        ..InputSnapshot::default()
    };
    let mapped = map_intent(&snapshot, true);
    assert!(mapped.intent.jump_pressed);
    assert!(!mapped.toggle_camera);
    assert!(
        mapped.intent.sprint,
        "sprint is the caller's held-key query"
    );

    let snapshot = InputSnapshot {
        keys_pressed: vec![Keycode::C],
        ..InputSnapshot::default()
    };
    let mapped = map_intent(&snapshot, false);
    assert!(mapped.toggle_camera);
    assert!(!mapped.intent.jump_pressed);

    // Focus loss and minimization map to the neutral intent — no
    // stuck movement across focus changes (the shell's anti-stuck
    // discipline, mirrored at the mapping boundary).
    let snapshot = InputSnapshot {
        move_forward: 1,
        move_strafe: 1,
        look_delta: (10.0, 10.0),
        keys_pressed: vec![Keycode::C],
        focus_lost: true,
        ..InputSnapshot::default()
    };
    let mapped = map_intent(&snapshot, true);
    assert_eq!(mapped.intent, engine_core::MoveIntent::default());
    assert!(!mapped.toggle_camera);

    let snapshot = InputSnapshot {
        move_forward: 1,
        minimized: true,
        ..InputSnapshot::default()
    };
    let mapped = map_intent(&snapshot, false);
    assert_eq!(mapped.intent, engine_core::MoveIntent::default());
}

/// The post-orientation hardening (H2): mouse-look direction through
/// the ACTUAL mapping and the engine's look application — mouse-right
/// turns the look direction toward world +X, mouse-left toward −X,
/// the VERTICAL mapping is unchanged (mouse-up looks up), turning
/// right and walking moves in the newly faced direction, and
/// strafing is not reversed. Never an intermediate sign assertion:
/// every check reads the resulting world-space look direction or the
/// resulting world-space movement through the real engine math.
#[test]
fn mouse_look_direction_is_correct_through_the_real_mapping() {
    use engine_core::player::look_direction;
    use engine_core::{CollisionWorld, PlayerConfig, PlayerDriver, PlayerState};

    let driver = || {
        PlayerDriver::new(
            PlayerState::spawn([0.0, 1.0, 0.0]),
            PlayerConfig::default(),
            CollisionWorld::new(0.0),
            60,
        )
    };

    // Mouse-RIGHT: positive look_delta.x through map_intent, applied
    // by the real driver over one tick — the look direction must turn
    // toward world +X (the right half-space).
    let snapshot = InputSnapshot {
        look_delta: (100.0, 0.0),
        ..InputSnapshot::default()
    };
    let mut turn_right = driver();
    let intent = map_intent(&snapshot, false).intent;
    turn_right.run_headless(1, |_| intent);
    let state = turn_right.state();
    let look = look_direction(state.yaw, state.pitch);
    assert!(
        look[0] > 0.2,
        "mouse-right turns the look toward world +X: {look:?} (yaw {})",
        state.yaw
    );

    // Mouse-LEFT: the mirror image.
    let snapshot = InputSnapshot {
        look_delta: (-100.0, 0.0),
        ..InputSnapshot::default()
    };
    let mut turn_left = driver();
    let intent = map_intent(&snapshot, false).intent;
    turn_left.run_headless(1, |_| intent);
    let state = turn_left.state();
    let look = look_direction(state.yaw, state.pitch);
    assert!(
        look[0] < -0.2,
        "mouse-left turns the look toward world −X: {look:?}"
    );

    // The VERTICAL mapping unchanged: mouse-up (the shell's negative
    // y delta) pitches the look direction UP, with no yaw coupling.
    let snapshot = InputSnapshot {
        look_delta: (0.0, -100.0),
        ..InputSnapshot::default()
    };
    let mut look_up = driver();
    let intent = map_intent(&snapshot, false).intent;
    look_up.run_headless(1, |_| intent);
    let state = look_up.state();
    let look = look_direction(state.yaw, state.pitch);
    assert!(
        look[1] > 0.2,
        "mouse-up looks up (the vertical mapping is unchanged): {look:?}"
    );
    assert!(
        look[0].abs() < 1e-6,
        "pure vertical input does not yaw: {look:?}"
    );

    // Turn right, then W: the movement follows the NEW facing — the
    // horizontal displacement points into the same half-space the
    // camera faces (dot product positive), dominated by +X.
    let mut walker = driver();
    let turn = map_intent(
        &InputSnapshot {
            look_delta: (100.0, 0.0),
            ..InputSnapshot::default()
        },
        false,
    )
    .intent;
    walker.run_headless(10, |_| turn);
    let facing = look_direction(walker.state().yaw, walker.state().pitch);
    let walk = map_intent(
        &InputSnapshot {
            move_forward: 1,
            ..InputSnapshot::default()
        },
        false,
    )
    .intent;
    let before = walker.state().position;
    walker.run_headless(30, |_| walk);
    let after = walker.state().position;
    let displacement = [after[0] - before[0], after[2] - before[2]];
    let dot = displacement[0] * facing[0] + displacement[1] * facing[2];
    assert!(
        dot > 0.5,
        "forward after a right turn moves along the new facing: \
         displacement {displacement:?} vs facing {:.3}/{:.3}",
        facing[0],
        facing[2]
    );
    assert!(
        displacement[0] > 0.5,
        "a right turn then forward moves toward +X: {displacement:?}"
    );

    // Strafing is NOT reversed: strafe-right at identity facing moves
    // toward +X exactly as before the hardening.
    let mut strafer = driver();
    let strafe = map_intent(
        &InputSnapshot {
            move_strafe: 1,
            ..InputSnapshot::default()
        },
        false,
    )
    .intent;
    strafer.run_headless(30, |_| strafe);
    let after = strafer.state().position;
    assert!(
        after[0] > 0.5,
        "strafe-right still moves toward +X: {:?}",
        after
    );
}
