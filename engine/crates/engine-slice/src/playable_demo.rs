//! Cycle 2 C2-P1 — the playable-world interaction loop (the gatehouse
//! courtyard).
//!
//! A REAL gameplay loop on the Cycle-1 player prototype, demonstrated
//! together: climb the staircase (the grounded step-up) → toggle the
//! switch (the interaction framework: proximity + facing + E-edge +
//! exact-slab line-of-sight) → the gate OPENS (a game-layer world swap
//! via `set_world`, with visible feedback) → traverse the narrow
//! corridor in third person (the obstruction pull-in) → collect three
//! pickups (one deliberately occluded by a pillar until the player
//! strafes past — the LOS filter visibly acting) → step on the goal
//! pad (a demo-layer CONTACT trigger, per the challenge-F3 decision:
//! the finale is navigation, not another key press).
//!
//! Engine/game boundary: the mechanics (step-up, `segment_hit`,
//! `camera_pose_obstructed`, `set_world`, `select_target`) live in
//! engine-core; this module owns the world content, the interactable
//! semantics, the feedback, and the 0.55 m step / 2.5 m reach / 45°
//! cone policy values.
//!
//! The course is deliberately LINEAR along −Z with every interactable
//! approachable dead-ahead at eye-friendly heights (challenge F4): the
//! scripted headless run stays libm-free (yaw and pitch exactly zero —
//! no look input), so its trace is cross-platform bit-stable under
//! the existing `engine-core/player-trace/1` domain, and the camera
//! segment is exercised by scripted toggles plus obstructed-pose
//! SAMPLES logged with their pull-in distances (challenge F1).
//!
//! Transient state (explicit): the switch/gate/pickups/goal/event log
//! exist only in the running process — nothing persisted, no canonical
//! formats touched; a restart resets the courtyard.

use std::time::{Duration, Instant};

use engine_core::{
    CameraMode, CameraPose, CollisionWorld, InteractionQuery, InteractionTarget, MoveIntent,
    PlayerConfig, PlayerDriver, PlayerState, StaticCollider, camera_pose, camera_pose_obstructed,
    look_direction, player_box, select_target,
};
use engine_renderer::scene::SceneVertex;
use engine_renderer::{PresentModePreference, RenderConfig, Renderer, ValidationPreference};
use engine_shell::{DriverPreference, InputSnapshot, Keycode, ShellApp, ShellConfig};

use crate::frame_timing::FrameTiming;
use crate::math;

// ==================================================== the gatehouse layout
//
// Hand-written (the F2 rule: an independent literal, never derived
// from the builders it checks — regression-pinned in
// tests/playable_world.rs). A linear −Z course: spawn at z = +8,
// staircase at z ∈ [−3, 0], the switch platform z ∈ [−7, −3], the
// gate wall z ∈ [−10, −9.5], the corridor z ∈ [−20, −9.5] (with the
// obstruction lintel and the occluder pillar), the goal pad at
// z = −22.

/// One gatehouse box: an axis-aligned extent plus its role.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GatehouseBox {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub role: GatehouseRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatehouseRole {
    /// Courtyard perimeter walls (solid, gray).
    Perimeter,
    /// The ascending staircase (solid, stone).
    Step,
    /// The switch platform (solid).
    Platform,
    /// The switch's pillar up to head height (solid).
    SwitchPillar,
    /// The gate-wall segments flanking the corridor mouth (solid).
    GateWall,
    /// The gate itself (solid while closed; removed by the world swap).
    Gate,
    /// The corridor's side walls (solid).
    Corridor,
    /// The overhead roof section the TP camera pulls in against
    /// (solid) — deep enough that the rising eye→camera segment
    /// genuinely crosses it mid-range (a shallow lintel would be
    /// passed below before the segment rises into its band).
    Lintel,
    /// The occluder pillar that hides pickup 2 until passed (solid).
    Occluder,
    /// A floating pickup orb (visual only — never a collider).
    Pickup,
    /// The goal pad marker (visual only — a contact trigger, not a
    /// collider).
    GoalPad,
    /// The switch head on its pillar (visual only; recolors on use).
    SwitchTop,
}

impl GatehouseRole {
    /// Whether the role participates in collision.
    pub fn is_solid(self) -> bool {
        !matches!(
            self,
            GatehouseRole::Pickup | GatehouseRole::GoalPad | GatehouseRole::SwitchTop
        )
    }
}

