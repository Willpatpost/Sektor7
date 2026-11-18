//! Cycle 3 P6 — the integrated playable combat encounter (the arena).
//!
//! A bounded, repeatable, WINNABLE combat encounter plus a reproducible
//! FAILURE scenario, on the playable-world foundations: the fixed-step
//! `PlayerDriver` movement, the exact-slab line-of-sight query, the
//! obstructed camera, and the overlay feedback mechanism — all the
//! existing engine mechanics, wired into one fight.
//!
//! The course: the player drops into the arena (settle) → crossing the
//! activation line wakes the DORMANT adversary (the encounter starts) →
//! the adversary pursues and threatens (melee attacks, range- and
//! LOS-validated) → the player fires back (edge-triggered, cooldown-
//! gated hitscan with range/occlusion validation) → the adversary is
//! DEFEATED (a short drain, then the encounter is WON) — or the player
//! falls (LOST). A reset returns everything to DORMANT for another
//! round.
//!
//! Engine/game boundary: the mechanics (fixed-step movement,
//! `segment_hit`, `camera_pose_obstructed`) live in engine-core; this
//! module owns the arena content, the encounter orchestration, the
//! provisional combat policy values, and the feedback.
//!
//! ## Seams (the Cycle-3 shared-interface protocol)
//!
//! This module was delivered as a scaffold ahead of the sibling
//! seats' contracts; the dependent seams are now WIRED where the
//! contracts have landed:
//!
//! - **SEAT A — WIRED** (`COMBAT-INTERFACE.md` v1, merged at
//!   `f772d01`): the player's fire resolves through
//!   `engine_core::resolve_attack` (the pure instant trace: exact-
//!   slab occlusion, wall-wins-ties, nearest-hit), the adversary's
//!   melee attack resolves through the SAME contract against the
//!   player's `player_box` body, and both combatants' health runs
//!   the `engine_core::Health` transition domain whose
//!   `DamageOutcome::Defeated` is the engine's ONLY defeat
//!   declaration. The demo owns the policy: the weapon profile
//!   values, the OUT-OF-RANGE feedback distinction on top of the
//!   contract's `Miss::Clear` (a local ray-box probe, labeled), the
//!   encounter orchestration, and the id convention
//!   ([`PLAYER_ID`]/[`ADVERSARY_ID`] — game-owned per the contract).
//! - **SEAT B — the adversary BEHAVIOR remains provisional**
//!   (`ProvisionalAdversary`): shaped exactly like B's published
//!   F-facing hooks (F activates the DORMANT adversary, F spawns/
//!   resets, F reads the DEFEATED drain) and conforming to B's
//!   wired DORMANT rule (damage does NOT wake a dormant adversary —
//!   activation is the encounter line's authority). B's reviewed
//!   `engine-core/src/adversary.rs` replaces it wholesale at the
//!   orchestrator's sequencing; the ATTACK resolution itself
//!   already goes through A's contract.
//! - **SEAT C — WIRED** (`AUDIO-CUE-API.md`, merged at `1627810`):
//!   the interactive loop constructs `Audio::initialize` once,
//!   routes the frame's combat events to `audio.play(Cue::…)` at
//!   the observation point (the event-log delta — the shared
//!   transition never touches audio), and prints `audio.status()`
//!   in the run summary. The headless paths NEVER construct audio;
//!   the encounter is fully playable silent.
//!
//! ## Honest labeling (the GO's transience rule)
//!
//! ALL combat values are PLACEHOLDERS in [`CombatConfig`] — no weapon
//! balance, difficulty, or creative approval is claimed. The encounter
//! state is TRANSIENT: process-local, never persisted, no canonical
//! schema contact; [`CombatDemo::reset`] is an honest round reset, NOT
//! a checkpoint (no production checkpoint/rollback claims). The
//! adversary body is non-solid (walk-through, labeled) and its pursuit
//! is straight-line with axis slide (it can stall behind cover in
//! edge cases — B's state machine owns real movement).
//!
//! The scripted scenarios keep yaw and pitch exactly zero (the
//! dead-ahead discipline): the player trace stays libm-free and
//! cross-platform bit-stable under `engine-core/player-trace/1`.

use std::time::{Duration, Instant};

use engine_core::{
    AttackProfile, AttackRequest, CameraMode, CollisionWorld, Combatant, DamageOutcome, Health,
    HitResolution, MissKind, MoveIntent, PlayerDriver, PlayerState, StaticCollider,
    camera_pose_obstructed, health_full, look_direction, player_box, resolve_attack,
};
use engine_renderer::scene::SceneVertex;
use engine_renderer::{PresentModePreference, RenderConfig, Renderer, ValidationPreference};
use engine_shell::{
    Audio, AudioStatus, Cue, DriverPreference, InputSnapshot, Keycode, MouseButton, ShellApp,
    ShellConfig,
};

use crate::frame_timing::FrameTiming;
use crate::math;
use crate::playable_demo::{map_playable_input, playable_player_config};

// ==================================================== the arena layout
//
// Hand-written (the F2 rule: an independent literal, never derived
// from the builders it checks — regression-pinned in
// tests/combat_world.rs). A bounded courtyard: the player spawns at
// z = +6 facing −Z (yaw/pitch zero — dead-ahead), the adversary
// DORMANT at z = −4 on the same centerline; the centerline corridor
// x ∈ [−0.6, 0.6] between those spawns stays clear of every solid
// box (the scripted scenarios' shots, pursuit, and melee all ride
// it); the four pillars (2.4 m — above the 1.6 m eye) sit off the
// centerline as genuine shot blockers; the low walls (1.1 m — below
// the eye) are movement cover only and never block a standing shot.

/// One arena box: an axis-aligned extent plus its role.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArenaBox {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub role: ArenaRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArenaRole {
    /// The courtyard perimeter walls (solid).
    Perimeter,
    /// A full-height cover pillar (solid; blocks eye-level shots).
    CoverPillar,
    /// A below-eye low wall (solid; movement cover only — a standing
    /// shot passes over it).
    LowWall,
    /// The activation pad marker (visual only — the contact trigger
    /// is the demo-layer z-line, not a collider).
    ActivationPad,
    /// The adversary's spawn marker (visual only).
    AdversaryMarker,
}

impl ArenaRole {
    /// Whether the role participates in collision.
    pub fn is_solid(self) -> bool {
        !matches!(self, ArenaRole::ActivationPad | ArenaRole::AdversaryMarker)
    }
}

