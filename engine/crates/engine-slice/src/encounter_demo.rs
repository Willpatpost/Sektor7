//! Cycle 4 capability D — the integrated encounter (`--encounter`):
//! ONE cohesive, interactively playable technical demonstration
//! exercising the new engine mechanisms together.
//!
//! **Integration state (the dependency walls, named):** this module
//! lands in three stages. (1) NOW: the full encounter LIFECYCLE —
//! the dormant-start activation, the genuine terminal freeze with
//! the immutable terminal record, the full-round reset, the ratified
//! EVENT-TIMESTAMPS contract applied from the first commit, the
//! single-precision weapon through the ACCEPTED combat contract
//! (bit-identical to SEAT B's `try_fire` precision pattern per
//! WEAPONS-INTERFACE §6.3 — the migration base), the victory/failure
//! paths, and the deterministic headless scenarios. (2) WEAPONS
//! (B's reviewed surface, at integration): the `Arsenal`/`try_fire`
//! migration — the prepared wiring is contract-exact. (3) The
//! multi-adversary wave composition (A's COMBATANTS-INTERFACE) and
//! the rotated cover pieces (C's COLLISION-INTERFACE) — the arena
//! literals reserve their slots; no population or rotated-geometry
//! logic exists here before those contracts land (GO §2/§4).
//!
//! **The timestamp contract (ratified v1, binding):** every event
//! label is the SPINE-ALIGNED 1-BASED index of the executed logical
//! tick (T1/T2): both progression paths derive it increment-first,
//! so the same executed tick carries the SAME label on the headless
//! and live routes (T6 — cadence equivalence is provable by direct
//! trace comparison), and the label equals the spine clock advanced
//! by that transition's `run_tick` (T4 — asserted by regression).
//! A full-round reset REBUILDS the spine (clock 0), so the round's
//! logical clock restarts with it — labels are round-scoped and the
//! `rounds` counter disambiguates (a contract clarification flagged
//! to the orchestrator; no event is ever stamped 0).
//!
//! Engine/game boundary: the mechanisms (hit resolution, damage,
//! the adversary behavior machine, the spine) are engine-core's
//! accepted surface; this module owns the encounter POLICY — the
//! placeholder values, the activation lines, the wave composition
//! (pending A), the weapon policy table (pending B's types), the
//! target mapping, the HUD DATA (the rendering is SEAT F's), and
//! the feedback wiring. Everything here is TRANSIENT,
//! process-local, never persisted; the reset is an honest round
//! reset, NOT a checkpoint.

use std::time::Duration;

use engine_core::adversary::{AdversaryConfig, AdversaryState, AdversaryWorld};
use engine_core::{
    Arsenal, AttackProfile, CameraMode, CollisionWorld, Combatant, DamageOutcome, Fingerprinter,
    FireContext, FireOutcome, FireRefusal, FiringPattern, Health, HitResolution, MissKind,
    MoveIntent, PlayerDriver, PlayerState, RotatedCollider, Spine, StaticCollider, Tick, WeaponId,
    WeaponSet, WeaponSpec, camera_pose_obstructed, health_full, look_direction, try_fire,
};
use engine_renderer::scene::SceneVertex;
use engine_renderer::{
    DemoScenePreference, PresentModePreference, RenderConfig, Renderer, ValidationPreference,
};
use engine_shell::{
    Audio, AudioStatus, DriverPreference, InputSnapshot, Keycode, MouseButton, ShellApp,
    ShellConfig,
};

use crate::encounter_events::{PresentationEvent, PresentationStream, RayFact, RayImpact, Volley};
use crate::encounter_frame::EncounterFrame;
use crate::frame_timing::FrameTiming;
use crate::math;
use crate::playable_demo::{map_playable_input, playable_player_config};

// ==================================================== the arena layout
//
// Hand-written (the F2 rule: an independent literal, never derived
// from the builders it checks — regression-pinned in
// tests/encounter_world.rs). A bounded crossfire courtyard along −Z:
// the player spawns at z = +8 facing −Z (yaw/pitch zero —
// dead-ahead), the first adversary DORMANT on the centerline ground
// at z = −4; the centerline corridor x ∈ [−0.6, 0.6] from spawn to
// the far wall stays clear of every solid box (the scripted shots,
// pursuit, and melee ride it). The central block and the wing walls
// are AXIS-ALIGNED structural cover (the baseline the rotated
// pieces extend); SEAT C's ROTATED cover slots are marked below and
// land with the COLLISION-INTERFACE (no rotated logic exists here
// before that contract).

/// One courtyard box: an axis-aligned extent plus its role.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EncounterBox {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub role: EncounterRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncounterRole {
    /// Courtyard perimeter walls (solid, gray).
    Perimeter,
    /// The central structural block (solid — the axis-aligned
    /// baseline cover the rotated pieces extend).
    CentralBlock,
    /// The corridor-forming wing walls (solid).
    WingWall,
}

impl EncounterRole {
    pub fn is_solid(self) -> bool {
        true
    }
}

/// The full courtyard layout, box for box (rendered, collided, and
/// occluded from the same list — collide-what-you-see).
pub const ENCOUNTER_BOXES: &[EncounterBox] = &[
    // Perimeter (x ±13, z −16..+10 — the ~26 m courtyard).
    EncounterBox {
        min: [-13.0, 0.0, -16.0],
        max: [-12.7, 3.0, 10.0],
        role: EncounterRole::Perimeter,
    },
    EncounterBox {
        min: [12.7, 0.0, -16.0],
        max: [13.0, 3.0, 10.0],
        role: EncounterRole::Perimeter,
    },
    EncounterBox {
        min: [-13.0, 0.0, 9.7],
        max: [13.0, 3.0, 10.0],
        role: EncounterRole::Perimeter,
    },
    EncounterBox {
        min: [-13.0, 0.0, -16.0],
        max: [13.0, 3.0, -15.7],
        role: EncounterRole::Perimeter,
    },
    // The central structural block (off the centerline — the
    // fight-around cover; 2.4 m, above the 1.6 m eye).
    EncounterBox {
        min: [2.0, 0.0, -8.0],
        max: [5.0, 2.4, -6.0],
        role: EncounterRole::CentralBlock,
    },
    // The wing walls (the corridor mouths toward the far ground).
    EncounterBox {
        min: [-6.3, 0.0, -12.0],
        max: [-6.0, 2.4, -9.0],
        role: EncounterRole::WingWall,
    },
    EncounterBox {
        min: [6.0, 0.0, -12.0],
        max: [6.3, 2.4, -9.0],
        role: EncounterRole::WingWall,
    },
    // SEAT C's rotated cover slots (COLLISION-INTERFACE pending):
    // a ~30° slab across the x ∈ [−5, −1] lane at z ≈ −5, and a
    // ~60° pillar near the second circuit at z ≈ −10 — deliberately
    // NOT axis-aligned placeholders here: rotated geometry lands as
    // real query-surface content or not at all (no visual-only
    // hitboxes — GO §4).
];

/// The player's spawn (the proven courtyard discipline: fall to the
/// ground, face −Z dead-ahead).
pub const PLAYER_SPAWN: [f32; 3] = [0.0, 2.0, 8.0];

/// The activation line (z ≤ this, once — the Dormant phase gate).
pub const ACTIVATION_Z: f32 = 2.0;

/// The SECOND activation line (z ≤ this, once while Active): the
/// deeper crossing that wakes wave 2 while the fight is live — the
/// per-adversary later-activation staging (the mid-fight ambush).
pub const ACTIVATION_LINE_B_Z: f32 = -4.0;

/// The adversary half-extents (the hit/visual body).
pub const ADVERSARY_HALF: [f32; 3] = [0.45, 0.9, 0.45];

/// The first circuit's center (the near adversary ground).
pub const ADVERSARY_SPAWN: [f32; 3] = [0.0, 0.0, -4.0];

/// The game-owned id space (the contract §4: the engine carries ids,
/// the game defines them).
pub const PLAYER_ID: u32 = 1;
pub const ADVERSARY_ID_BASE: u32 = 10;

/// The ~30° cover slab (the LEFT lane, mid-court): a long thin
/// rotated wall blocking one engagement angle while the other stays
/// clear — SEAT C's `RotatedCollider` (the COLLISION-INTERFACE §7
/// mapping; the worked geometry and the discriminator pairs are in
/// `D-rotated-cover-ready.md`). Height 2.4 — above the 1.6 m eye, a
/// true shot blocker. Shared by the collision world AND the render
/// boxes (collide-what-you-see, parameter for parameter).
pub fn cover_slab() -> RotatedCollider {
    RotatedCollider::from_base_yaw(
        [-5.5, 0.0, -5.0],
        4.0,
        0.6,
        2.4,
        std::f32::consts::FRAC_PI_6,
    )
    .expect("the cover slab is finite and positive")
}

/// The ~60° cover pillar (near the wave-2 circuit ground): a rotated
/// square pillar whose TRUE faces the movement, LOS, and fire must
/// respect. Height 3.0 — above the eye and the TP camera lift.
pub fn cover_pillar() -> RotatedCollider {
    RotatedCollider::from_base_yaw(
        [4.0, 0.0, -10.0],
        1.6,
        1.6,
        3.0,
        std::f32::consts::FRAC_PI_3,
    )
    .expect("the cover pillar is finite and positive")
}

/// Builds the courtyard collision world (the single literal every
/// consumer copies — the accepted three-copy composition). The
/// rotated cover pieces ride BESIDE the axis-aligned structure (the
/// contract §6.3: mixed worlds are the normal case; the empty
/// rotated list would take the exact existing paths — this world is
/// no longer empty, so every query is rotated-aware together).
pub fn encounter_arena_world() -> CollisionWorld {
    let mut world = CollisionWorld::new(0.0);
    for class_box in ENCOUNTER_BOXES {
        if !class_box.role.is_solid() {
            continue;
        }
        world = world.with_collider(StaticCollider {
            min: class_box.min,
            max: class_box.max,
        });
    }
    world = world.with_rotated_collider(Some(cover_slab()));
    world = world.with_rotated_collider(Some(cover_pillar()));
    world.with_bounds([-12.5, 0.0, -15.5], [12.5, 40.0, 9.5])
}

