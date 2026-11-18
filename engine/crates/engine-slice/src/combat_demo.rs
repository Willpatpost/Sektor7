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

use engine_core::adversary::{AdversaryConfig, AdversaryState, AdversaryWorld};
use engine_core::{
    AttackProfile, AttackRequest, CameraMode, CollisionWorld, Combatant, DamageOutcome, Health,
    HitResolution, MissKind, MoveIntent, PlayerDriver, PlayerState, Spine, StaticCollider, Tick,
    camera_pose_obstructed, health_full, look_direction, player_box, resolve_attack,
};
use engine_renderer::scene::SceneVertex;
use engine_renderer::{
    DemoScenePreference, PresentModePreference, RenderConfig, Renderer, ValidationPreference,
};
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
/// The adversary's home ground (the patrol circuit's center,
/// centerline, dead-ahead — B's `AdversaryConfig::spawn_center`).
pub const ADVERSARY_SPAWN: [f32; 3] = [0.0, 0.0, -4.0];
/// The activation line: the encounter's orchestration marker flips
/// when the player's z crosses down through this (the demo-layer
/// contact trigger).
pub const ACTIVATION_Z: f32 = 2.0;
/// The adversary's body half-extent (the axis-aligned-box body
/// model, mirroring `StaticCollider`'s shape discipline).
pub const ADVERSARY_HALF: [f32; 3] = [0.45, 0.9, 0.45];
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
    /// The adversary's pursuit speed (m/s; B's wake-cadenced
    /// movement — the placeholder tuning).
    pub adversary_speed: f32,
    /// The adversary's melee reach (horizontal, center-to-center).
    pub adversary_attack_range: f32,
    /// The adversary's attack cooldown in ticks.
    pub adversary_attack_cooldown_ticks: u32,
    /// The adversary's melee damage per landed attack (F's policy,
    /// applied from B's surfaced resolution).
    pub adversary_damage: f32,
    /// The DEFEATED drain length in ticks (F's presentation window
    /// over B's terminal state).
    pub defeated_drain_ticks: u32,
    /// The adversary hit-flash length in ticks (visual feedback).
    pub hit_flash_ticks: u32,
    /// The adversary world's deterministic seed (B's circuit phase,
    /// spawn spread, and perception jitter — a placeholder knob the
    /// refusal-path tests also use to place the circuit spawn).
    pub adversary_seed: u64,
}

/// The placeholder combat values (the scenario tuning: the winnable
/// script lands 4 fires / 4 hits with two adversary attacks taken —
/// a threatened, meaningful win; B's machine pressures the walk).
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
        adversary_seed: 0xA11CE,
    }
}

// ==================================================== the adversary composition (SEAT B's module)

/// The encounter's adversary configuration — SEAT B's
/// `AdversaryConfig` with the demo's placeholder policy values (the
/// mapping from [`CombatConfig`] is explicit and labeled: EVERY
/// number is a placeholder; B's structural defaults are adopted
/// where the arena's tuning does not bind). The adversary world runs
/// on the SAME spine discipline B delivered (the 200/60 reservation
/// — the owner-gated default, untouched) over the SAME arena
/// collision content.
fn adversary_config(config: &CombatConfig) -> AdversaryConfig {
    AdversaryConfig {
        seed: config.adversary_seed, // The demo's placeholder knob (deterministic).
        count: 1,                    // ONE adversary — the encounter's shape.
        tick_seconds: 1.0 / 60.0,    // The demo's 60 Hz fixed step.
        aggro_radius: 8.0,           // B's placeholder perception.
        deaggro_radius: 10.0,        // B's placeholder hysteresis.
        los_loss_ticks: 24,          // B's placeholder tolerance.
        patrol_cadence: 12,          // B's placeholder idle cadence.
        aggro_cadence: 2,            // B's placeholder threat cadence.
        recover_cadence: 6,          // B's placeholder recovery window.
        attack_cooldown: u64::from(config.adversary_attack_cooldown_ticks),
        attack_range: config.adversary_attack_range,
        speed: config.adversary_speed,
        waypoint_radius: 2.5, // Fits the arena's clear interior around the spawn center.
        spawn_center: ADVERSARY_SPAWN, // The circuit's center (the arena's adversary ground).
        max_health: config.adversary_hp,
        eye_height: 1.5, // B's placeholder (≠ half the player body height).
        player: playable_player_config(), // B's player view matches the demo's actual player.
        player_id: PLAYER_ID, // The game-owned id convention (the contract §4).
        adversary_id_base: ADVERSARY_ID,
    }
}