/// The full arena layout, box for box (rendered and collided from the
/// same list — collide-what-you-see).
pub const ARENA_BOXES: &[ArenaBox] = &[
    // Perimeter (x/z ±8, 0.3 thick, 3 m tall).
    ArenaBox {
        min: [-8.0, 0.0, -8.0],
        max: [-7.7, 3.0, 8.0],
        role: ArenaRole::Perimeter,
    },
    ArenaBox {
        min: [7.7, 0.0, -8.0],
        max: [8.0, 3.0, 8.0],
        role: ArenaRole::Perimeter,
    },
    ArenaBox {
        min: [-8.0, 0.0, -8.0],
        max: [8.0, 3.0, -7.7],
        role: ArenaRole::Perimeter,
    },
    ArenaBox {
        min: [-8.0, 0.0, 7.7],
        max: [8.0, 3.0, 8.0],
        role: ArenaRole::Perimeter,
    },
    // Cover pillars (0.8 x 0.8 footprints, 2.4 m tall — above the
    // 1.6 m eye; staggered off the centerline).
    ArenaBox {
        min: [-3.9, 0.0, -1.4],
        max: [-3.1, 2.4, -0.6],
        role: ArenaRole::CoverPillar,
    },
    ArenaBox {
        min: [3.1, 0.0, -1.4],
        max: [3.9, 2.4, -0.6],
        role: ArenaRole::CoverPillar,
    },
    ArenaBox {
        min: [-2.9, 0.0, 2.6],
        max: [-2.1, 2.4, 3.4],
        role: ArenaRole::CoverPillar,
    },
    ArenaBox {
        min: [2.1, 0.0, 2.6],
        max: [2.9, 2.4, 3.4],
        role: ArenaRole::CoverPillar,
    },
    // Low walls (1.1 m tall — below the eye; flanking the activation
    // line as movement cover).
    ArenaBox {
        min: [-4.5, 0.0, 2.0],
        max: [-2.0, 1.1, 2.3],
        role: ArenaRole::LowWall,
    },
    ArenaBox {
        min: [2.0, 0.0, 2.0],
        max: [4.5, 1.1, 2.3],
        role: ArenaRole::LowWall,
    },
    // The activation pad (visual only).
    ArenaBox {
        min: [-1.5, 0.001, 1.8],
        max: [1.5, 0.05, 2.2],
        role: ArenaRole::ActivationPad,
    },
    // The adversary spawn marker (visual only).
    ArenaBox {
        min: [-1.0, 0.001, -4.4],
        max: [1.0, 0.05, -3.6],
        role: ArenaRole::AdversaryMarker,
    },
];

/// The player's spawn (dropped 2 m, like the gatehouse; settles).
pub const PLAYER_SPAWN: [f32; 3] = [0.0, 2.0, 6.0];
/// The adversary's DORMANT spawn (centerline, dead-ahead).
pub const ADVERSARY_SPAWN: [f32; 3] = [0.0, 0.0, -4.0];
/// The activation line: the encounter starts when the player's z
/// crosses down through this (the demo-layer contact trigger).
pub const ACTIVATION_Z: f32 = 2.0;
/// The adversary's body half-extent (the axis-aligned-box body
/// model, mirroring `StaticCollider`'s shape discipline).
pub const ADVERSARY_HALF: [f32; 3] = [0.45, 0.9, 0.45];
/// The adversary's body height offset (the box spans y ∈ [0, 1.8] at
/// a flat-floor position).
pub const ADVERSARY_EYE_LIFT: f32 = 1.6;
/// The encounter's combatant ids — GAME-OWNED per the contract §4
/// (the engine only carries them; F defines the convention).
pub const PLAYER_ID: u32 = 1;
pub const ADVERSARY_ID: u32 = 2;

/// Builds the arena collision world (the solid boxes only; the
/// adversary is NOT a collider — non-solid, honestly labeled).
pub fn arena_world() -> CollisionWorld {
    let mut world = CollisionWorld::new(0.0);
    for class_box in ARENA_BOXES {
        if !class_box.role.is_solid() {
            continue;
        }
        world = world.with_collider(StaticCollider {
            min: class_box.min,
            max: class_box.max,
        });
    }
    world.with_bounds([-7.5, 0.0, -7.5], [7.5, 40.0, 7.5])
}

/// The adversary's body box at a position (axis-aligned, flat-floor).
pub fn adversary_body(position: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    (
        [
            position[0] - ADVERSARY_HALF[0],
            0.0,
            position[2] - ADVERSARY_HALF[2],
        ],
        [
            position[0] + ADVERSARY_HALF[0],
            2.0 * ADVERSARY_HALF[1],
            position[2] + ADVERSARY_HALF[2],
        ],
    )
}

// ==================================================== the provisional config

/// The encounter's placeholder policy values — EVERY number here is a
/// PROVISIONAL stand-in (no balance, difficulty, or creative
/// approval); the win/fail scenario tuning is documented as
/// placeholder evidence in the delivery note.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CombatConfig {
    /// The weapon's effective range in meters (the exact hit
    /// distance must not exceed it).
    pub weapon_range: f32,
    /// The flat damage per hit (placeholder).
    pub weapon_damage: f32,
    /// The refire cooldown in ticks (edge-triggered fire; a press
    /// during cooldown is refused with feedback).
    pub weapon_cooldown_ticks: u32,
    /// The player's maximum (and starting) health.
    pub player_hp: f32,
    /// The adversary's maximum (and starting) health.
    pub adversary_hp: f32,
    /// The adversary's pursuit speed (m/s, straight-line; the
    /// provisional model has no pathing).
    pub adversary_speed: f32,
    /// The adversary's melee reach (horizontal, center-to-center).
    pub adversary_attack_range: f32,
    /// The adversary's attack cooldown in ticks.
    pub adversary_attack_cooldown_ticks: u32,
    /// The adversary's melee damage per landed attack.
    pub adversary_damage: f32,
    /// The DEFEATED drain length in ticks (mirrors SEAT B's
    /// published drain-to-zero read surface).
    pub defeated_drain_ticks: u32,
    /// The adversary hit-flash length in ticks (visual feedback).
    pub hit_flash_ticks: u32,
}

/// The placeholder combat values (the scenario tuning: activation gap
/// 6 m; the winnable script lands 4 fires / 4 hits with exactly one
/// adversary attack taken — a threatened, meaningful win).
pub fn combat_config() -> CombatConfig {
    CombatConfig {
        weapon_range: 30.0,
        weapon_damage: 25.0,
        weapon_cooldown_ticks: 18,
        player_hp: 100.0,
        adversary_hp: 100.0,
        adversary_speed: 5.0,
        adversary_attack_range: 2.0,
        adversary_attack_cooldown_ticks: 24,
        adversary_damage: 20.0,
        defeated_drain_ticks: 6,
        hit_flash_ticks: 6,
    }
}

// ==================================================== the provisional adversary

/// One adversary step's outcome (the F-facing attack read surface).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AdversaryEvent {
    /// A landed melee attack — the RESOLVED damage the encounter
    /// applies to the player's `Health` (the adversary never mutates
    /// it; the contract's Health-ownership convention).
    AttackLanded { damage: f32 },
    /// An attack attempt foiled by cover (honest feedback: the
    /// contract's `MissKind::Occluded`).
    AttackBlocked,
}

