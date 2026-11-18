//! Cycle 3 P1a — the reusable combat primitives and deterministic
//! resolution contracts.
//!
//! Engine MECHANICS only, mirroring the module precedents (`player.rs`,
//! `interaction.rs`): validated hit resolution (an instant, purely
//! geometric hitscan trace through the existing exact-slab segment
//! query), saturating damage/health transitions whose `Defeated`
//! outcome is the engine's ONLY defeat declaration, the combat event
//! vocabulary the spine can ledger, and their input-validity and
//! determinism rules. GAME POLICY — weapon statistics, rate of fire,
//! adversary temperament, encounter scripting, feedback — lives in
//! the application layer, which supplies every policy value
//! (`AttackProfile`, damage amounts, target slices). The published
//! contract is `cycle3/interfaces/COMBAT-INTERFACE.md` (v1); this
//! module is its compilable form.
//!
//! Determinism discipline (the spine's rules, mirrored): no
//! wall-clock time ever enters logical state — `combat.rs` reads no
//! clock at all; every public function is pure over its explicit
//! inputs (repeated calls with equal inputs give bitwise-equal
//! results; reordered target slices give the same selection). The
//! float discipline is layered honestly: the transition path
//! (`apply_damage`/`apply_heal`) uses add/sub/compare only, and the
//! resolution path additionally mul/div/sqrt — all IEEE
//! exactly-rounded operations — so both are cross-platform
//! BIT-STABLE for identical inputs; the AIM direction is supplied by
//! the caller (typically `player::look_direction`, whose own
//! nonzero-angle discipline is documented there and is NOT claimed
//! here). Combat world state (in tests) fingerprints under the NEW
//! domain `engine-core/combat/1` — no historical fingerprint domain
//! is touched.
//!
//! Input validity (the R4/R4b fail-visible discipline extended): a
//! non-finite or semantically invalid request/profile is REFUSED
//! with a typed reason before any geometry or state mutation; a
//! non-finite or inverted target box is SKIPPED — never an
//! unpredictable comparison and never displacing a valid candidate;
//! a refused health transition leaves the value bit-identical.
//!
//! Proportionality (charter §6, structural): the engine keeps NO
//! registry, NO target list, NO turn scheduler and NO per-frame or
//! per-tick hooks — resolution work happens exactly when a driver
//! resolves an attack, and an idle encounter costs the engine
//! nothing.

use crate::event::EventClass;
use crate::player::{CollisionWorld, StaticCollider};

/// The combat event payload namespace's type tag (the top 16 bits of
/// a combat event's spine payload). Distinct from every other live
/// namespace: the legacy NPC layout (top bits zero for npc < 2^16),
/// the npc routine chains (`0x5E47`), the sustained-load routine and
/// demand overlays (`0x5F12` / `0x0F12`), the soak bursts (`0x5336`),
/// and the adversary chains (`0xB00B`, SEAT B's reservation — see
/// the contract's §8 tag table).
pub const COMBAT_MAGIC: u64 = 0xC0AB;

/// The RECOMMENDED spine class for combat occurrences
/// (`EventClass::new(0x0C0A)` — above every existing demo/test class
/// id). A recommendation, not a requirement: the payload magic is
/// the interop guarantee; classes organize spine queue policy and
/// remain the driver's choice.
pub const COMBAT_CLASS: EventClass = EventClass::new(0x0C0A);

/// The combat domain's fingerprint tag — namespaced and versioned;
/// no historical domain is touched by this module.
pub const COMBAT_FINGERPRINT_DOMAIN: &str = "engine-core/combat/1";

/// Stable combatant identity. The id SPACE is game-owned; the engine
/// only carries it (parity with `InteractionTarget::id`).
pub type CombatantId = u32;

/// One hittable body, as the engine sees it: an identity plus an
/// axis-aligned extent. [`StaticCollider`] is reused deliberately —
/// the combatant body model is the SAME box model as the collision
/// world, so the exact-slab math applies unchanged, and the player's
/// box is already derivable (`player::player_box`). A box with any
/// non-finite coordinate or an inverted axis (`min > max`) is a
/// validity failure SKIPPED by resolution — never an unpredictable
/// comparison, never displacing a valid candidate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Combatant {
    pub id: CombatantId,
    /// The hittable extent (min/max corners).
    pub hit_box: StaticCollider,
}

impl Combatant {
    /// Whether the extent satisfies the validity contract (all
    /// coordinates finite; `min <= max` on every axis).
    pub fn hit_box_valid(&self) -> bool {
        box_valid(&self.hit_box)
    }
}

/// The geometric weapon policy for ONE trace — game-layer values
/// (PLACEHOLDERS in any default; no balance approval is claimed).
/// Damage is deliberately NOT here: what a hit DOES is game policy,
/// applied through [`Health::apply_damage`] by the driver — keeping
/// resolution purely geometric (the contract's §0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttackProfile {
    /// Effective range (m) — the trace length. The input contract:
    /// finite and nonnegative (`0` = point-blank only: a target
    /// containing the origin still hits at distance 0).
    pub range: f32,
    /// Static-collider expansion for the occlusion query (m). The
    /// input contract: finite and nonnegative. `0.0` (the expected
    /// value) blocks walls exactly at their faces; a positive margin
    /// closes paper-thin gaps. Targets are NEVER expanded by this —
    /// it inflates walls only.
    pub occlusion_margin: f32,
}

impl Default for AttackProfile {
    fn default() -> Self {
        // A neutral placeholder profile (the demo layer supplies the
        // real policy): a 30 m range and exact-face occlusion.
        AttackProfile {
            range: 30.0,
            occlusion_margin: 0.0,
        }
    }
}

/// Which profile field violated its contract (typed refusal detail).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileField {
    /// `range` was non-finite or negative.
    Range,
    /// `occlusion_margin` was non-finite or negative.
    OcclusionMargin,
}