/// The adversary's body box at a position (the target mapping's
/// geometry).
pub fn adversary_body(position: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    (
        [
            position[0] - ADVERSARY_HALF[0],
            position[1],
            position[2] - ADVERSARY_HALF[2],
        ],
        [
            position[0] + ADVERSARY_HALF[0],
            position[1] + 2.0 * ADVERSARY_HALF[1],
            position[2] + ADVERSARY_HALF[2],
        ],
    )
}

// ==================================================== the encounter policy

/// The encounter's POLICY table — every number a PLACEHOLDER (the
/// owner ratifies balance; the engine contracts govern the
/// mechanics). The weapon fields are the PRECISION placeholder
/// profile expressed as encounter policy: at SEAT B's integration
/// these become the `WeaponSet` specs (the migration is
/// trace-preserving by the contract's §6.3 bit-identity), and the
/// SPREAD weapon's policy values join them (prepared wiring).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EncounterConfig {
    /// The precision weapon's effective range (m).
    pub weapon_range: f32,
    /// The precision weapon's flat damage per hit.
    pub weapon_damage: f32,
    /// The precision weapon's refire cooldown in CANONICAL LOGICAL
    /// TICKS (the WEAPONS-INTERFACE §4 deadline form — set only on a
    /// successful fire, compared, never decremented).
    pub weapon_cooldown_ticks: u64,
    /// The player's maximum (and starting) health.
    pub player_hp: f32,
    /// Each adversary's maximum (and starting) health.
    pub adversary_hp: f32,
    /// The adversary pursuit speed (m/s).
    pub adversary_speed: f32,
    /// The adversary melee reach (horizontal, center-to-center).
    pub adversary_attack_range: f32,
    /// The adversary attack cooldown in ticks.
    pub adversary_attack_cooldown_ticks: u32,
    /// The adversary melee damage per landed attack (the encounter
    /// applies it from the surfaced resolution).
    pub adversary_damage: f32,
    /// The DEFEATED drain length in ticks (the presentation window
    /// over the terminal state).
    pub defeated_drain_ticks: u32,
    /// The adversary world's deterministic seed.
    pub adversary_seed: u64,
    /// The adversary circuit's spawn center (the seeded circuits
    /// disperse around it; the default is the courtyard's adversary
    /// ground — the focused cover regressions override it).
    pub adversary_spawn: [f32; 3],
    /// How many adversaries the encounter spawns.
    pub adversary_count: usize,
    /// How many adversaries the FIRST activation line wakes (wave 1);
    /// the remainder wake at the second, deeper line (wave 2 — the
    /// mid-fight staging). Must be ≤ `adversary_count`.
    pub wave1_count: usize,
    // ---- The spread weapon's policy values (the WEAPONS-INTERFACE
    // ---- §1 WeaponSet construction; every number a PLACEHOLDER).
    /// The spread fan's ray count (B's bounded fan; 1..=64).
    pub spread_rays: u32,
    /// The spread fan's lateral step (m of perpendicular offset per
    /// ray index).
    pub spread_lateral_step: f32,
    /// The spread weapon's effective range (m).
    pub spread_range: f32,
    /// The spread weapon's refire cooldown in logical ticks.
    pub spread_cooldown_ticks: u64,
    /// The spread weapon's damage PER RAY (the encounter's per-ray,
    /// no-dedupe application policy).
    pub spread_damage: f32,
}

/// The SINGLE-adversary encounter values — the original pinned
/// scenario configuration (the winnable/failure headless runners run
/// THIS explicitly; their traces and pins are guards per the H
/// review's merge conditions — the wave composition is a SEPARATE
/// policy, not a repin of these).
pub fn encounter_single_config() -> EncounterConfig {
    EncounterConfig {
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
        adversary_seed: 0xE4C0u64,
        adversary_spawn: ADVERSARY_SPAWN,
        adversary_count: 1,
        wave1_count: 1,
        spread_rays: 5,
        spread_lateral_step: 0.08,
        spread_range: 12.0,
        spread_cooldown_ticks: 30,
        spread_damage: 8.0,
    }
}

/// The WAVE encounter values — the full capability-D composition
/// (the interactive `--encounter` default): four adversaries in two
/// activation waves, both weapons, the crossfire courtyard. Every
/// number a PLACEHOLDER (the owner ratifies balance); tuned so the
/// wave winnable script wins deterministically (measured + pinned).
pub fn encounter_config() -> EncounterConfig {
    EncounterConfig {
        weapon_range: 30.0,
        weapon_damage: 25.0,
        weapon_cooldown_ticks: 18,
        player_hp: 160.0,
        adversary_hp: 75.0,
        adversary_speed: 5.0,
        adversary_attack_range: 2.0,
        adversary_attack_cooldown_ticks: 24,
        adversary_damage: 12.0,
        defeated_drain_ticks: 6,
        adversary_seed: 0xE4C0u64,
        adversary_spawn: ADVERSARY_SPAWN,
        adversary_count: 4,
        wave1_count: 2,
        spread_rays: 5,
        spread_lateral_step: 0.08,
        spread_range: 12.0,
        spread_cooldown_ticks: 30,
        spread_damage: 8.0,
    }
}

/// The encounter's weapon policy table — SEAT B's `WeaponSet` built
/// from the encounter's placeholder values (WEAPONS-INTERFACE §1;
/// the precision spec reproduces the interim single-weapon values so
/// the pinned single-adversary traces are preserved by the §6.3
/// bit-identity).
fn encounter_weapon_set(config: &EncounterConfig) -> WeaponSet {
    WeaponSet {
        specs: [
            WeaponSpec {
                pattern: FiringPattern::Precision,
                profile: AttackProfile {
                    range: config.weapon_range,
                    occlusion_margin: 0.0,
                },
                cooldown_ticks: config.weapon_cooldown_ticks,
            },
            WeaponSpec {
                pattern: FiringPattern::Spread {
                    rays: config.spread_rays,
                    lateral_step: config.spread_lateral_step,
                },
                profile: AttackProfile {
                    range: config.spread_range,
                    occlusion_margin: 0.0,
                },
                cooldown_ticks: config.spread_cooldown_ticks,
            },
        ],
    }
}

// ==================================================== the adversary composition

/// The encounter's adversary configuration — the accepted mapping
/// from [`EncounterConfig`] (every number a placeholder; B's
/// structural defaults where the courtyard does not bind).
fn encounter_adversary_config(config: &EncounterConfig) -> AdversaryConfig {
    AdversaryConfig {
        seed: config.adversary_seed,
        count: config.adversary_count,
        tick_seconds: 1.0 / 60.0,
        aggro_radius: 8.0,
        deaggro_radius: 10.0,
        los_loss_ticks: 24,
        patrol_cadence: 12,
        aggro_cadence: 2,
        recover_cadence: 6,
        attack_cooldown: u64::from(config.adversary_attack_cooldown_ticks),
        attack_range: config.adversary_attack_range,
        speed: config.adversary_speed,
        waypoint_radius: 2.5,
        spawn_center: config.adversary_spawn,
        max_health: config.adversary_hp,
        eye_height: 1.5,
        player: playable_player_config(),
        player_id: PLAYER_ID,
        adversary_id_base: ADVERSARY_ID_BASE,
    }
}

/// Builds the encounter's spine over the adversary world: the
/// 200/60 reservation (the owner-gated default, untouched) and the
/// Cycle-3.1 dormant-start composition — every adversary DORMANT,
/// NOTHING scheduled at construction (`install_adversary_schedules`
/// is NOT called: its code schedules unconditionally over count
/// while its doc claims non-dormant-only — the recorded follow-up
/// SEAT A reconciles; a dormant-start encounter composes through the
/// per-adversary `activation_spec` event alone).
fn encounter_adversary_spine(config: &EncounterConfig) -> Spine<AdversaryWorld> {
    let mut spine = Spine::with_reservation(
        AdversaryWorld::new(encounter_adversary_config(config), encounter_arena_world()),
        200,
        60,
    );
    let count = spine.world().count();
    for index in 0..count {
        spine.world_mut().set_dormant(index);
    }
    spine
}

// ==================================================== the encounter state

/// The encounter phase (transient — process-local, never persisted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncounterPhase {
    /// Every adversary is DORMANT (pre-activation).
    Dormant,
    /// The fight is live.
    Active,
    /// Every adversary defeated (terminal until reset).
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

/// The outcome vocabulary of the terminal record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncounterOutcome {
    Won,
    Lost,
}

/// The fire/feedback counters (shared by the live state and the
/// terminal record's snapshot).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EncounterCounters {
    pub fires: u32,
    pub hits: u32,
    pub misses: u32,
    pub blocked: u32,
    pub out_of_range: u32,
    /// Landed adversary attacks against the player.
    pub attacks_taken: u32,
}

/// THE TERMINAL RECORD (the GO §7 follow-up made structural): the
/// IMMUTABLE value captured exactly once at the freeze — the
/// outcome, its logical tick (the ratified contract's base), and the
/// counters snapshot. Post-terminal presentation (the overlay, the
/// cues, SEAT F's defeated-player polish) READS this record; nothing
/// mutates it after capture. The record is a plain copied value, not
/// a live view — the single-writer discipline carried into the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalRecord {
    pub outcome: EncounterOutcome,
    pub tick: u64,
    pub counters: EncounterCounters,
}

/// The transient encounter state (never persisted; the counters and
/// the event log are the evidence record the headless runners
/// print). The adversary's live state lives in the spine's
/// [`AdversaryWorld`]; the encounter holds only the orchestration
/// reads and the player's [`Health`] (the Health-ownership
/// convention: the player's owning simulation is this encounter).
#[derive(Debug, Clone)]
pub struct EncounterState {
    pub phase: EncounterPhase,
    /// The player's health.
    pub player: Health,
    /// The fire/feedback counters.
    pub counters: EncounterCounters,
    /// The surfaced attacks already applied (the emitted-delta
    /// detection, per adversary — index-aligned).
    pub attacks_seen: Vec<u64>,
    /// Whether the last adversary's DEFEATED declaration was
    /// observed (the drain then runs down in the encounter).
    pub adversary_defeated: bool,
    /// The remaining DEFEATED drain ticks (the completion window).
    pub drain_remaining: u32,
    /// The completed round count (incremented by each reset).
    pub rounds: u32,
    /// Whether wave 2 has been activated (the deeper line's
    /// once-guard — restored to false by a full-round reset).
    pub wave2_activated: bool,
    /// The adversary's last hit tick (the hit-flash read).
    pub last_hit_tick: Option<u64>,
    /// The player's last damage tick (the HUD damage line).
    pub last_damage_tick: Option<u64>,
    /// The last fire outcome's HUD line (targeting feedback).
    pub last_fire_line: Option<String>,
    /// The last weapon-selection feedback line (the HUD surface —
    /// never the event log; the selection is player policy, not a
    /// transition event).
    pub last_selection_line: Option<String>,
    /// (tick, line) — the deterministic event log, labels per the
    /// ratified EVENT-TIMESTAMPS contract (round-scoped, 1-based).
    pub events: Vec<(u64, String)>,
    /// Disposable render/audio output, outside the authoritative fingerprint.
    pub presentation: PresentationStream,
    /// The immutable terminal record, captured once at the freeze.
    pub terminal: Option<TerminalRecord>,
    /// Whether the next executed tick logs the round banner (set by
    /// a reset; consumed at tick 1 — no event is ever stamped 0).
    round_banner_pending: bool,
}