/// The full gatehouse layout, box for box (rendered and collided from
/// the same list — collide-what-you-see).
pub const GATEHOUSE_BOXES: &[GatehouseBox] = &[
    // Perimeter.
    GatehouseBox {
        min: [-6.0, 0.0, -24.0],
        max: [-5.7, 3.0, 10.0],
        role: GatehouseRole::Perimeter,
    },
    GatehouseBox {
        min: [5.7, 0.0, -24.0],
        max: [6.0, 3.0, 10.0],
        role: GatehouseRole::Perimeter,
    },
    GatehouseBox {
        min: [-6.0, 0.0, 9.7],
        max: [6.0, 3.0, 10.0],
        role: GatehouseRole::Perimeter,
    },
    GatehouseBox {
        min: [-6.0, 0.0, -24.0],
        max: [6.0, 3.0, -23.7],
        role: GatehouseRole::Perimeter,
    },
    // Staircase (each rise 0.4 <= the 0.55 step height).
    GatehouseBox {
        min: [-2.0, 0.0, -1.0],
        max: [2.0, 0.4, 0.0],
        role: GatehouseRole::Step,
    },
    GatehouseBox {
        min: [-2.0, 0.0, -2.0],
        max: [2.0, 0.8, -1.0],
        role: GatehouseRole::Step,
    },
    GatehouseBox {
        min: [-2.0, 0.0, -3.0],
        max: [2.0, 1.2, -2.0],
        role: GatehouseRole::Step,
    },
    // The switch platform.
    GatehouseBox {
        min: [-2.0, 0.0, -7.0],
        max: [2.0, 1.2, -3.0],
        role: GatehouseRole::Platform,
    },
    // The switch pillar and its head.
    GatehouseBox {
        min: [-0.15, 1.2, -5.15],
        max: [0.15, 2.6, -4.85],
        role: GatehouseRole::SwitchPillar,
    },
    GatehouseBox {
        min: [-0.2, 2.6, -5.2],
        max: [0.2, 2.9, -4.8],
        role: GatehouseRole::SwitchTop,
    },
    // The gate wall and the gate.
    GatehouseBox {
        min: [-6.0, 0.0, -10.0],
        max: [-1.5, 3.0, -9.5],
        role: GatehouseRole::GateWall,
    },
    GatehouseBox {
        min: [1.5, 0.0, -10.0],
        max: [6.0, 3.0, -9.5],
        role: GatehouseRole::GateWall,
    },
    GatehouseBox {
        min: [-1.5, 0.0, -10.0],
        max: [1.5, 3.0, -9.5],
        role: GatehouseRole::Gate,
    },
    // The corridor (interior x ∈ ±1.2 — narrow enough that the lintel
    // above obstructs the third-person camera).
    GatehouseBox {
        min: [-1.5, 0.0, -20.0],
        max: [-1.2, 3.0, -9.5],
        role: GatehouseRole::Corridor,
    },
    GatehouseBox {
        min: [1.2, 0.0, -20.0],
        max: [1.5, 3.0, -9.5],
        role: GatehouseRole::Corridor,
    },
    GatehouseBox {
        min: [-1.5, 2.4, -15.0],
        max: [1.5, 3.0, -12.0],
        role: GatehouseRole::Lintel,
    },
    // The occluder pillar (hides pickup 2 from the centerline).
    GatehouseBox {
        min: [0.2, 0.0, -17.5],
        max: [0.5, 3.0, -17.2],
        role: GatehouseRole::Occluder,
    },
    // The pickup orbs (visual only; eye-friendly heights so the
    // 45° cone holds at the scripted press distances).
    GatehouseBox {
        min: [-0.2, 1.8, -16.7],
        max: [0.2, 2.2, -16.3],
        role: GatehouseRole::Pickup,
    },
    GatehouseBox {
        min: [0.6, 1.8, -18.2],
        max: [1.0, 2.2, -17.8],
        role: GatehouseRole::Pickup,
    },
    GatehouseBox {
        min: [0.6, 1.8, -19.7],
        max: [1.0, 2.2, -19.3],
        role: GatehouseRole::Pickup,
    },
    // The goal pad.
    GatehouseBox {
        min: [0.0, 0.001, -22.8],
        max: [1.6, 0.05, -21.2],
        role: GatehouseRole::GoalPad,
    },
];

/// The interactable identities: the switch, then the three pickups.
pub const SWITCH_ID: u32 = 1;
pub const PICKUP_IDS: [u32; 3] = [2, 3, 4];

/// The switch's interaction point (on its pillar head, at a height the
/// eye reaches dead-ahead).
pub const SWITCH_POSITION: [f32; 3] = [0.0, 2.75, -5.0];
pub const SWITCH_RADIUS: f32 = 0.5;

/// The pickup centers (matching the visual orbs).
pub const PICKUP_POSITIONS: [[f32; 3]; 3] =
    [[0.0, 2.0, -16.5], [0.8, 2.0, -18.0], [0.8, 2.0, -19.5]];
pub const PICKUP_RADIUS: f32 = 0.4;

/// The goal pad's contact trigger (challenge F3: a demo-layer contact
/// policy — stepping on the pad completes the loop).
pub const GOAL_CENTER: [f32; 2] = [0.8, -22.0];
pub const GOAL_RADIUS: f32 = 1.0;

/// Builds the gatehouse collision world. The gate is included while
/// closed and removed when open — the game-layer world swap.
pub fn gatehouse_world(gate_open: bool) -> CollisionWorld {
    let mut world = CollisionWorld::new(0.0);
    for class_box in GATEHOUSE_BOXES {
        if !class_box.role.is_solid() {
            continue;
        }
        if class_box.role == GatehouseRole::Gate && gate_open {
            continue;
        }
        world = world.with_collider(StaticCollider {
            min: class_box.min,
            max: class_box.max,
        });
    }
    world.with_bounds([-5.5, 0.0, -23.5], [5.5, 40.0, 9.5])
}