/// Why an [`AttackRequest`] was refused ([`HitResolution::Refused`]).
/// Refusals happen BEFORE any geometry runs — no state is consulted
/// or mutated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttackRefusal {
    /// A NaN/inf component in the origin.
    NonFiniteOrigin,
    /// A NaN/inf component in the direction.
    NonFiniteDirection,
    /// The direction has no usable length — the zero vector, or a
    /// length that overflows f32 (finite components whose squares
    /// overflow) — there is no aim to normalize.
    ZeroDirection,
    /// A profile field violated its contract.
    InvalidProfile(ProfileField),
}

/// One aim/fire input — what the driver supplies at the fire moment.
/// The attack is an INSTANT trace (hitscan): no projectile state.
/// The aim direction is ANY nonzero finite vector — the resolver
/// normalizes it. The attacker id is carried for ledger/feedback
/// identity; the geometry never consults it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttackRequest {
    /// Who fired.
    pub attacker: CombatantId,
    /// Trace origin (m) — the player's eye, an adversary's eye,
    /// whatever the driver's policy defines.
    pub origin: [f32; 3],
    /// Aim direction (unnormalized is fine).
    pub direction: [f32; 3],
    /// The geometric profile.
    pub profile: AttackProfile,
}

/// Why a valid trace hit nothing (a VALID resolution outcome — not
/// a refusal).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MissKind {
    /// A static collider occluded the trace before any target.
    /// `distance`/`point` are the wall entry (the same conventions
    /// as [`HitResolution::Hit`]) — usable for impact feedback.
    Occluded { distance: f32, point: [f32; 3] },
    /// The trace ran its full range unobstructed and touched no
    /// target box.
    Clear,
}

/// The resolution of one attack — a pure function of (world,
/// request, targets) with three outcomes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HitResolution {
    /// A combatant box was intersected with a clear line, strictly
    /// nearer than any occluder.
    Hit {
        target: CombatantId,
        /// Distance along the NORMALIZED direction from the origin
        /// to the box entry face (m). Always in `[0, range]`.
        distance: f32,
        /// The impact point `origin + normalized_direction * distance`.
        point: [f32; 3],
    },
    /// No combatant hit.
    Miss { kind: MissKind },
    /// The request violated the input contract — refused BEFORE any
    /// geometry runs.
    Refused(AttackRefusal),
}

/// Whether a box satisfies the extent validity contract: all
/// coordinates finite, `min <= max` on every axis (shared by the
/// target-skip rule and the public primitive's skip rule).
fn box_valid(box_extent: &StaticCollider) -> bool {
    let finite = |v: [f32; 3]| v.iter().all(|x| x.is_finite());
    finite(box_extent.min)
        && finite(box_extent.max)
        && (0..3).all(|a| box_extent.min[a] <= box_extent.max[a])
}

/// The exact-slab segment-vs-box test — the single-box primitive
/// [`CollisionWorld::segment_hit`] applies per collider. Returns the
/// smallest `t` in `[0, 1]` at which the segment `from → to` enters
/// the box, or `None` when the segment misses. Pure arithmetic (the
/// ray–AABB slab method) — no sampling, so no aliasing. `Some(0.0)`
/// when the segment starts inside. A box with any non-finite
/// coordinate or an inverted axis returns `None` (skipped — never an
/// unpredictable comparison).
pub fn segment_box_hit(from: [f32; 3], to: [f32; 3], box_extent: StaticCollider) -> Option<f32> {
    if !box_valid(&box_extent) {
        return None;
    }
    let direction = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let mut t_enter = 0.0f32;
    let mut t_exit = 1.0f32;
    for axis in 0..3 {
        let d = direction[axis];
        let origin = from[axis];
        if d == 0.0 {
            if origin < box_extent.min[axis] || origin > box_extent.max[axis] {
                return None;
            }
        } else {
            let mut t1 = (box_extent.min[axis] - origin) / d;
            let mut t2 = (box_extent.max[axis] - origin) / d;
            if t1 > t2 {
                std::mem::swap(&mut t1, &mut t2);
            }
            t_enter = t_enter.max(t1);
            t_exit = t_exit.min(t2);
            if t_enter > t_exit {
                return None;
            }
        }
    }
    if t_exit >= 0.0 && t_enter <= 1.0 {
        Some(t_enter.max(0.0))
    } else {
        None
    }
}