impl EncounterState {
    fn fresh(config: &EncounterConfig) -> EncounterState {
        EncounterState {
            phase: EncounterPhase::Dormant,
            player: health_full(config.player_hp)
                .expect("the placeholder player maximum is finite and nonnegative"),
            counters: EncounterCounters::default(),
            attacks_seen: vec![0; config.adversary_count],
            adversary_defeated: false,
            drain_remaining: 0,
            rounds: 0,
            wave2_activated: false,
            last_hit_tick: None,
            last_damage_tick: None,
            last_fire_line: None,
            last_selection_line: None,
            events: Vec::new(),
            presentation: PresentationStream::default(),
            terminal: None,
            round_banner_pending: true,
        }
    }
}

// ==================================================== the HUD data (SEAT F renders)

/// One adversary's HUD line (the multi-target awareness data).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdversaryHudLine {
    pub id: u32,
    /// The state word (DORMANT/PATROL/AGGRO/ATTACK/RECOVER/DEFEATED).
    pub state: &'static str,
    pub health: (f32, f32),
    /// Whether the adversary's attack cooldown has elapsed (the
    /// threat read).
    pub attack_ready: bool,
}

/// The weapon's HUD line (the selection discriminant + readiness —
/// the WEAPONS-INTERFACE's F-facing data).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeaponHudLine {
    /// The selected weapon (the WEAPONS-INTERFACE identity — the
    /// HUD discriminant).
    pub selected: WeaponId,
    /// The SELECTED weapon's readiness deadline in the round's tick
    /// base (a readiness bar's target tick).
    pub ready_at: u64,
    /// Whether the selected weapon is ready at the read's tick.
    pub ready: bool,
}

/// The structured HUD/feedback snapshot — SEAT D owns the DATA, SEAT
/// F renders it. Computed on read (no per-frame maintenance), from
/// the live state; after a terminal the `terminal` record is the
/// frozen read.
#[derive(Debug, Clone, PartialEq)]
pub struct EncounterHud {
    pub phase: EncounterPhase,
    pub rounds: u32,
    pub player_health: (f32, f32),
    pub player_position: [f32; 3],
    pub player_mode: &'static str,
    pub adversaries: Vec<AdversaryHudLine>,
    pub weapon: WeaponHudLine,
    pub counters: EncounterCounters,
    pub terminal: Option<TerminalRecord>,
    /// The last fire outcome's feedback line (targeting data).
    pub last_fire_line: Option<String>,
    /// The last weapon-selection feedback line.
    pub last_selection_line: Option<String>,
}

// ==================================================== the demo

/// The integrated encounter demo: the player driver plus the
/// transient encounter state, with the per-tick encounter logic
/// shared by the interactive and headless paths (the single-
/// transition discipline — both progression routes run the SAME
/// `apply_encounter_transition`, so the gameplay semantics exist
/// exactly once, and both label the same executed tick identically
/// per the ratified timestamp contract).
pub struct EncounterDemo {
    pub driver: PlayerDriver,
    pub encounter: EncounterState,
    /// The adversary world, driven on the spine (the accepted
    /// six-state behavior machine; the encounter-facing hooks are
    /// the reads and `apply_player_hit`).
    pub spine: Spine<AdversaryWorld>,
    /// The fire resolution's collision copy — the SAME courtyard
    /// content as the driver's and the adversary's worlds (built
    /// from the same literal; the arena is static in the round, so
    /// the copies never diverge). The accepted split-borrow
    /// composition (the C3-R1 seam needs a collision reference the
    /// observer closure can hold while the driver advances).
    fire_world: CollisionWorld,
    /// SEAT B's weapon state: the selection + the per-weapon
    /// cooldown deadlines (the WEAPONS-INTERFACE §2/§4 surface —
    /// pure state, zero idle cost; rebuilt to the starting weapon by
    /// a full-round reset).
    pub arsenal: Arsenal,
    /// The encounter's weapon policy table (the §1 WeaponSet —
    /// static for the demo's lifetime).
    weapons: WeaponSet,
    config: EncounterConfig,
    /// The ROUND's logical tick counter (restarted by a reset with
    /// the rebuilt spine — the timestamp contract's round scoping).
    ticks: u64,
    /// A fire press observed on a frame that executed no logical
    /// tick: buffered to the next executed tick's transition — input
    /// is never dropped (the review-F2 pacing discipline).
    pending_fire: bool,
}

impl Default for EncounterDemo {
    fn default() -> Self {
        Self::new()
    }
}

impl EncounterDemo {
    pub fn new() -> EncounterDemo {
        EncounterDemo::new_with_config(encounter_config())
    }

    /// Test constructor with an explicit policy config.
    pub fn new_with_config(config: EncounterConfig) -> EncounterDemo {
        let driver = PlayerDriver::new(
            PlayerState::spawn(PLAYER_SPAWN),
            playable_player_config(),
            encounter_arena_world(),
            crate::player_demo::DEMO_TICK_HZ,
        );
        EncounterDemo {
            driver,
            encounter: EncounterState::fresh(&config),
            spine: encounter_adversary_spine(&config),
            fire_world: encounter_arena_world(),
            arsenal: Arsenal::new(WeaponId::Precision),
            weapons: encounter_weapon_set(&config),
            config,
            ticks: 0,
            pending_fire: false,
        }
    }

    pub fn config(&self) -> &EncounterConfig {
        &self.config
    }

    /// The ROUND's next logical tick index (the label the next
    /// executed tick will carry — 1-based per the contract).
    pub fn tick_index(&self) -> u64 {
        self.ticks
    }

    /// Whether the SELECTED weapon is ready at the round's current
    /// tick (the §4 pure comparison, against B's Arsenal).
    pub fn weapon_ready(&self) -> bool {
        self.arsenal.is_ready(Tick(self.ticks))
    }

    /// The weapon-selection edge (the interactive mapping and the
    /// headless scripts both drive this; POLICY state, not combat
    /// resolution — it never enters the per-tick transition, so the
    /// cadence equivalence and the event log's pure transition
    /// semantics are unaffected). The lifecycle rules are honest: a
    /// selection while DORMANT or post-terminal is refused with a
    /// feedback line and changes NOTHING (the fire edge's own
    /// discipline, mirrored); otherwise the Arsenal's instant,
    /// cooldown-untouched `select` applies. The feedback surfaces in
    /// the HUD data (`last_selection_line`), never the event log.
    pub fn apply_selection(&mut self, weapon: WeaponId) {
        if self.encounter.phase.is_terminal() {
            self.encounter.last_selection_line =
                Some("weapon switch refused — the encounter is over ([R] reset)".to_string());
            return;
        }
        if self.encounter.phase == EncounterPhase::Dormant {
            self.encounter.last_selection_line = Some(
                "weapon switch refused — the adversaries are dormant (cross the line)".to_string(),
            );
            return;
        }
        if self.arsenal.select(weapon) {
            self.encounter
                .presentation
                .push(self.ticks, PresentationEvent::Switched(weapon));
            self.encounter.last_selection_line =
                Some(format!("weapon selected — {}", weapon_word(weapon)));
        }
    }

    /// The spine's clock (the timestamp contract's T4 alignment
    /// read — the label base).
    pub fn spine_clock(&self) -> Tick {
        self.spine.clock()
    }

    /// THE FULL-ROUND RESET (R): rebuild the driver, the dormant
    /// spine, and the fresh encounter state — restoring the
    /// pre-activation protection exactly (every adversary DORMANT,
    /// nothing scheduled, healths full, the weapon ready). The
    /// round's logical clock restarts with the rebuilt spine (the
    /// timestamp contract's round scoping); the `rounds` counter
    /// carries the history. An honest transient round reset — NOT a
    /// checkpoint.
    pub fn reset(&mut self) {
        let config = self.config;
        let rounds = self.encounter.rounds + 1;
        self.driver = PlayerDriver::new(
            PlayerState::spawn(PLAYER_SPAWN),
            playable_player_config(),
            encounter_arena_world(),
            crate::player_demo::DEMO_TICK_HZ,
        );
        // G's NB-3 pairing rule (BINDING): the spine is REBUILT, not
        // in-place reset — the fresh dormant world plus the fresh
        // Arsenal (the starting weapon, all deadlines ready) and the
        // fresh encounter state together.
        self.spine = encounter_adversary_spine(&config);
        self.fire_world = encounter_arena_world();
        self.arsenal = Arsenal::new(WeaponId::Precision);
        self.encounter = EncounterState::fresh(&config);
        self.encounter.rounds = rounds;
        self.ticks = 0;
        self.pending_fire = false;
    }

    /// The structured HUD snapshot (the data SEAT F renders;
    /// computed on read).
    pub fn hud(&self) -> EncounterHud {
        let mut adversaries = Vec::with_capacity(self.spine.world().count());
        for index in 0..self.spine.world().count() {
            let adversary = self.spine.world().adversary(index);
            adversaries.push(AdversaryHudLine {
                id: ADVERSARY_ID_BASE + index as u32,
                state: adversary_state_word(adversary.state),
                health: (adversary.health.current, adversary.health.maximum),
                attack_ready: adversary.cooldown_until <= self.spine_clock().0,
            });
        }
        EncounterHud {
            phase: self.encounter.phase,
            rounds: self.encounter.rounds,
            player_health: (self.encounter.player.current, self.encounter.player.maximum),
            player_position: self.driver.state().position,
            player_mode: match self.driver.state().camera_mode {
                CameraMode::FirstPerson => "first",
                CameraMode::ThirdPerson => "third",
            },
            adversaries,
            weapon: WeaponHudLine {
                selected: self.arsenal.selected(),
                ready_at: self.arsenal.ready_at(self.arsenal.selected()),
                ready: self.arsenal.is_ready(Tick(self.ticks)),
            },
            counters: self.encounter.counters,
            terminal: self.encounter.terminal,
            last_fire_line: self.encounter.last_fire_line.clone(),
            last_selection_line: self.encounter.last_selection_line.clone(),
        }
    }

