//! Cycle 1 P3 — the reusable player movement and collision prototype.
//!
//! Kinematic, std-only, deterministic fixed-step mechanics: the first
//! player capability beyond the free-flight debug camera. This is a
//! REVERSIBLE PROTOTYPE and architectural-feedback instrument, not a
//! ratified production physics backend — the collision query surface
//! ([`CollisionWorld`]) is deliberately narrow so an established physics
//! library can slot behind the same contract in a later ratified cycle
//! (Rapier was the evaluated candidate; see the Cycle-1 P3 matrix).
//!
//! Determinism discipline (the spine's rules, mirrored): wall-clock time
//! never enters logical state — [`PlayerDriver`] accrues elapsed time
//! into due ticks exactly like `SimDriver` (bounded catch-up per frame,
//! no-discard accounting: due == executed + outstanding, always);
//! [`step_player`] is a function of state, intent, and the fixed tick
//! `dt` only. Traces are FNV-1a fingerprints over IEEE-754 bit patterns
//! under the NEW domain `engine-core/player-trace/1` — no historical
//! fingerprint domain is touched. Cross-platform bitwise determinism is
//! claimed ONLY for libm-free scripted workloads (yaw and pitch exactly
//! zero through the run, so `sin`/`cos` return their exact values and
//! only correctly-rounded arithmetic remains); look math at nonzero
//! angles is same-binary-same-platform reproducible, never claimed
//! stronger (charter rule: no unproven determinism claims).
//!
//! Engine/game boundary: this module owns MECHANICS — movement
//! integration, collision queries, camera-mode derivation. Game policy
//! (speeds, key bindings, world content, presentation) stays in the
//! application layer, which supplies [`PlayerConfig`] values.
//!
//! Collision model (prototype scope, documented): axis-aligned boxes
//! against a ground plane and optional world bounds; per-axis swept
//! resolution in a fixed Y→X→Z order with a [`SKIN`] contact margin, so
//! resting contact never blocks movement along a perpendicular axis and
//! no speed can tunnel through a wall thinner than one tick's travel.
//! Overlapping (spawned-inside) states depenetrate along the
//! minimal-penetration axis to the NEAR face, never through to the far
//! side. One depenetration pass per step (multi-box pile-ups are out of
//! prototype scope). The third-person camera has no collision of its
//! own (documented limitation).

use std::time::Duration;

use crate::fingerprint::Fingerprinter;

/// Contact separation skin: swept targets clamp this far short of a
/// face, so resting contact (overlap of exactly [`SKIN`]) never counts
/// as a perpendicular-axis block and the nonpenetration invariant has a
/// strict margin to rest on.
pub const SKIN: f32 = 1e-4;

/// Movement intent for one tick — the application maps its input
/// snapshots onto this; the engine never sees keys or devices.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MoveIntent {
    /// +1 forward, −1 backward (W/S axes cancel at the shell).
    pub forward: i8,
    /// +1 right, −1 left (D/A axes cancel at the shell).
    pub strafe: i8,
    /// A fresh jump press this tick (edge-triggered; holding is not a
    /// new press).
    pub jump_pressed: bool,
    pub sprint: bool,
    /// Look delta for this tick in radians: (yaw, pitch).
    pub look: (f32, f32),
}

/// Player mechanics parameters — policy values supplied by the
/// application layer (the engine/game boundary).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerConfig {
    /// Horizontal speed cap when walking (m/s).
    pub walk_speed: f32,
    /// Horizontal speed cap when sprinting (m/s).
    pub sprint_speed: f32,
    /// Downward acceleration (m/s²).
    pub gravity: f32,
    /// Jump impulse: the exact upward velocity a grounded jump sets.
    pub jump_speed: f32,
    /// Maximum downward speed (terminal velocity clamp).
    pub terminal_fall: f32,
    /// Player AABB height (m).
    pub height: f32,
    /// Player AABB half-extent in x and z (m).
    pub radius: f32,
    /// First-person eye height above the feet (m).
    pub eye_height: f32,
    /// Third-person camera distance behind the eye (m).
    pub tp_distance: f32,
    /// Third-person camera lift above the eye (m).
    pub tp_lift: f32,
    /// Pitch clamp (radians, symmetric).
    pub pitch_limit: f32,
}

impl Default for PlayerConfig {
    fn default() -> Self {
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
            pitch_limit: 1.54,
        }
    }
}

/// One axis-aligned static collider. `min`/`max` are the box corners.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StaticCollider {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl StaticCollider {
    /// A box from a center-bottom base, a full footprint width, and a
    /// height — the same parameters `scene.rs`'s cube builders use, so
    /// the collidable world can mirror the visible one box for box.
    pub fn from_base(base: [f32; 3], footprint: f32, height: f32) -> StaticCollider {
        let half = footprint / 2.0;
        StaticCollider {
            min: [base[0] - half, base[1], base[2] - half],
            max: [base[0] + half, base[1] + height, base[2] + half],
        }
    }
}

/// The static collision world: boxes, a ground plane, optional bounds.
/// This is the reusable query contract — a future physics backend
/// implements the same queries.
#[derive(Debug, Clone, Default)]
pub struct CollisionWorld {
    colliders: Vec<StaticCollider>,
    bounds: Option<StaticCollider>,
    ground_y: f32,
}

impl CollisionWorld {
    /// A world whose floor sits at `ground_y`.
    pub fn new(ground_y: f32) -> CollisionWorld {
        CollisionWorld {
            colliders: Vec::new(),
            bounds: None,
            ground_y,
        }
    }

    /// Adds one box (see [`StaticCollider::from_base`]).
    pub fn with_box(mut self, base: [f32; 3], footprint: f32, height: f32) -> CollisionWorld {
        self.colliders
            .push(StaticCollider::from_base(base, footprint, height));
        self
    }

    /// Adds movement bounds: the player box stays inside this region,
    /// clamped at the faces with outward velocity zeroed.
    pub fn with_bounds(mut self, min: [f32; 3], max: [f32; 3]) -> CollisionWorld {
        self.bounds = Some(StaticCollider { min, max });
        self
    }

    /// Adds a collider in general (rectangular) form — walls and other
    /// non-square geometry the square-footprint builder cannot express.
    pub fn with_collider(mut self, collider: StaticCollider) -> CollisionWorld {
        self.colliders.push(collider);
        self
    }

