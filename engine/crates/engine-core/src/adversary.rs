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
//! claimed.** The ATTACK/DEFEATED seams are STUBS pending seat A's
//! `COMBAT-INTERFACE.md`: attacks are counted (`attacks_emitted`) but
//! resolve through A's hit-resolution contract once it lands; adversary
//! health/DEFEATED use placeholder fields that A's damage/health
//! transition contract will govern. No spine semantic changes; no
//! persistence-schema changes (encounter state is transient, honestly
//! labeled); the 200/60 budget is owner-gated and untouched.
//!
//! Determinism: transitions are pure functions of logical state, the
//! driver-sampled player position, and seeded jitter (a local SplitMix64
//! finalizer — engine-core cannot reach engine-slice's `npc::mix64`);
//! the world fingerprints under its own namespaced domain; wall-clock
//! time never enters logical state.

use crate::clock::Tick;
use crate::event::{Event, EventClass, EventSpec, Field, Schedule};
use crate::fingerprint::Fingerprinter;
use crate::player::CollisionWorld;
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
    /// ATTACK cooldown in ticks (placeholder 18).
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
    /// Placeholder health until A's damage/health contract governs
    /// (contract item 2).
    pub max_health: u32,
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
            max_health: 100,
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
    /// Ticks of attack cooldown remaining.
    pub cooldown: u32,
    /// Consecutive perception wake-ups with LOS lost (hysteresis).
    pub los_lost: u32,
    /// Placeholder health — A's contract governs (contract item 2).
    pub health: u32,
    /// The LAST-PERCEIVED player position (sampled on wake-up only —
    /// never per-frame).
    pub perceived: Option<[f32; 3]>,
    /// The STUBBED attack seam: hit-resolution requests emitted
    /// (A's contract turns these into damage once it lands).
    pub attacks_emitted: u64,
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
    /// waypoint circuits, PATROL state, full health.
    pub fn new(config: AdversaryConfig, collision: CollisionWorld) -> Self {
        let spawn_center = config.spawn_center;
        let adversaries = (0..config.count)
            .map(|index| {
                let position = circuit_waypoint(&config, index, 0);
                Adversary {
                    state: AdversaryState::Patrol,
                    position,
                    waypoint: 1,
                    cursor: 0,
                    cooldown: 0,
                    los_lost: 0,
                    health: config.max_health,
                    perceived: None,
                    attacks_emitted: 0,
                }
            })
            .collect();
        AdversaryWorld {
            config,
            collision,
            adversaries,
            player_position: spawn_center,
        }
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

    /// The STUBBED incoming-damage seam (contract item 2): placeholder
    /// health decrement + provocation (damage received forces AGGRO).
    /// A's damage/health transition contract will govern the real
    /// semantics; this seam keeps the behavior machine testable now.
    pub fn apply_damage(&mut self, index: usize, amount: u32) {
        let Some(adversary) = self.adversaries.get_mut(index) else {
            return;
        };
        if adversary.state == AdversaryState::Defeated {
            return;
        }
        adversary.health = adversary.health.saturating_sub(amount);
        if adversary.health == 0 {
            adversary.state = AdversaryState::Defeated;
            adversary.perceived = None;
        } else if adversary.state != AdversaryState::Dormant {
            // Provocation: damage received forces AGGRO.
            adversary.state = AdversaryState::Aggro;
            adversary.los_lost = 0;
        }
    }

    /// F's activation schedule for a DORMANT adversary (the encounter
    /// scripting entry): the first wake-up spec. Returns `None` when
    /// the adversary is not dormant.
    pub fn activation_spec(&self, index: usize, due: Tick) -> Option<EventSpec> {
        let adversary = self.adversaries.get(index)?;
        if adversary.state != AdversaryState::Dormant {
            return None;
        }
        Some(
            EventSpec::new(
                EventClass::new(index as u16),
                Schedule::AtLogicalTime { due },
                adversary_payload(index, 0),
            )
            .in_field(Field::Far),
        )
    }

    /// The distance on the ground plane (the perception metric).
    fn ground_distance(a: [f32; 3], b: [f32; 3]) -> f32 {
        let dx = a[0] - b[0];
        let dz = a[2] - b[2];
        (dx * dx + dz * dz).sqrt()
    }

    /// One adversary's perception at a wake-up: distance gate, then a
    /// LOS query through the collision world (`segment_hit` from the
    /// adversary's eye height to the player). Hysteresis bookkeeping
    /// (`los_lost`) is the caller's — this returns the raw perception.
    fn perceives(&self, index: usize) -> bool {
        let adversary = &self.adversaries[index];
        let player = self.player_position;
        if Self::ground_distance(adversary.position, player) > self.config.aggro_radius {
            return false;
        }
        let eye = [
            adversary.position[0],
            adversary.position[1] + 1.5,
            adversary.position[2],
        ];
        let target = [player[0], player[1] + 1.5, player[2]];
        self.collision.segment_hit(eye, target, 0.05).is_none()
    }

    /// Move `adversary` one wake-up step toward `target` on the ground
    /// plane, through the collision world: the straight step is
    /// LOS-checked (`segment_hit` with the body margin) and a blocked
    /// step is REFUSED in place (the patrol circuit routes around; the
    /// pursuit stops at cover — the collide-what-you-stop-at minimal
    /// semantics; full sweep movement is a later refinement, stated).
    fn move_toward(&mut self, index: usize, target: [f32; 3], cadence: u64) {
        let adversary = &mut self.adversaries[index];
        let step = self.config.speed * self.config.tick_seconds * cadence as f32;
        let dx = target[0] - adversary.position[0];
        let dz = target[2] - adversary.position[2];
        let length = (dx * dx + dz * dz).sqrt();
        if length <= f32::EPSILON || length <= step {
            adversary.position = [target[0], adversary.position[1], target[2]];
            return;
        }
        let next = [
            adversary.position[0] + dx / length * step,
            adversary.position[1],
            adversary.position[2] + dz / length * step,
        ];
        let blocked = self
            .collision
            .segment_hit(
                [
                    adversary.position[0],
                    adversary.position[1] + 0.5,
                    adversary.position[2],
                ],
                [next[0], next[1] + 0.5, next[2]],
                0.4,
            )
            .is_some();
        if !blocked {
            adversary.position = next;
        }
    }

    /// One adversary's wake-up: the state machine step (a pure
    /// function of logical state, the player sample, and the seeded
    /// circuit — deterministic, headless-reproducible).
    fn wake(&mut self, index: usize) {
        let cadence = self.adversaries[index].state.cadence(&self.config);
        if cadence == 0 {
            return;
        }
        let mut transition = None;
        match self.adversaries[index].state {
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
                    && self.adversaries[index].cooldown == 0
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
                // The STUBBED attack seam: one hit-resolution request
                // per window (counted; A's contract resolves). The
                // cooldown gate lives here.
                self.adversaries[index].attacks_emitted += 1;
                self.adversaries[index].cooldown = self.config.attack_cooldown as u32;
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
            AdversaryState::Dormant | AdversaryState::Defeated => {}
        }
        if let Some(next) = transition {
            self.adversaries[index].state = next;
        }
        if self.adversaries[index].cooldown > 0 {
            self.adversaries[index].cooldown -= 1;
        }
    }
}

impl World for AdversaryWorld {
    fn execute(&mut self, event: &Event, ctx: &mut ExecCtx) {
        let Some((index, _cursor)) = adversary_parts(event.payload) else {
            return;
        };
        if index >= self.adversaries.len() {
            return;
        }
        self.adversaries[index].cursor += 1;
        self.wake(index);
        let state = self.adversaries[index].state;
        let cadence = state.cadence(&self.config);
        if cadence > 0 {
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
            fp.write_u32(adversary.cooldown);
            fp.write_u32(adversary.los_lost);
            fp.write_u32(adversary.health);
            fp.write_u64(adversary.attacks_emitted);
            fp.write_u8(u8::from(adversary.perceived.is_some()));
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

/// Installs one routine chain per NON-DORMANT adversary — the seeded
/// spread over the first patrol cadence, in the state's lane class.
/// DORMANT adversaries are NOT scheduled (F activates them via
/// `AdversaryWorld::activation_spec`); the installer therefore
/// schedules exactly the adversaries that exist in an active state.
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
        let spread = patrol_cadence.max(1);
        let offset = mix64(seed ^ ((index as u64) << 32) ^ 0x5EED_0000) % spread;
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