    /// The scripted-logical-tick path: one movement tick plus the
    /// encounter transition (observing that tick's post-step state).
    /// `fire` is the FIRE EDGE for this tick. The label is
    /// INCREMENT-FIRST (the ratified contract's T2): the first
    /// executed tick of a round is tick 1 — identical to the live
    /// path's labels (T6).
    pub fn tick(&mut self, intent: MoveIntent, fire: bool) {
        self.ticks += 1;
        let tick = self.ticks;
        self.driver.run_headless(1, |_| intent);
        let state = *self.driver.state();
        let eye_height = self.driver.config().eye_height;
        Self::apply_encounter_transition(
            &mut self.encounter,
            &mut self.spine,
            &mut self.arsenal,
            &self.weapons,
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
    /// accepted C3-R1 interleaved composition (`advance_observed`):
    /// the movement and the encounter transition advance TOGETHER at
    /// each logical tick, each call observing THAT tick's own
    /// resulting player state; the frame's fire edge lands on the
    /// FIRST executed tick; a 0-tick frame runs NO transition and
    /// BUFFERS the press. The labels are the live path's existing
    /// increment-first convention (T3) — identical to [`Self::tick`]
    /// (T6, asserted by regression).
    pub fn advance_frame(
        &mut self,
        elapsed: Duration,
        intent: MoveIntent,
        fire: bool,
    ) -> engine_core::DriveStep {
        let EncounterDemo {
            driver,
            encounter,
            spine,
            arsenal,
            weapons,
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
            Self::apply_encounter_transition(
                encounter,
                spine,
                arsenal,
                weapons,
                fire_world,
                config,
                state,
                eye_height,
                *ticks,
                fire_edge && index == 1,
            );
        });
        if u64::from(step.executed_ticks) == 0 && fire_edge {
            *pending_fire = true;
        }
        step
    }

    /// THE ENCOUNTER TRANSITION — the single implementation both
    /// progression routes run (the deterministic step order is
    /// player-first: the fire edge resolves before the adversary
    /// tick, exactly the accepted combat order). The parts-scoped
    /// parameters are exactly what the split-field composition needs
    /// (the accepted combat-transition pattern).
    #[allow(clippy::too_many_arguments)]
    fn apply_encounter_transition(
        encounter: &mut EncounterState,
        spine: &mut Spine<AdversaryWorld>,
        arsenal: &mut Arsenal,
        weapons: &WeaponSet,
        fire_world: &CollisionWorld,
        config: &EncounterConfig,
        player: &PlayerState,
        player_eye_height: f32,
        tick: u64,
        fire: bool,
    ) {
        // 0. THE TERMINAL GUARD (the Cycle-3.1 defect-B guarantee):
        //    WON and LOST are GENUINE terminal states — the result
        //    and its logical timestamp are preserved (the immutable
        //    terminal record), the counters freeze, and ALL combat
        //    effects stop (no further damage, no weapon resolution,
        //    no AI effects — the spine stops advancing). The
        //    presentation continues OUTSIDE this transition (the
        //    render loop, the camera, the overlay, the cues); R is
        //    the explicit fresh-round reset. A post-terminal press
        //    gets the honest refusal line.
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
        // 0.5 The round banner (a reset's marker, logged at the
        //     round's FIRST executed tick — never at 0, per T1).
        if encounter.round_banner_pending {
            encounter.round_banner_pending = false;
            encounter
                .presentation
                .push(tick, PresentationEvent::RoundStarted);
            Self::log(
                encounter,
                tick,
                format!(
                    "round {} begins — every adversary dormant",
                    encounter.rounds + 1
                ),
            );
        }
        // 1. Activation: the WAVE staging. Line A (once — the
        //    Dormant phase gate makes it single-shot) starts the
        //    encounter and wakes wave 1 (the first `wave1_count`
        //    adversaries); the DEEPER line B (once while Active)
        //    wakes wave 2 while the fight is live — the per-adversary
        //    later activation without duplicate chains (A's
        //    COMBATANTS-INTERFACE: the activation payload wakes only
        //    a DORMANT adversary; duplicates are inert). Every first
        //    wake-up is scheduled through its own `activation_spec`
        //    at the NEXT spine tick (the deterministic event-driven
        //    mechanism; nothing is scheduled at construction).
        if encounter.phase == EncounterPhase::Dormant && player.position[2] <= ACTIVATION_Z {
            encounter.phase = EncounterPhase::Active;
            let due = Tick(spine.run_ticks(0).0 + 1);
            let wave1 = config.wave1_count.min(spine.world().count());
            for index in 0..wave1 {
                if let Some(spec) = spine.world().activation_spec(index, due) {
                    spine
                        .schedule(spec)
                        .expect("the activation schedule is dependency-free");
                }
            }
            let count = spine.world().count();
            // The single-wave composition keeps the ORIGINAL event
            // text (the pinned single-adversary traces fingerprint
            // the event log — guards per the H-review conditions);
            // the wave wording appears only when a second wave
            // exists.
            let activation_line = if wave1 < count {
                format!(
                    "encounter activated — wave 1 of {wave1} adversary stirring \
                     ({count} total)"
                )
            } else {
                format!("encounter activated — {count} adversary stirring")
            };
            Self::log(encounter, tick, activation_line);
            encounter
                .presentation
                .push(tick, PresentationEvent::Activated);
        }
        if encounter.phase == EncounterPhase::Active
            && !encounter.wave2_activated
            && player.position[2] <= ACTIVATION_LINE_B_Z
        {
            encounter.wave2_activated = true;
            let due = Tick(spine.run_ticks(0).0 + 1);
            let wave1 = config.wave1_count.min(spine.world().count());
            let mut woken = 0;
            for index in wave1..spine.world().count() {
                if let Some(spec) = spine.world().activation_spec(index, due) {
                    spine
                        .schedule(spec)
                        .expect("the activation schedule is dependency-free");
                    woken += 1;
                }
            }
            Self::log(
                encounter,
                tick,
                format!("wave 2 stirs — {woken} more adversary awakening mid-fight"),
            );
        }
        // 2. The fire edge. Pre-activation (the Cycle-3.1 defect-A
        //    companion): the weapon does NOT resolve before the
        //    line — no incoherent damage on a dormant adversary, no
        //    kill-in-its-sleep, no permanently-dormant unwinnable
        //    state (the activation at step 1 happens FIRST within
        //    the same tick, so a press on the crossing tick resolves
        //    normally).
        if fire && encounter.phase == EncounterPhase::Dormant {
            Self::log(
                encounter,
                tick,
                "fire refused — the adversaries are dormant (cross the line)",
            );
        } else if fire {
            Self::fire_weapon(
                encounter,
                spine,
                arsenal,
                weapons,
                fire_world,
                config,
                player,
                player_eye_height,
                tick,
            );
        }
        // 3. The adversary tick: the once-per-tick player sample
        //    (THIS tick's post-step position), then the spine's
        //    logical tick (the routine chains advance on their own
        //    cadences — the 200/60 reservation).
        spine.world_mut().set_player_position(player.position);
        spine.run_tick();
        // 4. The surfaced attacks (the per-adversary emitted-delta
        //    drain — the multi-adversary read, index-ascending, the
        //    deterministic same-tick order): when an adversary's
        //    `attacks_emitted` advanced this tick, the encounter —
        //    the player's owning simulation — applies the RESOLVED
        //    outcome with the encounter's damage policy (the
        //    Health-ownership convention: the adversary world
        //    surfaces, the encounter applies).
        for index in 0..spine.world().count() {
            let (emitted, last_attack) = {
                let adversary = spine.world().adversary(index);
                (adversary.attacks_emitted, adversary.last_attack)
            };
            if emitted > encounter.attacks_seen[index] {
                encounter.attacks_seen[index] = emitted;
                match last_attack {
                    Some(HitResolution::Hit { .. }) => {
                        let damage = config.adversary_damage;
                        match encounter.player.apply_damage(damage) {
                            Ok(DamageOutcome::Wounded { remaining, .. }) => {
                                encounter.counters.attacks_taken += 1;
                                encounter.last_damage_tick = Some(tick);
                                encounter
                                    .presentation
                                    .push(tick, PresentationEvent::PlayerDamaged);
                                Self::log(
                                    encounter,
                                    tick,
                                    format!(
                                        "adversary {index} attack — player takes {:.0} \
                                         ({:.0}/{:.0})",
                                        damage, remaining, encounter.player.maximum
                                    ),
                                );
                            }
                            Ok(DamageOutcome::Defeated { .. }) => {
                                encounter.counters.attacks_taken += 1;
                                encounter.last_damage_tick = Some(tick);
                                encounter
                                    .presentation
                                    .push(tick, PresentationEvent::PlayerDamaged);
                                Self::log(
                                    encounter,
                                    tick,
                                    format!(
                                        "adversary {index} attack — player takes {:.0} \
                                         (0/{:.0})",
                                        damage, encounter.player.maximum
                                    ),
                                );
                            }
                            Ok(DamageOutcome::Graze) => {
                                encounter.counters.attacks_taken += 1;
                                Self::log(
                                    encounter,
                                    tick,
                                    "adversary attack — a graze (no damage)",
                                );
                            }
                            Err(error) => {
                                // Impossible with these finite placeholder
                                // values — surfaced, never panicked.
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
        }
        // 5. The DEFEATED read (the engine's declaration, entered
        //    through `apply_player_hit`): the encounter arms the
        //    presentation drain, then declares the win when it
        //    completes. Victory is EVERY adversary defeated — read
        //    through the ENGINE's `all_defeated()`, which explicitly
        //    REFUSES to declare an empty world victorious (the C4-R3
        //    corrective: the previous inline `all()` over the index
        //    range returned true for a zero-adversary population —
        //    a silent empty victory the engine's contract rejects).
        if encounter.phase == EncounterPhase::Active {
            let all_defeated = spine.world().all_defeated();
            if all_defeated {
                if !encounter.adversary_defeated {
                    encounter.adversary_defeated = true;
                    encounter.drain_remaining = config.defeated_drain_ticks;
                }
                if encounter.drain_remaining > 0 {
                    encounter.drain_remaining -= 1;
                }
                if encounter.drain_remaining == 0 {
                    encounter.phase = EncounterPhase::Won { tick };
                    encounter
                        .presentation
                        .push(tick, PresentationEvent::Terminal(EncounterOutcome::Won));
                    encounter.terminal = Some(TerminalRecord {
                        outcome: EncounterOutcome::Won,
                        tick,
                        counters: encounter.counters,
                    });
                    Self::log(
                        encounter,
                        tick,
                        format!(
                            "ALL ADVERSARIES DEFEATED — encounter WON at tick {tick} \
                             (fires {} · hits {} · attacks taken {})",
                            encounter.counters.fires,
                            encounter.counters.hits,
                            encounter.counters.attacks_taken
                        ),
                    );
                }
            }
        }
        // 6. The player's terminal (checked after the surfaced
        //    attack's application — the same-tick simultaneity
        //    resolves player-first via the step order above).
        if encounter.phase == EncounterPhase::Active && encounter.player.is_defeated() {
            encounter.phase = EncounterPhase::Lost { tick };
            encounter
                .presentation
                .push(tick, PresentationEvent::Terminal(EncounterOutcome::Lost));
            encounter.terminal = Some(TerminalRecord {
                outcome: EncounterOutcome::Lost,
                tick,
                counters: encounter.counters,
            });
            Self::log(
                encounter,
                tick,
                format!(
                    "PLAYER DEFEATED — encounter LOST at tick {tick} \
                     (fires {} · attacks taken {})",
                    encounter.counters.fires, encounter.counters.attacks_taken
                ),
            );
        }
    }

    /// The fire resolution — SEAT B's `try_fire` through the
    /// ratified `FireContext` form (WEAPONS-INTERFACE v1): the
    /// engine derives the rays, resolves each against the targets
    /// through the combat contract, and keeps the per-weapon
    /// cooldown deadlines (§4 — set only on a successful fire; a
    /// refused press spends nothing). The encounter owns the policy:
    /// the weapon values (the `WeaponSet`), the per-shot damage
    /// application (PER RAY, no same-target dedupe within a volley —
    /// the ray count IS the spread weapon's character against a
    /// single target; the Health-ownership convention: `try_fire`
    /// never mutates `Health`, the encounter routes through
    /// `apply_player_hit`), and the out-of-range feedback
    /// distinction on top of the contract's `Miss::Clear`. The
    /// single-shot (precision) feedback lines keep the ORIGINAL
    /// format exactly — the pinned single-adversary traces
    /// fingerprint the event log (guards); the spread's multi-shot
    /// volleys carry a per-ray marker.
    #[allow(clippy::too_many_arguments)]
    fn fire_weapon(
        encounter: &mut EncounterState,
        spine: &mut Spine<AdversaryWorld>,
        arsenal: &mut Arsenal,
        weapons: &WeaponSet,
        fire_world: &CollisionWorld,
        config: &EncounterConfig,
        player: &PlayerState,
        player_eye_height: f32,
        tick: u64,
    ) {
        // The target mapping (the encounter's policy): the live
        // adversary boxes, skipping DEFEATED bodies.
        let mut targets = Vec::with_capacity(spine.world().count());
        encounter_targets(&mut targets, spine.world());
        // THE CONTEXT (the contract's §3 FireContext form): THIS
        // tick's eye + look direction (camera-agnostic — the same
        // request in both camera modes), the targets, the shared
        // fire world, and the canonical tick.
        let origin = [
            player.position[0],
            player.position[1] + player_eye_height,
            player.position[2],
        ];
        let aim = look_direction(player.yaw, player.pitch);
        let context = FireContext {
            attacker: PLAYER_ID,
            origin,
            aim,
            targets: &targets,
            world: fire_world,
            now: Tick(tick),
        };
        let outcome = try_fire(arsenal, weapons, &context);
        match outcome {
            FireOutcome::Fired { weapon, shots } => {
                encounter.counters.fires += 1;
                let fire_index = encounter.counters.fires;
                let volley = shots.len();
                let mut rays = encounter.presentation.ray_buffer(volley);
                let damage = match weapon {
                    WeaponId::Precision => config.weapon_damage,
                    WeaponId::Spread => config.spread_damage,
                };
                let mut last_line = None;
                for (ray, shot) in shots.iter().enumerate() {
                    // The single-shot volley keeps the ORIGINAL line
                    // format (the pinned traces); the multi-ray
                    // spread carries the ray marker.
                    let prefix = if volley == 1 {
                        format!("fire {fire_index}")
                    } else {
                        format!("fire {fire_index} ray {ray}")
                    };
                    let impact;
                    let line = match &shot.resolution {
                        HitResolution::Hit {
                            target, distance, ..
                        } => {
                            let index = usize::try_from(*target - ADVERSARY_ID_BASE)
                                .unwrap_or(usize::MAX)
                                .min(spine.world().count().saturating_sub(1));
                            // THE INACTIVE-BODY POLICY (the C4-R2
                            // corrective): a DORMANT, not-yet-activated
                            // adversary's body is a NON-DAMAGEABLE
                            // PHYSICAL OBSTRUCTION — it intercepts the
                            // ray (collide-what-you-see: the visible
                            // body is physically present) but the
                            // encounter's damage policy refuses to
                            // route weapon damage to it before ITS
                            // activation event, with honest feedback.
                            // The engine's dormant-damage capability
                            // (`apply_player_hit` on a dormant
                            // adversary) is PRESERVED for scenarios
                            // that intentionally attack inactive
                            // characters — this is the two-wave
                            // encounter-composition layer's policy.
                            if spine.world().adversary(index).state == AdversaryState::Dormant {
                                encounter.counters.blocked += 1;
                                impact = Some(RayImpact::Dormant { target: *target });
                                format!(
                                    "{prefix} — the DORMANT body of adversary {index} \
                                     absorbs the shot (no damage before its activation)"
                                )
                            } else {
                                encounter.counters.hits += 1;
                                encounter.last_hit_tick = Some(tick);
                                let applied = spine.world_mut().apply_player_hit(index, damage);
                                impact = Some(RayImpact::Adversary {
                                    target: *target,
                                    damaged: matches!(
                                        applied,
                                        Some(Ok(DamageOutcome::Wounded { .. }
                                            | DamageOutcome::Defeated { .. }))
                                    ),
                                    defeated: matches!(
                                        applied,
                                        Some(Ok(DamageOutcome::Defeated { .. }))
                                    ),
                                });
                                match applied {
                                    Some(Ok(DamageOutcome::Wounded { remaining, .. })) => {
                                        format!(
                                            "{prefix} — HIT adversary {index} at {distance:.1} m \
                                             ({remaining:.0} hp left)"
                                        )
                                    }
                                    Some(Ok(DamageOutcome::Defeated { .. })) => format!(
                                        "{prefix} — HIT adversary {index} at {distance:.1} m \
                                         (DEFEATED)"
                                    ),
                                    Some(Ok(DamageOutcome::Graze)) => format!(
                                        "{prefix} — HIT adversary {index} at {distance:.1} m \
                                         (a graze)"
                                    ),
                                    Some(Err(error)) => format!(
                                        "{prefix} — HIT adversary {index} but the damage \
                                         refused ({error:?})"
                                    ),
                                    None => format!(
                                        "{prefix} — HIT adversary {index} at {distance:.1} m \
                                         (already defeated)"
                                    ),
                                }
                            }
                        }
                        HitResolution::Miss {
                            kind: MissKind::Occluded { .. },
                        } => {
                            encounter.counters.blocked += 1;
                            impact = Some(RayImpact::Cover);
                            format!("{prefix} — BLOCKED by cover")
                        }
                        HitResolution::Miss {
                            kind: MissKind::Clear,
                        } => {
                            // The honest out-of-range distinction (the
                            // accepted demo's feedback policy): an
                            // unobstructed ray that WOULD have reached a
                            // live target beyond the range says so;
                            // otherwise the shot was simply wide.
                            let mut reachable = false;
                            for target in &targets {
                                if ray_box_entry(
                                    &origin,
                                    &shot.direction,
                                    &target.hit_box.min,
                                    &target.hit_box.max,
                                )
                                .is_some()
                                {
                                    reachable = true;
                                    break;
                                }
                            }
                            if reachable {
                                encounter.counters.out_of_range += 1;
                                impact = Some(RayImpact::OutOfRange);
                                format!("{prefix} — out of range")
                            } else {
                                encounter.counters.misses += 1;
                                impact = Some(RayImpact::Miss);
                                format!("{prefix} — wide (no target on the line)")
                            }
                        }
                        HitResolution::Refused(reason) => {
                            impact = None;
                            format!("{prefix} — refused ({reason:?})")
                        }
                    };
                    if let (Some(impact), Some(endpoint)) = (
                        impact,
                        shot.resolution.endpoint(
                            origin,
                            shot.direction,
                            weapons.spec(weapon).profile.range,
                        ),
                    ) {
                        rays.push(RayFact {
                            direction: shot.direction,
                            endpoint,
                            impact,
                        });
                    } else {
                        encounter.presentation.invalid_ray();
                    }
                    last_line = Some(line.clone());
                    Self::log(encounter, tick, line);
                }
                if let Some(line) = last_line {
                    encounter.last_fire_line = Some(line);
                }
                encounter.presentation.push(
                    tick,
                    PresentationEvent::Fired(Volley {
                        id: fire_index,
                        weapon,
                        origin,
                        aim,
                        rays,
                    }),
                );
            }
            FireOutcome::Refused(reason) => {
                let line = match reason {
                    FireRefusal::Cooldown { ready_at } => {
                        format!("fire refused — the weapon is cooling (ready at tick {ready_at})")
                    }
                    other => format!("fire refused — {other:?}"),
                };
                encounter.last_fire_line = Some(line.clone());
                Self::log(encounter, tick, line);
            }
        }
    }

    /// The parts-scoped event log push.
    fn log(encounter: &mut EncounterState, tick: u64, line: impl Into<String>) {
        encounter.events.push((tick, line.into()));
    }

    pub fn log_public(&mut self, tick: u64, line: impl Into<String>) {
        self.encounter.events.push((tick, line.into()));
    }

    /// The player's eye (the aim origin — the same derivation the
    /// playable demos use).
    pub fn eye(&self) -> [f32; 3] {
        let state = self.driver.state();
        [
            state.position[0],
            state.position[1] + self.driver.config().eye_height,
            state.position[2],
        ]
    }

    /// The camera pose with obstruction handling (both paths use
    /// this).
    pub fn camera_pose(&self) -> engine_core::CameraPose {
        camera_pose_obstructed(
            self.driver.state(),
            self.driver.config(),
            self.driver.world(),
        )
    }
}

/// Rebuilds the live hittable roster. Dormant bodies physically absorb shots.
pub(crate) fn encounter_targets(out: &mut Vec<Combatant>, world: &AdversaryWorld) {
    out.clear();
    for index in 0..world.count() {
        let adversary = world.adversary(index);
        if adversary.state == AdversaryState::Defeated {
            continue;
        }
        let (min, max) = adversary_body(adversary.position);
        out.push(Combatant {
            id: ADVERSARY_ID_BASE + index as u32,
            hit_box: StaticCollider { min, max },
        });
    }
}

/// The adversary state's HUD/evidence word.
fn adversary_state_word(state: AdversaryState) -> &'static str {
    match state {
        AdversaryState::Dormant => "DORMANT",
        AdversaryState::Patrol => "PATROL",
        AdversaryState::Aggro => "AGGRO",
        AdversaryState::Attack => "ATTACK",
        AdversaryState::Recover => "RECOVER",
        AdversaryState::Defeated => "DEFEATED",
    }
}

/// The weapon's HUD/feedback word (the selection discriminant's
/// label).
pub fn weapon_word(weapon: WeaponId) -> &'static str {
    match weapon {
        WeaponId::Precision => "precision",
        WeaponId::Spread => "spread",
    }
}

/// The exact-slab ray–box entry distance (the accepted demo's
/// out-of-range feedback distinction — the same arithmetic the
/// combat resolver applies per box).
fn ray_box_entry(
    origin: &[f32; 3],
    direction: &[f32; 3],
    min: &[f32; 3],
    max: &[f32; 3],
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

/// Runs a scripted phase until the condition holds (panicking
/// loudly on a stall — a deterministic script that stalls is a
/// layout or mechanic regression, never a skip).
fn run_until(
    demo: &mut EncounterDemo,
    intent: MoveIntent,
    fire: bool,
    condition: impl Fn(&EncounterDemo) -> bool,
    label: &str,
) {
    for _ in 0..3_000 {
        if condition(demo) {
            return;
        }
        demo.tick(intent, fire);
    }
    panic!("the encounter script stalled at '{label}'");
}

/// The WINNABLE scripted scenario: settle → cross the activation
/// line → hold and fight on cooldown whenever the adversary is
/// dead-ahead (the zero-angle discipline — the scripted player only
/// shoots when the target crosses the aim line) until the encounter
/// resolves.
pub fn run_encounter_winnable(demo: &mut EncounterDemo) {
    // 1. Settle to the courtyard floor.
    run_until(
        demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle",
    );
    // 2. Walk forward to the activation line.
    run_until(
        demo,
        forward(),
        false,
        |d| d.encounter.phase == EncounterPhase::Active,
        "activate the encounter",
    );
    // 3. Hold position and wait for the adversary's own wake (the
    //    perception — AGGRO or beyond on its wake-up cadence).
    run_until(
        demo,
        idle(),
        false,
        |d| {
            (0..d.spine.world().count()).any(|index| {
                matches!(
                    d.spine.world().adversary(index).state,
                    AdversaryState::Aggro
                        | AdversaryState::Attack
                        | AdversaryState::Recover
                        | AdversaryState::Defeated
                )
            })
        },
        "wait for the adversary's aggro",
    );
    // 4. Fire on cooldown whenever the adversary is dead-ahead,
    //    until the encounter resolves.
    for _ in 0..3_000 {
        if demo.encounter.phase != EncounterPhase::Active {
            break;
        }
        let on_line = (0..demo.spine.world().count())
            .any(|index| demo.spine.world().adversary(index).position[0].abs() <= 0.4);
        let fire = demo.weapon_ready() && on_line;
        demo.tick(idle(), fire);
    }
    assert!(
        demo.encounter.phase != EncounterPhase::Active,
        "the encounter script stalled at 'win the encounter'"
    );
}

/// The WINNABLE headless scenario (`--encounter-headless`): the
/// script over a fresh demo, returning the printable evidence
/// record, byte-identical across identical invocations. Runs the
/// PINNED single-adversary configuration explicitly (the traces and
/// pins are guards per the H-review merge conditions — the wave
/// composition is a separate policy).
pub fn run_encounter_winnable_headless() -> String {
    let mut demo = EncounterDemo::new_with_config(encounter_single_config());
    run_encounter_winnable(&mut demo);
    let done_tick = terminal_tick(&demo);
    for _ in 0..5 {
        demo.tick(idle(), false);
    }
    encounter_evidence(&demo, "WINNABLE", done_tick)
}

/// The FAILURE headless scenario: settle → walk through activation,
/// well inside the adversary's perception ground → stand still,
/// never fire → the machine aggros, pursues, and the cooldown-gated
/// attacks defeat the player. The reproducible failure. The walk's
/// stop condition is PLAYER-ONLY (a fixed z-line, never the
/// adversary's position) so the player trace stays cross-platform
/// bit-stable. Runs the PINNED single-adversary configuration.
pub fn run_encounter_failure_headless() -> String {
    let mut demo = EncounterDemo::new_with_config(encounter_single_config());
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle",
    );
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.driver.state().position[2] <= 1.0,
        "enter the adversary's ground",
    );
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.encounter.phase != EncounterPhase::Active,
        "lose the encounter",
    );
    let done_tick = terminal_tick(&demo);
    for _ in 0..5 {
        demo.tick(idle(), false);
    }
    encounter_evidence(&demo, "FAILURE", done_tick)
}

/// The WAVE winnable scenario (the full capability-D composition,
/// `--encounter-wave-headless`): the 4-adversary 2-wave encounter —
/// cross line A (wave 1 wakes), fight with BOTH weapons (the spread
/// volley when adversaries share the line, the precision pick
/// otherwise), cross the deeper line B (wave 2 stirs mid-fight),
/// finish all four → WON. NEW pins (the wave policy is a new
/// scenario, never a repin of the single-adversary guards).
pub fn run_encounter_wave_winnable_headless() -> String {
    let mut demo = EncounterDemo::new();
    // 1. Settle.
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle (wave)",
    );
    // 2. Cross line A.
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.encounter.phase == EncounterPhase::Active,
        "activate wave 1",
    );
    // 3. The wave-1 fight: hold and fire on cooldown — the SPREAD
    //    volley whenever TWO OR MORE live adversaries are close
    //    (the multi-target discriminator), otherwise whichever
    //    weapon is ready (the alternating-cooldowns policy — the
    //    honest both-weapons fight; the zero-angle discipline: fire
    //    only when a target is dead-ahead). Exits when wave 1 is
    //    cleared (or the encounter resolved — the failure guard).
    for _ in 0..6_000 {
        if demo.encounter.phase != EncounterPhase::Active || wave1_cleared(&demo) {
            break;
        }
        let spread_ready = demo.arsenal.ready_at(WeaponId::Spread) <= demo.tick_index();
        let live_close = (0..demo.config().wave1_count)
            .filter(|&index| {
                let adversary = demo.spine.world().adversary(index);
                adversary.state != AdversaryState::Defeated
                    && adversary.position[0].abs() <= 1.2
                    && (adversary.position[2] - demo.driver.state().position[2]).abs() <= 9.0
            })
            .count();
        let on_line = (0..demo.spine.world().count())
            .any(|index| demo.spine.world().adversary(index).position[0].abs() <= 0.4);
        if spread_ready && live_close >= 2 {
            demo.apply_selection(WeaponId::Spread);
            demo.tick(idle(), on_line);
        } else if demo.weapon_ready() {
            demo.apply_selection(WeaponId::Precision);
            demo.tick(idle(), on_line);
        } else if spread_ready {
            demo.apply_selection(WeaponId::Spread);
            demo.tick(idle(), on_line);
        } else {
            demo.apply_selection(WeaponId::Precision);
            demo.tick(idle(), false);
        }
    }
    // 4. Cross the deeper line B (wave 2 stirs).
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.encounter.wave2_activated,
        "activate wave 2",
    );
    // 5. The full fight to the terminal (the same alternating-
    //    cooldowns policy over ALL adversaries).
    for _ in 0..6_000 {
        if demo.encounter.phase != EncounterPhase::Active {
            break;
        }
        let spread_ready = demo.arsenal.ready_at(WeaponId::Spread) <= demo.tick_index();
        let live_close = (0..demo.spine.world().count())
            .filter(|&index| {
                let adversary = demo.spine.world().adversary(index);
                adversary.state != AdversaryState::Defeated
                    && adversary.position[0].abs() <= 1.2
                    && (adversary.position[2] - demo.driver.state().position[2]).abs() <= 9.0
            })
            .count();
        let on_line = (0..demo.spine.world().count())
            .any(|index| demo.spine.world().adversary(index).position[0].abs() <= 0.4);
        if spread_ready && live_close >= 2 {
            demo.apply_selection(WeaponId::Spread);
            demo.tick(idle(), on_line);
        } else if demo.weapon_ready() {
            demo.apply_selection(WeaponId::Precision);
            demo.tick(idle(), on_line);
        } else if spread_ready {
            demo.apply_selection(WeaponId::Spread);
            demo.tick(idle(), on_line);
        } else {
            demo.apply_selection(WeaponId::Precision);
            demo.tick(idle(), false);
        }
    }
    let done_tick = terminal_tick(&demo);
    for _ in 0..5 {
        demo.tick(idle(), false);
    }
    encounter_evidence(&demo, "WAVE-WINNABLE", done_tick)
}