/// Resolves one attack: a pure, instant, geometric trace from
/// `request.origin` along the normalized `request.direction`, for
/// `request.profile.range` meters, occluded by the static collision
/// world through [`CollisionWorld::segment_hit`] (the existing
/// exact-slab query), intersected against the caller-supplied
/// [`Combatant`] boxes. The selection: nearest hit distance wins,
/// equal distances break to the LOWEST id, slice order never
/// matters; a target is a candidate only STRICTLY nearer than an
/// occluder (the wall wins ties — the collision world is the
/// authoritative geometry). Invalid targets are skipped; an invalid
/// request is refused before any geometry. No allocation beyond the
/// returned value.
pub fn resolve_attack(
    world: &CollisionWorld,
    request: &AttackRequest,
    targets: &[Combatant],
) -> HitResolution {
    if !request.origin.iter().all(|v| v.is_finite()) {
        return HitResolution::Refused(AttackRefusal::NonFiniteOrigin);
    }
    if !request.direction.iter().all(|v| v.is_finite()) {
        return HitResolution::Refused(AttackRefusal::NonFiniteDirection);
    }
    let direction_len = (request.direction[0] * request.direction[0]
        + request.direction[1] * request.direction[1]
        + request.direction[2] * request.direction[2])
        .sqrt();
    if direction_len == 0.0 || !direction_len.is_finite() {
        return HitResolution::Refused(AttackRefusal::ZeroDirection);
    }
    if !request.profile.range.is_finite() || request.profile.range < 0.0 {
        return HitResolution::Refused(AttackRefusal::InvalidProfile(ProfileField::Range));
    }
    if !request.profile.occlusion_margin.is_finite() || request.profile.occlusion_margin < 0.0 {
        return HitResolution::Refused(AttackRefusal::InvalidProfile(
            ProfileField::OcclusionMargin,
        ));
    }
    let look = [
        request.direction[0] / direction_len,
        request.direction[1] / direction_len,
        request.direction[2] / direction_len,
    ];
    let to = [
        request.origin[0] + look[0] * request.profile.range,
        request.origin[1] + look[1] * request.profile.range,
        request.origin[2] + look[2] * request.profile.range,
    ];
    // Occlusion first (the existing public exact-slab query): t in
    // [0, 1] of the segment maps to meters along the trace.
    let wall_t = world.segment_hit(request.origin, to, request.profile.occlusion_margin);
    let wall_distance = wall_t.map(|t| t * request.profile.range);
    let mut best: Option<(CombatantId, f32)> = None;
    for target in targets {
        // Invalid targets are skipped, never an unpredictable
        // comparison and never displacing a valid candidate.
        let Some(t) = segment_box_hit(request.origin, to, target.hit_box) else {
            continue;
        };
        let distance = t * request.profile.range;
        // The wall wins ties: a target must be STRICTLY nearer than
        // an occluder.
        if wall_distance.is_some_and(|wall| distance >= wall) {
            continue;
        }
        let better = match best {
            None => true,
            Some((best_id, best_distance)) => {
                distance < best_distance || (distance == best_distance && target.id < best_id)
            }
        };
        if better {
            best = Some((target.id, distance));
        }
    }
    match best {
        Some((target, distance)) => HitResolution::Hit {
            target,
            distance,
            point: [
                request.origin[0] + look[0] * distance,
                request.origin[1] + look[1] * distance,
                request.origin[2] + look[2] * distance,
            ],
        },
        None => match wall_distance {
            Some(distance) => HitResolution::Miss {
                kind: MissKind::Occluded {
                    distance,
                    point: [
                        request.origin[0] + look[0] * distance,
                        request.origin[1] + look[1] * distance,
                        request.origin[2] + look[2] * distance,
                    ],
                },
            },
            None => HitResolution::Miss {
                kind: MissKind::Clear,
            },
        },
    }
}

/// Why a health operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthError {
    /// The amount was NaN or infinite.
    NonFiniteAmount,
    /// The amount was negative (use [`Health::apply_heal`]).
    NegativeAmount,
    /// The target maximum was invalid at construction
    /// (`health_full`).
    InvalidMaximum,
    /// The `Health` value itself violates its invariants (only
    /// possible through external mutation) — the operation refuses
    /// rather than silently repairing it.
    InvalidState,
}

/// The outcome of one damage application.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DamageOutcome {
    /// `amount == 0.0` — a VALID graze: the hit landed, feedback
    /// fires, health is UNCHANGED (a defeated value stays defeated).
    Graze,
    /// Damage applied; `remaining > 0`. `dealt == amount`.
    Wounded { remaining: f32, dealt: f32 },
    /// THE DEFEATED DECLARATION — the engine's only defeat
    /// authority: the damage application that touched or crossed
    /// zero (`current <= amount`). `current` is now EXACTLY `0.0`
    /// (saturating, never negative).
    Defeated { dealt: f32 },
}

/// The outcome of one heal application.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HealOutcome {
    /// Healed, still below maximum. `gained` is the applied amount.
    Restored { current: f32, gained: f32 },
    /// Saturated at maximum. `gained` is the APPLIED amount (the
    /// clamped remainder is not silently claimed as healed).
    Capped { current: f32, gained: f32 },
}

/// A combatant's health state — the authoritative transition domain.
/// INVARIANTS (enforced at construction and before every
/// transition): `current` and `maximum` finite; `maximum >= 0`;
/// `0 <= current <= maximum`. A value that violates them (only
/// possible through external mutation — the fields are pub for
/// fingerprinting/inspection) refuses every transition with
/// [`HealthError::InvalidState`], never a silent repair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Health {
    pub current: f32,
    pub maximum: f32,
}

/// Constructs full health (`current == maximum`). `maximum` must be
/// finite and nonnegative — `maximum == 0.0` is a valid zero-health
/// combatant (born defeated; useful for pre-defeated encounter
/// states). This is ALSO the transient encounter reset's
/// reconstruction step (the contract's §9 — no persistence claims).
pub fn health_full(maximum: f32) -> Result<Health, HealthError> {
    if !maximum.is_finite() || maximum < 0.0 {
        return Err(HealthError::InvalidMaximum);
    }
    Ok(Health {
        current: maximum,
        maximum,
    })
}

impl Health {
    fn state_valid(&self) -> bool {
        self.current.is_finite()
            && self.maximum.is_finite()
            && self.maximum >= 0.0
            && self.current >= 0.0
            && self.current <= self.maximum
    }

    /// The defeated read: exactly `current == 0.0` under the
    /// invariants. A convenience read, not a second authority —
    /// after any successful `apply_damage` it agrees exactly with
    /// the `Defeated` declaration.
    pub fn is_defeated(&self) -> bool {
        self.current == 0.0
    }

    /// Applies damage: the saturating transition whose `Defeated`
    /// outcome is the ONLY defeat declaration in the engine — the
    /// predicate is exactly `current <= amount` (the application
    /// that touches or crosses zero). Zero damage is a VALID graze
    /// (health unchanged; a defeated value stays defeated). Refused
    /// inputs ([`HealthError`]) leave the value BIT-IDENTICAL —
    /// refused before any mutation. Plain f32 subtract/compare only
    /// (the contract's §6 float discipline: cross-platform
    /// bit-stable).
    pub fn apply_damage(&mut self, amount: f32) -> Result<DamageOutcome, HealthError> {
        if !amount.is_finite() {
            return Err(HealthError::NonFiniteAmount);
        }
        if amount < 0.0 {
            return Err(HealthError::NegativeAmount);
        }
        if !self.state_valid() {
            return Err(HealthError::InvalidState);
        }
        if amount == 0.0 {
            return Ok(DamageOutcome::Graze);
        }
        if self.current <= amount {
            self.current = 0.0;
            return Ok(DamageOutcome::Defeated { dealt: amount });
        }
        self.current -= amount;
        Ok(DamageOutcome::Wounded {
            remaining: self.current,
            dealt: amount,
        })
    }