    pub fn colliders(&self) -> &[StaticCollider] {
        &self.colliders
    }

    pub fn bounds(&self) -> Option<&StaticCollider> {
        self.bounds.as_ref()
    }

    pub fn ground_y(&self) -> f32 {
        self.ground_y
    }

    /// Whether the given box strictly overlaps any collider (the
    /// nonpenetration check; contact at the [`SKIN`] margin is clear).
    pub fn box_overlaps(&self, min: [f32; 3], max: [f32; 3]) -> bool {
        self.colliders
            .iter()
            .any(|col| (0..3).all(|a| min[a] < col.max[a] && max[a] > col.min[a]))
    }

    /// Swept movement along one axis: the largest displacement not
    /// exceeding `delta` that keeps the player box out of every
    /// collider (and above the ground plane for downward moves), plus
    /// whether anything clamped it. Perpendicular axes use a [`SKIN`]
    /// overlap margin, so resting contact never blocks movement along
    /// the moving axis. The clamp targets sit [`SKIN`] short of each
    /// face, which is what makes tunneling impossible at any speed:
    /// the box stops before crossing, it never passes through.
    pub fn sweep_axis(
        &self,
        player_min: [f32; 3],
        player_max: [f32; 3],
        axis: usize,
        delta: f32,
    ) -> (f32, bool) {
        if delta == 0.0 {
            return (0.0, false);
        }
        let mut allowed = delta;
        let mut hit = false;
        if axis == 1 && delta < 0.0 {
            let floor = self.ground_y - player_min[1];
            if floor > allowed {
                allowed = floor;
                hit = true;
            }
        }
        for col in &self.colliders {
            let mut overlaps_perpendicular = true;
            for o in 0..3 {
                if o == axis {
                    continue;
                }
                if player_min[o] >= col.max[o] - SKIN || player_max[o] <= col.min[o] + SKIN {
                    overlaps_perpendicular = false;
                    break;
                }
            }
            if !overlaps_perpendicular {
                continue;
            }
            if delta > 0.0 && player_max[axis] <= col.min[axis] {
                let face = col.min[axis] - SKIN - player_max[axis];
                if face < allowed {
                    allowed = face;
                    hit = true;
                }
            } else if delta < 0.0 && player_min[axis] >= col.max[axis] {
                let face = col.max[axis] + SKIN - player_min[axis];
                if face > allowed {
                    allowed = face;
                    hit = true;
                }
            }
        }
        (allowed, hit)
    }

    /// Minimal-axis depenetration: if the box overlaps a collider, the
    /// smallest push that clears it — to the NEAR face (never through
    /// to the far side), [`SKIN`] short of it. Deterministic: candidate
    /// magnitudes compare with pure +/− arithmetic (cross-platform
    /// stable) and the first strictly-smaller candidate wins, so
    /// mathematically equal pushes resolve by iteration order —
    /// identically on every run and platform.
    pub fn depenetration(&self, player_min: [f32; 3], player_max: [f32; 3]) -> Option<[f32; 3]> {
        let mut best: Option<(usize, f32, f32)> = None;
        for col in &self.colliders {
            let overlapping =
                (0..3).all(|a| player_min[a] < col.max[a] && player_max[a] > col.min[a]);
            if !overlapping {
                continue;
            }
            for axis in 0..3 {
                // Exiting through the min face moves the player's MAX
                // face to it; through the max face, the player's MIN —
                // the pushed-out box ends fully clear of the collider,
                // never merely re-aligned with the near face.
                let push_neg = col.min[axis] - SKIN - player_max[axis];
                let push_pos = col.max[axis] + SKIN - player_min[axis];
                // The ground plane is solid: a downward push that would
                // cross it is not an escape (the ground clamp would push
                // the player straight back into the box), so the
                // negative candidate is invalid on the vertical axis.
                let neg_valid = !(axis == 1 && player_min[1] + push_neg < self.ground_y);
                let (push, magnitude) = if neg_valid && push_neg.abs() <= push_pos {
                    (push_neg, push_neg.abs())
                } else {
                    (push_pos, push_pos.abs())
                };
                if best.is_none_or(|(_, _, best_magnitude)| magnitude < best_magnitude) {
                    best = Some((axis, push, magnitude));
                }
            }
        }
        best.map(|(axis, push, _)| {
            let mut vector = [0.0f32; 3];
            vector[axis] = push;
            vector
        })
    }

    /// Clamps the feet-center position so the player box stays inside
    /// the bounds; returns which axes clamped (outward velocity is the
    /// caller's to zero — the boundary behavior).
    pub fn clamp_to_bounds(&self, position: &mut [f32; 3], config: &PlayerConfig) -> [bool; 3] {
        let Some(bounds) = &self.bounds else {
            return [false; 3];
        };
        let mut clamped = [false; 3];
        for axis in [0, 2] {
            let lo = bounds.min[axis] + config.radius;
            let hi = bounds.max[axis] - config.radius;
            if position[axis] < lo {
                position[axis] = lo;
                clamped[axis] = true;
            }
            if position[axis] > hi {
                position[axis] = hi;
                clamped[axis] = true;
            }
        }
        if position[1] < bounds.min[1] {
            position[1] = bounds.min[1];
            clamped[1] = true;
        }
        if position[1] > bounds.max[1] - config.height {
            position[1] = bounds.max[1] - config.height;
            clamped[1] = true;
        }
        clamped
    }
}

/// The player's authoritative kinematic state. Independent of camera
/// placement: camera poses are DERIVED from this (see [`camera_pose`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerState {
    /// Feet-center position.
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub camera_mode: CameraMode,
}

impl PlayerState {
    pub fn spawn(position: [f32; 3]) -> PlayerState {
        PlayerState {
            position,
            velocity: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
            camera_mode: CameraMode::FirstPerson,
        }
    }

    /// Toggles the camera mode — single-call semantics (one toggle per
    /// key press edge; the application's job). Player state is
    /// otherwise untouched: switching cameras never teleports or resets
    /// the player (the charter invariant, proven by test).
    pub fn toggle_camera_mode(&mut self) {
        self.camera_mode = match self.camera_mode {
            CameraMode::FirstPerson => CameraMode::ThirdPerson,
            CameraMode::ThirdPerson => CameraMode::FirstPerson,
        };
    }
}

