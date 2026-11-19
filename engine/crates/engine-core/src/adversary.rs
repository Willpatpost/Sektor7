//! The Cycle-3 P1b adversary behavior module (SEAT B, 2026-11-17):
//! at least one adversary with meaningful, threatening behavior on the
//! deterministic spine — a six-state behavior machine (DORMANT → PATROL →
//! AGGRO → ATTACK → RECOVER; DEFEATED terminal) where **cadence IS the
//! activity level**: one spine routine chain per adversary, an idle
//! adversary holds exactly one queued wake-up and costs zero per-frame
//! work (the charter's §6 proportionality standard, by construction).
//!
//! Design note: `cycle3/interfaces/ADVERSARY-DESIGN.md` (the persisted
//! contract with seats A/F). **Every numeric value here is a PLACEHOLDER
//! parameter (`AdversaryConfig`) — no balance or difficulty approval is
//! claimed.** The ATTACK/DEFEATED seams are WIRED through seat A's
//! combat contract v1 (`cycle3/interfaces/COMBAT-INTERFACE.md`, merged
//! at the `s7-cycle3-a-combat` skeleton commit): each ATTACK wake
//! resolves ONE `AttackRequest` through `combat::resolve_attack` (from
//! the adversary's eye toward the player body's center, B's placeholder
//! profile) and SURFACES the `HitResolution` per-adversary — F applies
//! player damage (the Health-ownership convention: B never mutates
//! player Health); adversary `Health` lives here as `combat::Health`,
//! player-fired damage routes through the public `apply_player_hit`,
//! and DEFEATED enters ONLY from `DamageOutcome::Defeated` — the
//! engine's single defeat declaration. No spine semantic changes; no
//! persistence-schema changes (encounter state is transient, honestly
//! labeled); the 200/60 budget is owner-gated and untouched.
//!
//! Determinism: transitions are pure functions of logical state, the
//! driver-sampled player position, and seeded jitter (a local SplitMix64
//! finalizer — engine-core cannot reach engine-slice's `npc::mix64`);
//! the world fingerprints under its own namespaced domain; wall-clock
//! time never enters logical state.
//!
//! Cycle 4 capability A (2026-11-18, SEAT A): the H31-1 scheduler
//! reconciliation and the multi-adversary contract —
//! `cycle4/interfaces/COMBATANTS-INTERFACE.md` (v1). The installer
//! schedules only ELIGIBLE adversaries (active state, no chain
//! installed — idempotent); the state/payload guard matrix makes
//! unauthorized activation STRUCTURALLY impossible (only the
//! [`ADVERSARY_ACTIVATION_CURSOR`] payload wakes a DORMANT adversary;
//! duplicates and stray routine schedules are inert); dormancy is a
//! pre-scheduling composition state (`set_dormant` refuses a possibly-
//! live chain); `reset` restores every combatant to construction
//! state; `combatants`/`defeated_count`/`all_defeated` are the
//! multi-candidate targeting and victory-condition hooks D and B
//! consume. The C3.1 scenario traces are preserved byte-identical
//! (no hashed field changed; the activation payload marker changes
//! no state sequence in any legal flow).

use crate::clock::Tick;
use crate::combat::{
    AttackProfile, AttackRefusal, AttackRequest, Combatant, CombatantId, DamageOutcome,
    HealthError, HitResolution, MissKind, resolve_attack,
};
use crate::event::{Event, EventClass, EventSpec, Field, Schedule};
use crate::fingerprint::Fingerprinter;
use crate::player::{CollisionWorld, PlayerConfig, StaticCollider, player_box};
use crate::spine::{ExecCtx, Spine, World};

/// The adversary chain codec's type tag — namespaced away from the
/// game workload's `ROUTINE_MAGIC`, the burst suite's `BURST_MAGIC`,
/// and the sustained harness's tags. PENDING seat A's payload/event
/// encoding convention (contract item 5).
pub const ADVERSARY_MAGIC: u64 = 0xB00B;

/// Encodes one adversary wake-up payload: `magic(16) | index(16) |
/// cursor(32)` — the npc.rs identity+cursor discipline.
pub fn adversary_payload(index: usize, cursor: u64) -> u64 {
    (ADVERSARY_MAGIC << 48) | ((index as u64) << 32) | (cursor & 0xFFFF_FFFF)
}

/// Decodes an adversary payload: `Some((index, cursor))` only when the
/// tag matches. Foreign payloads stay foreign (the world ignores them).
pub fn adversary_parts(payload: u64) -> Option<(usize, u64)> {
    if payload >> 48 != ADVERSARY_MAGIC {
        return None;
    }
    Some((((payload >> 32) & 0xFFFF) as usize, payload & 0xFFFF_FFFF))
}

/// The reserved cursor value that marks an ACTIVATION wake-up — the
/// only payload kind that may transition a DORMANT adversary (the
/// Cycle-4 combatants contract §3). Routine chains use cursors
/// 0, 1, 2, … and never reach this value (an adversary would need
/// 2^32−1 executed wake-ups first). Against every non-dormant state
/// the activation payload is INERT (a duplicate activation cannot
/// fork a chain); against DORMANT a routine payload is INERT (the
/// H31-1 reconciliation: a stray schedule cannot activate a dormant
/// adversary, whatever composed it).
pub const ADVERSARY_ACTIVATION_CURSOR: u64 = 0xFFFF_FFFF;

/// The adversary body's half-extents (m) — the placeholder body model
/// the Cycle-3 demo used (`[0.45, 0.9, 0.45]`: a 0.9 × 1.8 × 0.9
/// body on the ground plane), mirroring `player_box`'s role for the
/// player. Per-adversary body variation is a later cycle.
pub const ADVERSARY_BODY_HALF: [f32; 3] = [0.45, 0.9, 0.45];

/// The behavior states. DORMANT adversaries hold no chain at all
/// (activation is F's encounter scripting — `activation_spec`); DEFEATED
/// is terminal (the chain stops; F reads it as encounter completion).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdversaryState {
    Dormant,
    Patrol,
    Aggro,
    Attack,
    Recover,
    Defeated,
}

impl AdversaryState {
    fn code(self) -> u8 {
        match self {
            AdversaryState::Dormant => 0,
            AdversaryState::Patrol => 1,
            AdversaryState::Aggro => 2,
            AdversaryState::Attack => 3,
            AdversaryState::Recover => 4,
            AdversaryState::Defeated => 5,
        }
    }

    /// The wake-up cadence in ticks — the activity level itself.
    /// Zero cadence ends the chain (DORMANT pre-activation, DEFEATED
    /// terminal).
    fn cadence(self, config: &AdversaryConfig) -> u64 {
        match self {
            AdversaryState::Dormant | AdversaryState::Defeated => 0,
            AdversaryState::Patrol => config.patrol_cadence,
            AdversaryState::Aggro | AdversaryState::Attack => config.aggro_cadence,
            AdversaryState::Recover => config.recover_cadence,
        }
    }

    /// The lane class: idle-ish states ride the far lane, active
    /// threat states the near lane (the §5 near/far discipline).
    fn lane(self) -> Field {
        match self {
            AdversaryState::Aggro | AdversaryState::Attack => Field::Near,
            _ => Field::Far,
        }
    }
}

/// All placeholder values — parameters, never policy (the owner
/// ratifies balance; A's contract governs the combat numbers).
#[derive(Debug, Clone, PartialEq)]
pub struct AdversaryConfig {
    /// Deterministic workload seed (spawn spread, waypoint circuits,
    /// perception jitter).
    pub seed: u64,
    /// How many adversaries exist (spawned on seeded circuits).
    pub count: usize,
    /// Logical-tick duration in seconds (the fixed-step contract).
    pub tick_seconds: f32,
    /// Perception: aggro distance (placeholder 8.0 m).
    pub aggro_radius: f32,
    /// Perception: de-aggro distance with hysteresis (placeholder 10.0 m).
    pub deaggro_radius: f32,
    /// Perception: LOS-loss tolerance before de-aggro (placeholder 24
    /// wake-ups).
    pub los_loss_ticks: u32,
    /// PATROL wake-up cadence (placeholder 12 ticks).
    pub patrol_cadence: u64,
    /// AGGRO/ATTACK wake-up cadence (placeholder 2 ticks).
    pub aggro_cadence: u64,
    /// RECOVER wake-up cadence (placeholder 6 ticks).
    pub recover_cadence: u64,
    /// ATTACK cooldown in LOGICAL TICKS (placeholder 18) — the
    /// canonical unit (C3-R2): the attack-readiness deadline is set
    /// at the ATTACK emit as `emit_tick + cooldown` (saturating) and
    /// compared against the execution tick at the AGGRO gate; it is
    /// NEVER decremented per wake (the old per-wake decrement made
    /// the effective duration a function of the wake frequency,
    /// silently changing units between states).
    pub attack_cooldown: u64,
    /// ATTACK range (placeholder 2.0 m — must be ≤ aggro_radius).
    pub attack_range: f32,
    /// Movement speed in m/s (placeholder 4.0).
    pub speed: f32,
    /// The patrol circuit radius around the spawn center (placeholder
    /// 12.0 m).
    pub waypoint_radius: f32,
    /// The spawn center (the encounter area's origin).
    pub spawn_center: [f32; 3],
    /// Full-health construction value for every adversary
    /// (`combat::health_full` — finite, >= 0; the damage/health
    /// transition contract governs the domain).
    pub max_health: f32,
    /// The adversary's eye height above the ground (m) — the
    /// perception LOS origin and the attack trace origin. Must
    /// differ from half the player body height or a converged
    /// adversary's aim degenerates to the zero vector (the resolver
    /// would surface `Refused(ZeroDirection)` honestly).
    pub eye_height: f32,
    /// B's placeholder view of the player's body — the `Combatant`
    /// box the ATTACK seam resolves against is `player_box(sample,
    /// &this)` over the per-tick player sample.
    pub player: PlayerConfig,
    /// The id the ATTACK seam attributes to the player target it
    /// builds (id spaces are game-owned — F's convention overrides
    /// this placeholder; the surfaced resolution carries it).
    pub player_id: CombatantId,
    /// The ATTACK seam's attacker ids are `base + index` (the same
    /// game-owned-id placeholder discipline; the id is carried for
    /// feedback, never consulted by geometry).
    pub adversary_id_base: CombatantId,
}

impl Default for AdversaryConfig {
    fn default() -> Self {
        AdversaryConfig {
            seed: 0xA11CE,
            count: 1,
            tick_seconds: 0.1,
            aggro_radius: 8.0,
            deaggro_radius: 10.0,
            los_loss_ticks: 24,
            patrol_cadence: 12,
            aggro_cadence: 2,
            recover_cadence: 6,
            attack_cooldown: 18,
            attack_range: 2.0,
            speed: 4.0,
            waypoint_radius: 12.0,
            spawn_center: [0.0, 0.0, 0.0],
            max_health: 100.0,
            eye_height: 1.5,
            player: PlayerConfig::default(),
            player_id: 1,
            adversary_id_base: 0,
        }
    }
}

/// One adversary's authoritative logical state (the spine chain is the
/// scheduler; this struct is the state the chain advances).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Adversary {
    pub state: AdversaryState,
    /// World position (y = the collision world's ground plane).
    pub position: [f32; 3],
    /// The current waypoint index on the seeded circuit.
    pub waypoint: usize,
    /// The chain cursor (payload bookkeeping — also fingerprinted).
    pub cursor: u64,
    /// The logical tick at which the adversary is attack-eligible
    /// again (C3-R2: the canonical logical-tick DEADLINE — set at the
    /// ATTACK emit, read at the AGGRO gate as a pure comparison;
    /// never decremented, so zero idle per-wake bookkeeping and no
    /// wake-frequency-dependent unit drift).
    pub cooldown_until: u64,
    /// Consecutive perception wake-ups with LOS lost (hysteresis).
    pub los_lost: u32,
    /// The adversary's health — `combat::Health` (the contract's
    /// transition domain; transitions happen only through
    /// `AdversaryWorld::apply_player_hit`).
    pub health: crate::combat::Health,
    /// The LAST-PERCEIVED player position (sampled on wake-up only —
    /// never per-frame).
    pub perceived: Option<[f32; 3]>,
    /// Hit-resolution requests emitted (one per ATTACK window).
    pub attacks_emitted: u64,
    /// The SURFACED resolution of the last ATTACK wake (the contract
    /// §5 seam: F reads this — and applies player damage with F's
    /// policy — on encounter-relevant ticks; `attacks_emitted` counts
    /// the fires).
    pub last_attack: Option<HitResolution>,
    /// The conservative scheduler mirror (the Cycle-4 combatants
    /// contract §1): TRUE once the installer has scheduled this
    /// adversary or any follow-up has been emitted — i.e. a live
    /// chain MAY exist. Cleared only by [`AdversaryWorld::reset`].
    /// NOT authoritative logical state (the spine's queue is the
    /// scheduling authority) and deliberately NOT fingerprinted —
    /// it exists to refuse `set_dormant` on a possibly-live chain
    /// (dormancy is a pre-scheduling composition state).
    pub scheduled: bool,
}