    /// Applies healing: saturates at `maximum` (a revive is a policy
    /// decision the driver owns; the mechanism is uniform). Same
    /// refusal discipline — refused inputs leave the value
    /// bit-identical.
    pub fn apply_heal(&mut self, amount: f32) -> Result<HealOutcome, HealthError> {
        if !amount.is_finite() {
            return Err(HealthError::NonFiniteAmount);
        }
        if amount < 0.0 {
            return Err(HealthError::NegativeAmount);
        }
        if !self.state_valid() {
            return Err(HealthError::InvalidState);
        }
        let headroom = self.maximum - self.current;
        if amount <= headroom {
            self.current += amount;
            Ok(HealOutcome::Restored {
                current: self.current,
                gained: amount,
            })
        } else {
            let gained = headroom.max(0.0);
            self.current = self.maximum;
            Ok(HealOutcome::Capped {
                current: self.maximum,
                gained,
            })
        }
    }
}

/// What kind of combat occurrence a payload records. Stable
/// discriminants — additive-only in future contract versions, never
/// renumbered (the contract's §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombatEventKind {
    /// A shot was fired (the attacker's id).
    Fire = 1,
    /// A trace hit something (a wall or a body — the subject's id).
    Impact = 2,
    /// Damage was applied (the subject's id).
    Damage = 3,
    /// The DEFEATED declaration landed (the subject's id).
    Defeat = 4,
    /// Healing was applied (the subject's id).
    Heal = 5,
    /// A transient encounter reset occurred.
    Reset = 6,
}

impl CombatEventKind {
    /// Decodes the discriminant; `None` for values outside the
    /// ratified vocabulary (a future kind is FOREIGN until this
    /// contract's version says otherwise — never a coerced decode).
    fn from_discriminant(discriminant: u64) -> Option<CombatEventKind> {
        match discriminant {
            1 => Some(CombatEventKind::Fire),
            2 => Some(CombatEventKind::Impact),
            3 => Some(CombatEventKind::Damage),
            4 => Some(CombatEventKind::Defeat),
            5 => Some(CombatEventKind::Heal),
            6 => Some(CombatEventKind::Reset),
            _ => None,
        }
    }
}

/// The combatant-id bound for LEDGER payloads: ids at or above 2^24
/// refuse at encode (never a truncated payload). The runtime
/// [`CombatantId`] stays u32; the ledger field is 24 bits.
pub const COMBAT_PAYLOAD_ID_BOUND: u64 = 1 << 24;

/// Encodes a combat event payload: `COMBAT_MAGIC(16) | kind(8) |
/// combatant(24) | detail(16)`. Refuses (`None`) a combatant id ≥
/// 2^24 — never a truncated payload (the detail field is u16 by
/// type, so it cannot overflow). The payload is a CLASSIFICATION
/// token, not a serializer: quantitative state lives in the owning
/// world's state and logs.
pub fn combat_payload(kind: CombatEventKind, combatant: CombatantId, detail: u16) -> Option<u64> {
    if u64::from(combatant) >= COMBAT_PAYLOAD_ID_BOUND {
        return None;
    }
    Some(
        (COMBAT_MAGIC << 48)
            | (u64::from(kind as u8) << 40)
            | (u64::from(combatant) << 16)
            | u64::from(detail),
    )
}