/// Camera mode: which derived view [`camera_pose`] produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraMode {
    FirstPerson,
    ThirdPerson,
}

/// A derived camera pose — pure function of player state and config.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraPose {
    pub position: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    /// Always zero in this prototype (the free-flight roll input stays
    /// with the debug camera; the player camera has no roll).
    pub roll: f32,
}

/// The look direction for yaw/pitch (Ry·Rx applied to −Z, matching the
/// application camera's convention: identity at zero angles).
pub fn look_direction(yaw: f32, pitch: f32) -> [f32; 3] {
    let (sin_yaw, cos_yaw) = (yaw.sin(), yaw.cos());
    let (sin_pitch, cos_pitch) = (pitch.sin(), pitch.cos());
    [-cos_pitch * sin_yaw, sin_pitch, -cos_pitch * cos_yaw]
}

/// The camera pose derived from the player state: first person puts
/// the eye at the player's eye height; third person pulls it back
/// along the look direction with a lift. The third-person camera has
/// NO collision handling (documented prototype limitation).
pub fn camera_pose(state: &PlayerState, config: &PlayerConfig) -> CameraPose {
    let eye = [
        state.position[0],
        state.position[1] + config.eye_height,
        state.position[2],
    ];
    match state.camera_mode {
        CameraMode::FirstPerson => CameraPose {
            position: eye,
            yaw: state.yaw,
            pitch: state.pitch,
            roll: 0.0,
        },
        CameraMode::ThirdPerson => {
            let forward = look_direction(state.yaw, state.pitch);
            CameraPose {
                position: [
                    eye[0] - forward[0] * config.tp_distance,
                    eye[1] - forward[1] * config.tp_distance + config.tp_lift,
                    eye[2] - forward[2] * config.tp_distance,
                ],
                yaw: state.yaw,
                pitch: state.pitch,
                roll: 0.0,
            }
        }
    }
}

/// Observable per-step outcome — the fail-visible surface for input
/// sanitization (F5): non-finite look deltas are zeroed AND counted;
/// a non-finite state is refused AND flagged. Never silent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StepOutcome {
    pub jumped: bool,
    /// Ground contact regained this step (was airborne).
    pub landed: bool,
    pub hit_x: bool,
    pub hit_z: bool,
    /// Upward motion clamped by a collider underside.
    pub hit_ceiling: bool,
    /// Downward motion clamped by ground or a box top.
    pub hit_ground: bool,
    /// Non-finite look-delta components zeroed this step.
    pub sanitized_look: u32,
    /// The step was refused: the incoming state was non-finite and is
    /// left untouched for the caller to handle.
    pub refused_nonfinite_state: bool,
}

/// The player's AABB (feet-center position, config extents).
pub fn player_box(position: [f32; 3], config: &PlayerConfig) -> ([f32; 3], [f32; 3]) {
    let r = config.radius;
    (
        [position[0] - r, position[1], position[2] - r],
        [
            position[0] + r,
            position[1] + config.height,
            position[2] + r,
        ],
    )
}

fn state_is_finite(state: &PlayerState) -> bool {
    state
        .position
        .iter()
        .chain(state.velocity.iter())
        .all(|v| v.is_finite())
        && state.yaw.is_finite()
        && state.pitch.is_finite()
}

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

/// One fixed-dt movement step. Order: refuse non-finite state (nothing
/// mutated); sanitize and apply look; set horizontal velocity from the
/// intent in the yaw frame (diagonal normalized to the speed cap);
/// gravity (terminal-clamped), then a grounded jump's exact impulse;
/// depenetrate; swept per-axis moves Y→X→Z with velocity zeroed on the
/// clamped axis; bounds clamp. `on_ground` is true exactly when the
/// downward move clamped this step — resting contact re-clamps every
/// tick, so the resting state is a stable fixed point.
pub fn step_player(
    state: &mut PlayerState,
    config: &PlayerConfig,
    world: &CollisionWorld,
    intent: &MoveIntent,
    dt: f32,
) -> StepOutcome {
    let mut outcome = StepOutcome::default();

    if !state_is_finite(state) {
        outcome.refused_nonfinite_state = true;
        return outcome;
    }

    if !intent.look.0.is_finite() {
        outcome.sanitized_look += 1;
    }
    if !intent.look.1.is_finite() {
        outcome.sanitized_look += 1;
    }
    let look = (finite_or_zero(intent.look.0), finite_or_zero(intent.look.1));
    state.yaw += look.0;
    state.pitch = (state.pitch + look.1).clamp(-config.pitch_limit, config.pitch_limit);

    let forward_input = f32::from(intent.forward);
    let strafe_input = f32::from(intent.strafe);
    let speed = if intent.sprint {
        config.sprint_speed
    } else {
        config.walk_speed
    };
    let (sin_yaw, cos_yaw) = (state.yaw.sin(), state.yaw.cos());
    let mut dir_x = -sin_yaw * forward_input + cos_yaw * strafe_input;
    let mut dir_z = -cos_yaw * forward_input - sin_yaw * strafe_input;
    let magnitude_sq = dir_x * dir_x + dir_z * dir_z;
    if magnitude_sq > 1.0 {
        let scale = speed / magnitude_sq.sqrt();
        dir_x *= scale;
        dir_z *= scale;
    } else {
        dir_x *= speed;
        dir_z *= speed;
    }
    state.velocity[0] = dir_x;
    state.velocity[2] = dir_z;

    state.velocity[1] = (state.velocity[1] - config.gravity * dt).max(-config.terminal_fall);
    if intent.jump_pressed && state.on_ground {
        state.velocity[1] = config.jump_speed;
        outcome.jumped = true;
    }

    let (min, max) = player_box(state.position, config);
    if let Some(push) = world.depenetration(min, max) {
        for (position, component) in state.position.iter_mut().zip(push) {
            *position += component;
        }
    }

    let was_on_ground = state.on_ground;
    state.on_ground = false;

    let (min, max) = player_box(state.position, config);
    let (dy, hit_y) = world.sweep_axis(min, max, 1, state.velocity[1] * dt);
    state.position[1] += dy;
    if hit_y {
        if state.velocity[1] < 0.0 {
            state.on_ground = true;
            outcome.hit_ground = true;
            if !was_on_ground {
                outcome.landed = true;
            }
        } else {
            outcome.hit_ceiling = true;
        }
        state.velocity[1] = 0.0;
    }

    let (min, max) = player_box(state.position, config);
    let (dx, hit_x) = world.sweep_axis(min, max, 0, state.velocity[0] * dt);
    state.position[0] += dx;
    if hit_x {
        outcome.hit_x = true;
        state.velocity[0] = 0.0;
    }

    let (min, max) = player_box(state.position, config);
    let (dz, hit_z) = world.sweep_axis(min, max, 2, state.velocity[2] * dt);
    state.position[2] += dz;
    if hit_z {
        outcome.hit_z = true;
        state.velocity[2] = 0.0;
    }

    let clamped = world.clamp_to_bounds(&mut state.position, config);
    if clamped[0] {
        outcome.hit_x = true;
        state.velocity[0] = 0.0;
    }
    if clamped[1] {
        if state.velocity[1] < 0.0 {
            state.on_ground = true;
            outcome.hit_ground = true;
            if !was_on_ground {
                outcome.landed = true;
            }
        }
        state.velocity[1] = 0.0;
    }
    if clamped[2] {
        outcome.hit_z = true;
        state.velocity[2] = 0.0;
    }

    outcome
}