/// The currently offered interaction targets (the switch always; the
/// uncollected pickups).
pub fn gatehouse_interactables(pickups_collected: &[bool; 3]) -> Vec<InteractionTarget> {
    let mut targets = vec![InteractionTarget {
        id: SWITCH_ID,
        position: SWITCH_POSITION,
        radius: SWITCH_RADIUS,
    }];
    for (index, collected) in pickups_collected.iter().enumerate() {
        if !collected {
            targets.push(InteractionTarget {
                id: PICKUP_IDS[index],
                position: PICKUP_POSITIONS[index],
                radius: PICKUP_RADIUS,
            });
        }
    }
    targets
}

/// The goal contact trigger: grounded and horizontally within the pad.
pub fn goal_reached(state: &PlayerState) -> bool {
    let dx = state.position[0] - GOAL_CENTER[0];
    let dz = state.position[2] - GOAL_CENTER[1];
    state.on_ground && (dx * dx + dz * dz) < GOAL_RADIUS * GOAL_RADIUS
}

/// The playable config: the Cycle-1 demo values plus the ENABLED
/// step-up (0.55 m — the staircase's 0.4 rises climb without
/// jumping). The Cycle-1 `--player` paths keep step_height 0.0.
pub fn playable_player_config() -> PlayerConfig {
    PlayerConfig {
        walk_speed: 4.0,
        sprint_speed: 12.0,
        gravity: 25.0,
        jump_speed: 8.0,
        terminal_fall: 50.0,
        height: 1.7,
        radius: 0.35,
        eye_height: 1.6,
        tp_distance: 4.5,
        tp_lift: 1.2,
        tp_min_distance: 1.2,
        pitch_limit: 1.54,
        step_height: 0.55,
    }
}

/// The interaction policy values (demo-owned).
pub const PLAYABLE_QUERY: InteractionQuery = InteractionQuery {
    range: 2.5,
    cone_half_angle_cos: std::f32::consts::FRAC_1_SQRT_2,
};

// ==================================================== the demo state

/// The gatehouse's transient game state (never persisted).
#[derive(Debug, Clone, Default)]
pub struct GatehouseState {
    pub switch_on: bool,
    pub pickups: [bool; 3],
    /// The completion tick, once the goal pad is reached.
    pub complete: Option<u64>,
    /// (tick, line) — the deterministic event log.
    pub events: Vec<(u64, String)>,
}

impl GatehouseState {
    /// The gate is open exactly when the switch is on (the demo's
    /// one-way coupling, public for the battery's assertions).
    pub fn gate_open(&self) -> bool {
        self.switch_on
    }

    pub fn collected_count(&self) -> usize {
        self.pickups.iter().filter(|collected| **collected).count()
    }
}

/// The playable demo: the player driver plus the gatehouse state,
/// with the per-tick interaction and goal logic shared by the
/// interactive and headless paths.
pub struct PlayableDemo {
    pub driver: PlayerDriver,
    pub gatehouse: GatehouseState,
    ticks: u64,
}

impl Default for PlayableDemo {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayableDemo {
    pub fn new() -> PlayableDemo {
        let driver = PlayerDriver::new(
            PlayerState::spawn([0.0, 2.0, 8.0]),
            playable_player_config(),
            gatehouse_world(false),
            crate::player_demo::DEMO_TICK_HZ,
        );
        PlayableDemo {
            driver,
            gatehouse: GatehouseState::default(),
            ticks: 0,
        }
    }

    pub fn tick_index(&self) -> u64 {
        self.ticks
    }

    /// One movement tick plus the frame-level interaction and goal
    /// logic. `interact` is the E EDGE for this tick (the mapping
    /// layer derives it from `keys_pressed`; holding is not a new
    /// press).
    pub fn tick(&mut self, intent: MoveIntent, interact: bool) {
        let tick = self.ticks;
        self.ticks += 1;
        self.driver.run_headless(1, |_| intent);
        if interact {
            self.try_interact(tick);
        }
        if self.gatehouse.complete.is_none() && goal_reached(self.driver.state()) {
            self.gatehouse.complete = Some(tick);
            self.log(
                tick,
                format!(
                    "GOAL — scenario complete ({} ticks, {}/3 pickups)",
                    tick,
                    self.gatehouse.collected_count()
                ),
            );
        }
    }

    /// The interaction resolution: select (engine mechanics), filter
    /// by the exact-slab line of sight, then apply the demo-layer
    /// semantics (what interactions DO lives here).
    pub fn try_interact(&mut self, tick: u64) {
        let Some(target) = self.current_target() else {
            return;
        };
        let (id, position) = target;
        let eye = self.eye();
        if self
            .driver
            .world()
            .segment_hit(eye, position, 0.0)
            .is_some()
        {
            self.log(
                tick,
                format!("target {id} occluded — line of sight blocked"),
            );
            return;
        }
        if id == SWITCH_ID {
            self.gatehouse.switch_on = !self.gatehouse.switch_on;
            let word = if self.gatehouse.gate_open() {
                "opens"
            } else {
                "closes"
            };
            let world = gatehouse_world(self.gatehouse.gate_open());
            self.driver.set_world(world);
            self.log(tick, format!("switch toggled — the gate {word}"));
        } else if let Some(index) = PICKUP_IDS.iter().position(|pid| *pid == id) {
            self.gatehouse.pickups[index] = true;
            self.log(
                tick,
                format!(
                    "pickup {} collected ({}/3)",
                    index + 1,
                    self.gatehouse.collected_count()
                ),
            );
        }
    }