/// Whether wave 1 is fully defeated (the step-3 exit condition —
/// both wave-1 adversaries DEFEATED).
fn wave1_cleared(demo: &EncounterDemo) -> bool {
    (0..demo.config().wave1_count)
        .all(|index| demo.spine.world().adversary(index).state == AdversaryState::Defeated)
}

/// The WAVE failure scenario (`--encounter-wave-failure-headless`):
/// walk through BOTH lines (all four adversaries wake) and stand
/// still — the multi-adversary pressure defeats the player. NEW
/// pins.
pub fn run_encounter_wave_failure_headless() -> String {
    let mut demo = EncounterDemo::new();
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.driver.state().on_ground,
        "settle (wave failure)",
    );
    run_until(
        &mut demo,
        forward(),
        false,
        |d| d.encounter.wave2_activated,
        "wake all four",
    );
    run_until(
        &mut demo,
        idle(),
        false,
        |d| d.encounter.phase != EncounterPhase::Active,
        "lose the wave encounter",
    );
    let done_tick = terminal_tick(&demo);
    for _ in 0..5 {
        demo.tick(idle(), false);
    }
    encounter_evidence(&demo, "WAVE-FAILURE", done_tick)
}

fn terminal_tick(demo: &EncounterDemo) -> u64 {
    match demo.encounter.phase {
        EncounterPhase::Won { tick } | EncounterPhase::Lost { tick } => tick,
        _ => demo.tick_index(),
    }
}