/// One recorded tick of the deterministic trace.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TraceEntry {
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub on_ground: bool,
    pub camera_mode: CameraMode,
}

/// One `advance` call's outcome (the no-discard accounting view).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriveStep {
    pub executed_ticks: u32,
    /// Due ticks retained (not yet executed) — a reported obligation,
    /// never silently discarded; drains when catch-up capacity allows.
    pub outstanding_ticks: u32,
}

/// Fixed-step movement driver — the SimDriver discipline applied to
/// the player: elapsed monotonic time decides WHEN ticks are due;
/// logical state ever sees only the fixed tick. `advance` is the
/// frame-sampled live-input path (every tick in one frame's catch-up
/// batch consumes the same live intent sample — a genuinely
/// cadence-dependent external input, exactly like camera presence; no
/// cross-cadence determinism is claimed through it). `run_headless`
/// is the scripted deterministic path: the intent is a function of the
/// logical tick index, so trace history is cadence-independent.
pub struct PlayerDriver {
    state: PlayerState,
    config: PlayerConfig,
    world: CollisionWorld,
    dt: f32,
    period_ns: u128,
    max_catchup_per_frame: u32,
    accumulator_ns: u128,
    total_advanced_ns: u128,
    headless_ticks: u64,
    executed_ticks: u64,
    record_trace: bool,
    trace: Vec<TraceEntry>,
}

impl PlayerDriver {
    pub fn new(
        state: PlayerState,
        config: PlayerConfig,
        world: CollisionWorld,
        tick_hz: u32,
    ) -> PlayerDriver {
        Self::with_catchup(state, config, world, tick_hz, 8)
    }

    pub fn with_catchup(
        state: PlayerState,
        config: PlayerConfig,
        world: CollisionWorld,
        tick_hz: u32,
        max_catchup_per_frame: u32,
    ) -> PlayerDriver {
        let rate = tick_hz.max(1);
        PlayerDriver {
            state,
            config,
            world,
            dt: 1.0 / (rate as f32),
            period_ns: 1_000_000_000u128 / u128::from(rate),
            max_catchup_per_frame: max_catchup_per_frame.max(1),
            accumulator_ns: 0,
            total_advanced_ns: 0,
            headless_ticks: 0,
            executed_ticks: 0,
            record_trace: true,
            trace: Vec::new(),
        }
    }

    pub fn state(&self) -> &PlayerState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut PlayerState {
        &mut self.state
    }

    pub fn config(&self) -> &PlayerConfig {
        &self.config
    }

    pub fn world(&self) -> &CollisionWorld {
        &self.world
    }

    pub fn trace(&self) -> &[TraceEntry] {
        &self.trace
    }

    pub fn set_trace_recording(&mut self, on: bool) {
        self.record_trace = on;
    }

    /// Advances by elapsed monotonic time (frame-sampled live intent).
    pub fn advance(&mut self, elapsed: Duration, intent: MoveIntent) -> DriveStep {
        let nanos = elapsed.as_nanos();
        self.total_advanced_ns += nanos;
        self.accumulator_ns += nanos;
        let due = (self.accumulator_ns / self.period_ns) as u32;
        let executed = due.min(self.max_catchup_per_frame);
        for _ in 0..executed {
            self.tick(intent);
        }
        self.accumulator_ns -= u128::from(executed) * self.period_ns;
        DriveStep {
            executed_ticks: executed,
            outstanding_ticks: (self.accumulator_ns / self.period_ns) as u32,
        }
    }

    /// Runs exactly `ticks` fixed ticks with scripted intents — the
    /// headless deterministic path (`intent_at` receives the global
    /// headless tick index).
    pub fn run_headless(&mut self, ticks: u64, intent_at: impl Fn(u64) -> MoveIntent) {
        for _ in 0..ticks {
            let intent = intent_at(self.headless_ticks);
            self.headless_ticks += 1;
            self.tick(intent);
        }
    }

    fn tick(&mut self, intent: MoveIntent) {
        step_player(&mut self.state, &self.config, &self.world, &intent, self.dt);
        self.executed_ticks += 1;
        if self.record_trace {
            self.trace.push(TraceEntry {
                position: self.state.position,
                velocity: self.state.velocity,
                on_ground: self.state.on_ground,
                camera_mode: self.state.camera_mode,
            });
        }
    }

    /// Ticks due from every source: elapsed-time advancement (executed
    /// or outstanding) plus headless execution.
    pub fn ticks_due_total(&self) -> u64 {
        ((self.total_advanced_ns / self.period_ns) as u64) + self.headless_ticks
    }

    pub fn executed_ticks_total(&self) -> u64 {
        self.executed_ticks
    }

    pub fn outstanding_ticks(&self) -> u32 {
        (self.accumulator_ns / self.period_ns) as u32
    }