    /// The currently offered target after selection AND the
    /// line-of-sight filter: `(id, position)` when a target is
    /// offered with a clear line, `(id, position)` with the occluded
    /// flag surfaced by [`Self::current_target_with_los`].
    pub fn current_target(&self) -> Option<(u32, [f32; 3])> {
        self.current_target_with_los()
            .filter(|(_, _, clear)| *clear)
            .map(|(id, position, _)| (id, position))
    }

    /// The selection + LOS probe: `(id, position, line_clear)`.
    pub fn current_target_with_los(&self) -> Option<(u32, [f32; 3], bool)> {
        let state = self.driver.state();
        let eye = self.eye();
        let forward = look_direction(state.yaw, state.pitch);
        let targets = gatehouse_interactables(&self.gatehouse.pickups);
        let id = select_target(&targets, eye, forward, &PLAYABLE_QUERY)?;
        let position = targets.iter().find(|target| target.id == id)?.position;
        let clear = self
            .driver
            .world()
            .segment_hit(eye, position, 0.0)
            .is_none();
        Some((id, position, clear))
    }

    pub fn eye(&self) -> [f32; 3] {
        let state = self.driver.state();
        [
            state.position[0],
            state.position[1] + self.driver.config().eye_height,
            state.position[2],
        ]
    }

    /// The camera pose with obstruction handling (the interactive and
    /// headless paths both use this).
    pub fn camera_pose(&self) -> CameraPose {
        camera_pose_obstructed(
            self.driver.state(),
            self.driver.config(),
            self.driver.world(),
        )
    }

    fn log(&mut self, tick: u64, line: String) {
        self.gatehouse.events.push((tick, line));
    }

    /// The visible world: every layout box in its current state —
    /// collide-what-you-see, and every interaction visibly confirmed
    /// (the gate disappears when open, collected pickups vanish, the
    /// switch head recolors).
    pub fn render_boxes(&self) -> Vec<([f32; 3], [f32; 3], [f32; 3])> {
        let mut boxes = Vec::new();
        for class_box in GATEHOUSE_BOXES {
            let color = match class_box.role {
                GatehouseRole::Perimeter => [0.35, 0.35, 0.40],
                GatehouseRole::Step => [0.55, 0.50, 0.45],
                GatehouseRole::Platform => [0.50, 0.55, 0.50],
                GatehouseRole::SwitchPillar => [0.40, 0.35, 0.30],
                GatehouseRole::GateWall => [0.30, 0.30, 0.35],
                GatehouseRole::Gate => {
                    if self.gatehouse.gate_open() {
                        continue;
                    }
                    [0.70, 0.25, 0.20]
                }
                GatehouseRole::Corridor => [0.42, 0.42, 0.48],
                GatehouseRole::Lintel => [0.25, 0.25, 0.30],
                GatehouseRole::Occluder => [0.50, 0.30, 0.30],
                GatehouseRole::Pickup => {
                    let index = pickup_index_of(class_box);
                    if self.gatehouse.pickups[index] {
                        continue;
                    }
                    [0.90, 0.75, 0.10]
                }
                GatehouseRole::GoalPad => {
                    if self.gatehouse.complete.is_some() {
                        [0.20, 0.80, 0.40]
                    } else {
                        [0.20, 0.40, 0.90]
                    }
                }
                GatehouseRole::SwitchTop => {
                    if self.gatehouse.switch_on {
                        [0.20, 0.80, 0.30]
                    } else {
                        [0.80, 0.20, 0.20]
                    }
                }
            };
            boxes.push((class_box.min, class_box.max, color));
        }
        // The player proxy in third person (the same derived-pose
        // rule as the Cycle-1 demo).
        if self.driver.state().camera_mode == CameraMode::ThirdPerson {
            let (min, max) = player_box(self.driver.state().position, self.driver.config());
            boxes.push((min, max, [0.20, 0.80, 0.35]));
        }
        boxes
    }
}

fn pickup_index_of(class_box: &GatehouseBox) -> usize {
    PICKUP_POSITIONS
        .iter()
        .position(|center| {
            center[0] == (class_box.min[0] + class_box.max[0]) / 2.0
                && center[2] == (class_box.min[2] + class_box.max[2]) / 2.0
        })
        .unwrap_or(0)
}

// ==================================================== input mapping

/// One frame's mapped playable input.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PlayableMappedInput {
    pub intent: MoveIntent,
    /// A fresh E press this frame (edge-triggered; holding is not a
    /// new press).
    pub interact_pressed: bool,
    pub toggle_camera: bool,
}