/// The shared evidence record (the event log, the healths, the
/// outcome, the movement accounting, the terminal record, and the
/// trace fingerprints — the player trace under the accepted
/// player-trace domain, the ENCOUNTER state under the NEW
/// `engine-slice/encounter-world/1` domain).
fn encounter_evidence(demo: &EncounterDemo, scenario: &str, done_tick: u64) -> String {
    let mut out = String::new();
    out.push_str("Sektor7 C4-D - integrated encounter headless run (the courtyard)\n");
    out.push_str(&format!("scenario: {scenario}\n"));
    out.push_str("events:\n");
    for (tick, line) in &demo.encounter.events {
        out.push_str(&format!("  tick {tick}: {line}\n"));
    }
    let state = demo.driver.state();
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
    for index in 0..demo.spine.world().count() {
        let adversary = demo.spine.world().adversary(index);
        out.push_str(&format!(
            "adversary {index}: {} · hp {:.0}/{:.0} · pos ({:.3}, {:.3})\n",
            adversary_state_word(adversary.state),
            adversary.health.current,
            adversary.health.maximum,
            adversary.position[0],
            adversary.position[2],
        ));
    }
    let counters = demo.encounter.counters;
    out.push_str(&format!(
        "outcome: {} at tick {done_tick} (fires {} · hits {} · blocked {} · out of range \
         {} · attacks taken {})\n",
        match demo.encounter.phase {
            EncounterPhase::Won { .. } => "WON",
            EncounterPhase::Lost { .. } => "LOST",
            _ => "UNRESOLVED",
        },
        counters.fires,
        counters.hits,
        counters.blocked,
        counters.out_of_range,
        counters.attacks_taken,
    ));
    if let Some(record) = demo.encounter.terminal {
        out.push_str(&format!(
            "terminal record: {:?} at tick {} (immutable — the counters snapshot)\n",
            record.outcome, record.tick
        ));
    }
    out.push_str(&format!(
        "movement ticks executed={} outstanding={} due={} refused={}\n",
        demo.driver.executed_ticks_total(),
        demo.driver.outstanding_ticks(),
        demo.driver.ticks_due_total(),
        demo.driver.refused_steps(),
    ));
    out.push_str(&format!(
        "player-trace={}\n",
        demo.driver.trace_fingerprint_hex()
    ));
    out.push_str(&format!(
        "encounter-fingerprint={}\n",
        encounter_fingerprint_hex(demo)
    ));
    out
}