    /// FNV-1a over the recorded trace (bit patterns, domain
    /// `engine-core/player-trace/1`). Empty when trace recording is
    /// off.
    pub fn trace_fingerprint_hex(&self) -> String {
        let mut fp = Fingerprinter::new();
        fp.write_str("engine-core/player-trace/1");
        fp.write_usize(self.trace.len());
        for entry in &self.trace {
            for value in entry.position {
                fp.write_u32(value.to_bits());
            }
            for value in entry.velocity {
                fp.write_u32(value.to_bits());
            }
            fp.write_u8(u8::from(entry.on_ground));
            fp.write_u8(match entry.camera_mode {
                CameraMode::FirstPerson => 1,
                CameraMode::ThirdPerson => 2,
            });
        }
        fp.finish_hex()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn bits(values: [f32; 3]) -> [u32; 3] {
        values.map(f32::to_bits)
    }

    fn idle() -> MoveIntent {
        MoveIntent::default()
    }

    /// Runs idle steps until grounded (bounded — a fall that never
    /// lands must fail the test, not hang it).
    fn settle(state: &mut PlayerState, config: &PlayerConfig, world: &CollisionWorld) {
        for _ in 0..600 {
            step_player(state, config, world, &idle(), DT);
            if state.on_ground {
                return;
            }
        }
        panic!("the player never landed within 600 ticks");
    }

    #[test]
    fn axes_map_to_world_velocity() {
        let config = PlayerConfig::default();
        let world = CollisionWorld::new(0.0);

        let mut state = PlayerState::spawn([0.0, 5.0, 0.0]);
        step_player(
            &mut state,
            &config,
            &world,
            &MoveIntent {
                forward: 1,
                ..idle()
            },
            DT,
        );
        assert_eq!(state.velocity[0], 0.0);
        assert_eq!(state.velocity[2], -config.walk_speed, "W moves -Z at yaw 0");

        let mut state = PlayerState::spawn([0.0, 5.0, 0.0]);
        step_player(
            &mut state,
            &config,
            &world,
            &MoveIntent {
                strafe: 1,
                ..idle()
            },
            DT,
        );
        assert_eq!(state.velocity[0], config.walk_speed, "D moves +X at yaw 0");
        assert_eq!(state.velocity[2], 0.0);

        let mut state = PlayerState::spawn([0.0, 5.0, 0.0]);
        step_player(&mut state, &config, &world, &idle(), DT);
        assert_eq!(
            state.velocity[0], 0.0,
            "W and S cancel at the shell; zero intent means zero motion"
        );
        assert_eq!(state.velocity[2], 0.0);

        // Nonzero yaw: same-binary reproducibility (the trig path is
        // deliberately unpinned — see test 16 and the F1 amendment).
        let mut first = PlayerState::spawn([0.0, 5.0, 0.0]);
        first.yaw = 0.5;
        let mut second = first;
        let intent = MoveIntent {
            forward: 1,
            strafe: 1,
            ..idle()
        };
        step_player(&mut first, &config, &world, &intent, DT);
        step_player(&mut second, &config, &world, &intent, DT);
        assert_eq!(bits(first.velocity), bits(second.velocity));
    }

    #[test]
    fn horizontal_speed_capped() {
        let config = PlayerConfig::default();
        let world = CollisionWorld::new(0.0);
        let diagonal = MoveIntent {
            forward: 1,
            strafe: 1,
            ..idle()
        };

        let mut state = PlayerState::spawn([0.0, 50.0, 0.0]);
        step_player(&mut state, &config, &world, &diagonal, DT);
        let magnitude =
            (state.velocity[0] * state.velocity[0] + state.velocity[2] * state.velocity[2]).sqrt();
        assert!(
            (magnitude - config.walk_speed).abs() < 1e-5,
            "diagonal input is normalized to the cap: {magnitude}"
        );

        let mut state = PlayerState::spawn([0.0, 50.0, 0.0]);
        step_player(
            &mut state,
            &config,
            &world,
            &MoveIntent {
                forward: 1,
                sprint: true,
                ..idle()
            },
            DT,
        );
        assert_eq!(state.velocity[2], -config.sprint_speed);
    }

    #[test]
    fn gravity_bounded_by_terminal_velocity() {
        let config = PlayerConfig::default();
        let world = CollisionWorld::new(0.0);
        let mut state = PlayerState::spawn([0.0, 1000.0, 0.0]);
        for _ in 0..300 {
            step_player(&mut state, &config, &world, &idle(), DT);
        }
        assert_eq!(
            state.velocity[1], -config.terminal_fall,
            "free fall clamps at the terminal velocity"
        );
        assert!(!state.on_ground);
        assert!(state.position[1] > 0.0, "still airborne: no floor reached");
    }

    #[test]
    fn jump_requires_ground_and_fresh_press() {
        let config = PlayerConfig::default();
        let world = CollisionWorld::new(0.0);
        let mut airborne_press = PlayerState::spawn([0.0, 10.0, 0.0]);
        step_player(&mut airborne_press, &config, &world, &idle(), DT);
        let falling = airborne_press.velocity[1];
        step_player(
            &mut airborne_press,
            &config,
            &world,
            &MoveIntent {
                jump_pressed: true,
                ..idle()
            },
            DT,
        );
        assert!(
            airborne_press.velocity[1] < 0.0 && airborne_press.velocity[1] < falling,
            "an airborne jump press grants no impulse"
        );

        let mut state = PlayerState::spawn([0.0, 0.0, 0.0]);
        settle(&mut state, &config, &world);
        let mut outcome = StepOutcome::default();
        for _ in 0..3 {
            outcome = step_player(
                &mut state,
                &config,
                &world,
                &MoveIntent {
                    jump_pressed: true,
                    ..idle()
                },
                DT,
            );
        }
        assert!(
            !outcome.jumped,
            "a held key is not a fresh press: only the first tick jumps"
        );
    }

    #[test]
    fn jump_settles_stably() {
        let config = PlayerConfig::default();
        let world = CollisionWorld::new(0.0);
        let mut state = PlayerState::spawn([0.0, 0.0, 0.0]);
        settle(&mut state, &config, &world);

        let outcome = step_player(
            &mut state,
            &config,
            &world,
            &MoveIntent {
                jump_pressed: true,
                ..idle()
            },
            DT,
        );
        assert!(outcome.jumped);
        assert_eq!(state.velocity[1], config.jump_speed, "the exact impulse");

        for _ in 0..200 {
            step_player(&mut state, &config, &world, &idle(), DT);
            if state.on_ground {
                break;
            }
        }
        assert!(state.on_ground, "the jump lands");
        assert_eq!(state.velocity[1], 0.0);

        let resting = bits(state.position);
        let resting_velocity = bits(state.velocity);
        for _ in 0..50 {
            step_player(&mut state, &config, &world, &idle(), DT);
        }
        assert_eq!(
            bits(state.position),
            resting,
            "the resting state is a fixed point — no drift"
        );
        assert_eq!(bits(state.velocity), resting_velocity);
    }

    #[test]
    fn ground_contact_box_top_and_plane() {
        let config = PlayerConfig::default();
        let world = CollisionWorld::new(0.0).with_box([3.0, 0.0, 0.0], 2.0, 2.0);
        let mut state = PlayerState::spawn([3.0, 5.0, 0.0]);
        settle(&mut state, &config, &world);
        assert!(state.on_ground);
        assert_eq!(
            state.position[1],
            2.0 + SKIN,
            "box-top contact rests a skin above the face"
        );

        // Walk off the box (resting contact must not block the walk).
        let walk = MoveIntent {
            strafe: 1,
            ..idle()
        };
        let mut left_ground = false;
        for _ in 0..80 {
            step_player(&mut state, &config, &world, &walk, DT);
            if !state.on_ground {
                left_ground = true;
                break;
            }
        }
        assert!(left_ground, "walking past the edge starts a fall");
        settle(&mut state, &config, &world);
        assert_eq!(state.position[1], 0.0, "the ground plane holds at y=0");
    }

    #[test]
    fn nonpenetration_holds_after_resolution() {
        let config = PlayerConfig::default();
        let world = CollisionWorld::new(0.0).with_box([0.0, 0.0, -5.0], 2.0, 3.0);
        let mut state = PlayerState::spawn([0.0, 0.0, 0.0]);
        settle(&mut state, &config, &world);

        let walk = MoveIntent {
            forward: 1,
            ..idle()
        };
        for _ in 0..200 {
            step_player(&mut state, &config, &world, &walk, DT);
        }
        let (min, max) = player_box(state.position, &config);
        assert!(
            !world.box_overlaps(min, max),
            "no interpenetration after clamping"
        );
        assert_eq!(
            state.position[2],
            -4.0 + SKIN + config.radius,
            "the wall stops the player a skin short of its near face"
        );

        // Adversarial: spawned INSIDE a tall box, off-center so the
        // minimal push is unambiguously +X (the downward candidate
        // would cross the solid ground plane and is invalid; a centered
        // spawn makes x and z mathematically tie, resolving by ulp —
        // correct but not a stable expectation to pin).
        let world = CollisionWorld::new(0.0).with_box([0.0, 0.0, -5.0], 2.0, 5.0);
        let mut inside = PlayerState::spawn([0.5, 2.0, -5.0]);
        step_player(&mut inside, &config, &world, &idle(), DT);
        let (min, max) = player_box(inside.position, &config);
        assert!(!world.box_overlaps(min, max));
        assert!(
            inside.position[0] > 1.0 && inside.position[0] < 1.5,
            "pushed out to the near (+X) face, minimally: {}",
            inside.position[0]
        );
        assert!(inside.position[1] < 2.0, "the escape does not hover");
        assert!(!inside.on_ground, "still airborne after the push-out");
    }

    #[test]
    fn no_tunneling_at_max_speed() {
        let config = PlayerConfig::default();
        // A wall thinner than one sprint tick's travel (0.2 m).
        let world = CollisionWorld::new(0.0).with_box([0.0, 0.0, -5.0], 0.2, 3.0);
        let mut state = PlayerState::spawn([0.0, 0.0, 0.0]);
        settle(&mut state, &config, &world);

        let sprint = MoveIntent {
            forward: 1,
            sprint: true,
            ..idle()
        };
        for _ in 0..100 {
            step_player(&mut state, &config, &world, &sprint, DT);
        }
        assert_eq!(
            state.position[2],
            -4.9 + SKIN + config.radius,
            "the swept clamp stops the player at the near face — a wall \
             thinner than one tick's travel cannot be tunneled"
        );
        let (min, max) = player_box(state.position, &config);
        assert!(!world.box_overlaps(min, max));
    }

    #[test]
    fn wall_collision_zeroes_axis_velocity() {
        let config = PlayerConfig::default();
        // A wall long enough that the slide never passes its end.
        let world = CollisionWorld::new(0.0).with_collider(StaticCollider {
            min: [-20.0, 0.0, -6.0],
            max: [20.0, 3.0, -4.0],
        });
        let mut state = PlayerState::spawn([0.0, 0.0, 0.0]);
        settle(&mut state, &config, &world);

        let diagonal = MoveIntent {
            forward: 1,
            strafe: 1,
            ..idle()
        };
        for _ in 0..200 {
            step_player(&mut state, &config, &world, &diagonal, DT);
        }
        assert_eq!(state.velocity[2], 0.0, "the blocked axis zeroes");
        assert!(
            state.velocity[0] > 0.0,
            "the unblocked axis keeps sliding along the wall"
        );
        assert!(state.position[0] > 1.0, "the slide actually moved");
    }

    #[test]
    fn ceiling_blocks_ascent() {
        let config = PlayerConfig::default();
        // An overhang the jump reaches (apex head height 2.98 > 2.5).
        let world = CollisionWorld::new(0.0).with_box([0.0, 2.5, 0.0], 2.0, 2.0);
        let mut state = PlayerState::spawn([0.0, 0.0, 0.0]);
        settle(&mut state, &config, &world);

        let mut hit_ceiling = false;
        let outcome = step_player(
            &mut state,
            &config,
            &world,
            &MoveIntent {
                jump_pressed: true,
                ..idle()
            },
            DT,
        );
        assert!(outcome.jumped);
        for _ in 0..200 {
            let outcome = step_player(&mut state, &config, &world, &idle(), DT);
            hit_ceiling |= outcome.hit_ceiling;
            if state.on_ground {
                break;
            }
        }
        assert!(hit_ceiling, "the overhang clamps the ascent");
        assert!(state.on_ground, "gravity resumes after the ceiling hit");
        assert_eq!(state.velocity[1], 0.0, "the ceiling zeroed the ascent");
    }

    #[test]
    fn corner_collision_stable() {
        let config = PlayerConfig::default();
        // An inside corner of long walls: one spanning z blocks +X,
        // one spanning x blocks +Z — the diagonal cannot slip past
        // either end.
        let world = CollisionWorld::new(0.0)
            .with_collider(StaticCollider {
                min: [2.0, 0.0, -6.0],
                max: [4.0, 3.0, 6.0],
            })
            .with_collider(StaticCollider {
                min: [-6.0, 0.0, 2.0],
                max: [6.0, 3.0, 4.0],
            });
        let mut state = PlayerState::spawn([0.0, 0.0, 0.0]);
        settle(&mut state, &config, &world);

        // backward (+Z) plus strafe (+X): diagonally into the corner.
        let into_corner = MoveIntent {
            forward: -1,
            strafe: 1,
            ..idle()
        };
        for _ in 0..100 {
            step_player(&mut state, &config, &world, &into_corner, DT);
        }
        assert!(
            state.position[0] > 1.5 && state.position[0] < 2.0,
            "clamped at the x wall's near face: {}",
            state.position[0]
        );
        assert!(
            state.position[2] > 1.5 && state.position[2] < 2.0,
            "clamped at the z wall's near face: {}",
            state.position[2]
        );
        let corner = bits(state.position);
        for _ in 0..20 {
            step_player(&mut state, &config, &world, &into_corner, DT);
        }
        assert_eq!(
            bits(state.position),
            corner,
            "multi-axis contact is a stable fixed point — no jitter"
        );
        let (min, max) = player_box(state.position, &config);
        assert!(!world.box_overlaps(min, max));
    }

    #[test]
    fn world_bounds_clamp() {
        let config = PlayerConfig::default();
        let world = CollisionWorld::new(0.0).with_bounds([-10.0, 0.0, -10.0], [10.0, 50.0, 10.0]);
        let mut state = PlayerState::spawn([0.0, 0.0, 0.0]);
        settle(&mut state, &config, &world);

        let sprint_diagonal = MoveIntent {
            forward: -1,
            strafe: 1,
            sprint: true,
            ..idle()
        };
        for _ in 0..300 {
            step_player(&mut state, &config, &world, &sprint_diagonal, DT);
        }
        assert_eq!(state.position[0], 10.0 - config.radius);
        assert_eq!(state.position[2], 10.0 - config.radius);
        assert_eq!(state.velocity[0], 0.0, "outward velocity zeroed at bounds");
        assert_eq!(state.velocity[2], 0.0);
        assert!(state.on_ground);
    }

    #[test]
    fn nonfinite_intent_sanitized() {
        let config = PlayerConfig::default();
        let world = CollisionWorld::new(0.0);
        let mut state = PlayerState::spawn([0.0, 0.0, 0.0]);
        settle(&mut state, &config, &world);
        let before = (state.yaw.to_bits(), state.pitch.to_bits());

        let outcome = step_player(
            &mut state,
            &config,
            &world,
            &MoveIntent {
                look: (f32::NAN, f32::INFINITY),
                ..idle()
            },
            DT,
        );
        assert_eq!(outcome.sanitized_look, 2, "the F5 observable surface");
        assert!(!outcome.refused_nonfinite_state);
        assert_eq!(
            (state.yaw.to_bits(), state.pitch.to_bits()),
            before,
            "non-finite look deltas are dropped, not applied"
        );

        let mut corrupt = PlayerState::spawn([f32::NAN, 0.0, 0.0]);
        let position_before = bits(corrupt.position);
        let velocity_before = bits(corrupt.velocity);
        let outcome = step_player(&mut corrupt, &config, &world, &idle(), DT);
        assert!(
            outcome.refused_nonfinite_state,
            "a non-finite state is refused, visibly"
        );
        assert_eq!(bits(corrupt.position), position_before);
        assert_eq!(bits(corrupt.velocity), velocity_before);
    }

    #[test]
    fn fixed_step_batch_equivalence() {
        // 100 Hz: the period divides a second exactly, so 64 ticks are
        // 640 ms — the batch, the drip, and the headless run cover the
        // same due ticks (within the catch-up bound; the over-bound
        // case belongs to no_discard_accounting below — F3).
        let make = || {
            PlayerDriver::with_catchup(
                PlayerState::spawn([0.0, 0.0, 0.0]),
                PlayerConfig::default(),
                CollisionWorld::new(0.0).with_box([0.0, 0.0, -2.0], 1.0, 1.0),
                100,
                64,
            )
        };
        let intent = MoveIntent {
            forward: 1,
            ..idle()
        };

        let mut batched = make();
        batched.advance(Duration::from_nanos(640_000_000), intent);

        let mut dripped = make();
        for _ in 0..64 {
            dripped.advance(Duration::from_nanos(10_000_000), intent);
        }

        let mut headless = make();
        headless.run_headless(64, |_| intent);

        assert_eq!(batched.ticks_due_total(), 64);
        assert_eq!(batched.executed_ticks_total(), 64);
        assert_eq!(dripped.executed_ticks_total(), 64);
        assert_eq!(headless.executed_ticks_total(), 64);
        assert_eq!(
            bits(batched.state().position),
            bits(dripped.state().position),
            "batching does not change logical state"
        );
        assert_eq!(
            bits(batched.state().position),
            bits(headless.state().position),
            "the headless path is the same fixed step"
        );
        assert_eq!(
            batched.trace_fingerprint_hex(),
            dripped.trace_fingerprint_hex()
        );
        assert_eq!(
            batched.trace_fingerprint_hex(),
            headless.trace_fingerprint_hex()
        );
    }

    #[test]
    fn no_discard_accounting() {
        let mut driver = PlayerDriver::with_catchup(
            PlayerState::spawn([0.0, 0.0, 0.0]),
            PlayerConfig::default(),
            CollisionWorld::new(0.0),
            100,
            8,
        );
        let step = driver.advance(Duration::from_nanos(2_000_000_000), idle());
        assert_eq!(step.executed_ticks, 8, "the catch-up bound holds");
        assert_eq!(step.outstanding_ticks, 192, "the remainder is retained");

        // The accounting invariant at every observation boundary.
        for _ in 0..25 {
            driver.advance(Duration::from_nanos(100_000_000), idle());
            assert_eq!(
                driver.ticks_due_total(),
                driver.executed_ticks_total() + u64::from(driver.outstanding_ticks()),
                "due == executed + outstanding, never silently discarded"
            );
        }
        let mut drained = 0;
        while driver.outstanding_ticks() > 0 {
            // Zero elapsed time: the retained obligation drains purely
            // from catch-up capacity — no new time accrues (the
            // no-discard discipline's drain semantics).
            driver.advance(Duration::ZERO, idle());
            drained += 1;
            assert!(drained < 10_000, "the drain must terminate");
        }
        assert_eq!(
            driver.ticks_due_total(),
            driver.executed_ticks_total(),
            "every due tick eventually executes — conservation"
        );
    }

    #[test]
    fn deterministic_trace_reproducible() {
        // Same-binary-same-platform reproducibility, WITH turns and
        // jumps (the trig path — deliberately not pinned cross-platform).
        let make = || {
            PlayerDriver::new(
                PlayerState::spawn([0.0, 3.0, 0.0]),
                PlayerConfig::default(),
                CollisionWorld::new(0.0)
                    .with_box([1.0, 0.0, -4.0], 2.0, 2.0)
                    .with_box([-2.0, 0.0, -8.0], 1.0, 1.0),
                60,
            )
        };
        let pattern = |tick: u64| MoveIntent {
            forward: if tick.is_multiple_of(3) { 1 } else { 0 },
            strafe: if tick.is_multiple_of(5) { 1 } else { 0 },
            jump_pressed: tick == 10 || tick == 100,
            sprint: tick.is_multiple_of(2),
            look: if tick.is_multiple_of(7) {
                (0.01, -0.003)
            } else {
                (0.0, 0.0)
            },
        };
        let mut first = make();
        first.run_headless(300, pattern);
        let mut second = make();
        second.run_headless(300, pattern);
        assert_eq!(
            first.trace_fingerprint_hex(),
            second.trace_fingerprint_hex(),
            "identical scripted runs produce identical traces"
        );
        assert_eq!(bits(first.state().position), bits(second.state().position));
    }

    #[test]
    fn idle_player_settles_and_stays() {
        let mut driver = PlayerDriver::new(
            PlayerState::spawn([0.0, 3.0, 0.0]),
            PlayerConfig::default(),
            CollisionWorld::new(0.0),
            60,
        );
        driver.run_headless(200, |_| idle());
        assert!(driver.state().on_ground);
        assert_eq!(driver.state().position[1], 0.0);

        let resting = bits(driver.state().position);
        let resting_velocity = bits(driver.state().velocity);
        driver.run_headless(50, |_| idle());
        assert_eq!(bits(driver.state().position), resting, "no drift at rest");
        assert_eq!(bits(driver.state().velocity), resting_velocity);
    }

    #[test]
    fn camera_pose_first_person_from_state() {
        let config = PlayerConfig::default();
        let mut state = PlayerState::spawn([1.0, 2.0, 3.0]);
        state.yaw = 0.3;
        state.pitch = -0.2;
        let pose = camera_pose(&state, &config);
        assert_eq!(pose.position, [1.0, 2.0 + config.eye_height, 3.0]);
        assert_eq!(pose.yaw, 0.3);
        assert_eq!(pose.pitch, -0.2);
        assert_eq!(pose.roll, 0.0);
        assert_eq!(
            bits(camera_pose(&state, &config).position),
            bits(pose.position),
            "the pose is a pure function of state"
        );
    }

    #[test]
    fn camera_pose_third_person_offset() {
        let config = PlayerConfig::default();
        let mut state = PlayerState::spawn([1.0, 2.0, 3.0]);
        state.camera_mode = CameraMode::ThirdPerson;

        // Zero angles: exact arithmetic (the libm-free path — F1).
        let pose = camera_pose(&state, &config);
        assert_eq!(
            pose.position,
            [
                1.0,
                2.0 + config.eye_height + config.tp_lift,
                3.0 + config.tp_distance
            ]
        );

        // Nonzero angles: the documented offset formula.
        state.yaw = 0.7;
        state.pitch = -0.4;
        let pose = camera_pose(&state, &config);
        let forward = look_direction(state.yaw, state.pitch);
        let eye = [1.0, 2.0 + config.eye_height, 3.0];
        let expected = [
            eye[0] - forward[0] * config.tp_distance,
            eye[1] - forward[1] * config.tp_distance + config.tp_lift,
            eye[2] - forward[2] * config.tp_distance,
        ];
        assert_eq!(bits(pose.position), bits(expected));
        assert_eq!(pose.yaw, 0.7);
        assert_eq!(pose.pitch, -0.4);
    }

    #[test]
    fn camera_toggle_preserves_player_state_bitwise() {
        let config = PlayerConfig::default();
        let mut state = PlayerState::spawn([1.0, 2.0, 3.0]);
        state.velocity = [0.5, -0.25, -1.0];
        state.yaw = 0.7;
        state.pitch = -0.3;
        state.on_ground = true;
        let position = bits(state.position);
        let velocity = bits(state.velocity);
        let (yaw, pitch) = (state.yaw.to_bits(), state.pitch.to_bits());

        for toggles in 1..=5 {
            state.toggle_camera_mode();
            assert_eq!(
                state.camera_mode,
                if toggles % 2 == 1 {
                    CameraMode::ThirdPerson
                } else {
                    CameraMode::FirstPerson
                }
            );
            assert_eq!(bits(state.position), position, "no teleport (charter)");
            assert_eq!(bits(state.velocity), velocity, "no reset");
            assert_eq!(state.yaw.to_bits(), yaw);
            assert_eq!(state.pitch.to_bits(), pitch);
            assert!(state.on_ground);
        }

        // Only the DERIVED pose differs between modes (state is
        // ThirdPerson after the odd toggle count).
        let mut fp = state;
        fp.camera_mode = CameraMode::FirstPerson;
        assert_ne!(
            bits(camera_pose(&state, &config).position),
            bits(camera_pose(&fp, &config).position)
        );
    }
}