/// The PROVISIONAL demo-local adversary — SEAT B's remaining seam.
/// Shaped like B's published F-facing hooks: F activates (DORMANT →
/// wake), F spawns/resets, F reads DEFEATED (the drain), and it
/// conforms to B's wired DORMANT rule — damage does NOT wake a
/// dormant adversary (the activation line is the encounter's
/// authority). Straight-line pursuit with axis slide against the
/// arena colliders (no pathing — it can stall behind cover in edge
/// cases; B's state machine owns real movement). Walks the flat
/// arena floor (no vertical movement). The body is non-solid for the
/// player (walk-through, labeled). The ATTACK itself resolves
/// through SEAT A's contract (`resolve_attack` against the player's
/// `player_box` body — B's wired ATTACK-seam shape).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProvisionalAdversary {
    /// The flat-floor position (y is always 0).
    pub position: [f32; 3],
    /// The adversary's health — the contract's transition domain
    /// (constructed via `health_full`; DEFEATED comes only from
    /// `DamageOutcome::Defeated`).
    pub health: Health,
    /// Whether the DORMANT adversary has been activated.
    pub active: bool,
    /// Whether DEFEATED was declared (the drain then runs down).
    pub defeated: bool,
    /// The remaining DEFEATED drain ticks.
    pub drain_remaining: u32,
    /// The remaining attack cooldown ticks.
    pub attack_cooldown: u32,
}

impl ProvisionalAdversary {
    /// The DORMANT adversary at its spawn (the encounter's initial
    /// round state). Panics only on an invalid placeholder maximum
    /// (the config constructor's documented contract).
    pub fn at_spawn(config: &CombatConfig) -> ProvisionalAdversary {
        ProvisionalAdversary {
            position: ADVERSARY_SPAWN,
            health: health_full(config.adversary_hp)
                .expect("the placeholder adversary maximum is finite and nonnegative"),
            active: false,
            defeated: false,
            drain_remaining: 0,
            attack_cooldown: 0,
        }
    }

    /// F-driven activation (B's DORMANT → wake hook).
    pub fn activate(&mut self) {
        self.active = true;
    }

    /// One tick of adversary behavior: pursue the player (straight
    /// line, axis slide) and attack when in reach — the attack
    /// resolves ONE `AttackRequest` through the contract (the
    /// adversary eye toward the player's body center, B's wired
    /// shape) against the caller-supplied player `Combatant`.
    /// Returns the attack event, if any. Never touches the player's
    /// health — the encounter applies the resolved damage.
    fn step(
        &mut self,
        player: &PlayerState,
        player_target: Combatant,
        world: &CollisionWorld,
        config: &CombatConfig,
    ) -> Option<AdversaryEvent> {
        if !self.active || self.defeated {
            return None;
        }
        if self.attack_cooldown > 0 {
            self.attack_cooldown -= 1;
        }
        // The horizontal gap (center-to-center) — the melee-reach
        // gate: pursue while beyond it, attempt the contract attack
        // within it (the box entry at this gap sits well inside the
        // profile range, so a clear-line attempt lands; the
        // resolution is still the authority — cover blocks, and a
        // just-out-of-reach attempt is a silent retry).
        let dx = player.position[0] - self.position[0];
        let dz = player.position[2] - self.position[2];
        let horizontal = (dx * dx + dz * dz).sqrt();
        if horizontal <= config.adversary_attack_range {
            if self.attack_cooldown == 0 {
                self.attack_cooldown = config.adversary_attack_cooldown_ticks;
                // THE ATTACK SEAM (the contract's §5 B-row): one
                // request from the adversary's eye toward the
                // player's body center, the placeholder melee
                // profile; the resolution decides.
                let adversary_eye = [self.position[0], ADVERSARY_EYE_LIFT, self.position[2]];
                let player_center = [
                    (player_target.hit_box.min[0] + player_target.hit_box.max[0]) / 2.0,
                    (player_target.hit_box.min[1] + player_target.hit_box.max[1]) / 2.0,
                    (player_target.hit_box.min[2] + player_target.hit_box.max[2]) / 2.0,
                ];
                let request = AttackRequest {
                    attacker: ADVERSARY_ID,
                    origin: adversary_eye,
                    direction: [
                        player_center[0] - adversary_eye[0],
                        player_center[1] - adversary_eye[1],
                        player_center[2] - adversary_eye[2],
                    ],
                    profile: AttackProfile {
                        range: config.adversary_attack_range,
                        occlusion_margin: 0.0,
                    },
                };
                return match resolve_attack(world, &request, &[player_target]) {
                    HitResolution::Hit { .. } => Some(AdversaryEvent::AttackLanded {
                        damage: config.adversary_damage,
                    }),
                    HitResolution::Miss {
                        kind: MissKind::Occluded { .. },
                    } => Some(AdversaryEvent::AttackBlocked),
                    // Just out of reach (a refused request is
                    // impossible with these finite inputs): no
                    // event; the cadence retries.
                    _ => None,
                };
            }
            return None;
        }
        // Pursuit: one 60 Hz tick's straight-line step, axis slide
        // against the solid arena boxes, clamped to the arena bounds.
        let step = config.adversary_speed / 60.0;
        let length = horizontal.max(1e-9);
        let unit = [dx / length, dz / length];
        let try_move = |position: [f32; 3]| -> Option<[f32; 3]> {
            let candidate = [
                position[0] + unit[0] * step,
                0.0,
                position[2] + unit[1] * step,
            ];
            let (min, max) = adversary_body(candidate);
            let blocked = ARENA_BOXES
                .iter()
                .filter(|class_box| class_box.role.is_solid())
                .any(|class_box| {
                    min[0] < class_box.max[0]
                        && max[0] > class_box.min[0]
                        && min[1] < class_box.max[1]
                        && max[1] > class_box.min[1]
                        && min[2] < class_box.max[2]
                        && max[2] > class_box.min[2]
                });
            if blocked {
                None
            } else {
                Some([
                    candidate[0].clamp(-7.2, 7.2),
                    0.0,
                    candidate[2].clamp(-7.2, 7.2),
                ])
            }
        };
        self.position = try_move(self.position)
            .or_else(|| {
                // Axis slide: the x-only step, then the z-only step.
                try_move([self.position[0] + unit[0] * step, 0.0, self.position[2]])
            })
            .or_else(|| try_move([self.position[0], 0.0, self.position[2] + unit[1] * step]))
            .unwrap_or(self.position);
        None
    }
}

// ==================================================== the encounter state

/// The encounter phase (F-owned, TRANSIENT — process-local, never
/// persisted; the reset is an honest round reset, NOT a checkpoint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncounterPhase {
    /// The adversary is DORMANT (pre-activation).
    Dormant,
    /// The fight is live.
    Active,
    /// The adversary was DEFEATED (terminal until reset).
    Won { tick: u64 },
    /// The player was defeated (terminal until reset).
    Lost { tick: u64 },
}

impl EncounterPhase {
    /// Whether the encounter has reached a terminal outcome.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            EncounterPhase::Won { .. } | EncounterPhase::Lost { .. }
        )
    }
}

/// The transient encounter state (never persisted; the counters and
/// the event log are the evidence record the headless runners print).
#[derive(Debug, Clone)]
pub struct EncounterState {
    pub phase: EncounterPhase,
    /// The player's health — the contract's transition domain, held
    /// by the encounter per the Health-ownership convention (the
    /// player's owning simulation is F's encounter state).
    pub player: Health,
    /// The adversary's live state (SEAT B's seam).
    pub adversary: ProvisionalAdversary,
    /// Fire counters (the feedback inventory).
    pub fires: u32,
    pub hits: u32,
    pub misses: u32,
    pub blocked: u32,
    pub out_of_range: u32,
    /// Landed adversary attacks against the player.
    pub attacks_taken: u32,
    /// The completed round count (incremented by each reset).
    pub rounds: u32,
    /// The remaining weapon cooldown ticks.
    pub cooldown_remaining: u32,
    /// The adversary's last hit tick (the hit-flash read).
    pub last_hit_tick: Option<u64>,
    /// The player's last damage tick (the overlay damage line).
    pub last_damage_tick: Option<u64>,
    /// (tick, line) — the deterministic event log.
    pub events: Vec<(u64, String)>,
}