/// The adversary world: per-adversary state over the player module's
/// `CollisionWorld` (movement + LOS through the same public queries the
/// player uses). The driver samples the player position ONCE PER
/// LOGICAL TICK (`set_player_position`) — adversaries read it only on
/// their own wake-ups, so perception cost is per-wake-up, never
/// per-frame.
pub struct AdversaryWorld {
    config: AdversaryConfig,
    collision: CollisionWorld,
    adversaries: Vec<Adversary>,
    player_position: [f32; 3],
}

impl AdversaryWorld {
    /// Spawns `config.count` adversaries at seeded positions on their
    /// waypoint circuits, PATROL state, full health. `max_health` is
    /// validated by `combat::health_full` (fail-visible at
    /// construction — the placeholder contract: finite, >= 0).
    pub fn new(config: AdversaryConfig, collision: CollisionWorld) -> Self {
        let spawn_center = config.spawn_center;
        let full_health =
            crate::combat::health_full(config.max_health).expect("AdversaryConfig.max_health");
        let adversaries = (0..config.count)
            .map(|index| fresh_adversary(&config, index, full_health))
            .collect();
        AdversaryWorld {
            config,
            collision,
            adversaries,
            player_position: spawn_center,
        }
    }

    /// The full restoration of every combatant (the Cycle-4
    /// combatants contract §5): each adversary returns to its
    /// CONSTRUCTION state — PATROL, full health, the seeded spawn
    /// position and waypoint, zero cursor/cooldown/hysteresis
    /// bookkeeping, no perceived position, no last attack, the
    /// `scheduled` mirror cleared — and the player sample returns to
    /// the spawn center. In-place and deterministic: the restored
    /// world is fingerprint-identical to a fresh construction under
    /// the same config (pinned by test).
    ///
    /// PAIRING RULE (binding): reset restores the WORLD, not the
    /// spine — rebuild the spine around the reset world for exact
    /// replay equivalence. Stale spine events are NOT inert after a
    /// reset (every adversary is PATROL again, and a stale routine
    /// event is a normal wake). The Cycle-3 demo's
    /// reconstruct-everything reset remains the canonical simple
    /// path; this primitive serves an in-place round reset paired
    /// with a spine rebuild.
    pub fn reset(&mut self) {
        let spawn_center = self.config.spawn_center;
        let full_health =
            crate::combat::health_full(self.config.max_health).expect("AdversaryConfig.max_health");
        for index in 0..self.adversaries.len() {
            self.adversaries[index] = fresh_adversary(&self.config, index, full_health);
        }
        self.player_position = spawn_center;
    }

    pub fn config(&self) -> &AdversaryConfig {
        &self.config
    }

    pub fn count(&self) -> usize {
        self.adversaries.len()
    }

    pub fn adversary(&self, index: usize) -> &Adversary {
        &self.adversaries[index]
    }

    /// How many adversaries are actively threatening (AGGRO/ATTACK/
    /// RECOVER) — the proportionality measurement's numerator.
    pub fn active_count(&self) -> usize {
        self.adversaries
            .iter()
            .filter(|a| {
                matches!(
                    a.state,
                    AdversaryState::Aggro | AdversaryState::Attack | AdversaryState::Recover
                )
            })
            .count()
    }

    /// How many adversaries are DEFEATED (terminal) — the victory
    /// condition's numerator.
    pub fn defeated_count(&self) -> usize {
        self.adversaries
            .iter()
            .filter(|a| a.state == AdversaryState::Defeated)
            .count()
    }

    /// The configured victory condition's engine read (the Cycle-4
    /// combatants contract §5): TRUE exactly when at least one
    /// adversary exists AND every one is DEFEATED. An empty world is
    /// NOT "all defeated" — a vacuous win is not a win (a completion
    /// check on an empty encounter is a composition bug, and this
    /// read refuses to bless it).
    pub fn all_defeated(&self) -> bool {
        !self.adversaries.is_empty() && self.defeated_count() == self.adversaries.len()
    }

    /// The multi-candidate target slice (the Cycle-4 combatants
    /// contract §4): one [`Combatant`] per NON-DEFEATED adversary —
    /// id = `adversary_id_base + index`, hit box = the
    /// [`ADVERSARY_BODY_HALF`] body model at the live position
    /// (grounded like `player_box`). DORMANT adversaries are
    /// INCLUDED (the engine does not gate activation — the encounter
    /// layer's pre-activation press refusal is the guard); DEFEATED
    /// bodies are not hittable (terminal integrity — an engine
    /// guarantee, not policy). Allocates one Vec per call: fine at
    /// fire-edge frequency (edge-triggered), never per-frame.
    pub fn combatants(&self) -> Vec<Combatant> {
        self.adversaries
            .iter()
            .enumerate()
            .filter(|(_, adversary)| adversary.state != AdversaryState::Defeated)
            .map(|(index, adversary)| Combatant {
                id: self.config.adversary_id_base + index as CombatantId,
                hit_box: StaticCollider {
                    min: [
                        adversary.position[0] - ADVERSARY_BODY_HALF[0],
                        adversary.position[1],
                        adversary.position[2] - ADVERSARY_BODY_HALF[2],
                    ],
                    max: [
                        adversary.position[0] + ADVERSARY_BODY_HALF[0],
                        adversary.position[1] + 2.0 * ADVERSARY_BODY_HALF[1],
                        adversary.position[2] + ADVERSARY_BODY_HALF[2],
                    ],
                },
            })
            .collect()
    }

    /// The driver's once-per-logical-tick player sample. Invalid
    /// (non-finite) input is REFUSED and the previous sample kept —
    /// the player.rs validity discipline.
    pub fn set_player_position(&mut self, position: [f32; 3]) {
        if position.iter().all(|v| v.is_finite()) {
            self.player_position = position;
        }
    }

    /// The current player sample (driver-read; never adversary-mutated).
    pub fn player_position(&self) -> [f32; 3] {
        self.player_position
    }

    /// The incoming-damage seam (the contract §3/§5 Health-ownership
    /// convention): player-fired damage routes through the adversary's
    /// `combat::Health::apply_damage` HERE, inside the owning module.
    /// `Some(Ok(DamageOutcome::Defeated {..}))` is the ONLY entry into
    /// DEFEATED (the engine's single defeat declaration); `Wounded`/
    /// `Graze` provoke (damage received forces AGGRO — a DORMANT
    /// adversary takes the damage but stays dormant: activation is
    /// F's scripting authority); `Err(_)` is the Health refusal
    /// passthrough with the value bit-identical. `None` = no such
    /// adversary, or already DEFEATED (terminal — refuses further
    /// damage).
    pub fn apply_player_hit(
        &mut self,
        index: usize,
        amount: f32,
    ) -> Option<Result<DamageOutcome, HealthError>> {
        let adversary = self.adversaries.get_mut(index)?;
        if adversary.state == AdversaryState::Defeated {
            return None;
        }
        let outcome = adversary.health.apply_damage(amount);
        if let Ok(DamageOutcome::Defeated { .. }) = outcome {
            adversary.state = AdversaryState::Defeated;
            adversary.perceived = None;
        } else if outcome.is_ok() && adversary.state != AdversaryState::Dormant {
            // Provocation: damage received forces AGGRO.
            adversary.state = AdversaryState::Aggro;
            adversary.los_lost = 0;
        }
        Some(outcome)
    }

    /// F's activation schedule for ONE dormant adversary (the
    /// encounter scripting entry): the first wake-up spec, carrying
    /// the ACTIVATION payload ([`ADVERSARY_ACTIVATION_CURSOR`] — the
    /// Cycle-4 combatants contract §3: the only payload kind that
    /// may wake a DORMANT adversary; inert against every other
    /// state, so a duplicate activation cannot fork a chain).
    /// Returns `None` when the adversary is not dormant (activation
    /// authority is exclusive to the dormant state).
    pub fn activation_spec(&self, index: usize, due: Tick) -> Option<EventSpec> {
        let adversary = self.adversaries.get(index)?;
        if adversary.state != AdversaryState::Dormant {
            return None;
        }
        Some(
            EventSpec::new(
                EventClass::new(index as u16),
                Schedule::AtLogicalTime { due },
                adversary_payload(index, ADVERSARY_ACTIVATION_CURSOR),
            )
            .in_field(Field::Far),
        )
    }

    /// Sets the adversary to DORMANT (the encounter-composition
    /// surface, paired with [`Self::activation_spec`]): an encounter
    /// that gates its start on its own line begins its adversaries
    /// dormant — `install_adversary_schedules` then schedules NOTHING
    /// for them (it schedules only eligible adversaries), and the
    /// encounter schedules the first wake-up through
    /// `activation_spec` at its activation moment.
    ///
    /// THE DORMANCY RULE (the Cycle-4 combatants contract §1 — the
    /// H31-1 reconciliation): dormancy is a PRE-SCHEDULING
    /// composition state. Returns `true` when the adversary is now
    /// DORMANT (no chain was live); returns `false` — REFUSED, state
    /// unchanged — when there is no such adversary, or when its
    /// chain is or may be live (the conservative `scheduled` mirror:
    /// installed or activated since construction/reset). A `false`
    /// is a composition bug in the caller: construct dormant (or
    /// [`Self::reset`] first), then schedule. Never silently leaves
    /// live scheduled events attached to a newly dormant entity.
    pub fn set_dormant(&mut self, index: usize) -> bool {
        let Some(adversary) = self.adversaries.get_mut(index) else {
            return false;
        };
        if adversary.scheduled {
            return false;
        }
        adversary.state = AdversaryState::Dormant;
        adversary.perceived = None;
        adversary.los_lost = 0;
        true
    }

    /// The distance on the ground plane (the perception metric).
    fn ground_distance(a: [f32; 3], b: [f32; 3]) -> f32 {
        let dx = a[0] - b[0];
        let dz = a[2] - b[2];
        (dx * dx + dz * dz).sqrt()
    }

    /// One adversary's perception at a wake-up: distance gate, then a
    /// LOS query through the collision world (`segment_hit` from the
    /// adversary's eye to the player's eye — the contract §2
    /// recommendation: the exact-slab discipline, margin 0.0).
    /// Hysteresis bookkeeping (`los_lost`) is the caller's — this
    /// returns the raw perception.
    fn perceives(&self, index: usize) -> bool {
        let adversary = &self.adversaries[index];
        let player = self.player_position;
        if Self::ground_distance(adversary.position, player) > self.config.aggro_radius {
            return false;
        }
        let eye = [
            adversary.position[0],
            adversary.position[1] + self.config.eye_height,
            adversary.position[2],
        ];
        let target = [
            player[0],
            player[1] + self.config.player.eye_height,
            player[2],
        ];
        self.collision.segment_hit(eye, target, 0.0).is_none()
    }

    /// Move `adversary` one wake-up step toward `target` on the ground
    /// plane, through the collision world. The destination this wake
    /// is exactly ON the target when it lies within one step (the
    /// snap — bit-exact waypoint/spot arrival), else one step along
    /// the direction; BOTH destinations pass the SAME blocking check
    /// (`segment_hit` with the body margin) — the snap is NOT a
    /// teleport (review F1: at the default cadences the step is 0.8 m
    /// aggro / 4.8 m patrol, larger than the 0.4 m body margin, so an
    /// unblocked straight line is the only way to land on the target).
    /// A blocked move is REFUSED in place (the patrol circuit routes
    /// around; the pursuit stops at cover — the collide-what-you-stop-
    /// at minimal semantics; full sweep movement is a later
    /// refinement, stated).
    fn move_toward(&mut self, index: usize, target: [f32; 3], cadence: u64) {
        let adversary = &mut self.adversaries[index];
        let step = self.config.speed * self.config.tick_seconds * cadence as f32;
        let dx = target[0] - adversary.position[0];
        let dz = target[2] - adversary.position[2];
        let length = (dx * dx + dz * dz).sqrt();
        if length <= f32::EPSILON {
            // The sub-EPSILON residue normalization: assign the
            // target's bits without the blocking check — a ≤ ~1.2e-7 m
            // displacement, six orders of magnitude below the body
            // margin, unable to cross geometry (kept for bit-exact
            // arrival; NOT the F1 snap, which is checked below).
            adversary.position = [target[0], adversary.position[1], target[2]];
            return;
        }
        let destination = if length <= step {
            [target[0], adversary.position[1], target[2]]
        } else {
            [
                adversary.position[0] + dx / length * step,
                adversary.position[1],
                adversary.position[2] + dz / length * step,
            ]
        };
        let blocked = self
            .collision
            .segment_hit(
                [
                    adversary.position[0],
                    adversary.position[1] + 0.5,
                    adversary.position[2],
                ],
                [destination[0], destination[1] + 0.5, destination[2]],
                0.4,
            )
            .is_some();
        if !blocked {
            adversary.position = destination;
        }
    }