/// The encounter-state fingerprint — the NEW domain
/// `engine-slice/encounter-world/1` (namespaced, versioned; never a
/// repin of any existing domain). Fingerprints the full
/// encounter-relevant state: the phase, the round clock, the
/// counters, the healths, every adversary's state/position, and the
/// complete event log — the cadence-equivalence regression compares
/// tick-path and frame-path runs through THIS digest.
pub fn encounter_fingerprint_hex(demo: &EncounterDemo) -> String {
    let mut fp = Fingerprinter::new();
    fp.write_str("engine-slice/encounter-world/1");
    fp.write_u8(match demo.encounter.phase {
        EncounterPhase::Dormant => 0,
        EncounterPhase::Active => 1,
        EncounterPhase::Won { .. } => 2,
        EncounterPhase::Lost { .. } => 3,
    });
    fp.write_u64(demo.ticks);
    fp.write_u32(demo.encounter.rounds);
    let counters = demo.encounter.counters;
    fp.write_u32(counters.fires);
    fp.write_u32(counters.hits);
    fp.write_u32(counters.misses);
    fp.write_u32(counters.blocked);
    fp.write_u32(counters.out_of_range);
    fp.write_u32(counters.attacks_taken);
    fp.write_u32(demo.encounter.player.current.to_bits());
    fp.write_u32(demo.encounter.player.maximum.to_bits());
    fp.write_usize(demo.spine.world().count());
    for index in 0..demo.spine.world().count() {
        let adversary = demo.spine.world().adversary(index);
        fp.write_u8(match adversary.state {
            AdversaryState::Dormant => 0,
            AdversaryState::Patrol => 1,
            AdversaryState::Aggro => 2,
            AdversaryState::Attack => 3,
            AdversaryState::Recover => 4,
            AdversaryState::Defeated => 5,
        });
        fp.write_u32(adversary.health.current.to_bits());
        for value in adversary.position {
            fp.write_u32(value.to_bits());
        }
    }
    fp.write_usize(demo.encounter.events.len());
    for (tick, line) in &demo.encounter.events {
        fp.write_u64(*tick);
        fp.write_str(line);
    }
    fp.finish_hex()
}

// ==================================================== input mapping

/// One frame's mapped encounter input: the playable mapping
/// (movement, look, camera toggle — the SAME mapping, so the
/// corrected mouse controls hold) plus the fire edge.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EncounterMappedInput {
    pub intent: MoveIntent,
    /// A fresh left-mouse press this frame (edge-triggered).
    pub fire_pressed: bool,
    pub toggle_camera: bool,
    /// A fresh R press this frame (the full-round reset edge).
    pub reset_pressed: bool,
    /// A fresh weapon-selection edge this frame (1 = precision,
    /// 2 = spread — the direct edges).
    pub select: Option<WeaponId>,
    /// A fresh Q press this frame (the cycle edge — the loop flips
    /// to the other weapon; the current selection lives in the
    /// Arsenal, not the mapping layer).
    pub cycle_pressed: bool,
}

/// Maps a shell snapshot onto the encounter intents: the playable
/// mapping verbatim plus left-mouse fire, R reset, and the weapon
/// selection edges.
pub fn map_encounter_input(snapshot: &InputSnapshot, sprint_held: bool) -> EncounterMappedInput {
    let playable = map_playable_input(snapshot, sprint_held);
    let neutral = snapshot.focus_lost || snapshot.minimized;
    let pressed = |key: Keycode| !neutral && snapshot.keys_pressed.contains(&key);
    let select = if pressed(Keycode::_1) {
        Some(WeaponId::Precision)
    } else if pressed(Keycode::_2) {
        Some(WeaponId::Spread)
    } else {
        None
    };
    EncounterMappedInput {
        intent: playable.intent,
        fire_pressed: !neutral && snapshot.mouse_pressed.contains(&MouseButton::Left),
        toggle_camera: playable.toggle_camera,
        reset_pressed: pressed(Keycode::R),
        select,
        cycle_pressed: pressed(Keycode::Q),
    }
}

// ==================================================== the interactive demo

/// Demo options; CLI parsing lives in the binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncounterOptions {
    pub fps: u32,
    pub present: PresentModePreference,
    pub frames: Option<u64>,
    pub overlay: bool,
    pub validation: ValidationPreference,
}

impl Default for EncounterOptions {
    fn default() -> Self {
        EncounterOptions {
            fps: 60,
            present: PresentModePreference::Auto,
            frames: None,
            overlay: true,
            validation: ValidationPreference::Auto,
        }
    }
}

/// Eight corners are indexed by the high X, Y and Z bits respectively.
fn box_corners(min: [f32; 3], max: [f32; 3]) -> [[f32; 3]; 8] {
    std::array::from_fn(|i| {
        std::array::from_fn(|axis| {
            if i & (1 << axis) == 0 {
                min[axis]
            } else {
                max[axis]
            }
        })
    })
}

/// Clockwise faces as seen from outside, matching the Vulkan scene pipeline.
/// Both oriented cover and axis-aligned bodies use the same winding.
fn append_box(vertices: &mut Vec<SceneVertex>, corners: [[f32; 3]; 8], color: [f32; 3]) {
    for ([a, b, c, d], light) in [
        ([6, 2, 3, 7], 1.0),
        ([0, 4, 5, 1], 0.45),
        ([0, 1, 3, 2], 0.80),
        ([5, 4, 6, 7], 0.80),
        ([4, 0, 2, 6], 0.65),
        ([1, 5, 7, 3], 0.65),
    ] {
        let col = color.map(|v| v * light);
        vertices.extend([a, b, c, a, c, d].map(|i| SceneVertex {
            pos: corners[i],
            col,
        }));
    }
}

/// The visible world: the courtyard boxes, the ROTATED cover pieces
/// (the visual geometry built from the collider itself — parameter
/// for parameter, collide-what-you-see with NO axis-aligned
/// stand-in), the adversaries' bodies (colored by state — the
/// awareness feedback), and the player proxy in third person.
pub fn encounter_render_boxes(demo: &EncounterDemo) -> Vec<([f32; 3], [f32; 3], [f32; 3])> {
    let mut boxes = Vec::new();
    visit_encounter_boxes(demo, true, |min, max, color| boxes.push((min, max, color)));
    boxes
}