impl EncounterState {
    fn fresh(config: &CombatConfig) -> EncounterState {
        EncounterState {
            phase: EncounterPhase::Dormant,
            player: health_full(config.player_hp)
                .expect("the placeholder player maximum is finite and nonnegative"),
            adversary: ProvisionalAdversary::at_spawn(config),
            fires: 0,
            hits: 0,
            misses: 0,
            blocked: 0,
            out_of_range: 0,
            attacks_taken: 0,
            rounds: 0,
            cooldown_remaining: 0,
            last_hit_tick: None,
            last_damage_tick: None,
            events: Vec::new(),
        }
    }
}

// ==================================================== the demo

/// The combat demo: the player driver plus the transient encounter
/// state, with the per-tick encounter logic shared by the interactive
/// and headless paths (the playable demo's single-transition
/// discipline — both progression routes run the SAME
/// `apply_combat_transition`, so the gameplay semantics exist exactly
/// once).
pub struct CombatDemo {
    pub driver: PlayerDriver,
    pub encounter: EncounterState,
    config: CombatConfig,
    ticks: u64,
    /// A fire press observed on a frame that executed no logical tick
    /// (possible when frames outpace the 60 Hz tick rate): buffered to
    /// the next executed tick's transition — input is never dropped
    /// (the review-F2 pacing discipline).
    pending_fire: bool,
}

impl Default for CombatDemo {
    fn default() -> Self {
        Self::new()
    }
}

impl CombatDemo {
    pub fn new() -> CombatDemo {
        CombatDemo::new_with_config(combat_config())
    }

    /// Test/delivery constructor with an explicit placeholder config
    /// (the refusal-path tests shrink the range, for example).
    pub fn new_with_config(config: CombatConfig) -> CombatDemo {
        let driver = PlayerDriver::new(
            PlayerState::spawn(PLAYER_SPAWN),
            playable_player_config(),
            arena_world(),
            crate::player_demo::DEMO_TICK_HZ,
        );
        CombatDemo {
            driver,
            encounter: EncounterState::fresh(&config),
            config,
            ticks: 0,
            pending_fire: false,
        }
    }

    pub fn config(&self) -> &CombatConfig {
        &self.config
    }

    pub fn tick_index(&self) -> u64 {
        self.ticks
    }

    /// Whether the weapon may fire this tick (the cooldown gate; the
    /// scripts and the overlay read it).
    pub fn weapon_ready(&self) -> bool {
        self.encounter.cooldown_remaining == 0
    }

    /// The SINGLE application-level combat transition: the activation
    /// check, the cooldown advance, the fire edge, the adversary
    /// step, the DEFEATED drain, and the terminal declarations —
    /// applied to the post-movement state by BOTH progression paths
    /// (the scripted logical tick and the frame-sampled live frame),
    /// exactly as the playable demo deduplicates its gameplay
    /// semantics. The order within a tick is part of the provisional
    /// contract: activation → player fire → adversary step →
    /// terminals (the player acts first; SEAT A's contract may
    /// sequence differently — the seam note).
    fn apply_combat_transition(&mut self, tick: u64, fire: bool) {
        // 1. Activation: the contact trigger on the z-line (once —
        //    the DORMANT phase gate makes it single-shot).
        if self.encounter.phase == EncounterPhase::Dormant
            && self.driver.state().position[2] <= ACTIVATION_Z
        {
            self.encounter.phase = EncounterPhase::Active;
            self.encounter.adversary.activate();
            self.log(tick, "encounter activated — the adversary wakes");
        }
        // 2. The cooldown advance (one transition per tick/frame).
        if self.encounter.cooldown_remaining > 0 {
            self.encounter.cooldown_remaining -= 1;
        }
        // 3. The fire edge (one press, one resolution).
        if fire {
            self.fire_weapon(tick);
        }
        // 4. The adversary step (live fights only). The attack, if
        //    any, resolves through the contract inside the step; the
        //    encounter — the player's owning simulation — applies
        //    the resolved damage to the player's `Health`.
        if self.encounter.phase == EncounterPhase::Active {
            let player_box_extent = player_box(self.driver.state().position, self.driver.config());
            let player_target = Combatant {
                id: PLAYER_ID,
                hit_box: StaticCollider {
                    min: player_box_extent.0,
                    max: player_box_extent.1,
                },
            };
            let event = self.encounter.adversary.step(
                self.driver.state(),
                player_target,
                self.driver.world(),
                &self.config,
            );
            match event {
                Some(AdversaryEvent::AttackLanded { damage }) => {
                    // THE CONTRACT'S TRANSITION: the engine's only
                    // defeat declaration (`DamageOutcome::Defeated`)
                    // decides the failure scenario.
                    match self.encounter.player.apply_damage(damage) {
                        Ok(DamageOutcome::Wounded { remaining, .. }) => {
                            self.encounter.attacks_taken += 1;
                            self.encounter.last_damage_tick = Some(tick);
                            self.log(
                                tick,
                                format!(
                                    "adversary attack — player takes {:.0} ({:.0}/{:.0})",
                                    damage, remaining, self.encounter.player.maximum
                                ),
                            );
                        }
                        Ok(DamageOutcome::Defeated { .. }) => {
                            self.encounter.attacks_taken += 1;
                            self.encounter.last_damage_tick = Some(tick);
                            self.log(
                                tick,
                                format!(
                                    "adversary attack — player takes {:.0} (0/{:.0})",
                                    damage, self.encounter.player.maximum
                                ),
                            );
                        }
                        Ok(DamageOutcome::Graze) => {
                            self.encounter.attacks_taken += 1;
                            self.log(tick, "adversary attack — a graze (no damage)");
                        }
                        Err(error) => {
                            // Impossible with these finite
                            // placeholder values — surfaced, never
                            // panicked (the contract's discipline).
                            self.log(
                                tick,
                                format!("adversary attack — damage refused ({error:?})"),
                            );
                        }
                    }
                }
                Some(AdversaryEvent::AttackBlocked) => {
                    self.log(tick, "adversary attack BLOCKED — cover protects the player");
                }
                None => {}
            }
        }
        // 5. The DEFEATED drain (B's read surface: F polls the drain
        //    to zero, then declares the win).
        if self.encounter.phase == EncounterPhase::Active && self.encounter.adversary.defeated {
            if self.encounter.adversary.drain_remaining > 0 {
                self.encounter.adversary.drain_remaining -= 1;
            }
            if self.encounter.adversary.drain_remaining == 0 {
                self.encounter.phase = EncounterPhase::Won { tick };
                self.log(
                    tick,
                    format!(
                        "ADVERSARY DEFEATED — encounter WON at tick {} \
                         (fires {}, hits {}, damage taken {:.0})",
                        tick,
                        self.encounter.fires,
                        self.encounter.hits,
                        self.encounter.attacks_taken as f32 * self.config.adversary_damage
                    ),
                );
            }
        }
        // 6. The player's terminal (checked after the adversary's
        //    attack — the same-tick simultaneity resolves
        //    player-first via the step order above). The defeated
        //    read agrees exactly with the engine's declaration.
        if self.encounter.phase == EncounterPhase::Active && self.encounter.player.is_defeated() {
            self.encounter.phase = EncounterPhase::Lost { tick };
            self.log(
                tick,
                format!(
                    "PLAYER DEFEATED — encounter LOST at tick {} \
                     (adversary {:.0}/{:.0})",
                    tick,
                    self.encounter.adversary.health.current,
                    self.encounter.adversary.health.maximum
                ),
            );
        }
    }