    /// The ATTACK seam (the contract §1/§5): ONE hit-resolution
    /// request per attack window — from the adversary's eye toward
    /// the player body's center (the box over the per-tick sample,
    /// `player_box` with B's placeholder player view), through the
    /// shared collision world, with B's placeholder profile (range =
    /// `attack_range`, exact-face occlusion). The aim is valid by
    /// construction (the eye sits above the box center on the ground
    /// plane and all positions are validity-checked upstream), so a
    /// `Refused` outcome would surface honestly rather than occur by
    /// design. The outcome is SURFACED per-adversary (`last_attack`)
    /// — F applies player damage with F's policy; B never mutates
    /// player Health (the Health-ownership convention).
    fn resolve_adversary_attack(&self, index: usize) -> HitResolution {
        let adversary = &self.adversaries[index];
        let eye = [
            adversary.position[0],
            adversary.position[1] + self.config.eye_height,
            adversary.position[2],
        ];
        let (min, max) = player_box(self.player_position, &self.config.player);
        let center = [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ];
        let request = AttackRequest {
            attacker: self.config.adversary_id_base + index as CombatantId,
            origin: eye,
            direction: [center[0] - eye[0], center[1] - eye[1], center[2] - eye[2]],
            profile: AttackProfile {
                range: self.config.attack_range,
                occlusion_margin: 0.0,
            },
        };
        let targets = [Combatant {
            id: self.config.player_id,
            hit_box: StaticCollider { min, max },
        }];
        resolve_attack(&self.collision, &request, &targets)
    }

    /// One adversary's wake-up: the state machine step (a pure
    /// function of logical state, the player sample, and the seeded
    /// circuit — deterministic, headless-reproducible). `now` is the
    /// EXECUTION tick (the event ledger's canonical logical clock —
    /// never wall-clock). DEFEATED is terminal (no transition, and
    /// `execute` schedules no follow-up — the chain stops); a DORMANT
    /// adversary's activation wake-up (the spec F schedules)
    /// TRANSITIONS it to PATROL, whose cadence then carries the chain
    /// onward. Attack readiness is the `cooldown_until` logical-tick
    /// DEADLINE (C3-R2): set at the ATTACK emit, compared at the AGGRO
    /// gate — never decremented, so the effective duration is the
    /// configured value in ticks regardless of the wake cadences.
    fn wake(&mut self, index: usize, now: Tick) {
        if self.adversaries[index].state == AdversaryState::Defeated {
            return;
        }
        let cadence = self.adversaries[index].state.cadence(&self.config);
        let mut transition = None;
        match self.adversaries[index].state {
            AdversaryState::Dormant => {
                // The activation wake-up: F's `activation_spec` event.
                transition = Some(AdversaryState::Patrol);
            }
            AdversaryState::Patrol => {
                if self.perceives(index) {
                    self.adversaries[index].perceived = Some(self.player_position);
                    self.adversaries[index].los_lost = 0;
                    transition = Some(AdversaryState::Aggro);
                } else {
                    let target =
                        circuit_waypoint(&self.config, index, self.adversaries[index].waypoint);
                    if Self::ground_distance(self.adversaries[index].position, target)
                        <= self.config.speed * self.config.tick_seconds * cadence as f32
                    {
                        // Waypoint reached: advance the circuit (seeded
                        // wrap; two waypoints per adversary).
                        self.adversaries[index].waypoint =
                            (self.adversaries[index].waypoint + 1) % 2;
                    } else {
                        self.move_toward(index, target, cadence);
                    }
                }
            }
            AdversaryState::Aggro => {
                let distance =
                    Self::ground_distance(self.adversaries[index].position, self.player_position);
                if distance <= self.config.attack_range
                    && now.0 >= self.adversaries[index].cooldown_until
                    && self.perceives(index)
                {
                    self.adversaries[index].perceived = Some(self.player_position);
                    transition = Some(AdversaryState::Attack);
                } else if self.perceives(index) {
                    self.adversaries[index].los_lost = 0;
                    self.adversaries[index].perceived = Some(self.player_position);
                    let target = self.adversaries[index].perceived.unwrap();
                    self.move_toward(index, target, cadence);
                } else {
                    self.adversaries[index].los_lost += 1;
                    let lost = self.adversaries[index].los_lost >= self.config.los_loss_ticks;
                    let far = distance > self.config.deaggro_radius;
                    if lost || far {
                        self.adversaries[index].perceived = None;
                        self.adversaries[index].los_lost = 0;
                        transition = Some(AdversaryState::Patrol);
                    } else if let Some(target) = self.adversaries[index].perceived {
                        // Pursue the LAST-PERCEIVED position while the
                        // hysteresis window holds.
                        self.move_toward(index, target, cadence);
                    }
                }
            }
            AdversaryState::Attack => {
                // The attack seam: ONE hit-resolution request per
                // window through the combat contract — resolved and
                // SURFACED (F applies player damage). The cooldown
                // gate is the canonical logical-tick DEADLINE (C3-R2):
                // saturating so a huge configured cooldown never wraps
                // into instant re-eligibility, and never decremented —
                // readiness is a pure comparison at the next AGGRO
                // gate.
                let resolution = self.resolve_adversary_attack(index);
                self.adversaries[index].last_attack = Some(resolution);
                self.adversaries[index].attacks_emitted += 1;
                self.adversaries[index].cooldown_until =
                    now.saturating_add(self.config.attack_cooldown).0;
                transition = Some(AdversaryState::Recover);
            }
            AdversaryState::Recover => {
                transition = Some(if self.perceives(index) {
                    self.adversaries[index].los_lost = 0;
                    self.adversaries[index].perceived = Some(self.player_position);
                    AdversaryState::Aggro
                } else {
                    AdversaryState::Patrol
                });
            }
            AdversaryState::Defeated => {}
        }
        if let Some(next) = transition {
            self.adversaries[index].state = next;
        }
    }
}

impl World for AdversaryWorld {
    fn execute(&mut self, event: &Event, ctx: &mut ExecCtx) {
        let Some((index, cursor)) = adversary_parts(event.payload) else {
            return;
        };
        if index >= self.adversaries.len() {
            return;
        }
        // The state/payload guard matrix (the Cycle-4 combatants
        // contract §3 — the H31-1 reconciliation): only the
        // ACTIVATION payload may wake a DORMANT adversary, the
        // activation payload is inert against every non-dormant
        // state (a duplicate activation cannot fork a chain), and
        // DEFEATED is terminal. The cursor advances on every
        // executed event — the honest world mirror of the ledger
        // (an inert event DID execute) — but an inert combination
        // does no state-machine work and schedules no follow-up.
        let activation = cursor == ADVERSARY_ACTIVATION_CURSOR;
        let inert = match self.adversaries[index].state {
            AdversaryState::Defeated => true,
            AdversaryState::Dormant => !activation,
            _ => activation,
        };
        self.adversaries[index].cursor += 1;
        if inert {
            return;
        }
        self.wake(index, ctx.executed_tick());
        let state = self.adversaries[index].state;
        let cadence = state.cadence(&self.config);
        if cadence > 0 {
            // A live chain may exist from here on (the conservative
            // `scheduled` mirror — the dormancy rule's guard).
            self.adversaries[index].scheduled = true;
            ctx.follow_up_in_field(
                state.lane(),
                cadence,
                event.class,
                adversary_payload(index, self.adversaries[index].cursor),
                Vec::new(),
            );
        }
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        // The namespaced adversary domain — deliberately distinct from
        // every frozen fingerprint family (an unexpected change in any
        // pinned fingerprint is an investigation, never a repin).
        fp.write_str("engine-core/adversary-world/1");
        fp.write_u64(self.config.seed);
        fp.write_usize(self.adversaries.len());
        for adversary in &self.adversaries {
            fp.write_u8(adversary.state.code());
            for value in adversary.position {
                fp.write_u32(value.to_bits());
            }
            fp.write_u64(adversary.cursor);
            fp.write_u64(adversary.cooldown_until);
            fp.write_u32(adversary.los_lost);
            fp.write_u32(adversary.health.current.to_bits());
            fp.write_u32(adversary.health.maximum.to_bits());
            fp.write_u64(adversary.attacks_emitted);
            fp.write_u8(u8::from(adversary.perceived.is_some()));
            // The surfaced last resolution (bit-stable per the
            // contract's §6 float discipline — the resolution path is
            // IEEE exactly-rounded arithmetic only).
            match adversary.last_attack {
                None => fp.write_u8(0),
                Some(HitResolution::Hit {
                    target, distance, ..
                }) => {
                    fp.write_u8(1);
                    fp.write_u32(target);
                    fp.write_u32(distance.to_bits());
                }
                Some(HitResolution::Miss {
                    kind: MissKind::Occluded { distance, .. },
                }) => {
                    fp.write_u8(2);
                    fp.write_u32(distance.to_bits());
                }
                Some(HitResolution::Miss {
                    kind: MissKind::Clear,
                }) => fp.write_u8(3),
                Some(HitResolution::Refused(reason)) => {
                    fp.write_u8(4);
                    fp.write_u8(match reason {
                        AttackRefusal::NonFiniteOrigin => 0,
                        AttackRefusal::NonFiniteDirection => 1,
                        AttackRefusal::ZeroDirection => 2,
                        AttackRefusal::InvalidProfile(_) => 3,
                    });
                }
            }
        }
    }
}

/// The seeded waypoint `index`'s circuit position: two waypoints per
/// adversary on a circle of `waypoint_radius` around the spawn center
/// (the phase offset seeded per adversary).
fn circuit_waypoint(config: &AdversaryConfig, index: usize, waypoint: usize) -> [f32; 3] {
    let phase = mix64(config.seed ^ ((index as u64) << 32)) % 360;
    let angle = (phase + (waypoint as u64) * 180) as f32 * std::f32::consts::PI / 180.0;
    [
        config.spawn_center[0] + config.waypoint_radius * angle.cos(),
        config.spawn_center[1],
        config.spawn_center[2] + config.waypoint_radius * angle.sin(),
    ]
}

/// The local SplitMix64 finalizer (engine-core cannot reach
/// engine-slice's `npc::mix64` — identical constants, private copy).
fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// The construction-state adversary for `index` (shared verbatim by
/// [`AdversaryWorld::new`] and [`AdversaryWorld::reset`] — the full
/// restoration IS a reconstruction, no drift by construction).
fn fresh_adversary(
    config: &AdversaryConfig,
    index: usize,
    full_health: crate::combat::Health,
) -> Adversary {
    Adversary {
        state: AdversaryState::Patrol,
        position: circuit_waypoint(config, index, 0),
        waypoint: 1,
        cursor: 0,
        cooldown_until: 0,
        los_lost: 0,
        health: full_health,
        perceived: None,
        attacks_emitted: 0,
        last_attack: None,
        scheduled: false,
    }
}

