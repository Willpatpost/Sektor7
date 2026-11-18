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
    /// circuit — deterministic, headless-reproducible). DEFEATED is
    /// terminal (no transition, and `execute` schedules no follow-up —
    /// the chain stops); a DORMANT adversary's activation wake-up (the
    /// spec F schedules) TRANSITIONS it to PATROL, whose cadence then
    /// carries the chain onward.
    fn wake(&mut self, index: usize) {
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
            AdversaryState::Defeated => {}
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
    /// RECOVERS 6 ticks, re-agpros, and the 18-tick cooldown gates
    /// attack #2 to exactly tick 36. The whole trace double-runs
    /// identical.
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
        assert_eq!(adversary.attacks_emitted, 3, "attacks at ticks 11, 36, 61");
        // The exact milestone ticks (fail-first calibrated).
        assert_eq!(trace[10].3, 1, "attack #1 lands at tick 11 (index 10)");
        assert_eq!(
            trace[28].3, 1,
            "still one attack before the cooldown clears"
        );
        assert_eq!(trace[35].3, 2, "attack #2 lands at tick 36 (index 35)");
        assert_eq!(trace[60].3, 3, "attack #3 lands at tick 61 (index 60)");
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

        // Provocation forces AGGRO regardless of distance.
        let mut spine = Spine::with_reservation(
            AdversaryWorld::new(config.clone(), CollisionWorld::new(0.0)),
            200,
            60,
        );
        install_adversary_schedules(&mut spine);
        spine.world_mut().apply_damage(0, 10);
        assert_eq!(spine.world().adversary(0).state, AdversaryState::Aggro);
        assert_eq!(spine.world().adversary(0).health, 90);

        // (LOS-loss) the player at 9 m behind a wall: distance < deaggro
        // so only the LOS-loss window can fire — PATROL exactly at the
        // 25th wake (los_loss_ticks 24, aggro cadence 1).
        let mut walled = CollisionWorld::new(0.0);
        walled = walled.with_box([spawn[0] + 4.5, 0.0, spawn[2]], 1.0, 2.0);
        let mut spine =
            Spine::with_reservation(AdversaryWorld::new(config.clone(), walled), 200, 60);
        install_adversary_schedules(&mut spine);
        spine.world_mut().apply_damage(0, 10);
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
        spine.world_mut().apply_damage(0, 10);
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
        spine.world_mut().apply_damage(0, 100);
        assert_eq!(spine.world().adversary(0).state, AdversaryState::Defeated);
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

        // apply_damage refuses the defeated and the out-of-range.
        let mut world = AdversaryWorld::new(scenario_config(2), CollisionWorld::new(0.0));
        world.apply_damage(0, 100);
        assert_eq!(world.adversary(0).state, AdversaryState::Defeated);
        world.apply_damage(0, 50);
        assert_eq!(
            world.adversary(0).health,
            0,
            "the defeated refuse further damage"
        );
        world.apply_damage(999, 50);
        assert_eq!(
            world.adversary(1).state,
            AdversaryState::Patrol,
            "out-of-range damage is ignored"
        );
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
}