    /// The scripted-logical-tick path: one movement tick plus the
    /// combat transition. `fire` is the FIRE EDGE for this tick (the
    /// mapping layer derives it from the mouse press; holding is not
    /// a new press).
    pub fn tick(&mut self, intent: MoveIntent, fire: bool) {
        let tick = self.ticks;
        self.ticks += 1;
        self.driver.run_headless(1, |_| intent);
        self.apply_combat_transition(tick, fire);
    }

    /// The interactive frame path: one frame's elapsed time, the
    /// frame-sampled live intent, and the frame's fire edge. The
    /// movement stays on the driver's fixed-step accumulator; the
    /// combat update runs at the frame boundary on the post-advance
    /// state through the SAME single transition the headless
    /// [`Self::tick`] uses (the playable demo's R1/R2 discipline).
    pub fn advance_frame(
        &mut self,
        elapsed: Duration,
        intent: MoveIntent,
        fire: bool,
    ) -> engine_core::DriveStep {
        let step = self.driver.advance(elapsed, intent);
        let executed = u64::from(step.executed_ticks);
        self.ticks += executed;
        // The combat transition advances PER EXECUTED LOGICAL TICK
        // (the review-F2 fix: the cooldown decrement, the pursuit
        // step, the attack gate, and the DEFEATED drain are
        // rate-dependent — a frame-rate-coupled once-per-frame call
        // would pace them by the render cadence, against the GO §6
        // binding standard). A 0-tick frame (frames outpacing the
        // tick rate) runs NO transition, buffering the fire press to
        // the next executed tick; the frame's fire edge lands on the
        // FIRST executed tick — one press, one resolution, at the
        // correct logical index. At the 1:1 cadence (60 fps frames,
        // one tick each) the single transition receives exactly the
        // post-advance tick index — byte-identical to the headless
        // semantics (the equivalence the scenarios and the connected
        // route already pin).
        if executed == 0 {
            if fire {
                self.pending_fire = true;
            }
            return step;
        }
        let fire_edge = fire || self.pending_fire;
        self.pending_fire = false;
        for index in 0..executed {
            let tick = self.ticks - executed + index + 1;
            self.apply_combat_transition(tick, fire_edge && index == 0);
        }
        step
    }

    /// The fire resolution — THE CONTRACT CALL (SEAT A's v1, wired):
    /// one `AttackRequest` from the player's eye along the look
    /// direction (camera-agnostic — the same request in both camera
    /// modes), resolved by `engine_core::resolve_attack` against the
    /// adversary's body box. The demo owns the policy: the weapon
    /// profile values, the damage amount routed through the
    /// adversary's `Health`, and the OUT-OF-RANGE feedback
    /// distinction on top of the contract's `Miss::Clear`.
    fn fire_weapon(&mut self, tick: u64) {
        // The cooldown refusal (a press during cooldown is one
        // refused event — edge-triggered, never auto-repeat).
        if self.encounter.cooldown_remaining > 0 {
            self.log(
                tick,
                format!(
                    "fire refused — the weapon is cooling ({} ticks)",
                    self.encounter.cooldown_remaining
                ),
            );
            return;
        }
        self.encounter.fires += 1;
        self.encounter.cooldown_remaining = self.config.weapon_cooldown_ticks;
        let fire_index = self.encounter.fires;
        // THE REQUEST (the contract's §4 F-flow): eye + look
        // direction + the placeholder weapon profile.
        let state = self.driver.state();
        let request = AttackRequest {
            attacker: PLAYER_ID,
            origin: self.eye(),
            direction: look_direction(state.yaw, state.pitch),
            profile: AttackProfile {
                range: self.config.weapon_range,
                occlusion_margin: 0.0,
            },
        };
        let (body_min, body_max) = adversary_body(self.encounter.adversary.position);
        let targets = [Combatant {
            id: ADVERSARY_ID,
            hit_box: StaticCollider {
                min: body_min,
                max: body_max,
            },
        }];
        let resolution = resolve_attack(self.driver.world(), &request, &targets);
        match resolution {
            HitResolution::Hit { .. } => {
                self.encounter.hits += 1;
                self.encounter.last_hit_tick = Some(tick);
                // THE CONTRACT'S TRANSITION: the engine's only defeat
                // declaration arms the drain. A DORMANT adversary
                // takes the damage WITHOUT waking (B's wired rule —
                // the activation line is the encounter's authority).
                match self
                    .encounter
                    .adversary
                    .health
                    .apply_damage(self.config.weapon_damage)
                {
                    Ok(DamageOutcome::Wounded { remaining, .. }) => {
                        self.log(
                            tick,
                            format!(
                                "fire {fire_index}: HIT — adversary {:.0}/{:.0}",
                                remaining, self.encounter.adversary.health.maximum
                            ),
                        );
                    }
                    Ok(DamageOutcome::Defeated { .. }) => {
                        self.log(
                            tick,
                            format!(
                                "fire {fire_index}: HIT — adversary 0/{:.0}",
                                self.encounter.adversary.health.maximum
                            ),
                        );
                        if !self.encounter.adversary.defeated {
                            self.encounter.adversary.defeated = true;
                            self.encounter.adversary.drain_remaining =
                                self.config.defeated_drain_ticks;
                            self.encounter.adversary.active = false;
                        }
                    }
                    Ok(DamageOutcome::Graze) => {
                        self.log(
                            tick,
                            format!("fire {fire_index}: HIT — a graze (no damage)"),
                        );
                    }
                    Err(error) => {
                        // Impossible with these finite placeholder
                        // values — surfaced, never panicked.
                        self.log(
                            tick,
                            format!("fire {fire_index}: damage refused ({error:?})"),
                        );
                    }
                }
            }
            HitResolution::Miss {
                kind: MissKind::Occluded { .. },
            } => {
                self.encounter.blocked += 1;
                self.log(
                    tick,
                    format!("fire {fire_index}: BLOCKED — cover blocks the line"),
                );
            }
            HitResolution::Miss {
                kind: MissKind::Clear,
            } => {
                // Demo-layer feedback policy on top of the contract:
                // the contract's range is structural (a beyond-range
                // target is a Clear miss); the demo distinguishes the
                // OUT-OF-RANGE line with a local ray-box probe.
                let (min, max) = adversary_body(self.encounter.adversary.position);
                match ray_box_entry(request.origin, request.direction, min, max) {
                    Some(entry) if entry > self.config.weapon_range => {
                        self.encounter.out_of_range += 1;
                        self.log(
                            tick,
                            format!(
                                "fire {fire_index}: OUT OF RANGE — {:.1} m > {:.0} m",
                                entry, self.config.weapon_range
                            ),
                        );
                    }
                    _ => {
                        self.encounter.misses += 1;
                        self.log(
                            tick,
                            format!("fire {fire_index}: MISS — the aim is off the target"),
                        );
                    }
                }
            }
            HitResolution::Refused(reason) => {
                // Impossible with these finite inputs — surfaced,
                // never panicked (the contract's discipline).
                self.log(tick, format!("fire {fire_index}: refused — {reason:?}"));
            }
        }
    }