/// Installs one routine chain per ELIGIBLE adversary — the seeded
/// spread over the first patrol cadence, in the state's lane class.
/// ELIGIBLE (the Cycle-4 combatants contract §2 — the H31-1
/// reconciliation, reconciling the implementation with its
/// documentation): an ACTIVE behavior state (PATROL/AGGRO/ATTACK/
/// RECOVER — NOT DORMANT, NOT DEFEATED) with NO chain installed
/// since construction or reset. DORMANT adversaries are NOT
/// scheduled (D activates them via
/// `AdversaryWorld::activation_spec`); DEFEATED adversaries are not
/// scheduled; an already-installed adversary is not re-scheduled —
/// the installer is IDEMPOTENT (a second call adds nothing, so a
/// double install cannot fork chains). The `scheduled` mirror is set
/// for every adversary it schedules (the dormancy rule's guard).
pub fn install_adversary_schedules(spine: &mut Spine<AdversaryWorld>) {
    let (seed, count, patrol_cadence) = {
        let world = spine.world();
        (
            world.config().seed,
            world.count(),
            world.config().patrol_cadence,
        )
    };
    for index in 0..count {
        // Eligibility (read before any mutation): an active state
        // with no chain installed.
        let eligible = {
            let world = spine.world();
            let adversary = &world.adversaries[index];
            matches!(
                adversary.state,
                AdversaryState::Patrol
                    | AdversaryState::Aggro
                    | AdversaryState::Attack
                    | AdversaryState::Recover
            ) && !adversary.scheduled
        };
        if !eligible {
            continue;
        }
        let spread = patrol_cadence.max(1);
        let offset = mix64(seed ^ ((index as u64) << 32) ^ 0x5EED_0000) % spread;
        spine.world_mut().adversaries[index].scheduled = true;
        spine
            .schedule(
                EventSpec::new(
                    EventClass::new(index as u16),
                    Schedule::AtLogicalTime {
                        due: Tick(1 + offset),
                    },
                    adversary_payload(index, 0),
                )
                .in_field(Field::Far),
            )
            .expect("the adversary wake-up schedule is dependency-free");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One per-tick trace record: (state code, x bits, z bits,
    /// attacks emitted) — the double-run comparison key.
    type TraceRecord = (u8, u32, u32, u64);

    /// The exact-milestone scenario config: unit cadences make every
    /// wake-up land on a known tick (the placeholder parameters are
    /// test inputs, not policy).
    fn scenario_config(count: usize) -> AdversaryConfig {
        AdversaryConfig {
            count,
            patrol_cadence: 1,
            aggro_cadence: 1,
            recover_cadence: 6,
            attack_cooldown: 18,
            ..AdversaryConfig::default()
        }
    }

    /// Builds a spine over the scenario world and runs `ticks` logical
    /// ticks with the player held at `player`, recording the per-tick
    /// (state, position, attacks) trace.
    fn run_scenario(
        config: AdversaryConfig,
        collision: CollisionWorld,
        player: [f32; 3],
        ticks: u64,
    ) -> (Spine<AdversaryWorld>, Vec<TraceRecord>) {
        let mut spine = Spine::with_reservation(AdversaryWorld::new(config, collision), 200, 60);
        install_adversary_schedules(&mut spine);
        let mut trace: Vec<TraceRecord> = Vec::new();
        for _ in 0..ticks {
            spine.world_mut().set_player_position(player);
            spine.run_tick();
            let adversary = spine.world().adversary(0);
            trace.push((
                adversary.state.code(),
                adversary.position[0].to_bits(),
                adversary.position[2].to_bits(),
                adversary.attacks_emitted,
            ));
        }
        (spine, trace)
    }

    /// `run_scenario` for a scripted player: `player_at(tick)` supplies
    /// the sample set just before logical `tick` (1-based) runs.
    fn run_scripted(
        config: AdversaryConfig,
        collision: CollisionWorld,
        player_at: impl Fn(u64) -> [f32; 3],
        ticks: u64,
    ) -> (Spine<AdversaryWorld>, Vec<TraceRecord>) {
        let mut spine = Spine::with_reservation(AdversaryWorld::new(config, collision), 200, 60);
        install_adversary_schedules(&mut spine);
        let mut trace: Vec<TraceRecord> = Vec::new();
        for tick in 1..=ticks {
            spine.world_mut().set_player_position(player_at(tick));
            spine.run_tick();
            let adversary = spine.world().adversary(0);
            trace.push((
                adversary.state.code(),
                adversary.position[0].to_bits(),
                adversary.position[2].to_bits(),
                adversary.attacks_emitted,
            ));
        }
        (spine, trace)
    }

    /// The full deterministic sequence: PATROL perceives at its first
    /// wake-up (unit cadence → tick 1), pursues at 0.4 m/tick from
    /// 5 m, ATTACKS exactly at tick 11 (in range at tick 10's wake —
    /// distance 1.8 ≤ 2.0 — the attack lands on the ATTACK wake),
    /// RECOVERS 6 ticks, re-agpros, and the 18-tick LOGICAL-TICK
    /// cooldown deadline (C3-R2: set at the emit, never decremented)
    /// gates the gate wake to exactly tick 29 and attack #2 to tick
    /// 30 — then #3 at 49 and #4 at 68 (the 19-tick attack cycle:
    /// 18 cooldown + 1 ATTACK-cadence emit wake). The whole trace
    /// double-runs identical.
    #[test]
    fn state_machine_transitions_and_attack_cadence_are_deterministic() {
        let config = scenario_config(1);
        let spawn = circuit_waypoint(&config, 0, 0);
        let player = [spawn[0] + 5.0, spawn[1], spawn[2]];
        let (spine, trace) = run_scenario(config, CollisionWorld::new(0.0), player, 70);
        let adversary = spine.world().adversary(0);
        assert_eq!(
            trace[0].0,
            AdversaryState::Aggro.code(),
            "perceives at tick 1"
        );
        assert_eq!(
            adversary.attacks_emitted, 4,
            "attacks at ticks 11, 30, 49, 68"
        );
        // The exact milestone ticks (fail-first calibrated; the C3-R2
        // deadline indices supersede the defective per-wake-decrement
        // values 36/61).
        assert_eq!(trace[10].3, 1, "attack #1 lands at tick 11 (index 10)");
        assert_eq!(
            trace[28].3, 1,
            "still one attack at the tick-29 gate wake (the deadline)"
        );
        assert_eq!(trace[29].3, 2, "attack #2 lands at tick 30 (index 29)");
        assert_eq!(trace[48].3, 3, "attack #3 lands at tick 49 (index 48)");
        assert_eq!(trace[67].3, 4, "attack #4 lands at tick 68 (index 67)");
        let (_spine2, trace2) =
            run_scenario(scenario_config(1), CollisionWorld::new(0.0), player, 70);
        assert_eq!(trace, trace2, "the scenario double-runs identical");
    }

    /// Perception hysteresis + provocation. The control collapses the
    /// patrol circuit (waypoint_radius 0 — the adversary holds its
    /// spawn) so distances are static: 9 m (between aggro 8 and
    /// deaggro 10) never aggros from PATROL. Provocation forces AGGRO
    /// out-of-band. The de-aggro branches: (LOS-loss) a wall between
    /// at 9 m — distance under deaggro, so only the 24-wake LOS-loss
    /// window can fire, exactly; (far) a retreating player outruns
    /// the 0.4 m/tick pursuit and crosses the de-aggro radius.
    #[test]
    fn perception_hysteresis_and_provocation() {
        let mut config = scenario_config(1);
        config.waypoint_radius = 0.0;
        let spawn = circuit_waypoint(&config, 0, 0);
        let far = [spawn[0] + 9.0, spawn[1], spawn[2]];
        let (_spine, trace) = run_scenario(config.clone(), CollisionWorld::new(0.0), far, 30);
        assert!(
            trace.iter().all(|t| t.0 != AdversaryState::Aggro.code()),
            "9 m never crosses the 8 m aggro radius from PATROL"
        );

        // Provocation forces AGGRO regardless of distance — the damage
        // routes through the contract's Health transition.
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(config.clone(), CollisionWorld::new(0.0)),
            200,
            60,
        );
        install_adversary_schedules(&mut spine);
        let outcome = spine
            .world_mut()
            .apply_player_hit(0, 10.0)
            .expect("the adversary exists");
        assert_eq!(
            outcome,
            Ok(DamageOutcome::Wounded {
                remaining: 90.0,
                dealt: 10.0
            })
        );
        assert_eq!(spine.world().adversary(0).state, AdversaryState::Aggro);
        assert_eq!(spine.world().adversary(0).health.current, 90.0);

        // (LOS-loss) the player at 9 m behind a wall: distance < deaggro
        // so only the LOS-loss window can fire — PATROL exactly at the
        // 25th wake (los_loss_ticks 24, aggro cadence 1).
        let mut walled = CollisionWorld::new(0.0);
        walled = walled.with_box([spawn[0] + 4.5, 0.0, spawn[2]], 1.0, 2.0);
        let mut spine =
            Spine::with_reservation(AdversaryWorld::new(config.clone(), walled), 200, 60);
        install_adversary_schedules(&mut spine);
        assert_eq!(
            spine.world_mut().apply_player_hit(0, 10.0),
            Some(Ok(DamageOutcome::Wounded {
                remaining: 90.0,
                dealt: 10.0
            })),
            "provocation lands"
        );
        for tick in 0..30 {
            spine.world_mut().set_player_position(far);
            spine.run_tick();
            let state = spine.world().adversary(0).state;
            if tick < 23 {
                assert_eq!(
                    state,
                    AdversaryState::Aggro,
                    "tick {tick}: LOS window holds"
                );
            } else {
                assert_eq!(
                    state,
                    AdversaryState::Patrol,
                    "tick {tick}: LOS-loss de-aggro"
                );
            }
        }

        // (Far) a retreating player at 0.5 m/tick outruns the pursuit:
        // the distance crosses deaggro and the AGGRO falls back to
        // PATROL through the distance branch.
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(config, CollisionWorld::new(0.0)),
            200,
            60,
        );
        install_adversary_schedules(&mut spine);
        assert_eq!(
            spine.world_mut().apply_player_hit(0, 10.0),
            Some(Ok(DamageOutcome::Wounded {
                remaining: 90.0,
                dealt: 10.0
            })),
            "provocation lands"
        );
        let mut deaggroed = false;
        for tick in 0..60 {
            let retreat = [spawn[0] + 9.0 + 0.5 * tick as f32, spawn[1], spawn[2]];
            spine.world_mut().set_player_position(retreat);
            spine.run_tick();
            if spine.world().adversary(0).state == AdversaryState::Patrol {
                deaggroed = true;
                break;
            }
        }
        assert!(
            deaggroed,
            "the retreating player crosses the de-aggro radius"
        );
    }

    /// PATROL follows the seeded waypoint circuit (no player
    /// anywhere near): positions advance toward the circuit waypoints
    /// and the waypoint index cycles — collision-free, so the exact
    /// 0.4 m/tick arithmetic holds; the trace double-runs identical.
    #[test]
    fn patrol_follows_seeded_waypoints() {
        let config = scenario_config(1);
        let spawn = circuit_waypoint(&config, 0, 0);
        let nowhere = [spawn[0] + 500.0, spawn[1], spawn[2] + 500.0];
        let (spine, trace) = run_scenario(config, CollisionWorld::new(0.0), nowhere, 60);
        let adversary = spine.world().adversary(0);
        assert_eq!(adversary.state, AdversaryState::Patrol);
        let target = circuit_waypoint(spine.world().config(), 0, adversary.waypoint);
        let distance = AdversaryWorld::ground_distance(adversary.position, target);
        assert!(
            distance < 12.0,
            "the patrol stays on its circuit (distance to next waypoint {distance})"
        );
        let (_spine2, trace2) =
            run_scenario(scenario_config(1), CollisionWorld::new(0.0), nowhere, 60);
        assert_eq!(trace, trace2, "the patrol double-runs identical");
    }

    /// Collide-what-you-stop-at: with a box between the adversary and
    /// the player, the AGGRO pursuit's straight step is refused in
    /// place (no tunneling) — the position freezes while blocked.
    #[test]
    fn pursuit_stops_at_colliders() {
        let config = scenario_config(1);
        let mut collision = CollisionWorld::new(0.0);
        // A wall box ON the pursuit line (z = spawn z), spanning the
        // 0.4-margin body between spawn+2.5 and spawn+3.5.
        let spawn = circuit_waypoint(&config, 0, 0);
        collision = collision.with_box([spawn[0] + 3.0, 0.0, spawn[2]], 1.0, 2.0);
        let player = [spawn[0] + 6.0, spawn[1], spawn[2]];
        let (spine, _trace) = run_scenario(config, collision, player, 30);
        let adversary = spine.world().adversary(0);
        assert_eq!(adversary.state, AdversaryState::Aggro);
        let moved = AdversaryWorld::ground_distance(adversary.position, spawn);
        assert!(
            moved > 2.0 && moved < 2.8,
            "the pursuit advanced then stopped at the wall's 0.4-margin edge \
             (moved {moved} m from spawn; the box spans 2.5..3.5)"
        );
        assert!(
            AdversaryWorld::ground_distance(adversary.position, player) > 3.0,
            "no tunneling through the collider"
        );
    }

    /// The driver's player sample refuses non-finite input and keeps
    /// the last valid sample (the player.rs validity discipline).
    #[test]
    fn player_sample_validity_refusals() {
        let mut world = AdversaryWorld::new(scenario_config(1), CollisionWorld::new(0.0));
        let initial = world.player_position();
        world.set_player_position([f32::NAN, 0.0, 0.0]);
        assert_eq!(world.player_position(), initial, "NaN refused");
        world.set_player_position([f32::INFINITY, 0.0, 0.0]);
        assert_eq!(world.player_position(), initial, "infinity refused");
        world.set_player_position([1.0, 2.0, 3.0]);
        assert_eq!(world.player_position(), [1.0, 2.0, 3.0], "finite accepted");
    }

    /// The proportionality guard (charter §6): idle adversaries hold
    /// EXACTLY one queued wake-up — 8 patrolling adversaries at
    /// 12-tick cadence execute exactly 80 events over 120 ticks and
    /// hold exactly 8 pending at the end (never per-frame work).
    #[test]
    fn idle_adversaries_hold_one_wake_up_each() {
        let config = scenario_config(8);
        let mut config = config;
        config.patrol_cadence = 12;
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(config, CollisionWorld::new(0.0)),
            200,
            60,
        );
        install_adversary_schedules(&mut spine);
        let nowhere = [5000.0, 0.0, 5000.0];
        for _ in 0..120 {
            spine.world_mut().set_player_position(nowhere);
            spine.run_tick();
        }
        let stats = spine.stats();
        assert_eq!(stats.executed, 80, "8 adversaries × 10 wake-ups each");
        assert_eq!(spine.pending_count(), 8, "exactly one queued wake-up each");
    }

    /// DEFEATED drains and terminates: lethal damage stops the chain —
    /// the last queued wake-up drains and executed stops growing (the
    /// encounter-completion signal F reads).
    #[test]
    fn defeated_drains_and_terminates() {
        let config = scenario_config(2);
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(config, CollisionWorld::new(0.0)),
            200,
            60,
        );
        install_adversary_schedules(&mut spine);
        let nowhere = [5000.0, 0.0, 5000.0];
        for _ in 0..10 {
            spine.world_mut().set_player_position(nowhere);
            spine.run_tick();
        }
        let outcome = spine
            .world_mut()
            .apply_player_hit(0, 100.0)
            .expect("the adversary exists");
        assert_eq!(outcome, Ok(DamageOutcome::Defeated { dealt: 100.0 }));
        assert_eq!(spine.world().adversary(0).state, AdversaryState::Defeated);
        assert_eq!(
            spine.world_mut().apply_player_hit(0, 50.0),
            None,
            "DEFEATED refuses further damage"
        );
        for _ in 0..40 {
            spine.world_mut().set_player_position(nowhere);
            spine.run_tick();
        }
        let stats = spine.stats();
        // Adversary 0's chain has ended; adversary 1 still patrols.
        assert_eq!(
            spine.pending_count(),
            1,
            "only the live adversary holds a wake-up"
        );
        assert_eq!(spine.world().adversary(1).state, AdversaryState::Patrol);
        let executed_after_defeat = stats.executed;
        for _ in 0..20 {
            spine.run_tick();
        }
        // The defeated adversary contributes no further executions
        // beyond its last drained wake-up (adversary 1's patrol
        // continues at 1-tick cadence — the delta check below).
        let later = spine.stats().executed;
        assert!(later > executed_after_defeat, "the live chain continues");
        assert_eq!(spine.world().adversary(0).attacks_emitted, 0);
    }

    /// DORMANT activation (F's entry): a dormant adversary holds no
    /// chain; scheduling its `activation_spec` wakes it into PATROL,
    /// whose cadence then carries the chain (the scaffold bug this
    /// test caught: the activation wake-up must TRANSITION, not no-op).
    #[test]
    fn dormant_activation_wakes_into_patrol() {
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(scenario_config(1), CollisionWorld::new(0.0)),
            200,
            60,
        );
        spine.world_mut().adversaries[0].state = AdversaryState::Dormant;
        let spec = spine
            .world()
            .activation_spec(0, Tick(5))
            .expect("the dormant adversary has an activation spec");
        spine.schedule(spec).expect("the activation schedules");
        let nowhere = [5000.0, 0.0, 5000.0];
        for _ in 0..10 {
            spine.world_mut().set_player_position(nowhere);
            spine.run_tick();
        }
        assert_eq!(spine.world().adversary(0).state, AdversaryState::Patrol);
        assert!(spine.pending_count() >= 1, "the patrol chain carries on");
    }

    /// Pursuit toward the LAST-PERCEIVED position (the hysteresis-window
    /// branch): the adversary perceives the player at 5 m for two wakes,
    /// then the player slips behind a wall at 9 m — past the aggro radius
    /// but inside the de-aggro radius, so ONLY the LOS-loss window can
    /// end the chase. Live perception gone, the pursuit advances on the
    /// MEMORY: it converges exactly onto the last-perceived spot, HOLDS
    /// there through the rest of the 24-blind-wake window, de-aggros on
    /// the 24th blind wake, and patrols home — never approaching the
    /// player's live position. Double-run identical.
    #[test]
    fn pursuit_converges_on_last_perceived_position() {
        let mut config = scenario_config(1);
        config.waypoint_radius = 0.0;
        let spawn = circuit_waypoint(&config, 0, 0);
        let seen = [spawn[0] + 5.0, spawn[1], spawn[2]];
        let hidden = [spawn[0] + 9.0, spawn[1], spawn[2]];
        let (spine, trace) = run_scripted(
            config.clone(),
            CollisionWorld::new(0.0).with_box([spawn[0] + 6.0, 0.0, spawn[2]], 1.0, 2.0),
            |tick| if tick <= 2 { seen } else { hidden },
            40,
        );
        // Converged onto the LAST-PERCEIVED position exactly (the snap
        // sets the target bits) and holding through the window.
        for (index, record) in trace.iter().enumerate().skip(13).take(12) {
            assert_eq!(
                *record,
                (
                    AdversaryState::Aggro.code(),
                    seen[0].to_bits(),
                    seen[2].to_bits(),
                    0
                ),
                "index {index}: converged on the last-perceived spot and holding"
            );
        }
        for (index, record) in trace.iter().enumerate() {
            let x = f32::from_bits(record.1);
            assert!(
                (x - hidden[0]).abs() >= 3.5,
                "index {index}: the pursuit stays on the memory, never the \
                 live position (x {x:.3}, live x {})",
                hidden[0]
            );
        }
        // The 24th blind wake ends the chase; the patrol walks home.
        assert_eq!(
            trace[25].0,
            AdversaryState::Patrol.code(),
            "the 24th blind wake de-aggros"
        );
        assert_eq!(trace[26].0, AdversaryState::Patrol.code());
        let homing = f32::from_bits(trace[26].1);
        assert!(
            homing < seen[0] && homing > seen[0] - 1.0,
            "the patrol walks home (x {homing:.3})"
        );
        assert_eq!(spine.world().adversary(0).perceived, None);
        let (_spine2, trace2) = run_scripted(
            config,
            CollisionWorld::new(0.0).with_box([spawn[0] + 6.0, 0.0, spawn[2]], 1.0, 2.0),
            |tick| if tick <= 2 { seen } else { hidden },
            40,
        );
        assert_eq!(trace, trace2, "the chase double-runs identical");
    }

    /// RECOVER timing: after an attack lands, the adversary RECOVERS for
    /// exactly `recover_cadence` wake-ups — no wakes in between, the
    /// position frozen bit-for-bit across the whole window including the
    /// attack and re-aggro wakes — then re-aggros while the player is
    /// still perceived, movement resuming only after the window. The
    /// perception-lost variant: the same recover wake returns PATROL
    /// (walks home) instead of AGGRO.
    #[test]
    fn recover_window_freezes_then_reaggros() {
        let mut config = scenario_config(1);
        config.waypoint_radius = 0.0;
        let spawn = circuit_waypoint(&config, 0, 0);
        let seen = [spawn[0] + 5.0, spawn[1], spawn[2]];

        // (re-aggro) the player stays perceived through the window.
        let (_spine, trace) = run_scenario(config.clone(), CollisionWorld::new(0.0), seen, 40);
        let t_emit = trace
            .iter()
            .position(|record| record.3 == 1)
            .expect("an attack lands");
        assert_eq!(t_emit, 10, "the attack emits at tick 11 (index 10)");
        for (index, record) in trace.iter().enumerate().skip(t_emit).take(6) {
            assert_eq!(
                record.0,
                AdversaryState::Recover.code(),
                "index {index}: the recover window"
            );
        }
        let frozen = trace[t_emit - 1].1;
        for (index, record) in trace.iter().enumerate().skip(t_emit - 1).take(8) {
            assert_eq!(
                record.1, frozen,
                "index {index}: frozen through attack and recover"
            );
        }
        assert_eq!(
            trace[t_emit + 6].0,
            AdversaryState::Aggro.code(),
            "the recover wake re-aggros on perception"
        );
        assert!(
            f32::from_bits(trace[t_emit + 7].1) > f32::from_bits(frozen),
            "movement resumes only after the window"
        );

        // (perception lost) the player vanishes mid-window: the same
        // recover wake returns PATROL and the adversary walks home.
        let nowhere = [spawn[0] + 5000.0, spawn[1], spawn[2]];
        let (_spine, trace) = run_scripted(
            config,
            CollisionWorld::new(0.0),
            |tick| if tick <= 12 { seen } else { nowhere },
            40,
        );
        let t_emit = trace
            .iter()
            .position(|record| record.3 == 1)
            .expect("an attack lands");
        assert_eq!(t_emit, 10, "the same chase up to the emit");
        assert_eq!(
            trace[t_emit + 6].0,
            AdversaryState::Patrol.code(),
            "the recover wake de-aggros without perception"
        );
        assert_eq!(
            trace[t_emit + 6].1,
            frozen,
            "frozen through the recover wake"
        );
        let homing = f32::from_bits(trace[t_emit + 7].1);
        assert!(
            homing < f32::from_bits(frozen),
            "the patrol walks home after the window (x {homing:.3})"
        );
    }

    /// The ATTACK seam through the combat contract: the attack wake
    /// resolves ONE request (adversary eye → the player body's center,
    /// B's placeholder profile) and SURFACES the outcome per-adversary.
    /// (Hit) the milestone chase attacks from x = 3.2 with the eye at
    /// +1.5; the player box (the default body: x 4.65..5.35, y 0..1.7)
    /// is entered at its near face. (Occluded) a LOW wall — under the
    /// eye-to-eye perception line (y ≈ 1.5..1.6 passes above its 1.2
    /// top) but across the descending attack trace (the aim drops
    /// toward the body center at y 0.85) — occludes before the box;
    /// the pursuit itself passes under it (the movement check rides
    /// y 0.5, below the wall's margin-inflated floor at 0.6). Both
    /// double-run identical.
    #[test]
    fn attack_seam_resolves_through_the_combat_contract() {
        let mut config = scenario_config(1);
        config.waypoint_radius = 0.0;
        let spawn = circuit_waypoint(&config, 0, 0);
        let player = [spawn[0] + 5.0, spawn[1], spawn[2]];

        // (Hit) the empty-world chase.
        let (spine, trace) = run_scenario(config.clone(), CollisionWorld::new(0.0), player, 20);
        let adversary = spine.world().adversary(0);
        assert_eq!(adversary.attacks_emitted, 1, "one fire in 20 ticks");
        match adversary.last_attack {
            Some(HitResolution::Hit {
                target,
                distance,
                point,
            }) => {
                assert_eq!(target, 1, "the surfaced player target id");
                assert!(
                    (1.52..1.56).contains(&distance),
                    "the box near-face distance from the eye (got {distance})"
                );
                assert!(
                    (4.64..4.66).contains(&point[0]) && (0.96..0.99).contains(&point[1]),
                    "the impact at the box's near face (got {point:?})"
                );
            }
            other => panic!("expected a surfaced Hit, got {other:?}"),
        }
        let (spine2, trace2) = run_scenario(config.clone(), CollisionWorld::new(0.0), player, 20);
        assert_eq!(trace, trace2, "the chase double-runs identical");
        assert_eq!(
            spine.world().adversary(0).last_attack,
            spine2.world().adversary(0).last_attack,
            "the surfaced resolution double-runs identical"
        );

        // (Occluded) the low wall across the descending trace.
        let walled = CollisionWorld::new(0.0).with_collider(StaticCollider {
            min: [spawn[0] + 4.2, 1.0, spawn[2] - 2.0],
            max: [spawn[0] + 4.6, 1.2, spawn[2] + 2.0],
        });
        let (spine, trace) = run_scenario(config.clone(), walled.clone(), player, 20);
        let adversary = spine.world().adversary(0);
        assert_eq!(adversary.attacks_emitted, 1, "the same chase and fire");
        match adversary.last_attack {
            Some(HitResolution::Miss {
                kind: MissKind::Occluded { distance, point },
            }) => {
                assert!(
                    (1.0..1.12).contains(&distance),
                    "the low wall's entry before the box face (got {distance})"
                );
                assert!(
                    (4.15..4.25).contains(&point[0]),
                    "the wall's near face (got {})",
                    point[0]
                );
            }
            other => panic!("expected a surfaced Occluded miss, got {other:?}"),
        }
        let (spine2, trace2) = run_scenario(config, walled, player, 20);
        assert_eq!(trace, trace2, "the occluded chase double-runs identical");
        assert_eq!(
            spine.world().adversary(0).last_attack,
            spine2.world().adversary(0).last_attack,
            "the surfaced occlusion double-runs identical"
        );
    }

    /// Review F1 (the snap-branch teleport), at the DEFAULT cadences:
    /// the aggro wake step is 0.8 m (speed 4.0 × 0.1 s × cadence 2) —
    /// larger than the 0.4 m body margin — so the `<= step` snap
    /// branch could displace an adversary through blocking geometry.
    /// The knee wall (y 0..1.0 at x 0.3..0.7) blocks the ground
    /// movement line (y 0.5 inside its margin-inflated span) while
    /// the eye-to-eye perception line (y ≈ 1.5..1.6) passes clear
    /// above it: the adversary aggros and attacks across the wall,
    /// and its post-attack pursuit — the remembered player position
    /// 0.6 m away, inside one 0.8 m step — must be REFUSED in place,
    /// never snapped through the wall. The no-wall control proves the
    /// unblocked snap still lands bit-exactly.
    #[test]
    fn snap_branch_respects_colliders_at_default_cadences() {
        let config = AdversaryConfig {
            waypoint_radius: 0.0,
            ..AdversaryConfig::default()
        };
        let spawn = circuit_waypoint(&config, 0, 0);
        let player = [spawn[0] + 0.6, spawn[1], spawn[2]];

        // The walled run: the pursuit never leaves the spawn spot.
        let knee_wall = StaticCollider {
            min: [spawn[0] + 0.3, 0.0, spawn[2] - 2.0],
            max: [spawn[0] + 0.7, 1.0, spawn[2] + 2.0],
        };
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(
                config.clone(),
                CollisionWorld::new(0.0).with_collider(knee_wall),
            ),
            200,
            60,
        );
        install_adversary_schedules(&mut spine);
        for _ in 0..40 {
            spine.world_mut().set_player_position(player);
            spine.run_tick();
        }
        let adversary = spine.world().adversary(0);
        assert_eq!(
            adversary.position, spawn,
            "the snap must not teleport through the knee wall"
        );
        assert!(
            adversary.attacks_emitted >= 1,
            "the adversary still fights across the wall — the refusal is \
             movement discipline, not a dead machine"
        );
        assert!(
            matches!(
                adversary.state,
                AdversaryState::Aggro | AdversaryState::Attack | AdversaryState::Recover
            ),
            "the encounter stays engaged ({:?})",
            adversary.state
        );

        // The no-wall control: the same pursuit snaps onto the player
        // position bit-exactly (the unblocked snap is legitimate).
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(config, CollisionWorld::new(0.0)),
            200,
            60,
        );
        install_adversary_schedules(&mut spine);
        for _ in 0..40 {
            spine.world_mut().set_player_position(player);
            spine.run_tick();
        }
        assert_eq!(
            spine.world().adversary(0).position,
            player,
            "the unblocked snap still lands on the target exactly"
        );
    }

    /// C3-R2 (the cooldown time-unit corrective): attack readiness is
    /// enforced in CANONICAL LOGICAL-TICK units. The same configured
    /// `attack_cooldown` (18) under different AGGRO/RECOVER cadence
    /// mixes must yield the same logical-tick readiness boundary: the
    /// GATE wake (the AGGRO wake that passes the eligibility gate —
    /// the emit lands one ATTACK-cadence wake later, so gate = t2 −
    /// aggro_cadence) never fires before t1 + 18 and never later than
    /// one AGGRO-cadence quantum past that deadline. The DEFECTIVE
    /// per-wake decrement made the effective duration a function of
    /// the wake frequency (units silently changing between states:
    /// AGGRO wakes 1/2-tick, RECOVER 1/6-tick at the defaults) — this
    /// regression FAILS at the reviewed candidate 3a31627.
    #[test]
    fn cooldown_is_canonical_logical_ticks_across_cadence_mixes() {
        for (aggro_cadence, recover_cadence) in [(1u64, 6u64), (3, 6), (4, 6)] {
            let mut config = scenario_config(1);
            config.waypoint_radius = 0.0;
            config.aggro_cadence = aggro_cadence;
            config.recover_cadence = recover_cadence;
            let spawn = circuit_waypoint(&config, 0, 0);
            // Within attack range from the spawn — no chase: the
            // timing under test is purely the cooldown cycle.
            let player = [spawn[0] + 2.0, spawn[1], spawn[2]];
            let (_spine, trace) = run_scenario(config, CollisionWorld::new(0.0), player, 120);
            let t1 = trace
                .iter()
                .position(|record| record.3 == 1)
                .expect("the first attack lands") as u64
                + 1;
            let t2 = trace
                .iter()
                .position(|record| record.3 == 2)
                .expect("the second attack lands") as u64
                + 1;
            let gate = t2 - aggro_cadence;
            assert!(
                gate - t1 >= 18,
                "aggro {aggro_cadence}/recover {recover_cadence}: never before \
                 the configured logical-tick cooldown (t1 {t1}, t2 {t2})"
            );
            assert!(
                gate - t1 <= 18 + aggro_cadence,
                "aggro {aggro_cadence}/recover {recover_cadence}: eligible at \
                 the first wake at/after the deadline — the wake quantization \
                 is the only slack (t1 {t1}, t2 {t2})"
            );
        }
    }

    /// C3-R2 edge cases: ZERO cooldown re-arms at the state machine's
    /// floor (the ATTACK→RECOVER→AGGRO cycle — recover 6 + the first
    /// AGGRO wake 1 = gate at t1+7; no cooldown wait at all); a
    /// cooldown ≥ 2^32 does NOT silently truncate into a small
    /// per-wake count (the old `as u32` cast read 2^32+5 as 5 — the
    /// large-value sub-case FAILS at the reviewed candidate); and a
    /// u64::MAX cooldown SATURATES the deadline (a naive add would
    /// wrap past u64::MAX into instant re-eligibility — saturating
    /// prevents exactly that).
    #[test]
    fn cooldown_edge_cases_zero_and_large_values() {
        let mut config = scenario_config(1);
        config.waypoint_radius = 0.0;
        let spawn = circuit_waypoint(&config, 0, 0);
        let player = [spawn[0] + 2.0, spawn[1], spawn[2]];
        let emit_tick = |trace: &[TraceRecord], ordinal: u64| {
            trace
                .iter()
                .position(|record| record.3 == ordinal)
                .expect("the attack lands") as u64
                + 1
        };

        // (zero) the floor is the recover cycle plus one aggro wake.
        let mut zeroed = config.clone();
        zeroed.attack_cooldown = 0;
        let (_spine, trace) = run_scenario(zeroed, CollisionWorld::new(0.0), player, 60);
        let t1 = emit_tick(&trace, 1);
        let t2 = emit_tick(&trace, 2);
        assert_eq!(
            t2 - 1 - t1,
            7,
            "zero cooldown: the gate re-arms at the state machine's floor"
        );

        // (large, no truncation) 2^32 + 5 must NOT read as 5.
        let mut large = config.clone();
        large.attack_cooldown = u64::from(u32::MAX) + 5;
        let (_spine, trace) = run_scenario(large, CollisionWorld::new(0.0), player, 200);
        assert_eq!(
            trace.last().expect("the run completed").3,
            1,
            "a cooldown of 2^32 + 5 keeps exactly one attack in 200 ticks — \
             no u32 truncation"
        );

        // (large, saturating) u64::MAX never wraps into re-eligibility.
        let mut saturating = config;
        saturating.attack_cooldown = u64::MAX;
        let (_spine, trace) = run_scenario(saturating, CollisionWorld::new(0.0), player, 200);
        assert_eq!(
            trace.last().expect("the run completed").3,
            1,
            "the saturated deadline never re-arms"
        );
    }

    /// The chain codec and world guards: payloads round-trip across the
    /// full index/cursor range; any tag ≠ `ADVERSARY_MAGIC` decodes to
    /// `None`; `execute` ignores foreign payloads and out-of-range
    /// indices (no panic, no state change); `activation_spec` refuses a
    /// non-dormant adversary; `apply_damage` refuses the defeated and
    /// the out-of-range.
    #[test]
    fn codec_round_trip_and_world_guards() {
        for index in [0usize, 1, 65535] {
            for cursor in [0u64, 1, 0xFFFF_FFFF] {
                let payload = adversary_payload(index, cursor);
                assert_eq!(adversary_parts(payload), Some((index, cursor)));
            }
        }
        assert_eq!(adversary_parts(0), None, "the zero payload is foreign");
        assert_eq!(adversary_parts(u64::MAX), None);
        assert_eq!(
            adversary_parts((ADVERSARY_MAGIC + 1) << 48),
            None,
            "a neighboring tag is foreign"
        );
        assert_eq!(
            adversary_parts(crate::combat::COMBAT_MAGIC << 48),
            None,
            "combat event payloads never decode as adversary chains (the \
             contract §8 namespace table — the 0xB00B/0xC0AB reconciliation)"
        );

        // execute ignores out-of-range indices and foreign tags.
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(scenario_config(1), CollisionWorld::new(0.0)),
            200,
            60,
        );
        for (class, payload) in [
            (999u16, adversary_payload(999, 7)),
            (998, (ADVERSARY_MAGIC + 1) << 48),
            (997, 0xBAD_F00D_0000_0000),
        ] {
            spine
                .schedule(
                    EventSpec::new(
                        EventClass::new(class),
                        Schedule::AtLogicalTime { due: Tick(1) },
                        payload,
                    )
                    .in_field(Field::Far),
                )
                .expect("the guard schedule is accepted");
        }
        spine.run_tick();
        assert_eq!(
            spine.world().adversary(0).cursor,
            0,
            "ignored payloads advance nothing"
        );
        assert_eq!(spine.world().adversary(0).state, AdversaryState::Patrol);

        // activation_spec refuses a non-dormant adversary.
        let world = AdversaryWorld::new(scenario_config(1), CollisionWorld::new(0.0));
        assert!(
            world.activation_spec(0, Tick(5)).is_none(),
            "a patrolling adversary has no activation spec"
        );

        // apply_player_hit: the Health refusals pass through with the
        // value bit-identical; the defeated and the out-of-range are
        // refused; a DORMANT adversary takes damage without provoking
        // (activation is F's scripting authority).
        let mut world = AdversaryWorld::new(scenario_config(2), CollisionWorld::new(0.0));
        assert_eq!(
            world.apply_player_hit(999, 50.0),
            None,
            "out-of-range damage is refused"
        );
        assert_eq!(
            world.apply_player_hit(0, f32::NAN),
            Some(Err(HealthError::NonFiniteAmount))
        );
        assert_eq!(
            world.apply_player_hit(0, -5.0),
            Some(Err(HealthError::NegativeAmount))
        );
        assert_eq!(
            world.adversary(0).health.current,
            100.0,
            "refusals leave the value bit-identical"
        );
        world.adversaries[0].state = AdversaryState::Dormant;
        assert_eq!(
            world.apply_player_hit(0, 10.0),
            Some(Ok(DamageOutcome::Wounded {
                remaining: 90.0,
                dealt: 10.0
            })),
            "a dormant adversary takes the damage"
        );
        assert_eq!(
            world.adversary(0).state,
            AdversaryState::Dormant,
            "without provoking"
        );
        let outcome = world
            .apply_player_hit(0, 90.0)
            .expect("the adversary exists");
        assert_eq!(outcome, Ok(DamageOutcome::Defeated { dealt: 90.0 }));
        assert_eq!(world.adversary(0).state, AdversaryState::Defeated);
        assert_eq!(
            world.apply_player_hit(0, 50.0),
            None,
            "the defeated refuse further damage"
        );
        assert_eq!(world.adversary(1).state, AdversaryState::Patrol);
    }

    /// The namespaced world fingerprint: double-run identical, and
    /// sensitive to the seeded spawn spread and to logical progress
    /// (an unexpected change in any pinned fingerprint is an
    /// investigation, never a repin — this proves the adversary domain
    /// moves when state moves).
    #[test]
    fn world_fingerprint_is_deterministic_and_state_sensitive() {
        let config = scenario_config(1);
        let spawn = circuit_waypoint(&config, 0, 0);
        let player = [spawn[0] + 5.0, spawn[1], spawn[2]];
        let fingerprint_of = |config: AdversaryConfig, ticks: u64| {
            let (spine, _trace) = run_scenario(config, CollisionWorld::new(0.0), player, ticks);
            let mut fp = Fingerprinter::new();
            spine.world().fingerprint(&mut fp);
            fp.finish()
        };
        let first = fingerprint_of(scenario_config(1), 20);
        assert_eq!(
            first,
            fingerprint_of(scenario_config(1), 20),
            "the world fingerprint double-runs identical"
        );
        let mut reseeded = scenario_config(1);
        reseeded.seed = 0xDEAD;
        assert_ne!(
            first,
            fingerprint_of(reseeded, 20),
            "the seeded spawn spread is in the domain"
        );
        assert_ne!(
            first,
            fingerprint_of(scenario_config(1), 21),
            "one more wake of logical progress is in the domain"
        );
    }

    /// Charter §6 scaling evidence — pod-run only (`cargo test --release
    /// --lib adversary -- --ignored --nocapture`), archived to
    /// `cycle3/findings/B-adversary-scale.md`; NOT a CI threshold.
    /// Counts {8, 64, 512} × mixes (patrol-only, a seeded mid mix,
    /// all-aggro) × 600 ticks × double-run under the standing 200/60
    /// budget, plus one uncapped-budget envelope cell at 512: exact
    /// per-window event work, the events/tick proportionality context,
    /// public-boundary wall time, the state census, and the world
    /// digest. All parameters placeholders.
    #[test]
    #[ignore]
    fn adversarial_scale_measurements() {
        struct Cell {
            count: usize,
            name: &'static str,
            aggro_radius: f32,
            player: [f32; 3],
            budget: u32,
            near: u32,
        }
        /// The mid mix holds a STABLE partial activity level: the player
        /// stands just OUTSIDE the circuit ring (19 m from the center,
        /// ring radius 12, aggro radius 9), so only the adversaries
        /// whose near waypoint sits within 9 m of the player ever
        /// perceive (the |phase| ≤ 21.6° window — ≈12%, the seeded
        /// phases decide exactly which); the rest patrol a geometry
        /// that never comes within 9 m of the player.
        fn mid_player() -> [f32; 3] {
            [19.0, 0.0, 0.0]
        }
        let cells = [
            Cell {
                count: 8,
                name: "patrol",
                aggro_radius: 8.0,
                player: [5000.0, 0.0, 5000.0],
                budget: 200,
                near: 60,
            },
            Cell {
                count: 8,
                name: "mid",
                aggro_radius: 9.0,
                player: mid_player(),
                budget: 200,
                near: 60,
            },
            Cell {
                count: 8,
                name: "all-aggro",
                aggro_radius: 20.0,
                player: [0.0, 0.0, 0.0],
                budget: 200,
                near: 60,
            },
            Cell {
                count: 64,
                name: "patrol",
                aggro_radius: 8.0,
                player: [5000.0, 0.0, 5000.0],
                budget: 200,
                near: 60,
            },
            Cell {
                count: 64,
                name: "mid",
                aggro_radius: 9.0,
                player: mid_player(),
                budget: 200,
                near: 60,
            },
            Cell {
                count: 64,
                name: "all-aggro",
                aggro_radius: 20.0,
                player: [0.0, 0.0, 0.0],
                budget: 200,
                near: 60,
            },
            Cell {
                count: 512,
                name: "patrol",
                aggro_radius: 8.0,
                player: [5000.0, 0.0, 5000.0],
                budget: 200,
                near: 60,
            },
            Cell {
                count: 512,
                name: "mid",
                aggro_radius: 9.0,
                player: mid_player(),
                budget: 200,
                near: 60,
            },
            Cell {
                count: 512,
                name: "all-aggro",
                aggro_radius: 20.0,
                player: [0.0, 0.0, 0.0],
                budget: 200,
                near: 60,
            },
            Cell {
                count: 512,
                name: "all-aggro/envelope",
                aggro_radius: 20.0,
                player: [0.0, 0.0, 0.0],
                budget: 2048,
                near: 1024,
            },
        ];
        for cell in cells {
            let run = || {
                let config = AdversaryConfig {
                    count: cell.count,
                    aggro_radius: cell.aggro_radius,
                    ..AdversaryConfig::default()
                };
                let mut spine = Spine::with_reservation(
                    AdversaryWorld::new(config, CollisionWorld::new(0.0)),
                    cell.budget,
                    cell.near,
                );
                install_adversary_schedules(&mut spine);
                let mut windows: Vec<u64> = Vec::new();
                let mut last = 0;
                let started = std::time::Instant::now();
                for tick in 1..=600u64 {
                    spine.world_mut().set_player_position(cell.player);
                    spine.run_tick();
                    if tick % 120 == 0 {
                        let executed = spine.stats().executed;
                        windows.push(executed - last);
                        last = executed;
                    }
                }
                let elapsed = started.elapsed();
                let stats = spine.stats();
                let mut census = [0usize; 6];
                let world = spine.world();
                for index in 0..world.count() {
                    census[world.adversary(index).state.code() as usize] += 1;
                }
                let mut fp = Fingerprinter::new();
                world.fingerprint(&mut fp);
                (spine, windows, elapsed, stats, census, fp.finish())
            };
            let (spine, windows, elapsed, stats, census, digest) = run();
            let (_spine2, windows2, _elapsed2, stats2, census2, digest2) = run();
            assert_eq!(
                windows, windows2,
                "per-window event work double-runs identical"
            );
            assert_eq!(stats.executed, stats2.executed);
            assert_eq!(census, census2);
            assert_eq!(digest, digest2, "the world digest double-runs identical");

            if cell.name == "patrol" {
                assert_eq!(
                    stats.executed,
                    (cell.count as u64) * 50,
                    "the patrol cell executes exactly count × 50 wake-ups"
                );
                assert_eq!(
                    stats.budget_exhausted_ticks, 0,
                    "patrol never pressures the budget"
                );
            }
            if cell.count == 512 && cell.name == "all-aggro" {
                assert!(
                    stats.budget_exhausted_ticks > 400,
                    "the standing budget bounds the 512 all-aggro cell \
                     (exhausted {})",
                    stats.budget_exhausted_ticks
                );
                assert!(stats.executed <= 200 * 600);
            }

            let per_tick_us = elapsed.as_micros() as f64 / 600.0;
            let per_event_ns = if stats.executed > 0 {
                elapsed.as_nanos() as f64 / stats.executed as f64
            } else {
                0.0
            };
            eprintln!(
                "[scale {}/{} budget {}/{}] windows={:?} executed={} exhausted={} \
                 near/far pending={}/{} census={:?} digest={:016x} \
                 wall={:.1}us/tick {:.0}ns/event {:.3}us/tick/adversary",
                cell.count,
                cell.name,
                cell.budget,
                cell.near,
                windows,
                stats.executed,
                stats.budget_exhausted_ticks,
                spine.near_pending_count(),
                spine.far_pending_count(),
                census,
                digest,
                per_tick_us,
                per_event_ns,
                per_tick_us / cell.count as f64,
            );
        }
    }

    // ============================================ the Cycle-4 combatants
    // contract regressions (the H31-1 reconciliation + the
    // multi-adversary guarantees; fail-first documented in the
    // delivery report — the H31-1 tests fail against the
    // pre-reconciliation tree).

    /// A spine over the config's adversaries, all DORMANT (the legal
    /// pre-scheduling composition).
    fn dormant_spine(config: AdversaryConfig) -> Spine<AdversaryWorld> {
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(config, CollisionWorld::new(0.0)),
            200,
            60,
        );
        for index in 0..spine.world().count() {
            assert!(
                spine.world_mut().set_dormant(index),
                "pre-scheduling dormancy always applies"
            );
        }
        spine
    }

    /// H31-1 (the recorded Cycle-3.1 follow-up, the reconciliation's
    /// core regression): a ROUTINE payload scheduled against a
    /// DORMANT adversary — exactly what the old unconditional
    /// installer did — must be INERT. The adversary stays DORMANT
    /// (no unauthorized activation, no early attacks, no chain), the
    /// cursor honestly advances by exactly the drained events, and
    /// nothing follows. Against the pre-reconciliation tree this
    /// test FAILS (the old wake() transitioned any dormant wake into
    /// PATROL — the Cycle-3.1 pre-activation defect's root cause).
    #[test]
    fn h31_1_routine_payload_cannot_activate_a_dormant_adversary() {
        let config = scenario_config(1);
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(config, CollisionWorld::new(0.0)),
            200,
            60,
        );
        assert!(spine.world_mut().set_dormant(0));
        // The defective composition, reproduced verbatim: the OLD
        // installer's unconditional schedule — a ROUTINE payload
        // (cursor 0) against the dormant adversary.
        spine
            .schedule(
                EventSpec::new(
                    EventClass::new(0),
                    Schedule::AtLogicalTime { due: Tick(1) },
                    adversary_payload(0, 0),
                )
                .in_field(Field::Far),
            )
            .expect("the hand-scheduled stray event is dependency-free");
        let player = [5.0, 0.0, 0.0]; // Well within aggro range: an
        // activated adversary WOULD perceive and pursue.
        for _ in 0..20 {
            spine.world_mut().set_player_position(player);
            spine.run_tick();
        }
        let adversary = spine.world().adversary(0);
        assert_eq!(
            adversary.state,
            AdversaryState::Dormant,
            "the stray routine schedule never activates a dormant adversary"
        );
        assert_eq!(
            adversary.attacks_emitted, 0,
            "no unauthorized early attacks"
        );
        assert!(adversary.last_attack.is_none());
        assert_eq!(
            adversary.cursor, 1,
            "the drained event advanced the cursor honestly — and no \
             follow-up chain formed (exactly one execution)"
        );
        assert!(
            adversary.perceived.is_none(),
            "a dormant adversary never perceives"
        );
    }

    /// The installer schedules ONLY eligible adversaries in a mixed
    /// dormant/active world: the active ones wake (cursor advances,
    /// the scheduled mirror set), the dormant ones receive NOTHING
    /// (cursor stays zero — no event ever executed for them).
    #[test]
    fn installer_schedules_only_eligible_adversaries() {
        let config = scenario_config(4);
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(config, CollisionWorld::new(0.0)),
            200,
            60,
        );
        // A mixed composition: 1 and 3 dormant, 0 and 2 active.
        assert!(spine.world_mut().set_dormant(1));
        assert!(spine.world_mut().set_dormant(3));
        install_adversary_schedules(&mut spine);
        let player = [5.0, 0.0, 0.0];
        for _ in 0..10 {
            spine.world_mut().set_player_position(player);
            spine.run_tick();
        }
        for index in [0usize, 2] {
            let adversary = spine.world().adversary(index);
            assert_ne!(
                adversary.state,
                AdversaryState::Dormant,
                "the active adversary {index} was scheduled and woke"
            );
            assert!(adversary.cursor > 0, "its chain executed");
            assert!(adversary.scheduled, "the mirror was set at install");
        }
        for index in [1usize, 3] {
            let adversary = spine.world().adversary(index);
            assert_eq!(
                adversary.state,
                AdversaryState::Dormant,
                "the dormant adversary {index} stays dormant"
            );
            assert_eq!(
                adversary.cursor, 0,
                "NOTHING was scheduled for the dormant adversary {index} \
                 (the reconciled installer — the old form scheduled it \
                 unconditionally)"
            );
            assert!(!adversary.scheduled);
        }
    }

    /// The installer is idempotent: a second install adds nothing —
    /// the double-installed world's trace and fingerprint are
    /// IDENTICAL to the single-installed control (a fork would
    /// double the wake rate and diverge both).
    #[test]
    fn installer_is_idempotent_double_install_forks_nothing() {
        let run = |installs: usize| {
            let config = scenario_config(3);
            let mut spine = Spine::with_reservation(
                AdversaryWorld::new(config, CollisionWorld::new(0.0)),
                200,
                60,
            );
            for _ in 0..installs {
                install_adversary_schedules(&mut spine);
            }
            let player = [5.0, 0.0, 0.0];
            let mut trace: Vec<TraceRecord> = Vec::new();
            for _ in 0..40 {
                spine.world_mut().set_player_position(player);
                spine.run_tick();
                for index in 0..3 {
                    let adversary = spine.world().adversary(index);
                    trace.push((
                        adversary.state.code(),
                        adversary.position[0].to_bits(),
                        adversary.position[2].to_bits(),
                        adversary.attacks_emitted,
                    ));
                }
            }
            let mut fp = Fingerprinter::new();
            spine.world().fingerprint(&mut fp);
            (trace, fp.finish())
        };
        let (single_trace, single_fp) = run(1);
        let (double_trace, double_fp) = run(2);
        assert_eq!(single_trace, double_trace, "no fork: identical traces");
        assert_eq!(single_fp, double_fp, "no fork: identical fingerprints");
    }

    /// The dormancy rule: `set_dormant` applies BEFORE any scheduling
    /// and REFUSES once the chain is (or may be) live — the state is
    /// unchanged on refusal, never silently leaving live events
    /// attached. Out-of-range indices also refuse (false).
    #[test]
    fn set_dormant_refuses_a_live_chain() {
        let config = scenario_config(2);
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(config, CollisionWorld::new(0.0)),
            200,
            60,
        );
        assert!(spine.world_mut().set_dormant(0), "pre-scheduling applies");
        assert_eq!(spine.world().adversary(0).state, AdversaryState::Dormant);
        install_adversary_schedules(&mut spine);
        spine.run_tick();
        // Adversary 1 was installed — its chain is live.
        assert!(
            !spine.world_mut().set_dormant(1),
            "a live chain refuses dormancy"
        );
        assert_ne!(
            spine.world().adversary(1).state,
            AdversaryState::Dormant,
            "the refusal leaves the state unchanged"
        );
        // Adversary 0 is dormant with NO chain — re-dormancy is the
        // legal no-op (still dormant, still unscheduled).
        assert!(
            spine.world_mut().set_dormant(0),
            "an unscheduled dormant adversary re-dorms as a no-op"
        );
        assert!(!spine.world_mut().set_dormant(9), "out of range refuses");
    }

    /// A duplicated activation cannot fork a chain: scheduling the
    /// activation spec TWICE before the first executes leaves the
    /// behavioral trace IDENTICAL to the single-activation control
    /// (the first wakes the adversary; the duplicate is inert against
    /// its then-active state) with the cursor exactly ONE higher —
    /// the honest mirror of the single drained duplicate. A fork
    /// would double the wake rate: the trace diverges and the cursor
    /// grows without bound.
    #[test]
    fn duplicate_activation_cannot_fork_a_chain() {
        let run = |activations: usize| {
            let config = scenario_config(1);
            let mut spine = Spine::with_reservation(
                AdversaryWorld::new(config, CollisionWorld::new(0.0)),
                200,
                60,
            );
            assert!(spine.world_mut().set_dormant(0));
            for _ in 0..activations {
                let spec = spine
                    .world()
                    .activation_spec(0, Tick(2))
                    .expect("the dormant adversary activates");
                spine.schedule(spec).expect("dependency-free");
            }
            let player = [5.0, 0.0, 0.0];
            let mut trace: Vec<TraceRecord> = Vec::new();
            for _ in 0..30 {
                spine.world_mut().set_player_position(player);
                spine.run_tick();
                let adversary = spine.world().adversary(0);
                trace.push((
                    adversary.state.code(),
                    adversary.position[0].to_bits(),
                    adversary.position[2].to_bits(),
                    adversary.attacks_emitted,
                ));
            }
            (trace, spine.world().adversary(0).cursor)
        };
        let (single_trace, single_cursor) = run(1);
        let (double_trace, double_cursor) = run(2);
        assert_eq!(
            single_trace, double_trace,
            "the duplicate activation is behaviorally inert — identical traces"
        );
        assert_eq!(
            double_cursor,
            single_cursor + 1,
            "exactly one drained duplicate execution — no fork (a fork would \
             double the wake rate)"
        );
    }

    /// Per-adversary INDIVIDUAL activation: activating one dormant
    /// adversary among its dormant peers wakes exactly that one —
    /// the others stay dormant with zero executions.
    #[test]
    fn individual_activation_among_dormant_peers() {
        let config = scenario_config(3);
        let mut spine = dormant_spine(config);
        let spec = spine
            .world()
            .activation_spec(1, Tick(3))
            .expect("adversary 1 is dormant");
        spine.schedule(spec).expect("dependency-free");
        let player = [5.0, 0.0, 0.0];
        for _ in 0..15 {
            spine.world_mut().set_player_position(player);
            spine.run_tick();
        }
        let activated = spine.world().adversary(1);
        assert_ne!(
            activated.state,
            AdversaryState::Dormant,
            "the activated adversary woke"
        );
        assert!(activated.cursor > 0, "its chain carries");
        for index in [0usize, 2] {
            let peer = spine.world().adversary(index);
            assert_eq!(
                peer.state,
                AdversaryState::Dormant,
                "peer {index} was not activated"
            );
            assert_eq!(peer.cursor, 0, "peer {index} executed nothing");
        }
    }

    /// Multiple attacks on ONE logical tick: three adversaries in
    /// the ATTACK state, all woken on the same tick, each resolving
    /// and surfacing its OWN hit resolution — deterministic,
    /// independent, no shared scratch. The player is placed in range
    /// of all three (a clean open world), so every resolution is a
    /// Hit on the player's id with a per-adversary distance.
    #[test]
    fn multiple_adversaries_attack_on_one_logical_tick() {
        let attack_cooldown = scenario_config(3).attack_cooldown;
        let run = || {
            let config = scenario_config(3);
            let mut spine = Spine::with_reservation(
                AdversaryWorld::new(config, CollisionWorld::new(0.0)),
                200,
                60,
            );
            // All three in the ATTACK state, scheduled to wake on
            // the SAME tick (direct state composition — the state
            // machine's arrival paths are covered by the other
            // regressions; this pins the same-tick seam itself).
            for index in 0..3 {
                spine.world_mut().adversaries[index].state = AdversaryState::Attack;
                spine
                    .schedule(
                        EventSpec::new(
                            EventClass::new(index as u16),
                            Schedule::AtLogicalTime { due: Tick(5) },
                            adversary_payload(index, 0),
                        )
                        .in_field(Field::Near),
                    )
                    .expect("dependency-free");
            }
            // The player at the spawn center — within attack range
            // of every seeded circuit spawn? Not necessarily; the
            // resolutions may be Hit or Clear per adversary — what
            // is pinned is that ALL THREE resolve independently on
            // the one tick.
            spine.world_mut().set_player_position([0.0, 0.0, 0.0]);
            for _ in 0..5 {
                spine.run_tick();
            }
            let mut fp = Fingerprinter::new();
            spine.world().fingerprint(&mut fp);
            (
                spine
                    .world()
                    .adversaries
                    .iter()
                    .map(|a| (a.attacks_emitted, a.last_attack.is_some(), a.cursor))
                    .collect::<Vec<_>>(),
                spine
                    .world()
                    .adversaries
                    .iter()
                    .map(|a| a.cooldown_until)
                    .collect::<Vec<_>>(),
                fp.finish(),
            )
        };
        let (emitted, cooldowns, fp) = run();
        let (emitted2, cooldowns2, fp2) = run();
        for (index, (attacks, surfaced, cursor)) in emitted.iter().enumerate() {
            assert_eq!(
                *attacks, 1,
                "adversary {index} attacked exactly once on the shared tick"
            );
            assert!(*surfaced, "adversary {index} surfaced its own resolution");
            assert_eq!(*cursor, 1, "exactly one execution each — no forks");
        }
        assert_eq!(
            cooldowns,
            vec![5 + attack_cooldown; 3],
            "every cooldown deadline anchored at the shared execution tick"
        );
        assert_eq!(emitted, emitted2, "double-run identical");
        assert_eq!(cooldowns, cooldowns2);
        assert_eq!(fp, fp2, "double-run identical fingerprint");
    }

    /// Three adversaries genuinely threatening through the real state
    /// machine: with the player standing near the circuit center,
    /// EVERY adversary perceives, pursues, and reaches the threat
    /// states — multiple simultaneous threats in one encounter (the
    /// GO's capability-A requirement at the simulation level).
    #[test]
    fn three_adversaries_threaten_simultaneously_through_the_state_machine() {
        let config = scenario_config(3);
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(config, CollisionWorld::new(0.0)),
            200,
            60,
        );
        install_adversary_schedules(&mut spine);
        spine.world_mut().set_player_position([0.0, 0.0, 0.0]);
        for _ in 0..120 {
            spine.run_tick();
        }
        let active = spine.world().active_count();
        assert!(
            active >= 2,
            "most adversaries are actively threatening (active={active})"
        );
        let total_attacks: u64 = spine
            .world()
            .adversaries
            .iter()
            .map(|a| a.attacks_emitted)
            .sum();
        assert!(
            total_attacks > 0,
            "the encounter threatens: at least one landed attack window"
        );
    }

    /// The multi-candidate targeting hook: `combatants()` — ids are
    /// `base + index`, boxes are the body model at the live
    /// positions, DORMANT adversaries are included (activation
    /// gating is the encounter's), and DEFEATED bodies are excluded
    /// (terminal integrity).
    #[test]
    fn combatants_slice_ids_boxes_and_terminal_exclusion() {
        let config = scenario_config(3);
        let id_base = config.adversary_id_base;
        let max_health = config.max_health;
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(config, CollisionWorld::new(0.0)),
            200,
            60,
        );
        assert!(spine.world_mut().set_dormant(2));
        // Defeat adversary 1 outright (the damage seam).
        assert!(matches!(
            spine
                .world_mut()
                .apply_player_hit(1, max_health)
                .expect("the adversary exists"),
            Ok(DamageOutcome::Defeated { .. })
        ));
        let combatants = spine.world().combatants();
        assert_eq!(
            combatants.len(),
            2,
            "the defeated body is not hittable; the dormant one is"
        );
        assert_eq!(combatants[0].id, id_base);
        assert_eq!(combatants[1].id, id_base + 2);
        for (index, combatant) in combatants.iter().enumerate() {
            let source_index = if index == 0 { 0 } else { 2 };
            let adversary = spine.world().adversary(source_index);
            assert_eq!(
                combatant.hit_box.min[0],
                adversary.position[0] - ADVERSARY_BODY_HALF[0]
            );
            assert_eq!(
                combatant.hit_box.max[1],
                adversary.position[1] + 2.0 * ADVERSARY_BODY_HALF[1]
            );
            assert_eq!(
                combatant.hit_box.min[2],
                adversary.position[2] - ADVERSARY_BODY_HALF[2]
            );
        }
    }

    /// The victory-condition reads: `defeated_count` and
    /// `all_defeated` (an empty world is NOT "all defeated" — a
    /// vacuous win is not a win).
    #[test]
    fn all_defeated_and_defeated_count_reads() {
        let empty = AdversaryWorld::new(
            AdversaryConfig {
                count: 0,
                ..scenario_config(0)
            },
            CollisionWorld::new(0.0),
        );
        assert_eq!(empty.defeated_count(), 0);
        assert!(!empty.all_defeated(), "an empty encounter is not a win");

        let config = scenario_config(3);
        let max_health = config.max_health;
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(config, CollisionWorld::new(0.0)),
            200,
            60,
        );
        assert!(!spine.world().all_defeated());
        assert_eq!(spine.world().defeated_count(), 0);
        for index in 0..2 {
            assert!(matches!(
                spine
                    .world_mut()
                    .apply_player_hit(index, max_health)
                    .expect("the adversary exists"),
                Ok(DamageOutcome::Defeated { .. })
            ));
        }
        assert_eq!(spine.world().defeated_count(), 2);
        assert!(!spine.world().all_defeated(), "two of three is not a win");
        assert!(matches!(
            spine
                .world_mut()
                .apply_player_hit(2, max_health)
                .expect("the adversary exists"),
            Ok(DamageOutcome::Defeated { .. })
        ));
        assert_eq!(spine.world().defeated_count(), 3);
        assert!(
            spine.world().all_defeated(),
            "the configured victory condition"
        );
    }

    /// The full restoration: after a scenario with aggro, damage,
    /// defeat, and live chains, `reset()` returns EVERY combatant to
    /// the construction state — the reset world's fingerprint is
    /// IDENTICAL to a fresh construction's under the same config
    /// (positions, health, bookkeeping, and the scheduled mirror).
    #[test]
    fn reset_restores_every_combatant_to_construction_state() {
        let config = scenario_config(3);
        let max_health = config.max_health;
        let spawn_positions: Vec<[f32; 3]> = (0..3)
            .map(|index| circuit_waypoint(&config, index, 0))
            .collect();
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(config, CollisionWorld::new(0.0)),
            200,
            60,
        );
        install_adversary_schedules(&mut spine);
        spine.world_mut().set_player_position([0.0, 0.0, 0.0]);
        for _ in 0..60 {
            spine.run_tick();
        }
        // Damage and defeat (varied per adversary).
        let _ = spine.world_mut().apply_player_hit(0, 30.0);
        let _ = spine.world_mut().apply_player_hit(1, max_health);
        let before: Vec<_> = spine
            .world()
            .adversaries
            .iter()
            .map(|a| (a.state, a.health.current, a.cursor))
            .collect();
        assert!(
            before.iter().any(|(_, _, cursor)| *cursor > 0),
            "the scenario genuinely progressed before the reset"
        );
        spine.world_mut().reset();
        for (index, expected_spawn) in spawn_positions.iter().enumerate() {
            let adversary = spine.world().adversary(index);
            assert_eq!(adversary.state, AdversaryState::Patrol);
            assert_eq!(adversary.health.current, max_health);
            assert_eq!(adversary.cursor, 0);
            assert_eq!(adversary.cooldown_until, 0);
            assert!(!adversary.scheduled, "the mirror cleared");
            assert_eq!(
                adversary.position, *expected_spawn,
                "the seeded spawn position restored"
            );
        }
        // The fingerprint-identity proof against a fresh
        // construction (the world only — the spine is NOT rebuilt
        // here, which is the documented pairing rule).
        let mut reset_fp = Fingerprinter::new();
        spine.world().fingerprint(&mut reset_fp);
        let mut fresh_fp = Fingerprinter::new();
        AdversaryWorld::new(scenario_config(3), CollisionWorld::new(0.0))
            .fingerprint(&mut fresh_fp);
        assert_eq!(
            reset_fp.finish(),
            fresh_fp.finish(),
            "the reset world is fingerprint-identical to a fresh construction"
        );
    }
}