/// Maps a shell snapshot onto the playable intents. E interacts; C
/// toggles the camera; focus loss and minimization neutralize
/// everything (the shell's anti-stuck discipline).
pub fn map_playable_input(snapshot: &InputSnapshot, sprint_held: bool) -> PlayableMappedInput {
    let neutral = snapshot.focus_lost || snapshot.minimized;
    let intent = if neutral {
        MoveIntent::default()
    } else {
        MoveIntent {
            forward: snapshot.move_forward,
            strafe: snapshot.move_strafe,
            jump_pressed: snapshot.keys_pressed.contains(&Keycode::Space),
            sprint: sprint_held,
            look: (
                snapshot.look_delta.0 * crate::player_demo::DEMO_LOOK_SENSITIVITY,
                -snapshot.look_delta.1 * crate::player_demo::DEMO_LOOK_SENSITIVITY,
            ),
        }
    };
    PlayableMappedInput {
        intent,
        interact_pressed: !neutral && snapshot.keys_pressed.contains(&Keycode::E),
        toggle_camera: !neutral && snapshot.keys_pressed.contains(&Keycode::C),
    }
}

// ==================================================== the headless scenario

/// One obstructed-pose camera sample (challenge F1: the camera segment
/// as first-class headless evidence).
#[derive(Debug, Clone, Copy)]
pub struct CameraSample {
    pub tick: u64,
    pub name: &'static str,
    /// The desired (unobstructed) eye→camera distance.
    pub desired: f32,
    /// The actual distance after the pull-in.
    pub actual: f32,
}

impl PlayableDemo {
    fn camera_sample(&self, name: &'static str) -> CameraSample {
        let state = self.driver.state();
        let config = self.driver.config();
        let desired_pose = camera_pose(state, config);
        let actual_pose = camera_pose_obstructed(state, config, self.driver.world());
        let eye = self.eye();
        let length = |pose: &CameraPose| {
            ((pose.position[0] - eye[0]).powi(2)
                + (pose.position[1] - eye[1]).powi(2)
                + (pose.position[2] - eye[2]).powi(2))
            .sqrt()
        };
        CameraSample {
            tick: self.ticks,
            name,
            desired: length(&desired_pose),
            actual: length(&actual_pose),
        }
    }
}

fn idle() -> MoveIntent {
    MoveIntent::default()
}

fn forward() -> MoveIntent {
    MoveIntent {
        forward: 1,
        ..idle()
    }
}

fn strafe_right() -> MoveIntent {
    MoveIntent {
        strafe: 1,
        ..idle()
    }
}

fn strafe_left() -> MoveIntent {
    MoveIntent {
        strafe: -1,
        ..idle()
    }
}

/// Runs the scripted gatehouse scenario until the goal, panicking
/// loudly if any phase stalls (a deterministic script that stalls is a
/// layout or mechanic regression, never a skip).
fn run_until(
    demo: &mut PlayableDemo,
    intent: MoveIntent,
    condition: impl Fn(&PlayableDemo) -> bool,
    label: &str,
) {
    for _ in 0..3_000 {
        if condition(demo) {
            return;
        }
        demo.tick(intent, false);
    }
    panic!("the gatehouse script stalled at '{label}'");
}