/// Builds the encounter's spine over B's adversary world: the seeded
/// schedules installed, the 200/60 reservation (the owner-gated
/// default budget — B's delivered discipline, untouched).
fn adversary_spine(config: &CombatConfig) -> Spine<AdversaryWorld> {
    let mut spine = Spine::with_reservation(
        AdversaryWorld::new(adversary_config(config), arena_world()),
        200,
        60,
    );
    // The Cycle-3.1 defect-A fix: the encounter's adversaries start
    // DORMANT (B's opt-in composition surface) and NOTHING is
    // scheduled at construction — the dormant adversary's ONLY entry
    // is the `activation_spec` first wake-up that the activation
    // line schedules (the deterministic event-driven mechanism; the
    // wake transitions DORMANT → PATROL and the cadence chains carry
    // it onward). NOTE: `install_adversary_schedules` is NOT called
    // — its schedules are unconditional over `count` (its
    // "non-dormant only" doc does not match the code at this tree),
    // so it would wake the dormant adversary early; a dormant-start
    // encounter composes through the activation event alone. This
    // is the ENCOUNTER's composition policy; B's naturally-active
    // worlds (the default PATROL start + the installer) are
    // untouched.
    let count = spine.world().count();
    for index in 0..count {
        spine.world_mut().set_dormant(index);
    }
    spine
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
/// The adversary's live state lives in the spine's
/// [`AdversaryWorld`] (SEAT B's module); the encounter holds only
/// F's orchestration reads.
#[derive(Debug, Clone)]
pub struct EncounterState {
    pub phase: EncounterPhase,
    /// The player's health — the contract's transition domain, held
    /// by the encounter per the Health-ownership convention (the
    /// player's owning simulation is F's encounter state).
    pub player: Health,
    /// Fire counters (the feedback inventory).
    pub fires: u32,
    pub hits: u32,
    pub misses: u32,
    pub blocked: u32,
    pub out_of_range: u32,
    /// Landed adversary attacks against the player.
    pub attacks_taken: u32,
    /// The surfaced attacks already applied (B's `attacks_emitted`
    /// read — the delta detection).
    pub attacks_seen: u64,
    /// Whether B's DEFEATED declaration was observed (the drain then
    /// runs down in the encounter — F's read surface).
    pub adversary_defeated: bool,
    /// The remaining DEFEATED drain ticks (the completion window).
    pub drain_remaining: u32,
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
            fires: 0,
            hits: 0,
            misses: 0,
            blocked: 0,
            out_of_range: 0,
            attacks_taken: 0,
            attacks_seen: 0,
            adversary_defeated: false,
            drain_remaining: 0,
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
    /// SEAT B's adversary world, driven on the spine (the reviewed
    /// six-state behavior machine; the F-facing hooks are the reads
    /// and `apply_player_hit`).
    pub spine: Spine<AdversaryWorld>,
    /// The fire resolution's collision copy — the SAME arena content
    /// as the driver's and the adversary's worlds (built from the
    /// same literal; the arena is static in the encounter, so the
    /// three copies never diverge). The C3-R1 split-borrow
    /// composition needs a collision reference the observer closure
    /// can hold while the driver advances; B's module exposes no
    /// collision accessor (adversary.rs is out of this corrective's
    /// file ownership), and the driver's world is mutably borrowed
    /// by the advance — hence this owned copy (flagged for the
    /// reviewer; a future public accessor on the adversary world
    /// could replace it).
    fire_world: CollisionWorld,
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
            spine: adversary_spine(&config),
            fire_world: arena_world(),
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
    /// check, the cooldown advance, the fire edge, SEAT B's adversary
    /// tick (the spine over the reviewed six-state machine), the
    /// surfaced-attack application, the DEFEATED drain, and the
    /// terminal declarations. An ASSOCIATED fn over the PARTS (the
    /// encounter, the spine, the config) plus THIS tick's post-step
    /// player state — the C3-R1 form: the frame path interleaves it
    /// between the driver's movement steps (the `advance_observed`
    /// seam, each call observing THAT tick's own resulting state,
    /// never the frame's final state), and the headless tick path
    /// calls it with the post-tick state — one implementation, both
    /// progression routes, the semantics existing exactly once. The
    /// order within a tick: activation → player fire → B's adversary
    /// tick → the surfaced attack's application → the terminals (the
    /// player acts first).
    #[allow(clippy::too_many_arguments)]
    fn apply_combat_transition(
        encounter: &mut EncounterState,
        spine: &mut Spine<AdversaryWorld>,
        fire_world: &CollisionWorld,
        config: &CombatConfig,
        player: &PlayerState,
        player_eye_height: f32,
        tick: u64,
        fire: bool,
    ) {
        // 0. THE TERMINAL GUARD (the Cycle-3.1 defect-B fix): WON and
        //    LOST are GENUINE terminal states — the result and its
        //    logical timestamp are preserved, the counters freeze,
        //    and ALL combat effects stop (no further damage, no
        //    weapon resolution, no AI combat effects — the spine
        //    stops advancing; the encounter is over). The
        //    presentation continues normally OUTSIDE this transition
        //    (the render loop, the camera, the overlay); R is the
        //    explicit fresh-round reset. A post-terminal fire press
        //    gets the honest refusal line (feedback, not a counter).
        if encounter.phase.is_terminal() {
            if fire {
                Self::log(
                    encounter,
                    tick,
                    "fire refused — the encounter is over ([R] reset)",
                );
            }
            return;
        }
        // 1. Activation: the contact trigger on the z-line (once —
        //    the DORMANT phase gate makes it single-shot). The
        //    Cycle-3.1 defect-A fix makes the contract REAL: the
        //    adversary is DORMANT (no chain — nothing was scheduled
        //    at construction), and the activation schedules B's
        //    `activation_spec` first wake-up at the NEXT spine tick
        //    — the deterministic event-driven mechanism (the wake
        //    transitions DORMANT → PATROL; the cadence chains then
        //    carry the machine onward). No idle per-frame adversary
        //    processing exists anywhere.
        if encounter.phase == EncounterPhase::Dormant && player.position[2] <= ACTIVATION_Z {
            encounter.phase = EncounterPhase::Active;
            let due = Tick(spine.run_ticks(0).0 + 1);
            if let Some(spec) = spine.world().activation_spec(0, due) {
                spine
                    .schedule(spec)
                    .expect("the activation schedule is dependency-free");
            }
            Self::log(encounter, tick, "encounter activated — the adversary stirs");
        }
        // 2. The cooldown advance (one transition per logical tick).
        if encounter.cooldown_remaining > 0 {
            encounter.cooldown_remaining -= 1;
        }
        // 3. The fire edge. Pre-activation (the Cycle-3.1 defect-A
        //    companion): the weapon does NOT resolve before the
        //    line — no incoherent damage on a dormant adversary, no
        //    kill-in-its-sleep path, no permanently-dormant
        //    unwinnable state (the encounter starts at the line, and
        //    the activation at step 1 happens FIRST within the same
        //    tick, so a press on the crossing tick resolves
        //    normally).
        if fire && encounter.phase == EncounterPhase::Dormant {
            Self::log(
                encounter,
                tick,
                "fire refused — the adversary is dormant (cross the line)",
            );
        } else if fire {
            Self::fire_weapon(
                encounter,
                spine,
                fire_world,
                config,
                player,
                player_eye_height,
                tick,
            );
        }
        // 4. SEAT B's adversary tick: the once-per-tick player
        //    sample (THIS tick's post-step position), then the
        //    spine's logical tick (the routine chains advance on
        //    their own cadences — the reviewed behavior machine,
        //    the 200/60 reservation).
        spine.world_mut().set_player_position(player.position);
        spine.run_tick();
        // 5. The surfaced attack (B's F-facing seam): when
        //    `attacks_emitted` advanced this tick, the encounter —
        //    the player's owning simulation — applies the RESOLVED
        //    outcome with F's damage policy (the Health-ownership
        //    convention: B surfaces, F applies).
        let (emitted, last_attack, state, health) = {
            let adversary = spine.world().adversary(0);
            (
                adversary.attacks_emitted,
                adversary.last_attack,
                adversary.state,
                adversary.health,
            )
        };
        if emitted > encounter.attacks_seen {
            encounter.attacks_seen = emitted;
            match last_attack {
                Some(HitResolution::Hit { .. }) => {
                    let damage = config.adversary_damage;
                    match encounter.player.apply_damage(damage) {
                        Ok(DamageOutcome::Wounded { remaining, .. }) => {
                            encounter.attacks_taken += 1;
                            encounter.last_damage_tick = Some(tick);
                            Self::log(
                                encounter,
                                tick,
                                format!(
                                    "adversary attack — player takes {:.0} ({:.0}/{:.0})",
                                    damage, remaining, encounter.player.maximum
                                ),
                            );
                        }
                        Ok(DamageOutcome::Defeated { .. }) => {
                            encounter.attacks_taken += 1;
                            encounter.last_damage_tick = Some(tick);
                            Self::log(
                                encounter,
                                tick,
                                format!(
                                    "adversary attack — player takes {:.0} (0/{:.0})",
                                    damage, encounter.player.maximum
                                ),
                            );
                        }
                        Ok(DamageOutcome::Graze) => {
                            encounter.attacks_taken += 1;
                            Self::log(encounter, tick, "adversary attack — a graze (no damage)");
                        }
                        Err(error) => {
                            // Impossible with these finite
                            // placeholder values — surfaced, never
                            // panicked (the contract's discipline).
                            Self::log(
                                encounter,
                                tick,
                                format!("adversary attack — damage refused ({error:?})"),
                            );
                        }
                    }
                }
                Some(HitResolution::Miss {
                    kind: MissKind::Occluded { .. },
                }) => {
                    Self::log(
                        encounter,
                        tick,
                        "adversary attack BLOCKED — cover protects the player",
                    );
                }
                // The honest just-out-of-reach line (review F5): the
                // strike was emitted and fell short.
                Some(HitResolution::Miss {
                    kind: MissKind::Clear,
                }) => {
                    Self::log(
                        encounter,
                        tick,
                        "adversary attack misses — the strike falls short",
                    );
                }
                Some(HitResolution::Refused(reason)) => {
                    Self::log(
                        encounter,
                        tick,
                        format!("adversary attack refused — {reason:?}"),
                    );
                }
                None => {}
            }
        }
        // 6. The DEFEATED read (B's terminal state; the engine's
        //    declaration entered it through `apply_player_hit`): F
        //    arms the presentation drain, then declares the win when
        //    it completes.
        if encounter.phase == EncounterPhase::Active && state == AdversaryState::Defeated {
            if !encounter.adversary_defeated {
                encounter.adversary_defeated = true;
                encounter.drain_remaining = config.defeated_drain_ticks;
            }
            if encounter.drain_remaining > 0 {
                encounter.drain_remaining -= 1;
            }
            if encounter.drain_remaining == 0 {
                encounter.phase = EncounterPhase::Won { tick };
                Self::log(
                    encounter,
                    tick,
                    format!(
                        "ADVERSARY DEFEATED — encounter WON at tick {} \
                         (fires {}, hits {}, damage taken {:.0})",
                        tick,
                        encounter.fires,
                        encounter.hits,
                        encounter.attacks_taken as f32 * config.adversary_damage
                    ),
                );
            }
        }
        // 7. The player's terminal (checked after the surfaced
        //    attack's application — the same-tick simultaneity
        //    resolves player-first via the step order above). The
        //    defeated read agrees exactly with the engine's
        //    declaration.
        if encounter.phase == EncounterPhase::Active && encounter.player.is_defeated() {
            encounter.phase = EncounterPhase::Lost { tick };
            Self::log(
                encounter,
                tick,
                format!(
                    "PLAYER DEFEATED — encounter LOST at tick {} \
                     (adversary {:.0}/{:.0})",
                    tick, health.current, health.maximum
                ),
            );
        }
    }

    /// The parts-scoped event log push (the transition and the fire
    /// resolution operate on the split fields).
    fn log(encounter: &mut EncounterState, tick: u64, line: impl Into<String>) {
        encounter.events.push((tick, line.into()));
    }

    /// The scripted-logical-tick path: one movement tick plus the
    /// combat transition (observing that tick's post-step state).
    /// `fire` is the FIRE EDGE for this tick (the mapping layer
    /// derives it from the mouse press; holding is not a new press).
    pub fn tick(&mut self, intent: MoveIntent, fire: bool) {
        let tick = self.ticks;
        self.ticks += 1;
        self.driver.run_headless(1, |_| intent);
        let state = *self.driver.state();
        let eye_height = self.driver.config().eye_height;
        Self::apply_combat_transition(
            &mut self.encounter,
            &mut self.spine,
            &self.fire_world,
            &self.config,
            &state,
            eye_height,
            tick,
            fire,
        );
    }

    /// The interactive frame path: one frame's elapsed time, the
    /// frame-sampled live intent, and the frame's fire edge. The
    /// C3-R1 interleaved composition: the movement and the combat
    /// transition advance TOGETHER at each logical tick — the
    /// `advance_observed` seam invokes the transition BETWEEN the
    /// driver's catch-up steps, each call observing THAT tick's own
    /// resulting player state (never the frame's final state), with
    /// the driver's accumulator, catch-up bound, no-discard drain,
    /// input-validity refusals, and accounting all preserved inside
    /// the driver (the demo duplicates none of it). The frame's fire
    /// edge lands on the FIRST executed tick (one press, one
    /// resolution, at the correct logical index); a 0-tick frame
    /// (frames outpacing the tick rate) runs NO transition and
    /// BUFFERS the press to the next executed tick — input is never
    /// dropped. The headless [`Self::tick`] path runs the SAME
    /// transition with the same per-tick state — one implementation,
    /// consistent ordering.
    pub fn advance_frame(
        &mut self,
        elapsed: Duration,
        intent: MoveIntent,
        fire: bool,
    ) -> engine_core::DriveStep {
        // The split-field composition: the observer holds the
        // encounter, the spine, and the config while the driver
        // advances — disjoint borrows, no driver access from the
        // combat side (the state arrives as the seam's snapshot).
        let CombatDemo {
            driver,
            encounter,
            spine,
            fire_world,
            config,
            ticks,
            pending_fire,
        } = self;
        let pending_before = *pending_fire;
        *pending_fire = false;
        let fire_edge = fire || pending_before;
        let eye_height = driver.config().eye_height;
        let step = driver.advance_observed(elapsed, intent, |state, index| {
            *ticks += 1;
            Self::apply_combat_transition(
                encounter,
                spine,
                fire_world,
                config,
                state,
                eye_height,
                *ticks,
                fire_edge && index == 1,
            );
        });
        // A 0-tick frame runs no transition — the press (or a
        // carried pending press) buffers to the next executed tick.
        if u64::from(step.executed_ticks) == 0 && fire_edge {
            *pending_fire = true;
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
    fn fire_weapon(
        encounter: &mut EncounterState,
        spine: &mut Spine<AdversaryWorld>,
        fire_world: &CollisionWorld,
        config: &CombatConfig,
        player: &PlayerState,
        player_eye_height: f32,
        tick: u64,
    ) {
        // The cooldown refusal (a press during cooldown is one
        // refused event — edge-triggered, never auto-repeat).
        if encounter.cooldown_remaining > 0 {
            Self::log(
                encounter,
                tick,
                format!(
                    "fire refused — the weapon is cooling ({} ticks)",
                    encounter.cooldown_remaining
                ),
            );
            return;
        }
        encounter.fires += 1;
        encounter.cooldown_remaining = config.weapon_cooldown_ticks;
        let fire_index = encounter.fires;
        // THE REQUEST (the contract's §4 F-flow): THIS tick's eye +
        // look direction + the placeholder weapon profile.
        let request = AttackRequest {
            attacker: PLAYER_ID,
            origin: [
                player.position[0],
                player.position[1] + player_eye_height,
                player.position[2],
            ],
            direction: look_direction(player.yaw, player.pitch),
            profile: AttackProfile {
                range: config.weapon_range,
                occlusion_margin: 0.0,
            },
        };
        let (body_min, body_max) = adversary_body(spine.world().adversary(0).position);
        let targets = [Combatant {
            id: ADVERSARY_ID,
            hit_box: StaticCollider {
                min: body_min,
                max: body_max,
            },
        }];
        // C3-R1 note: the trace resolves against the demo's static
        // arena copy (the SAME content the adversary moves and
        // perceives through and the player collides with — identical
        // boxes, built from the same literal; the split-borrow
        // composition's flagged copy).
        let resolution = resolve_attack(fire_world, &request, &targets);
        match resolution {
            HitResolution::Hit { .. } => {
                encounter.hits += 1;
                encounter.last_hit_tick = Some(tick);
                // THE CONTRACT'S TRANSITION, routed through B's
                // application API (the Health-ownership convention:
                // the adversary's Health lives in B's world; the
                // engine's only defeat declaration —
                // `DamageOutcome::Defeated` — enters B's terminal
                // DEFEATED state there, and the encounter's step 6
                // reads it). Damage on a PATROLLING adversary
                // provokes AGGRO (B's wired rule); a DEFEATED
                // adversary refuses further damage (None).
                match spine.world_mut().apply_player_hit(0, config.weapon_damage) {
                    Some(Ok(DamageOutcome::Wounded { remaining, .. })) => {
                        Self::log(
                            encounter,
                            tick,
                            format!(
                                "fire {fire_index}: HIT — adversary {:.0}/{:.0}",
                                remaining,
                                spine.world().adversary(0).health.maximum
                            ),
                        );
                    }
                    Some(Ok(DamageOutcome::Defeated { .. })) => {
                        Self::log(
                            encounter,
                            tick,
                            format!(
                                "fire {fire_index}: HIT — adversary 0/{:.0}",
                                spine.world().adversary(0).health.maximum
                            ),
                        );
                    }
                    Some(Ok(DamageOutcome::Graze)) => {
                        Self::log(
                            encounter,
                            tick,
                            format!("fire {fire_index}: HIT — a graze (no damage)"),
                        );
                    }
                    Some(Err(error)) => {
                        // Impossible with these finite placeholder
                        // values — surfaced, never panicked.
                        Self::log(
                            encounter,
                            tick,
                            format!("fire {fire_index}: damage refused ({error:?})"),
                        );
                    }
                    None => {
                        // The body was hit but the adversary is
                        // already DEFEATED (terminal — B's module
                        // refuses further damage): honest feedback.
                        Self::log(
                            encounter,
                            tick,
                            format!("fire {fire_index}: HIT — the adversary is already defeated"),
                        );
                    }
                }
            }
            HitResolution::Miss {
                kind: MissKind::Occluded { .. },
            } => {
                encounter.blocked += 1;
                Self::log(
                    encounter,
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
                let (min, max) = adversary_body(spine.world().adversary(0).position);
                match ray_box_entry(request.origin, request.direction, min, max) {
                    Some(entry) if entry > config.weapon_range => {
                        encounter.out_of_range += 1;
                        Self::log(
                            encounter,
                            tick,
                            format!(
                                "fire {fire_index}: OUT OF RANGE — {:.1} m > {:.0} m",
                                entry, config.weapon_range
                            ),
                        );
                    }
                    _ => {
                        encounter.misses += 1;
                        Self::log(
                            encounter,
                            tick,
                            format!("fire {fire_index}: MISS — the aim is off the target"),
                        );
                    }
                }
            }
            HitResolution::Refused(reason) => {
                // Impossible with these finite inputs — surfaced,
                // never panicked (the contract's discipline).
                Self::log(
                    encounter,
                    tick,
                    format!("fire {fire_index}: refused — {reason:?}"),
                );
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
        self.spine = adversary_spine(&self.config);
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
        // The adversary's body (B's live state): patrolling gray,
        // threatening red, hit-flash white, defeated dark.
        let adversary = self.spine.world().adversary(0);
        let threatening = matches!(
            adversary.state,
            AdversaryState::Aggro | AdversaryState::Attack | AdversaryState::Recover
        );
        let adversary_color = if adversary.state == AdversaryState::Defeated {
            [0.20, 0.20, 0.25]
        } else if let Some(last) = self.encounter.last_hit_tick {
            if self.ticks.saturating_sub(last) <= u64::from(self.config.hit_flash_ticks) {
                [0.95, 0.95, 0.95]
            } else if threatening {
                [0.75, 0.25, 0.20]
            } else {
                [0.50, 0.50, 0.55]
            }
        } else if threatening {
            [0.75, 0.25, 0.20]
        } else {
            [0.50, 0.50, 0.55]
        };
        let (min, max) = adversary_body(adversary.position);
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
/// entry and the reset battery): settle → cross the activation line
/// → hold position → wait for B's machine to turn threatening
/// (perception AGGRO — bounded, loud on stall) → fire on cooldown
/// whenever the adversary crosses the dead-ahead aim line → DEFEATED
/// → the drain → WON.
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

    // 2. Walk forward to the activation line (the encounter's
    //    orchestration marker flips in-transition).
    run_until(
        demo,
        forward(),
        false,
        |d| d.encounter.phase == EncounterPhase::Active,
        "activate the encounter",
    );

    // 3. Hold position and wait for the adversary's own wake (B's
    //    perception — AGGRO or beyond on its wake-up cadence).
    run_until(
        demo,
        idle(),
        false,
        |d| {
            matches!(
                d.spine.world().adversary(0).state,
                AdversaryState::Aggro
                    | AdversaryState::Attack
                    | AdversaryState::Recover
                    | AdversaryState::Defeated
            )
        },
        "wait for the adversary's aggro",
    );

    // 4. Fire on cooldown whenever the adversary is dead-ahead (the
    //    zero-angle discipline: the scripted player only shoots when
    //    the target crosses the aim line — B's pursuit converges on
    //    the player's position, so it crosses repeatedly) until the
    //    encounter resolves.
    for _ in 0..3_000 {
        if demo.encounter.phase != EncounterPhase::Active {
            break;
        }
        let on_line = demo.spine.world().adversary(0).position[0].abs() <= 0.4;
        let fire = demo.weapon_ready() && on_line;
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
/// settle → walk through activation, well inside the adversary's
/// perception ground → stand still, never fire → B's machine aggros,
/// pursues, and the cooldown-gated attacks defeat the player. The
/// reproducible failure. The walk's stop condition is PLAYER-ONLY
/// (a fixed z-line, never the adversary's position) so the player
/// trace stays cross-platform bit-stable regardless of the seeded
/// circuit arithmetic inside B's module.
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

    // 2. Walk forward past the activation line, well inside the
    //    adversary's aggro ground (the player-only stop: z ≤ 1.0).
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.driver.state().position[2] <= 1.0,
        "enter the adversary's ground",
    );

    // 3. Stand still and take it (fires 0 — the player who does not
    //    fight back loses; B's machine does the closing).
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
    let adversary = demo.spine.world().adversary(0);
    let adversary_word = match adversary.state {
        AdversaryState::Defeated => "DEFEATED",
        AdversaryState::Dormant => "DORMANT",
        AdversaryState::Patrol => "PATROL",
        AdversaryState::Aggro => "AGGRO",
        AdversaryState::Attack => "ATTACK",
        AdversaryState::Recover => "RECOVER",
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
        adversary.health.current,
        adversary.health.maximum,
        adversary.position[0],
        adversary.position[2],
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

/// The HUD health bar — the Cycle-4 feedback design §2's ASCII form
/// (the overlay's glyph source is font8x8's ASCII-only legacy font:
/// every non-ASCII code point renders as `?`, so the bar is
/// `[####------]`-style `#`/`-` cells). A presentation-only rounding
/// to the nearest of 10 cells; the numeric value beside the bar
/// carries the precision. The degenerate `maximum == 0` (a
/// born-defeated combatant) renders the empty bar.
pub fn health_bar(current: f32, maximum: f32) -> String {
    const CELLS: usize = 10;
    let fraction = if maximum > 0.0 {
        (current / maximum).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = ((fraction * CELLS as f32).round() as usize).min(CELLS);
    format!("[{}{}]", "#".repeat(filled), "-".repeat(CELLS - filled))
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
    if line.starts_with("encounter activated") {
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
        // The composition fix at integration: D's scene separation added
        // this field after F's branch diverged — the combat demo is a game
        // scene, so the legacy Stage-2 test scene stays omitted (the
        // playable_demo idiom).
        demo_scene: DemoScenePreference::Omitted,
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
            let adversary_state = match demo.spine.world().adversary(0).state {
                AdversaryState::Defeated => "defeated",
                AdversaryState::Dormant => "dormant",
                AdversaryState::Patrol => "patrol",
                AdversaryState::Aggro => "aggro",
                AdversaryState::Attack => "attack",
                AdversaryState::Recover => "recover",
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
                demo.spine.world().adversary(0).health.current,
                demo.spine.world().adversary(0).health.maximum,
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