    /// The TRANSIENT round reset — everything returns to DORMANT at
    /// the spawn states (the player driver is rebuilt at the spawn).
    /// NOT a checkpoint: nothing persisted, nothing recovered; the
    /// round counter is the only thing that survives.
    pub fn reset(&mut self) {
        let rounds = self.encounter.rounds + 1;
        let driver = PlayerDriver::new(
            PlayerState::spawn(PLAYER_SPAWN),
            playable_player_config(),
            arena_world(),
            crate::player_demo::DEMO_TICK_HZ,
        );
        self.driver = driver;
        self.encounter = EncounterState::fresh(&self.config);
        self.encounter.rounds = rounds;
        self.ticks = 0;
        self.pending_fire = false;
        self.log_public(
            0,
            format!("encounter reset — transient state, round {rounds}"),
        );
    }

    pub fn eye(&self) -> [f32; 3] {
        let state = self.driver.state();
        [
            state.position[0],
            state.position[1] + self.driver.config().eye_height,
            state.position[2],
        ]
    }

    /// The camera pose with obstruction handling (both paths).
    pub fn camera_pose(&self) -> engine_core::CameraPose {
        camera_pose_obstructed(
            self.driver.state(),
            self.driver.config(),
            self.driver.world(),
        )
    }

    fn log(&mut self, tick: u64, line: impl Into<String>) {
        self.encounter.events.push((tick, line.into()));
    }

    /// Public event logging for the scripted scenarios' own markers.
    pub fn log_public(&mut self, tick: u64, line: impl Into<String>) {
        self.encounter.events.push((tick, line.into()));
    }

    /// The visible world: every arena box in its current state plus
    /// the adversary's body — collide-what-you-see, and every combat
    /// state visibly confirmed (the pad recolors on activation, the
    /// adversary recolors by phase and hit-flash, the player proxy
    /// appears in third person).
    pub fn render_boxes(&self) -> Vec<([f32; 3], [f32; 3], [f32; 3])> {
        let mut boxes = Vec::new();
        for class_box in ARENA_BOXES {
            let color = match class_box.role {
                ArenaRole::Perimeter => [0.35, 0.35, 0.40],
                ArenaRole::CoverPillar => [0.45, 0.33, 0.33],
                ArenaRole::LowWall => [0.55, 0.50, 0.45],
                ArenaRole::ActivationPad => {
                    if self.encounter.phase == EncounterPhase::Dormant {
                        [0.20, 0.40, 0.90]
                    } else {
                        [0.20, 0.80, 0.40]
                    }
                }
                ArenaRole::AdversaryMarker => [0.25, 0.25, 0.30],
            };
            boxes.push((class_box.min, class_box.max, color));
        }
        // The adversary's body: dormant gray, active red, hit-flash
        // white, defeated dark.
        let adversary_color = if self.encounter.adversary.defeated {
            [0.20, 0.20, 0.25]
        } else if let (Some(last), Some(now)) = (self.encounter.last_hit_tick, Some(self.ticks)) {
            if now.saturating_sub(last) <= u64::from(self.config.hit_flash_ticks) {
                [0.95, 0.95, 0.95]
            } else if self.encounter.adversary.active {
                [0.75, 0.25, 0.20]
            } else {
                [0.50, 0.50, 0.55]
            }
        } else if self.encounter.adversary.active {
            [0.75, 0.25, 0.20]
        } else {
            [0.50, 0.50, 0.55]
        };
        let (min, max) = adversary_body(self.encounter.adversary.position);
        boxes.push((min, max, adversary_color));
        // The player proxy in third person (the playable demo's rule).
        if self.driver.state().camera_mode == CameraMode::ThirdPerson {
            let (min, max) = player_box(self.driver.state().position, self.driver.config());
            boxes.push((min, max, [0.20, 0.80, 0.35]));
        }
        boxes
    }
}

/// The exact ray-vs-AABB entry distance (the slab method; comparisons
/// and division only — no trig, no libm). NOT the hit resolver —
/// SEAT A's `resolve_attack` owns that; this local probe only
/// distinguishes the demo's OUT-OF-RANGE feedback line on top of the
/// contract's `Miss::Clear` outcome. `None` when the ray misses the
/// box; a negative entry means the origin is inside.
fn ray_box_entry(
    origin: [f32; 3],
    direction: [f32; 3],
    min: [f32; 3],
    max: [f32; 3],
) -> Option<f32> {
    let mut entry = 0.0f32;
    let mut exit = f32::INFINITY;
    for axis in 0..3 {
        let dir = direction[axis];
        if dir == 0.0 {
            if origin[axis] < min[axis] || origin[axis] > max[axis] {
                return None;
            }
        } else {
            let mut near = (min[axis] - origin[axis]) / dir;
            let mut far = (max[axis] - origin[axis]) / dir;
            if near > far {
                std::mem::swap(&mut near, &mut far);
            }
            entry = entry.max(near);
            exit = exit.min(far);
            if entry > exit {
                return None;
            }
        }
    }
    Some(entry)
}

// ==================================================== input mapping

/// One frame's mapped combat input: the playable mapping (movement,
/// look, camera toggle — the SAME mapping, so the corrected mouse
/// controls hold) plus the fire edge.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CombatMappedInput {
    pub intent: MoveIntent,
    /// A fresh left-mouse press this frame (edge-triggered; holding
    /// is not a new press).
    pub fire_pressed: bool,
    pub toggle_camera: bool,
}

/// Maps a shell snapshot onto the combat intents: the playable
/// mapping verbatim (WASD, Space, Shift, mouse look, C camera, the
/// focus-loss neutralization) plus left-mouse fire.
pub fn map_combat_input(snapshot: &InputSnapshot, sprint_held: bool) -> CombatMappedInput {
    let playable = map_playable_input(snapshot, sprint_held);
    let neutral = snapshot.focus_lost || snapshot.minimized;
    CombatMappedInput {
        intent: playable.intent,
        fire_pressed: !neutral && snapshot.mouse_pressed.contains(&MouseButton::Left),
        toggle_camera: playable.toggle_camera,
    }
}

// ==================================================== the headless scenarios

fn idle() -> MoveIntent {
    MoveIntent::default()
}

fn forward() -> MoveIntent {
    MoveIntent {
        forward: 1,
        ..idle()
    }
}