/// The headless playable scenario (the `--playable-headless` entry):
/// the full loop — climb, toggle, traverse (with the camera samples),
/// collect, reach the goal — returning the printable evidence record
/// (the event log, the camera samples, the completion summary, and
/// the deterministic trace fingerprint). Byte-identical across
/// identical invocations.
pub fn run_playable_headless() -> String {
    let mut demo = PlayableDemo::new();
    let mut samples: Vec<CameraSample> = Vec::new();

    // 1. Settle to the ground.
    run_until(&mut demo, idle(), |d| d.driver.state().on_ground, "settle");
    let settle_tick = demo.tick_index();
    demo.log_public(settle_tick, "settled on the courtyard ground");

    // 2. Climb the staircase and cross the platform to the switch.
    run_until(
        &mut demo,
        forward(),
        |d| d.driver.state().position[2] <= -4.4,
        "approach the switch",
    );

    // 3. Toggle the switch (the E edge — one press, one event).
    demo.tick(idle(), true);

    // 4. Third person for the corridor (challenge F1's scripted
    // camera event).
    let camera_tick = demo.tick_index();
    demo.driver.state_mut().toggle_camera_mode();
    demo.log_public(camera_tick, "camera mode: third person");

    // 5. Detour left around the switch pillar (it stands on the
    //    centerline by design — the E-stop is at its face).
    run_until(
        &mut demo,
        strafe_left(),
        |d| d.driver.state().position[0] <= -0.5,
        "detour past the switch pillar",
    );

    // 6. Walk off the platform's far edge and land.
    run_until(
        &mut demo,
        forward(),
        |d| d.driver.state().position[2] <= -7.2,
        "reach the platform edge",
    );
    run_until(&mut demo, idle(), |d| d.driver.state().on_ground, "land");

    // 7. Into the corridor; the lintel sample (obstructed).
    run_until(
        &mut demo,
        forward(),
        |d| d.driver.state().position[2] <= -14.7,
        "reach the lintel",
    );
    samples.push(demo.camera_sample("lintel"));

    // 8. Pickup 1 — pressed past the lintel's margin-expanded face so
    //    the line of sight is genuinely clear.
    run_until(
        &mut demo,
        forward(),
        |d| d.driver.state().position[2] <= -15.5,
        "reach pickup 1",
    );
    demo.tick(idle(), true);

    // 9. The occlusion probe: from the detoured line, pickup 2 sits
    // behind the occluder pillar (challenge N1 — the LOS filter
    // visibly acting).
    run_until(
        &mut demo,
        forward(),
        |d| d.driver.state().position[2] <= -16.0,
        "close on the occluder",
    );
    let occluded_tick = demo.tick_index();
    match demo.current_target_with_los() {
        Some((id, _, false)) => {
            demo.log_public(
                occluded_tick,
                format!("target {id} occluded — line of sight blocked"),
            );
        }
        Some((id, _, true)) => {
            panic!("pickup 2 was expected occluded from the detoured line (got target {id} clear)");
        }
        None => panic!("pickup 2 was expected selected from the detoured line"),
    }

    // 10. Strafe right past the pillar; the LOS clears.
    run_until(
        &mut demo,
        strafe_right(),
        |d| d.driver.state().position[0] >= 0.8,
        "strafe past the occluder",
    );
    let clear_tick = demo.tick_index();
    match demo.current_target_with_los() {
        Some((id, _, true)) => {
            demo.log_public(clear_tick, format!("target {id} line of sight clear"));
        }
        _ => panic!("pickup 2 was expected clear after the strafe"),
    }

    // 11. Pickups 2 and 3 along the strafed line.
    run_until(
        &mut demo,
        forward(),
        |d| d.driver.state().position[2] <= -16.4,
        "reach pickup 2",
    );
    demo.tick(idle(), true);
    run_until(
        &mut demo,
        forward(),
        |d| d.driver.state().position[2] <= -17.9,
        "reach pickup 3",
    );
    demo.tick(idle(), true);

    // 11. The goal pad (the contact trigger).
    run_until(
        &mut demo,
        forward(),
        |d| d.gatehouse.complete.is_some(),
        "reach the goal",
    );
    let done_tick = demo.tick_index();
    for _ in 0..5 {
        demo.tick(idle(), false);
    }

    // The evidence record.
    let mut out = String::new();
    out.push_str("Sektor7 C2-P1 - playable-world headless run (the gatehouse)\n");
    out.push_str("events:\n");
    for (tick, line) in &demo.gatehouse.events {
        out.push_str(&format!("  tick {tick}: {line}\n"));
    }
    out.push_str("camera samples:\n");
    for sample in &samples {
        let state = if sample.actual < sample.desired - 1e-3 {
            "PULLED IN"
        } else {
            "clear"
        };
        out.push_str(&format!(
            "  tick {} '{}': desired {:.2} m -> actual {:.2} m ({})\n",
            sample.tick, sample.name, sample.desired, sample.actual, state
        ));
    }
    let state = demo.driver.state();
    out.push_str(&format!(
        "goal: {} at tick {} (pickups {}/3, switch {}, refused {})\n",
        if demo.gatehouse.complete.is_some() {
            "COMPLETE"
        } else {
            "INCOMPLETE"
        },
        done_tick,
        demo.gatehouse.collected_count(),
        if demo.gatehouse.switch_on {
            "on"
        } else {
            "off"
        },
        demo.driver.refused_steps(),
    ));
    out.push_str(&format!(
        "player pos ({:.3}, {:.3}, {:.3}) vel ({:.3}, {:.3}, {:.3}) on_ground={} mode={}\n",
        state.position[0],
        state.position[1],
        state.position[2],
        state.velocity[0],
        state.velocity[1],
        state.velocity[2],
        state.on_ground,
        match state.camera_mode {
            CameraMode::FirstPerson => "first",
            CameraMode::ThirdPerson => "third",
        },
    ));
    out.push_str(&format!(
        "movement ticks executed={} outstanding={} due={} refused={}\n",
        demo.driver.executed_ticks_total(),
        demo.driver.outstanding_ticks(),
        demo.driver.ticks_due_total(),
        demo.driver.refused_steps(),
    ));
    out.push_str(&format!(
        "player-trace={}",
        demo.driver.trace_fingerprint_hex()
    ));
    out
}

impl PlayableDemo {
    /// Public event logging for the scripted scenario's own markers.
    pub fn log_public(&mut self, tick: u64, line: impl Into<String>) {
        self.gatehouse.events.push((tick, line.into()));
    }
}

// ==================================================== the interactive demo

/// Demo options; CLI parsing lives in the binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayableOptions {
    pub fps: u32,
    pub present: PresentModePreference,
    pub frames: Option<u64>,
    pub overlay: bool,
    pub validation: ValidationPreference,
}

impl Default for PlayableOptions {
    fn default() -> Self {
        PlayableOptions {
            fps: 60,
            present: PresentModePreference::Auto,
            frames: None,
            overlay: true,
            validation: ValidationPreference::Auto,
        }
    }
}