/// Decodes a combat event payload: `Some((kind, combatant, detail))`
/// ONLY when the top 16 bits are [`COMBAT_MAGIC`] AND the kind
/// discriminant is inside the ratified vocabulary. Foreign payloads
/// — the legacy NPC layout, routine (`0x5E47`), sustained
/// (`0x5F12`/`0x0F12`), burst (`0x5336`), adversary (`0xB00B`) —
/// never decode as combat.
pub fn combat_payload_parts(payload: u64) -> Option<(CombatEventKind, CombatantId, u16)> {
    if payload >> 48 != COMBAT_MAGIC {
        return None;
    }
    let kind = CombatEventKind::from_discriminant((payload >> 40) & 0xFF)?;
    let combatant = ((payload >> 16) & 0x00FF_FFFF) as u32;
    let detail = (payload & 0xFFFF) as u16;
    Some((kind, combatant, detail))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1e-4
    }

    fn box_at(id: CombatantId, min: [f32; 3], max: [f32; 3]) -> Combatant {
        Combatant {
            id,
            hit_box: StaticCollider { min, max },
        }
    }

    fn request(origin: [f32; 3], direction: [f32; 3], range: f32) -> AttackRequest {
        AttackRequest {
            attacker: 0,
            origin,
            direction,
            profile: AttackProfile {
                range,
                occlusion_margin: 0.0,
            },
        }
    }

    #[test]
    fn resolves_nearest_target_with_a_clear_line() {
        let world = CollisionWorld::new(0.0);
        let targets = [
            box_at(1, [-0.5, 0.0, -5.0], [0.5, 1.8, -4.0]),
            box_at(2, [-0.5, 0.0, -9.0], [0.5, 1.8, -8.0]),
        ];
        let hit = resolve_attack(
            &world,
            &request([0.0, 1.6, 0.0], [0.0, 0.0, -2.0], 30.0),
            &targets,
        );
        match hit {
            HitResolution::Hit {
                target,
                distance,
                point,
            } => {
                assert_eq!(target, 1, "the nearer target wins");
                assert!(approx(distance, 4.0), "entry face at z=-4: {distance}");
                assert!(
                    approx(point[2], -4.0),
                    "the impact point is on the entry face"
                );
                assert!(approx(point[1], 1.6), "the trace stays at eye height");
            }
            other => panic!("expected a hit, got {other:?}"),
        }
    }

    #[test]
    fn unnormalized_directions_are_normalized_by_the_resolver() {
        let world = CollisionWorld::new(0.0);
        let targets = [box_at(7, [-0.5, 0.0, -5.0], [0.5, 1.8, -4.0])];
        let a = resolve_attack(
            &world,
            &request([0.0, 1.6, 0.0], [0.0, 0.0, -1.0], 30.0),
            &targets,
        );
        let b = resolve_attack(
            &world,
            &request([0.0, 1.6, 0.0], [0.0, 0.0, -7.5], 30.0),
            &targets,
        );
        assert_eq!(a, b, "aim magnitude never changes the resolution");
    }

    #[test]
    fn point_blank_target_containing_the_origin_hits_at_zero() {
        let world = CollisionWorld::new(0.0);
        let targets = [box_at(3, [-1.0, 0.0, -1.0], [1.0, 2.0, 1.0])];
        match resolve_attack(
            &world,
            &request([0.0, 1.6, 0.0], [0.0, 0.0, -1.0], 30.0),
            &targets,
        ) {
            HitResolution::Hit {
                target, distance, ..
            } => {
                assert_eq!(target, 3);
                assert_eq!(
                    distance, 0.0,
                    "a target containing the origin is point-blank"
                );
            }
            other => panic!("expected a point-blank hit, got {other:?}"),
        }
    }

    #[test]
    fn thin_wall_occludes_exactly_no_sampling_aliasing() {
        // A 0.05 m wall between the eye and the target: the exact
        // slab form blocks it (B2 — under any sampling march with a
        // step larger than the wall, this test fails).
        let world = CollisionWorld::new(0.0).with_collider(StaticCollider {
            min: [-3.0, 0.0, -2.05],
            max: [3.0, 3.0, -2.0],
        });
        let targets = [box_at(1, [-0.5, 0.0, -5.0], [0.5, 1.8, -4.0])];
        match resolve_attack(
            &world,
            &request([0.0, 1.6, 0.0], [0.0, 0.0, -1.0], 30.0),
            &targets,
        ) {
            HitResolution::Miss {
                kind: MissKind::Occluded { distance, point },
            } => {
                assert!(approx(distance, 2.0), "the wall's near face: {distance}");
                assert!(approx(point[2], -2.0));
            }
            other => panic!("the thin wall must occlude, got {other:?}"),
        }
    }

    #[test]
    fn the_wall_wins_distance_ties() {
        // A wall and a target whose entry faces sit at EXACTLY the
        // same distance (the same f32 arithmetic produces both —
        // both boxes cross the ray, entering at the same z plane):
        // the target must be strictly nearer — the wall blocks.
        let world = CollisionWorld::new(0.0).with_collider(StaticCollider {
            min: [-1.0, 0.0, -2.0],
            max: [1.0, 3.0, -1.0],
        });
        let targets = [box_at(5, [-0.5, 0.0, -2.0], [0.5, 1.8, -1.0])];
        let hit = resolve_attack(
            &world,
            &request([0.0, 1.6, 0.0], [0.0, 0.0, -1.0], 10.0),
            &targets,
        );
        match hit {
            HitResolution::Miss {
                kind: MissKind::Occluded { distance, .. },
            } => {
                assert!(approx(distance, 1.0), "the tied wall entry: {distance}");
            }
            other => panic!("an equal-distance target never beats the wall: {other:?}"),
        }
    }

    #[test]
    fn range_boundary_hits_exactly_at_range_and_misses_beyond() {
        let world = CollisionWorld::new(0.0);
        // Entry face exactly at z = -4 with range 4: t == 1.0 — a
        // hit (t_enter <= 1 is the documented boundary).
        let at_range = [box_at(1, [-0.5, 0.0, -5.0], [0.5, 1.8, -4.0])];
        match resolve_attack(
            &world,
            &request([0.0, 1.6, 0.0], [0.0, 0.0, -1.0], 4.0),
            &at_range,
        ) {
            HitResolution::Hit {
                target, distance, ..
            } => {
                assert_eq!(target, 1);
                assert_eq!(distance, 4.0, "the exactly-at-range boundary hits");
            }
            other => panic!("the at-range boundary must hit, got {other:?}"),
        }
        // Entry face beyond the range: no hit, no occluder — Clear.
        let beyond = [box_at(1, [-0.5, 0.0, -6.0], [0.5, 1.8, -4.1])];
        assert_eq!(
            resolve_attack(
                &world,
                &request([0.0, 1.6, 0.0], [0.0, 0.0, -1.0], 4.0),
                &beyond
            ),
            HitResolution::Miss {
                kind: MissKind::Clear
            }
        );
        // An empty target set with a clear trace is also Clear.
        assert_eq!(
            resolve_attack(
                &world,
                &request([0.0, 1.6, 0.0], [0.0, 0.0, -1.0], 4.0),
                &[]
            ),
            HitResolution::Miss {
                kind: MissKind::Clear
            }
        );
    }

    #[test]
    fn equal_distances_break_to_the_lowest_id_order_independently() {
        let world = CollisionWorld::new(0.0);
        // Two overlapping-in-x boxes, both crossing the ray, both
        // entering at the same z plane: exactly equal distances.
        let a = box_at(7, [-1.0, 0.0, -4.0], [0.2, 1.8, -3.0]);
        let b = box_at(3, [-0.2, 0.0, -4.0], [1.0, 1.8, -3.0]);
        let forward = request([0.0, 1.6, 0.0], [0.0, 0.0, -1.0], 30.0);
        let first = resolve_attack(&world, &forward, &[a, b]);
        let second = resolve_attack(&world, &forward, &[b, a]);
        assert_eq!(first, second, "slice order never changes the selection");
        match first {
            HitResolution::Hit {
                target, distance, ..
            } => {
                assert_eq!(target, 3, "ties break to the lower id");
                assert!(approx(distance, 3.0), "both enter at z=-3: {distance}");
            }
            other => panic!("expected a hit, got {other:?}"),
        }
    }

    #[test]
    fn resolution_is_pure_and_bitwise_repeatable() {
        let world = CollisionWorld::new(0.0).with_collider(StaticCollider {
            min: [-3.0, 0.0, -2.05],
            max: [3.0, 3.0, -2.0],
        });
        let targets = [
            box_at(1, [-0.5, 0.0, -5.0], [0.5, 1.8, -4.0]),
            box_at(2, [-0.5, 0.0, -9.0], [0.5, 1.8, -8.0]),
        ];
        let req = request([0.4, 1.1, 0.3], [0.1, -0.05, -1.0], 27.5);
        let a = resolve_attack(&world, &req, &targets);
        let b = resolve_attack(&world, &req, &targets);
        assert_eq!(a, b, "equal inputs give bitwise-equal results");
    }

    #[test]
    fn invalid_targets_are_skipped_never_displacing_a_valid_candidate() {
        let world = CollisionWorld::new(0.0);
        let valid = box_at(1, [-0.5, 0.0, -5.0], [0.5, 1.8, -4.0]);
        let nan_box = box_at(9, [f32::NAN, 0.0, -3.0], [0.5, 1.8, -2.0]);
        let inverted = box_at(
            8,
            [0.0, 0.0, -3.0],
            [-0.5, 1.8, -2.0], // min > max on x: inverted
        );
        let targets = [nan_box, inverted, valid];
        match resolve_attack(
            &world,
            &request([0.0, 1.6, 0.0], [0.0, 0.0, -1.0], 30.0),
            &targets,
        ) {
            HitResolution::Hit { target, .. } => assert_eq!(
                target, 1,
                "invalid targets never displace the valid candidate"
            ),
            other => panic!("expected the valid candidate, got {other:?}"),
        }
        assert!(!nan_box.hit_box_valid());
        assert!(!inverted.hit_box_valid());
        assert!(valid.hit_box_valid());
    }

    #[test]
    fn request_refusals_are_typed_and_exhaustive() {
        let world = CollisionWorld::new(0.0);
        let targets = [box_at(1, [-0.5, 0.0, -5.0], [0.5, 1.8, -4.0])];
        let cases = [
            (
                request([f32::NAN, 1.6, 0.0], [0.0, 0.0, -1.0], 30.0),
                AttackRefusal::NonFiniteOrigin,
            ),
            (
                request([0.0, f32::INFINITY, 0.0], [0.0, 0.0, -1.0], 30.0),
                AttackRefusal::NonFiniteOrigin,
            ),
            (
                request([0.0, 1.6, 0.0], [0.0, f32::NAN, -1.0], 30.0),
                AttackRefusal::NonFiniteDirection,
            ),
            (
                request([0.0, 1.6, 0.0], [0.0, 0.0, 0.0], 30.0),
                AttackRefusal::ZeroDirection,
            ),
            // Finite components whose squares overflow: no usable
            // length — refused, never a mis-normalized trace.
            (
                request([0.0, 1.6, 0.0], [f32::MAX, f32::MAX, 0.0], 30.0),
                AttackRefusal::ZeroDirection,
            ),
            (
                request([0.0, 1.6, 0.0], [0.0, 0.0, -1.0], -1.0),
                AttackRefusal::InvalidProfile(ProfileField::Range),
            ),
            (
                request([0.0, 1.6, 0.0], [0.0, 0.0, -1.0], f32::NAN),
                AttackRefusal::InvalidProfile(ProfileField::Range),
            ),
            (
                AttackRequest {
                    attacker: 0,
                    origin: [0.0, 1.6, 0.0],
                    direction: [0.0, 0.0, -1.0],
                    profile: AttackProfile {
                        range: 30.0,
                        occlusion_margin: f32::NAN,
                    },
                },
                AttackRefusal::InvalidProfile(ProfileField::OcclusionMargin),
            ),
            (
                AttackRequest {
                    attacker: 0,
                    origin: [0.0, 1.6, 0.0],
                    direction: [0.0, 0.0, -1.0],
                    profile: AttackProfile {
                        range: 30.0,
                        occlusion_margin: -0.1,
                    },
                },
                AttackRefusal::InvalidProfile(ProfileField::OcclusionMargin),
            ),
        ];
        for (req, refusal) in cases {
            assert_eq!(
                resolve_attack(&world, &req, &targets),
                HitResolution::Refused(refusal),
                "the request {req:?} must refuse as {refusal:?}"
            );
        }
        // Zero range and zero margin are VALID boundaries, not
        // refusals (a point-blank profile).
        assert!(matches!(
            resolve_attack(
                &world,
                &AttackRequest {
                    attacker: 0,
                    origin: [0.0, 1.6, 0.0],
                    direction: [0.0, 0.0, -1.0],
                    profile: AttackProfile {
                        range: 0.0,
                        occlusion_margin: 0.0,
                    }
                },
                &[box_at(1, [-1.0, 0.0, -1.0], [1.0, 2.0, 1.0])]
            ),
            HitResolution::Hit { target: 1, .. }
        ));
    }

    #[test]
    fn segment_box_hit_matches_the_world_query_on_identical_boxes() {
        // The parity claim (B2's evidence): the public single-box
        // primitive performs EXACTLY the world query's per-collider
        // arithmetic — identical results for every segment shape.
        let box_extent = StaticCollider {
            min: [-1.0, 0.0, -6.0],
            max: [1.0, 2.0, -4.0],
        };
        let world = CollisionWorld::new(0.0).with_collider(box_extent);
        let segments = [
            ([0.0f32, 1.6, 0.0], [0.0, 1.6, -10.0]), // straight through
            ([0.0, 1.6, 0.0], [0.0, 1.6, -5.0]),     // ends inside the box
            ([0.0, 1.6, 0.0], [0.0, 1.6, -3.0]),     // ends before the box
            ([0.5, 1.6, -5.0], [0.5, 1.6, -4.5]),    // starts inside
            ([3.0, 1.6, 0.0], [3.0, 1.6, -10.0]),    // passes beside
            ([0.0, 5.0, 0.0], [0.0, 0.0, -10.0]),    // diagonal, enters the top
            ([2.0, 1.0, -5.0], [-2.0, 1.0, -5.0]),   // perpendicular cross
        ];
        for (from, to) in segments {
            assert_eq!(
                segment_box_hit(from, to, box_extent),
                world.segment_hit(from, to, 0.0),
                "parity breaks on the segment {from:?} → {to:?}"
            );
        }
    }

    #[test]
    fn damage_transitions_hit_the_documented_boundaries() {
        let mut health = health_full(100.0).unwrap();
        assert_eq!(health.current, 100.0);

        match health.apply_damage(30.0).unwrap() {
            DamageOutcome::Wounded { remaining, dealt } => {
                assert_eq!(remaining, 70.0, "plain f32 subtraction, exactly");
                assert_eq!(dealt, 30.0);
            }
            other => panic!("expected Wounded, got {other:?}"),
        }

        // THE boundary: current == amount → Defeated at EXACTLY 0.0.
        match health.apply_damage(70.0).unwrap() {
            DamageOutcome::Defeated { dealt } => {
                assert_eq!(dealt, 70.0);
                assert_eq!(health.current, 0.0, "saturating, never negative");
                assert!(health.is_defeated());
            }
            other => panic!("the exact boundary must declare defeat, got {other:?}"),
        }

        // Crossing zero: current < amount → Defeated at 0.0.
        let mut small = health_full(10.0).unwrap();
        assert!(matches!(
            small.apply_damage(25.0).unwrap(),
            DamageOutcome::Defeated { dealt: 25.0 }
        ));
        assert_eq!(small.current, 0.0);

        // Just short of the boundary stays Wounded with a positive
        // remainder.
        let mut edge = health_full(30.0).unwrap();
        match edge.apply_damage(29.5).unwrap() {
            DamageOutcome::Wounded { remaining, .. } => {
                assert!(remaining > 0.0, "the remainder is strictly positive");
            }
            other => panic!("just-short must stay Wounded, got {other:?}"),
        }

        // Defeat declared from an already-defeated value (0 <= any
        // positive amount).
        let mut defeated = health_full(5.0).unwrap();
        defeated.apply_damage(5.0).unwrap();
        assert!(matches!(
            defeated.apply_damage(1.0).unwrap(),
            DamageOutcome::Defeated { .. }
        ));
    }

    #[test]
    fn zero_damage_is_a_valid_graze_not_a_refusal() {
        let mut health = health_full(100.0).unwrap();
        assert_eq!(health.apply_damage(0.0).unwrap(), DamageOutcome::Graze);
        assert_eq!(health.current, 100.0, "a graze changes nothing");

        // A graze on a defeated value: stays defeated.
        let mut defeated = health_full(5.0).unwrap();
        defeated.apply_damage(5.0).unwrap();
        assert_eq!(defeated.apply_damage(0.0).unwrap(), DamageOutcome::Graze);
        assert!(defeated.is_defeated());
    }

    #[test]
    fn health_refusals_leave_the_value_bit_identical() {
        let mut health = health_full(100.0).unwrap();
        health.apply_damage(30.0).unwrap();
        let before = health;
        assert_eq!(
            health.apply_damage(f32::NAN),
            Err(HealthError::NonFiniteAmount)
        );
        assert_eq!(
            health.apply_damage(f32::INFINITY),
            Err(HealthError::NonFiniteAmount)
        );
        assert_eq!(health.apply_damage(-1.0), Err(HealthError::NegativeAmount));
        assert_eq!(health, before, "refusals never mutate");

        // An externally-invalid state refuses every transition.
        let mut broken = Health {
            current: -1.0,
            maximum: 100.0,
        };
        assert_eq!(broken.apply_damage(5.0), Err(HealthError::InvalidState));
        let mut nonfinite = Health {
            current: f32::NAN,
            maximum: 100.0,
        };
        assert_eq!(nonfinite.apply_heal(5.0), Err(HealthError::InvalidState));

        // Construction refusals are typed.
        assert_eq!(health_full(-5.0), Err(HealthError::InvalidMaximum));
        assert_eq!(health_full(f32::NAN), Err(HealthError::InvalidMaximum));
        assert_eq!(health_full(f32::INFINITY), Err(HealthError::InvalidMaximum));
    }

    #[test]
    fn zero_maximum_is_a_valid_born_defeated_combatant() {
        let health = health_full(0.0).unwrap();
        assert_eq!(health.current, 0.0);
        assert_eq!(health.maximum, 0.0);
        assert!(health.is_defeated(), "born defeated is a valid state");
        // Damage on it: 0 <= any positive amount → Defeated again.
        let mut health = health;
        assert!(matches!(
            health.apply_damage(1.0).unwrap(),
            DamageOutcome::Defeated { .. }
        ));
    }

    #[test]
    fn healing_saturates_at_maximum_with_applied_amounts() {
        let mut health = health_full(100.0).unwrap();
        health.apply_damage(70.0).unwrap();
        match health.apply_heal(50.0).unwrap() {
            HealOutcome::Restored { current, gained } => {
                assert_eq!(current, 80.0);
                assert_eq!(gained, 50.0);
            }
            other => panic!("expected Restored, got {other:?}"),
        }
        match health.apply_heal(50.0).unwrap() {
            HealOutcome::Capped { current, gained } => {
                assert_eq!(current, 100.0);
                assert_eq!(gained, 20.0, "gained is the APPLIED amount only");
            }
            other => panic!("expected Capped, got {other:?}"),
        }
        // A revive from defeated is uniform mechanics (the policy
        // decision is the driver's).
        let mut defeated = health_full(40.0).unwrap();
        defeated.apply_damage(40.0).unwrap();
        assert!(matches!(
            defeated.apply_heal(15.0).unwrap(),
            HealOutcome::Restored {
                current: 15.0,
                gained: 15.0
            }
        ));
        // A zero heal is a valid no-op.
        assert!(matches!(
            defeated.apply_heal(0.0).unwrap(),
            HealOutcome::Restored { gained: 0.0, .. }
        ));
    }

    #[test]
    fn transition_sequences_are_bitwise_repeatable() {
        // The §6 float-discipline claim, exercised: two separately
        // constructed sequences give BITWISE-equal values (the
        // transition path is add/sub/compare only).
        let run = || {
            let mut health = health_full(100.0).unwrap();
            for amount in [30.0f32, 12.5, 7.25, 50.0] {
                let _ = health.apply_damage(amount);
            }
            let _ = health.apply_heal(33.75);
            (health.current.to_bits(), health.maximum.to_bits())
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn codec_round_trips_every_kind() {
        let kinds = [
            CombatEventKind::Fire,
            CombatEventKind::Impact,
            CombatEventKind::Damage,
            CombatEventKind::Defeat,
            CombatEventKind::Heal,
            CombatEventKind::Reset,
        ];
        for kind in kinds {
            for (combatant, detail) in [(0u32, 0u16), (1, 0xFFFF), (0x00FF_FFFF, 1), (1234, 5678)] {
                let payload = combat_payload(kind, combatant, detail)
                    .expect("in-range ids and u16 details always encode");
                assert_eq!(
                    combat_payload_parts(payload),
                    Some((kind, combatant, detail)),
                    "lossless round trip for {kind:?}"
                );
            }
        }
    }

    #[test]
    fn codec_refuses_out_of_bound_ids_never_truncating() {
        // The largest in-range id (2^24 − 1) round trips — the bound
        // itself is the refusal boundary.
        let top = combat_payload(CombatEventKind::Fire, 0x00FF_FFFF, 0).map(combat_payload_parts);
        assert_eq!(
            top,
            Some(Some((CombatEventKind::Fire, 0x00FF_FFFF, 0))),
            "the largest in-range id encodes and decodes losslessly"
        );
        assert_eq!(
            combat_payload(CombatEventKind::Fire, 0x100_0000, 0),
            None,
            "an id at the 2^24 bound refuses"
        );
        assert_eq!(
            combat_payload(CombatEventKind::Damage, u32::MAX, 9),
            None,
            "a u32-max id refuses"
        );
    }

    #[test]
    fn foreign_payloads_never_decode_as_combat() {
        // Every live namespace, plus the legacy layout and an
        // unknown-kind combat-magic payload: all FOREIGN.
        let foreign = [
            0u64,                                  // legacy npc (npc 0, activity 0)
            (5u64 << 16) | 3,                      // legacy npc (npc 5, activity 3)
            (0x5E47u64 << 48) | (7u64 << 32) | 99, // npc routine magic
            (0x5F12u64 << 48) | 1,                 // sustained routine magic
            (0x0F12u64 << 48) | 1,                 // sustained demand magic
            (0x5336u64 << 48) | 1,                 // soak burst magic
            (0xB00Bu64 << 48) | (3u64 << 32) | 7,  // SEAT B's adversary namespace
            u64::MAX,
        ];
        for payload in foreign {
            assert_eq!(
                combat_payload_parts(payload),
                None,
                "the foreign payload {payload:#x} never decodes as combat"
            );
        }
        // A combat-magic payload with an UNRATIFIED kind discriminant
        // is foreign too (a coerced decode would fabricate meaning).
        let unknown_kind = (COMBAT_MAGIC << 48) | (7u64 << 40) | (1u64 << 16) | 1;
        assert_eq!(combat_payload_parts(unknown_kind), None);
        let zero_kind = (COMBAT_MAGIC << 48) | (1u64 << 16) | 1;
        assert_eq!(
            combat_payload_parts(zero_kind),
            None,
            "kind 0 is not ratified"
        );
    }

    #[test]
    fn combat_fingerprint_domain_pins_a_fixed_scenario() {
        // The NEW domain's guard value (`engine-core/combat/1`):
        // measured once, pinned here — any change to this digest is
        // an investigation, never a silent repin.
        let mut fp = crate::fingerprint::Fingerprinter::new();
        fp.write_str(COMBAT_FINGERPRINT_DOMAIN);
        // A fixed health sequence: 100 → −30 (Wounded 70) → −70
        // (Defeated 0) → heal 25 (Restored 25).
        let mut health = health_full(100.0).unwrap();
        for amount in [30.0f32, 70.0] {
            let outcome = health.apply_damage(amount).unwrap();
            fp.write_u32(health.current.to_bits());
            fp.write_u8(match outcome {
                DamageOutcome::Graze => 0,
                DamageOutcome::Wounded { .. } => 1,
                DamageOutcome::Defeated { .. } => 2,
            });
        }
        let healed = health.apply_heal(25.0).unwrap();
        fp.write_u32(health.current.to_bits());
        fp.write_u8(match healed {
            HealOutcome::Restored { .. } => 1,
            HealOutcome::Capped { .. } => 2,
        });
        // A fixed resolution: the thin-wall occlusion scenario.
        let world = CollisionWorld::new(0.0).with_collider(StaticCollider {
            min: [-3.0, 0.0, -2.05],
            max: [3.0, 3.0, -2.0],
        });
        let targets = [box_at(1, [-0.5, 0.0, -5.0], [0.5, 1.8, -4.0])];
        let resolution = resolve_attack(
            &world,
            &request([0.0, 1.6, 0.0], [0.0, 0.0, -1.0], 30.0),
            &targets,
        );
        match resolution {
            HitResolution::Miss {
                kind: MissKind::Occluded { distance, .. },
            } => {
                fp.write_u8(3);
                fp.write_u32(distance.to_bits());
            }
            _ => panic!("the fixed scenario must occlude"),
        }
        // A fixed payload encode.
        fp.write_u64(
            combat_payload(CombatEventKind::Defeat, 42, 7).expect("the fixed encode is in range"),
        );
        // Measured at the first run of this test (2026-11-18) and
        // pinned: any change to this digest is an investigation,
        // never a silent repin.
        assert_eq!(fp.finish_hex(), "8a3978034487412e");
    }
}