/// The bounded phase loop (a deterministic script that stalls is a
/// layout or mechanic regression, never a skip — the playable demo's
/// loud-failure discipline).
fn run_until(
    demo: &mut CombatDemo,
    intent: MoveIntent,
    fire: bool,
    condition: impl Fn(&CombatDemo) -> bool,
    label: &str,
) {
    for _ in 0..3_000 {
        if condition(demo) {
            return;
        }
        demo.tick(intent, fire);
    }
    panic!("the combat script stalled at '{label}'");
}

/// The WINNABLE script over an existing demo (used by the headless
/// entry and the reset battery): settle → cross the activation line →
/// hold position and fire on cooldown → the adversary closes, lands
/// exactly one attack, and is DEFEATED → the drain → WON.
pub fn run_winnable_script(demo: &mut CombatDemo) {
    // 1. Settle to the arena floor.
    run_until(
        demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle",
    );
    let settle_tick = demo.tick_index();
    demo.log_public(settle_tick, "settled on the arena ground");

    // 2. Walk forward to the activation line (the encounter starts
    //    in-transition when the player crosses it).
    run_until(
        demo,
        forward(),
        false,
        |d| d.encounter.phase == EncounterPhase::Active,
        "activate the encounter",
    );

    // 3. Hold position and fire on cooldown until the encounter
    //    resolves (the fire edge only when the weapon is ready — one
    //    press, one resolution; the cooldown gates the cadence).
    for _ in 0..3_000 {
        if demo.encounter.phase != EncounterPhase::Active {
            break;
        }
        let fire = demo.weapon_ready();
        demo.tick(idle(), fire);
    }
    assert!(
        demo.encounter.phase != EncounterPhase::Active,
        "the combat script stalled at 'win the encounter'"
    );
}

/// The WINNABLE headless scenario (`--combat-headless`): the script
/// over a fresh demo, returning the printable evidence record,
/// byte-identical across identical invocations.
pub fn run_combat_headless() -> String {
    let mut demo = CombatDemo::new();
    run_winnable_script(&mut demo);
    let done_tick = match demo.encounter.phase {
        EncounterPhase::Won { tick } | EncounterPhase::Lost { tick } => tick,
        _ => demo.tick_index(),
    };
    for _ in 0..5 {
        demo.tick(idle(), false);
    }
    encounter_evidence(&demo, "WINNABLE", done_tick)
}

/// The FAILURE headless scenario (`--combat-failure-headless`):
/// settle → walk through activation and keep closing to melee →
/// stand still, never fire → the adversary's cooldown-gated attacks
/// defeat the player. The reproducible failure.
pub fn run_combat_failure_headless() -> String {
    let mut demo = CombatDemo::new();

    // 1. Settle.
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle",
    );
    let settle_tick = demo.tick_index();
    demo.log_public(settle_tick, "settled on the arena ground");

    // 2. Walk forward until melee range (the activation fires en
    //    route; the adversary closes the rest of the gap).
    run_until(
        &mut demo,
        forward(),
        false,
        |d| {
            let dx = d.driver.state().position[0] - d.encounter.adversary.position[0];
            let dz = d.driver.state().position[2] - d.encounter.adversary.position[2];
            (dx * dx + dz * dz).sqrt() <= d.config().adversary_attack_range + 0.2
        },
        "close to melee",
    );

    // 3. Stand still and take it (fires 0 — the player who does not
    //    fight back loses).
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.encounter.phase != EncounterPhase::Active,
        "lose the encounter",
    );
    let done_tick = match demo.encounter.phase {
        EncounterPhase::Won { tick } | EncounterPhase::Lost { tick } => tick,
        _ => demo.tick_index(),
    };
    for _ in 0..5 {
        demo.tick(idle(), false);
    }

    encounter_evidence(&demo, "FAILURE", done_tick)
}