fn visit_encounter_boxes(
    demo: &EncounterDemo,
    box_proxy: bool,
    mut visit: impl FnMut([f32; 3], [f32; 3], [f32; 3]),
) {
    for class_box in ENCOUNTER_BOXES {
        let color = match class_box.role {
            EncounterRole::Perimeter => [0.35, 0.35, 0.40],
            EncounterRole::CentralBlock => [0.42, 0.38, 0.32],
            EncounterRole::WingWall => [0.30, 0.32, 0.38],
        };
        visit(class_box.min, class_box.max, color);
    }
    for index in 0..demo.spine.world().count() {
        let adversary = demo.spine.world().adversary(index);
        let color = match adversary.state {
            AdversaryState::Dormant => [0.25, 0.25, 0.30],
            AdversaryState::Patrol => [0.45, 0.35, 0.55],
            AdversaryState::Aggro | AdversaryState::Attack => [0.80, 0.25, 0.20],
            AdversaryState::Recover => [0.85, 0.55, 0.20],
            AdversaryState::Defeated => [0.12, 0.12, 0.14],
        };
        let (min, max) = adversary_body(adversary.position);
        visit(min, max, color);
    }
    if box_proxy && demo.driver.state().camera_mode == CameraMode::ThirdPerson {
        let state = demo.driver.state();
        let (min, max) = engine_core::player_box(state.position, demo.driver.config());
        visit(min, max, [0.20, 0.80, 0.35]);
    }
}

/// The rotated cover pieces' visual colors (the slab — cool stone;
/// the pillar — warm stone), matching the axis-aligned structure's
/// palette discipline.
pub fn cover_render_pieces() -> [(RotatedCollider, [f32; 3]); 2] {
    [
        (cover_slab(), [0.38, 0.42, 0.48]),
        (cover_pillar(), [0.48, 0.42, 0.34]),
    ]
}

/// Assembles one encounter frame's scene geometry into `out`
/// (cleared first) — the shared presentation seam (the playable
/// demo's discipline).
pub fn build_encounter_vertices_into(out: &mut Vec<SceneVertex>, demo: &EncounterDemo) {
    build_arena(out, demo, true);
}

pub(crate) fn build_arena(out: &mut Vec<SceneVertex>, demo: &EncounterDemo, box_proxy: bool) {
    out.clear();
    visit_encounter_boxes(demo, box_proxy, |min, max, color| {
        append_box(out, box_corners(min, max), color)
    });
    // The rotated cover pieces: the TRUE rotated visual geometry,
    // built from the collider's own frame (never an axis-aligned
    // stand-in — the GO §4 hard rule).
    for (collider, color) in cover_render_pieces() {
        push_rotated_box(out, &collider, color);
    }
}

/// Append the exact collision shape, including its rotation, to the world batch.
pub fn push_rotated_box(
    vertices: &mut Vec<SceneVertex>,
    collider: &RotatedCollider,
    color: [f32; 3],
) {
    let corners = box_corners(collider.half.map(|v| -v), collider.half);
    append_box(vertices, corners.map(|p| collider.to_world(p)), color);
}

/// The interactive integrated encounter (`--encounter`): the
/// playable skeleton over the courtyard — fixed-step movement on
/// the full raw interval, the obstructed third-person camera,
/// left-mouse fire, C camera toggle, R full-round reset, visible
/// feedback everywhere, SEAT C's audio cues wired at the event
/// observation points (the encounter remains fully playable silent
/// — gameplay never gates on audio), and the HUD data consumed by
/// the basic overlay (SEAT F renders the refined presentation
/// against the same `EncounterHud` read).
pub fn run_encounter_demo(options: EncounterOptions) -> Result<(), String> {
    let shell_config = ShellConfig {
        title: "Cycle 5 — Courtyard encounter".to_string(),
        width: 1280,
        height: 720,
        target_fps: 60,
        driver: DriverPreference::Real,
    };
    let mut app = ShellApp::new(shell_config)?;
    app.set_relative_mouse(true);

    // C's capability (AUDIO-CUE-API contracts 1/3): construct ONCE
    // here — never in the headless paths; a missing device degrades
    // to the observable Silent status, never an error.
    let mut audio = Audio::initialize(app.platform());
    let mut frame = EncounterFrame::default();

    // The scene-separation idiom: a game scene — the default omitted
    // renderer with the demo's OWN geometry.
    let render_config = RenderConfig {
        present_mode: options.present,
        validation: options.validation,
        replace_pipelines: false,
        demo_scene: DemoScenePreference::Omitted,
    };
    let mut renderer =
        Renderer::new(app.window(), render_config).map_err(|error| error.to_string())?;
    renderer.set_overlay_enabled(options.overlay);

    let mut demo = EncounterDemo::new();
    demo.driver.set_trace_recording(false);

    {
        let stats = renderer.stats();
        println!("Cycle 5 — courtyard encounter");
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
            "  audio: {}",
            match audio.status() {
                AudioStatus::Active { driver, device } => format!("{driver} · {device}"),
                AudioStatus::Silent { reason } => format!("silent — {reason}"),
            }
        );
        println!(
            "  scene: the courtyard renders its own geometry — the Stage-2 demo \
             scene is omitted"
        );
        println!(
            "  controls: WASD move · mouse look · LMB fire · 1 precision / 2 spread / Q cycle · C camera · R reset · \
             O overlay · Esc/close quit"
        );
        println!(
            "  the loop: cross the activation line → the adversaries wake → fight \
             (cover matters) → win, or fall and reset"
        );
    }

    let mut cpu_work_ms: Vec<f64> = Vec::new();
    let mut presented: u64 = 0;
    let start = std::time::Instant::now();
    let frame_interval = if options.fps == 0 {
        None
    } else {
        Some(Duration::from_secs_f64(
            1.0 / f64::from(options.fps.clamp(1, 1000)),
        ))
    };
    let mut timing = FrameTiming::new();
    let mut diagnostic = String::new();
    let mut diagnostic_at = None;
    loop {
        let frame_start = std::time::Instant::now();
        let snapshot = app.frame();
        if snapshot.quit_requested || snapshot.keys_pressed.contains(&Keycode::Escape) {
            break;
        }
        if snapshot.keys_pressed.contains(&Keycode::O) {
            renderer.set_overlay_enabled(!renderer.overlay_enabled());
        }
        let split = timing.begin_frame(frame_start, !snapshot.minimized);

        let mapped = map_encounter_input(&snapshot, app.is_held(Keycode::LShift));
        if mapped.toggle_camera {
            demo.driver.state_mut().toggle_camera_mode();
        }
        if mapped.reset_pressed {
            demo.reset();
            // The fresh driver's trace recording defaults ON; the
            // interactive path never records (the cadence-dependent
            // live input is not a determinism surface).
            demo.driver.set_trace_recording(false);
            diagnostic_at = None;
        }
        // The weapon-selection edges (policy state — the honest
        // lifecycle refusals live in `apply_selection`): the direct
        // 1/2 edges, then the Q cycle (flip to the other weapon).
        if let Some(weapon) = mapped.select {
            demo.apply_selection(weapon);
        } else if mapped.cycle_pressed {
            let other = match demo.arsenal.selected() {
                WeaponId::Precision => WeaponId::Spread,
                WeaponId::Spread => WeaponId::Precision,
            };
            demo.apply_selection(other);
        }

        // The C3-R1 interleaved advance: movement + the encounter
        // transition together at each executed logical tick.
        demo.advance_frame(split.sim_advance, mapped.intent, mapped.fire_pressed);

        frame.observe(&mut demo, split.sim_advance, |cue| {
            if !snapshot.minimized {
                audio.play(cue);
            }
        });
        if snapshot.minimized {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        let size = app.window().size_in_pixels();
        frame.prepare(&demo, size);
        let debug = renderer.overlay_enabled().then(|| {
            use std::fmt::Write;
            // Sampling text at 4 Hz keeps formatting and percentile sorting off
            // the per-frame hot path. Gameplay readouts update independently.
            if diagnostic_at
                .is_none_or(|t| frame_start.duration_since(t) >= Duration::from_millis(250))
            {
                diagnostic_at = Some(frame_start);
                diagnostic.clear();
                let stats = renderer.stats();
                let _ = writeln!(
                    diagnostic,
                    "CPU {:.2}  GPU {:.2} ms p95",
                    math::percentile(&cpu_work_ms, 0.95),
                    stats.gpu_ms_p95()
                );
                let _ = writeln!(
                    diagnostic,
                    "Round {}  Tick {}",
                    demo.encounter.rounds + 1,
                    demo.tick_index()
                );
                for i in 0..demo.spine.world().count() {
                    let adversary = demo.spine.world().adversary(i);
                    let _ = writeln!(
                        diagnostic,
                        "{} {:8} {:3.0}/{:.0}",
                        ADVERSARY_ID_BASE + i as u32,
                        adversary_state_word(adversary.state),
                        adversary.health.current,
                        adversary.health.maximum
                    );
                }
            }
            diagnostic.as_str()
        });
        let frame_result = frame
            .draw(&mut renderer, debug, size)
            .map_err(|error| error.to_string())?;

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
    let hud = demo.hud();
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
        "outcome: {} · rounds {} · fires {} · hits {} · blocked {} · out of range {} · \
         attacks taken {}",
        match hud.phase {
            EncounterPhase::Won { .. } => "WON",
            EncounterPhase::Lost { .. } => "LOST",
            EncounterPhase::Dormant => "not activated",
            EncounterPhase::Active => "unresolved",
        },
        hud.rounds + 1,
        hud.counters.fires,
        hud.counters.hits,
        hud.counters.blocked,
        hud.counters.out_of_range,
        hud.counters.attacks_taken,
    );
    println!(
        "audio: {}",
        match audio.status() {
            AudioStatus::Active { driver, device } => format!("active ({driver} · {device})"),
            AudioStatus::Silent { reason } => format!("silent — {reason}"),
        }
    );
    println!(
        "controller state is transient (not persisted); offscreen CI runs are a \
         correctness smoke, not a substitute for a human-played run"
    );
    println!(
        "presentation events {:?}; effects {:?}",
        demo.encounter.presentation.stats(),
        frame.visuals.stats()
    );
    let stats = renderer.stats();
    println!(
        "GPU p50/p95/p99 {:.3}/{:.3}/{:.3} ms ({} timestamps); peak scene/HUD vertices {}/{}; omitted {}/{}",
        stats.gpu_ms_p50(),
        stats.gpu_ms_p95(),
        stats.gpu_ms_p99(),
        stats.gpu_sample_count(),
        stats.region_supplied_peak_vertices,
        stats.overlay_supplied_peak_vertices,
        stats.region_truncated_vertices,
        stats.overlay_truncated_vertices
    );
    if stats.validation_active && stats.validation_errors > 0 {
        return Err(format!(
            "validation layer reported {} error(s) while active — see the messages above",
            stats.validation_errors
        ));
    }
    Ok(())
}