/// Appends an axis-aligned box as non-indexed triangle vertices (36
/// per box), mirroring `push_box`'s face order, winding, and shading —
/// the rectangle generalization the gatehouse's walls need.
fn push_rect_box(vertices: &mut Vec<SceneVertex>, min: [f32; 3], max: [f32; 3], color: [f32; 3]) {
    let shade = |multiplier: f32| {
        [
            color[0] * multiplier,
            color[1] * multiplier,
            color[2] * multiplier,
        ]
    };
    let face = |vertices: &mut Vec<SceneVertex>, quad: [[f32; 3]; 4], face_color: [f32; 3]| {
        for position in [quad[0], quad[1], quad[2], quad[0], quad[2], quad[3]] {
            vertices.push(SceneVertex {
                pos: position,
                col: face_color,
            });
        }
    };
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    // Top and bottom.
    face(
        vertices,
        [[x0, y1, z1], [x0, y1, z0], [x1, y1, z0], [x1, y1, z1]],
        shade(1.0),
    );
    face(
        vertices,
        [[x0, y0, z0], [x0, y0, z1], [x1, y0, z1], [x1, y0, z0]],
        shade(0.45),
    );
    // Sides.
    face(
        vertices,
        [[x0, y0, z0], [x0, y1, z0], [x1, y1, z0], [x1, y0, z0]],
        shade(0.80),
    );
    face(
        vertices,
        [[x1, y0, z1], [x1, y1, z1], [x0, y1, z1], [x0, y0, z1]],
        shade(0.80),
    );
    face(
        vertices,
        [[x0, y0, z1], [x0, y1, z1], [x0, y1, z0], [x0, y0, z0]],
        shade(0.65),
    );
    face(
        vertices,
        [[x1, y0, z0], [x1, y1, z0], [x1, y1, z1], [x1, y0, z1]],
        shade(0.65),
    );
}