/// The shared evidence record (the event log, both healths, the
/// outcome, the movement accounting, and the trace fingerprint).
fn encounter_evidence(demo: &CombatDemo, scenario: &str, done_tick: u64) -> String {
    let mut out = String::new();
    out.push_str("Sektor7 C3-P6 - combat encounter headless run (the arena)\n");
    out.push_str(&format!("scenario: {scenario}\n"));
    out.push_str("events:\n");
    for (tick, line) in &demo.encounter.events {
        out.push_str(&format!("  tick {tick}: {line}\n"));
    }
    let state = demo.driver.state();
    let adversary_word = if demo.encounter.adversary.defeated {
        "DEFEATED"
    } else if demo.encounter.adversary.active {
        "ACTIVE"
    } else {
        "DORMANT"
    };
    out.push_str(&format!(
        "player: hp {:.0}/{:.0} · pos ({:.3}, {:.3}, {:.3}) on_ground {} mode {}\n",
        demo.encounter.player.current,
        demo.encounter.player.maximum,
        state.position[0],
        state.position[1],
        state.position[2],
        state.on_ground,
        match state.camera_mode {
            CameraMode::FirstPerson => "first",
            CameraMode::ThirdPerson => "third",
        },
    ));
    out.push_str(&format!(
        "adversary: {adversary_word} · hp {:.0}/{:.0} · pos ({:.3}, {:.3})\n",
        demo.encounter.adversary.health.current,
        demo.encounter.adversary.health.maximum,
        demo.encounter.adversary.position[0],
        demo.encounter.adversary.position[2],
    ));
    out.push_str(&format!(
        "outcome: {} at tick {done_tick} (fires {} · hits {} · blocked {} · \
         out of range {} · attacks taken {})\n",
        match demo.encounter.phase {
            EncounterPhase::Won { .. } => "WON",
            EncounterPhase::Lost { .. } => "LOST",
            _ => "UNRESOLVED",
        },
        demo.encounter.fires,
        demo.encounter.hits,
        demo.encounter.blocked,
        demo.encounter.out_of_range,
        demo.encounter.attacks_taken,
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

// ==================================================== the interactive demo

/// Demo options; CLI parsing lives in the binary (the playable
/// pattern).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CombatOptions {
    pub fps: u32,
    pub present: PresentModePreference,
    pub frames: Option<u64>,
    pub overlay: bool,
    pub validation: ValidationPreference,
}

impl Default for CombatOptions {
    fn default() -> Self {
        CombatOptions {
            fps: 60,
            present: PresentModePreference::Auto,
            frames: None,
            overlay: true,
            validation: ValidationPreference::Auto,
        }
    }
}

/// The event→cue mapping (C's published table, F's wiring): every
/// fire plays `Cue::Fire`, a landed hit adds `Cue::Impact`, an
/// adversary attack on the player plays `Cue::Damage`, the
/// activation plays `Cue::Interact` (the candidate mapping), and the
/// terminal phases play `Cue::EncounterComplete`. Cooldown refusals,
/// grazes' quiet feedback, and the reset play nothing. `play` is
/// non-blocking and idempotent when silent — no queueing needed.
fn play_cues_for(audio: &mut Audio, line: &str) {
    if line.starts_with("fire refused") {
        return;
    }
    if line.starts_with("fire ") {
        audio.play(Cue::Fire);
        if line.contains("HIT") && !line.contains("graze") {
            audio.play(Cue::Impact);
        }
        return;
    }
    if line.starts_with("adversary attack — player takes") {
        audio.play(Cue::Damage);
        return;
    }
    if line == "encounter activated — the adversary wakes" {
        audio.play(Cue::Interact);
        return;
    }
    if line.starts_with("ADVERSARY DEFEATED") || line.starts_with("PLAYER DEFEATED") {
        audio.play(Cue::EncounterComplete);
    }
}

/// Appends an axis-aligned box as non-indexed triangle vertices (the
/// playable demo's rectangle generalization — same face order,
/// winding, and shading).
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

/// The interactive combat demo (`--combat`): the playable demo's
/// skeleton over the arena — fixed-step movement on the full raw
/// interval, the obstructed third-person camera, left-mouse fire, C
/// to toggle the camera, R to reset the encounter, visible feedback
/// everywhere, and SEAT C's audio cues wired at the event
/// observation points (the encounter remains fully playable silent —
/// gameplay never gates on audio).
pub fn run_combat_demo(options: CombatOptions) -> Result<(), String> {
    let shell_config = ShellConfig {
        title: "Sektor7 original engine — Cycle 3 P6 combat encounter".to_string(),
        width: 1280,
        height: 720,
        target_fps: 60,
        driver: DriverPreference::Real,
    };
    let mut app = ShellApp::new(shell_config)?;
    app.set_relative_mouse(true);

    // SEAT C's capability (AUDIO-CUE-API.md contract 1/3): construct
    // ONCE here — never in the headless paths; a missing device
    // degrades to the observable Silent status, never an error.
    let mut audio = Audio::initialize(app.platform());
    let mut event_cursor: usize = 0;

    let render_config = RenderConfig {
        present_mode: options.present,
        validation: options.validation,
        replace_pipelines: false,
    };
    let mut renderer =
        Renderer::new(app.window(), render_config).map_err(|error| error.to_string())?;
    renderer.set_overlay_enabled(options.overlay);

    let mut demo = CombatDemo::new();
    demo.driver.set_trace_recording(false);

    {
        let stats = renderer.stats();
        println!("Sektor7 Cycle 3 P6 — the integrated combat encounter");
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
            "  controls: WASD move · Space jump · Shift sprint · LMB fire · \
             C camera mode · R reset the encounter · mouse look · O overlay · Esc/close quit"
        );
        println!(
            "  the encounter: cross the blue line to wake the adversary → \
             it pursues and attacks → fire back (range + line of sight \
             validated) → defeat it before it defeats you"
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
        if snapshot.keys_pressed.contains(&Keycode::R) {
            demo.reset();
            event_cursor = 0;
        }
        let split = timing.begin_frame(frame_start, !snapshot.minimized);

        let mapped = map_combat_input(&snapshot, app.is_held(Keycode::LShift));
        if mapped.toggle_camera {
            demo.driver.state_mut().toggle_camera_mode();
        }

        // Fixed-step movement on the full raw interval through the
        // SHARED combat transition (the playable demo's R1/R2
        // discipline): the fire edge applies once per press at the
        // frame boundary, and the event timestamps track the live
        // logical position.
        demo.advance_frame(split.sim_advance, mapped.intent, mapped.fire_pressed);

        // SEAT C's cues at the observation point (the API contract:
        // route gameplay events to `play` where they are observed —
        // the shared transition itself never touches audio, so the
        // headless paths stay audio-free by construction).
        while event_cursor < demo.encounter.events.len() {
            let (_, line) = &demo.encounter.events[event_cursor];
            play_cues_for(&mut audio, line);
            event_cursor += 1;
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

        // The visible arena + the adversary + the player proxy.
        scene_vertices.clear();
        for (min, max, color) in demo.render_boxes() {
            push_rect_box(&mut scene_vertices, min, max, color);
        }

        let state = demo.driver.state();
        let overlay_text = renderer.overlay_enabled().then(|| {
            let elapsed = start.elapsed().as_secs_f64().max(0.001);
            let stats = renderer.stats();
            let phase_line = match demo.encounter.phase {
                EncounterPhase::Dormant => "DORMANT — cross the blue line to start".to_string(),
                EncounterPhase::Active => "ACTIVE — the adversary pursues".to_string(),
                EncounterPhase::Won { tick } => {
                    format!("WON at tick {tick} — [R] reset for another round")
                }
                EncounterPhase::Lost { tick } => {
                    format!("LOST at tick {tick} — [R] reset and fight back")
                }
            };
            let adversary_state = if demo.encounter.adversary.defeated {
                "defeated"
            } else if demo.encounter.adversary.active {
                "active"
            } else {
                "dormant"
            };
            let last_fire = demo
                .encounter
                .events
                .iter()
                .rev()
                .find_map(|(_, line)| line.starts_with("fire ").then(|| line.clone()))
                .unwrap_or_else(|| "no fire yet".to_string());
            let damage_line = match (demo.encounter.last_damage_tick, demo.tick_index()) {
                (Some(last), now) if now.saturating_sub(last) <= 90 => {
                    format!(
                        "player took {:.0} damage recently",
                        demo.config().adversary_damage
                    )
                }
                _ => "no recent damage".to_string(),
            };
            format!(
                "Sektor7 C3-P6 - combat encounter\nFPS {:.0} (cpu work p95 {:.2} ms)\n{} {} / Vulkan {}\n\
                 player ({:.2}, {:.2}, {:.2}) ground {} mode {}\n\
                 encounter {} · round {}\n\
                 player hp {:.0}/{:.0} · adversary hp {:.0}/{:.0} ({adversary_state})\n\
                 {}\n\
                 {}\n\
                 fires {} · hits {} · blocked {} · out of range {}\n\
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
                phase_line,
                demo.encounter.rounds + 1,
                demo.encounter.player.current,
                demo.encounter.player.maximum,
                demo.encounter.adversary.health.current,
                demo.encounter.adversary.health.maximum,
                last_fire,
                damage_line,
                demo.encounter.fires,
                demo.encounter.hits,
                demo.encounter.blocked,
                demo.encounter.out_of_range,
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
        "outcome: {} · fires {} · hits {} · attacks taken {}",
        match demo.encounter.phase {
            EncounterPhase::Won { .. } => "WON",
            EncounterPhase::Lost { .. } => "LOST",
            EncounterPhase::Active => "active at exit",
            EncounterPhase::Dormant => "dormant at exit",
        },
        demo.encounter.fires,
        demo.encounter.hits,
        demo.encounter.attacks_taken,
    );
    println!(
        "player final: hp {:.0}/{:.0} · pos ({:.3}, {:.3}, {:.3}) ground {} mode {}",
        demo.encounter.player.current,
        demo.encounter.player.maximum,
        state.position[0],
        state.position[1],
        state.position[2],
        state.on_ground,
        match state.camera_mode {
            CameraMode::FirstPerson => "first",
            CameraMode::ThirdPerson => "third",
        }
    );
    // The audio status is always visible in the run summary (C's
    // contract 3/6: degradation is observable, never fatal).
    match audio.status() {
        AudioStatus::Active { driver, device } => {
            println!("audio: active — {driver} / {device}");
        }
        AudioStatus::Silent { reason } => {
            println!("audio: silent — {reason}");
        }
    }
    println!(
        "encounter state is transient (not persisted); offscreen CI runs are a \
         correctness smoke, not a substitute for a human-played run"
    );
    Ok(())
}