/// The interactive playable demo (`--playable`): the Cycle-1 demo
/// skeleton over the gatehouse — fixed-step movement on the full raw
/// interval, the obstructed third-person camera, E to interact, C to
/// toggle the camera, visible feedback everywhere.
pub fn run_playable_demo(options: PlayableOptions) -> Result<(), String> {
    let shell_config = ShellConfig {
        title: "Sektor7 original engine — Cycle 2 C2-P1 playable world".to_string(),
        width: 1280,
        height: 720,
        target_fps: 60,
        driver: DriverPreference::Real,
    };
    let mut app = ShellApp::new(shell_config)?;
    app.set_relative_mouse(true);

    let render_config = RenderConfig {
        present_mode: options.present,
        validation: options.validation,
        replace_pipelines: false,
    };
    let mut renderer =
        Renderer::new(app.window(), render_config).map_err(|error| error.to_string())?;
    renderer.set_overlay_enabled(options.overlay);

    let mut demo = PlayableDemo::new();
    demo.driver.set_trace_recording(false);

    {
        let stats = renderer.stats();
        println!("Sektor7 Cycle 2 C2-P1 — the playable-world interaction loop");
        println!(
            "  device: {} {} (Vulkan {})",
            stats.device_type, stats.gpu_name, stats.api_version
        );
        println!(
            "  present: {} (supported: {}) · {} swapchain images · {}x{} · validation: {}",
            stats.present_mode,
            stats.supported_present_modes.join("/"),
            stats.swapchain_images,
            stats.extent.0,
            stats.extent.1,
            stats.validation_active
        );
        println!(
            "  controls: WASD move · Space jump · Shift sprint · E interact · \
             C camera mode · mouse look · O overlay · Esc/close quit"
        );
        println!(
            "  the loop: climb the stairs → toggle the switch (E) → the gate opens → \
             the corridor (third person pulls in) → collect 3 pickups → the goal pad"
        );
    }

    let mut cpu_work_ms: Vec<f64> = Vec::new();
    let mut presented: u64 = 0;
    let start = Instant::now();
    let frame_interval = if options.fps == 0 {
        None
    } else {
        Some(Duration::from_secs_f64(
            1.0 / f64::from(options.fps.clamp(1, 1000)),
        ))
    };
    let mut timing = FrameTiming::new();
    let mut scene_vertices: Vec<SceneVertex> = Vec::new();

    loop {
        let frame_start = Instant::now();
        let snapshot = app.frame();
        if snapshot.quit_requested || snapshot.keys_pressed.contains(&Keycode::Escape) {
            break;
        }
        if snapshot.keys_pressed.contains(&Keycode::O) {
            renderer.set_overlay_enabled(!renderer.overlay_enabled());
        }
        let split = timing.begin_frame(frame_start, !snapshot.minimized);

        let mapped = map_playable_input(&snapshot, app.is_held(Keycode::LShift));
        if mapped.toggle_camera {
            demo.driver.state_mut().toggle_camera_mode();
        }

        // Fixed-step movement on the full raw interval (the Cycle-1
        // discipline); the interaction applies at the frame boundary
        // on the post-advance state.
        demo.driver.advance(split.sim_advance, mapped.intent);
        if mapped.interact_pressed {
            demo.try_interact(demo.tick_index());
        }

        if snapshot.minimized {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        let pose = demo.camera_pose();
        let (width, height) = renderer.stats().extent;
        let aspect = if height == 0 {
            16.0 / 9.0
        } else {
            width as f32 / height as f32
        };
        let view = math::camera_view(pose.position, pose.yaw, pose.pitch, pose.roll);
        let projection = math::perspective(std::f32::consts::FRAC_PI_3, aspect, 0.1, 200.0);
        let view_proj = math::mul(&projection, &view);

        // The visible gatehouse + the player proxy (retained buffer).
        scene_vertices.clear();
        for (min, max, color) in demo.render_boxes() {
            push_rect_box(&mut scene_vertices, min, max, color);
        }

        let state = demo.driver.state();
        let overlay_text = renderer.overlay_enabled().then(|| {
            let elapsed = start.elapsed().as_secs_f64().max(0.001);
            let stats = renderer.stats();
            let target_line = match demo.current_target() {
                Some((id, _)) if id == SWITCH_ID => "[E] toggle the switch".to_string(),
                Some((id, _)) if PICKUP_IDS.contains(&id) => {
                    format!("[E] collect pickup {}", PICKUP_IDS.iter().position(|p| *p == id).unwrap() + 1)
                }
                Some((id, _)) => format!("[E] interact {id}"),
                None => {
                    if demo.gatehouse.switch_on {
                        "no target — the gate is open".to_string()
                    } else {
                        "no target".to_string()
                    }
                }
            };
            let desired_pose = camera_pose(state, demo.driver.config());
            let camera_distance = (
                (pose.position[0] - demo.eye()[0]).powi(2)
                    + (pose.position[1] - demo.eye()[1]).powi(2)
                    + (pose.position[2] - demo.eye()[2]).powi(2)
            )
                .sqrt();
            let desired_distance = (
                (desired_pose.position[0] - demo.eye()[0]).powi(2)
                    + (desired_pose.position[1] - demo.eye()[1]).powi(2)
                    + (desired_pose.position[2] - demo.eye()[2]).powi(2)
            )
                .sqrt();
            format!(
                "Sektor7 C2-P1 - playable world\nFPS {:.0} (cpu work p95 {:.2} ms)\n{} {} / Vulkan {}\n\
                 player ({:.2}, {:.2}, {:.2}) ground {} mode {}\n\
                 gate {} · pickups {}/3 · goal {}\n\
                 {}\n\
                 cam {} {}/{} m (obstruction {})\n\
                 move tick {} @ {} Hz · out {} · refused {}",
                presented as f64 / elapsed,
                math::percentile(&cpu_work_ms, 0.95),
                stats.device_type,
                stats.gpu_name,
                stats.api_version,
                state.position[0],
                state.position[1],
                state.position[2],
                state.on_ground,
                match state.camera_mode {
                    CameraMode::FirstPerson => "first",
                    CameraMode::ThirdPerson => "third",
                },
                if demo.gatehouse.gate_open() { "open" } else { "closed" },
                demo.gatehouse.collected_count(),
                if demo.gatehouse.complete.is_some() { "COMPLETE" } else { "pending" },
                target_line,
                match state.camera_mode {
                    CameraMode::FirstPerson => "first",
                    CameraMode::ThirdPerson => "third",
                },
                camera_distance,
                desired_distance,
                if camera_distance < desired_distance - 1e-3 { "pull-in" } else { "clear" },
                demo.driver.executed_ticks_total(),
                crate::player_demo::DEMO_TICK_HZ,
                demo.driver.outstanding_ticks(),
                demo.driver.refused_steps(),
            )
        });

        let frame_result = renderer
            .render_frame(
                &view_proj,
                &scene_vertices,
                overlay_text.as_deref(),
                app.window_size(),
            )
            .map_err(|error| format!("{error}"))?;

        if frame_result.deferred {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        if frame_result.presented {
            presented += 1;
            cpu_work_ms.push((frame_start.elapsed().as_secs_f64() * 1000.0).min(1000.0));
            let len = cpu_work_ms.len();
            if len > 600 {
                cpu_work_ms.drain(0..len - 600);
            }
        }
        if let Some(limit) = options.frames
            && presented >= limit
        {
            break;
        }
        if let Some(interval) = frame_interval {
            let elapsed = frame_start.elapsed();
            if elapsed < interval {
                std::thread::sleep(interval - elapsed);
            }
        }
    }

    let elapsed = start.elapsed();
    let state = demo.driver.state();
    println!("--- run summary ---");
    println!(
        "presented {} frames over {:.1}s ({:.1} fps avg)",
        presented,
        elapsed.as_secs_f64(),
        presented as f64 / elapsed.as_secs_f64().max(0.001)
    );
    println!(
        "cpu work (poll→present, excl. pacing sleep): p50 {:.2} ms · p95 {:.2} ms · p99 {:.2} ms",
        math::percentile(&cpu_work_ms, 0.50),
        math::percentile(&cpu_work_ms, 0.95),
        math::percentile(&cpu_work_ms, 0.99)
    );
    println!(
        "goal: {} · pickups {}/3 · switch {} · events {}",
        if demo.gatehouse.complete.is_some() {
            "COMPLETE"
        } else {
            "not reached"
        },
        demo.gatehouse.collected_count(),
        if demo.gatehouse.switch_on {
            "on"
        } else {
            "off"
        },
        demo.gatehouse.events.len(),
    );
    println!(
        "player final: pos ({:.3}, {:.3}, {:.3}) ground {} mode {}",
        state.position[0],
        state.position[1],
        state.position[2],
        state.on_ground,
        match state.camera_mode {
            CameraMode::FirstPerson => "first",
            CameraMode::ThirdPerson => "third",
        }
    );
    println!(
        "movement ticks: executed {} · outstanding {} · refused {}",
        demo.driver.executed_ticks_total(),
        demo.driver.outstanding_ticks(),
        demo.driver.refused_steps(),
    );
    println!(
        "controller state is transient (not persisted); offscreen CI runs are a \
         correctness smoke, not a substitute for a human-played run"
    );
    Ok(())
}
