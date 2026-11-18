//! The Cycle-4 capability-B weapon mechanics (SEAT B, 2026-11-18): a
//! reusable weapon-selection and firing capability with a SECOND
//! mechanically distinct weapon — the deterministic lateral-fan
//! SPREAD shot against the single-ray PRECISION shot — built over
//! the combat contract v1 (`combat.rs`: `resolve_attack`, the
//! `AttackRequest`/`HitResolution` surface, unchanged).
//!
//! Contract: `cycle4/interfaces/WEAPONS-INTERFACE.md` (the persisted
//! form of this doc). **Every numeric value here is a PLACEHOLDER —
//! no balance or difficulty approval is claimed or implied**; the
//! balance is game policy supplied through `WeaponSpec`/`WeaponSet`.
//! No weapon economy, ammunition persistence, inventory, or
//! progression (out of scope by the Cycle-4 GO §3).
//!
//! Determinism: the spread fan's ray directions are
//! `aim + offset·perpendicular` UNNORMALIZED (the resolver normalizes
//! any nonzero direction — the ray's line is identical), so the fan
//! construction is add/mul/div/sqrt only — IEEE exactly-rounded,
//! cross-platform BIT-STABLE by construction (the combat contract §6
//! resolution-path discipline, a stronger claim than angular
//! rotation's libm sin/cos). The cooldown is the C3-R2 canonical
//! LOGICAL-TICK deadline discipline: set only on a successful fire
//! (`now.saturating_add(cooldown_ticks)`), read as a pure
//! comparison, never decremented, per-weapon, switching never resets
//! it, a refused press spends nothing. Zero idle per-frame cost: the
//! `Arsenal` is pure state, queried on fire edges and HUD reads.
//! No wall-clock ever enters logical state.

use crate::clock::Tick;
use crate::combat::{
    AttackProfile, AttackRequest, Combatant, CombatantId, HitResolution, ProfileField,
    resolve_attack,
};
use crate::player::CollisionWorld;

/// How many weapons the v1 arsenal carries (the `WeaponId` enum and
/// the slot array grow together through versioned contract changes).
pub const WEAPON_COUNT: usize = 2;

/// The bounded-work cap on spread rays (a game-supplied fan wider
/// than this is a typed refusal, never an unbounded loop).
pub const MAX_SPREAD_RAYS: u32 = 64;

/// Stable weapon identity — the selection surface and the HUD
/// discriminant. Additive-only in future versions (never renumbered).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeaponId {
    /// The single-ray precision shot — the EXISTING weapon,
    /// expressible bit-identically (the compatibility invariant).
    Precision,
    /// The deterministic lateral-fan spread shot.
    Spread,
}

impl WeaponId {
    /// The arsenal slot index (the deadline array's key).
    fn slot(self) -> usize {
        match self {
            WeaponId::Precision => 0,
            WeaponId::Spread => 1,
        }
    }
}

/// The mechanically distinct firing patterns — engine MECHANICS; the
/// numeric characteristics are game policy in [`WeaponSpec`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FiringPattern {
    /// ONE ray exactly along the aim — the precision shot.
    Precision,
    /// The deterministic lateral fan: `rays` rays whose directions
    /// are `aim + offset_k · perp` with `offset_k =
    /// (k − (rays−1)/2) · lateral_step` and `perp` the aim's
    /// HORIZONTAL unit perpendicular `(aim.z, 0, −aim.x)` normalized.
    /// Unnormalized directions (the resolver normalizes; the line is
    /// identical) — add/mul/div/sqrt only, cross-platform
    /// bit-stable. The CENTER ray of an odd fan is the aim
    /// bit-identically; `rays == 1` equals the precision pattern. A
    /// (near-)vertical aim has no horizontal perpendicular: the fan
    /// COLLAPSES to `rays` copies of the aim (documented, tested).
    Spread { rays: u32, lateral_step: f32 },
}

/// One weapon's full firing characteristics — GAME POLICY values
/// (every default a PLACEHOLDER). The geometric profile reuses the
/// combat contract's [`AttackProfile`] verbatim; the cooldown is
/// CANONICAL LOGICAL TICKS (the C3-R2 deadline discipline).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponSpec {
    pub pattern: FiringPattern,
    pub profile: AttackProfile,
    pub cooldown_ticks: u64,
}

impl WeaponSpec {
    /// The placeholder PRECISION weapon — the CURRENT single-shot
    /// semantics (range 30, exact-face occlusion, an 18-tick
    /// cooldown), for the demo's migration and the compatibility
    /// regressions. PLACEHOLDER — D overrides with encounter policy.
    pub fn placeholder_precision() -> Self {
        WeaponSpec {
            pattern: FiringPattern::Precision,
            profile: AttackProfile {
                range: 30.0,
                occlusion_margin: 0.0,
            },
            cooldown_ticks: 18,
        }
    }

    /// The placeholder SPREAD weapon — a 5-ray fan at lateral step
    /// 0.08 (an effective half-angle ≈ atan(0.16) ≈ 9.1°), range 12,
    /// a 30-tick cooldown. PLACEHOLDER — D overrides.
    pub fn placeholder_spread() -> Self {
        WeaponSpec {
            pattern: FiringPattern::Spread {
                rays: 5,
                lateral_step: 0.08,
            },
            profile: AttackProfile {
                range: 12.0,
                occlusion_margin: 0.0,
            },
            cooldown_ticks: 30,
        }
    }
}

/// The per-weapon spec table — the game layer's policy, passed to
/// each fire. `try_fire` always uses the SELECTED weapon's spec (a
/// selection/spec mismatch is structurally impossible).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponSet {
    pub specs: [WeaponSpec; WEAPON_COUNT],
}

impl WeaponSet {
    /// The placeholder pair (the two prototype weapons' default
    /// characteristics — PLACEHOLDERS).
    pub fn placeholders() -> Self {
        WeaponSet {
            specs: [
                WeaponSpec::placeholder_precision(),
                WeaponSpec::placeholder_spread(),
            ],
        }
    }

    /// The spec for one weapon identity.
    pub fn spec(&self, weapon: WeaponId) -> &WeaponSpec {
        &self.specs[weapon.slot()]
    }
}

/// The player's weapon state: the selection plus ONE canonical
/// logical-tick cooldown deadline PER WEAPON. Deadlines are set only
/// by successful fires; **switching NEVER resets or touches them** —
/// each weapon's own readiness governs, and switching mid-cooldown
/// and back leaves the original deadline in force exactly. Fields
/// are pub for HUD reads/fingerprinting; mutation goes through
/// [`Arsenal::select`] and [`try_fire`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arsenal {
    /// The selected weapon (the HUD discriminant).
    pub selected: WeaponId,
    /// The per-slot cooldown deadlines (logical-tick indexed; 0 =
    /// ready from the start).
    pub deadlines: [u64; WEAPON_COUNT],
}

impl Arsenal {
    /// A fresh arsenal: the starting weapon selected, every weapon
    /// ready (deadlines 0).
    pub fn new(starting: WeaponId) -> Self {
        Arsenal {
            selected: starting,
            deadlines: [0; WEAPON_COUNT],
        }
    }

    /// Selects `weapon`. Returns `true` iff the selection CHANGED.
    /// Never touches any deadline.
    pub fn select(&mut self, weapon: WeaponId) -> bool {
        if self.selected == weapon {
            return false;
        }
        self.selected = weapon;
        true
    }

    /// The selected weapon (the HUD discriminant).
    pub fn selected(&self) -> WeaponId {
        self.selected
    }

    /// The canonical tick at which `weapon` is ready again (the HUD
    /// readiness bar's target).
    pub fn ready_at(&self, weapon: WeaponId) -> u64 {
        self.deadlines[weapon.slot()]
    }

    /// Whether the SELECTED weapon may fire at `now` (the pure
    /// deadline comparison — the cooldown gate).
    pub fn is_ready(&self, now: Tick) -> bool {
        now.0 >= self.deadlines[self.selected.slot()]
    }
}

/// One fired ray: the direction AS PASSED to the resolver
/// (unnormalized — any nonzero vector per the combat contract; the
/// tracer line is `origin + t · direction`) and its resolution
/// through the combat contract.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shot {
    pub direction: [f32; 3],
    pub resolution: HitResolution,
}

/// The fire result.
#[derive(Debug, Clone, PartialEq)]
pub enum FireOutcome {
    /// The volley fired: per-ray shots in FAN ORDER (ray k's lateral
    /// offset `(k − (rays−1)/2) · lateral_step`; the precision
    /// weapon's single shot is k = 0). The selected weapon's cooldown
    /// deadline is SET (`now + cooldown_ticks`, saturating).
    Fired { weapon: WeaponId, shots: Vec<Shot> },
    /// Refused BEFORE any geometry ran; the deadlines are BIT-UNCHANGED
    /// (a refused press spends no cooldown).
    Refused(FireRefusal),
}

/// Why a fire was refused (typed, fail-early — the deadlines
/// untouched).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireRefusal {
    /// The selected weapon's cooldown deadline hasn't passed;
    /// `ready_at` is the canonical tick at which it will.
    Cooldown { ready_at: u64 },
    /// A NaN/inf component in the origin.
    NonFiniteOrigin,
    /// A NaN/inf component in the aim.
    NonFiniteAim,
    /// The aim is the zero vector.
    ZeroAim,
    /// The spec's profile violated the combat contract.
    InvalidProfile(ProfileField),
    /// The spread pattern's parameters are invalid (rays == 0 or
    /// above [`MAX_SPREAD_RAYS`]; a non-finite or negative lateral
    /// step).
    InvalidSpread(SpreadField),
}

/// Which spread parameter was invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadField {
    Rays,
    LateralStep,
}

/// The aim's horizontal unit perpendicular `(aim.z, 0, −aim.x)`
/// normalized — `None` when the aim is (near-)vertical (horizontal
/// length ≤ `f32::EPSILON`: the fan collapses; see
/// [`FiringPattern::Spread`]).
fn horizontal_perpendicular(aim: [f32; 3]) -> Option<[f32; 3]> {
    let length = (aim[0] * aim[0] + aim[2] * aim[2]).sqrt();
    if length <= f32::EPSILON {
        return None;
    }
    Some([aim[2] / length, 0.0, -aim[0] / length])
}

/// The per-fire context — who fires, from where, aiming how, at
/// what, in which world, at which logical tick. Built by the caller
/// per press edge (the demo's fire step); `origin`/`aim` are
/// caller-supplied (**FP/TP parity by construction** — the module is
/// camera-agnostic, a pure function of its arguments; the recommended
/// derivation is the player's eye + `look_direction` in BOTH camera
/// modes). `now` is the caller's logical tick — the spine's execution
/// tick or the demo's tick counter; the module never reads a clock.
/// (No `PartialEq`: the equality the contract guarantees — and the
/// tests pin — is on `FireOutcome`, not on the context.)
#[derive(Debug, Clone, Copy)]
pub struct FireContext<'a> {
    /// Who fired (identity for ledger/feedback; the geometry never
    /// consults it — the combat contract's `attacker` discipline).
    pub attacker: CombatantId,
    /// The trace origin (the player's eye).
    pub origin: [f32; 3],
    /// The aim — ANY nonzero finite vector (the resolver normalizes
    /// each ray; the fan derivation uses it as-is).
    pub aim: [f32; 3],
    /// The hittable targets (D's mapping policy over A's
    /// multi-adversary surface).
    pub targets: &'a [Combatant],
    /// The shared collision world (occlusion, cover).
    pub world: &'a CollisionWorld,
    /// The caller's logical tick (the canonical clock for the
    /// cooldown deadline).
    pub now: Tick,
}

/// Fires the SELECTED weapon (the full contract §3 surface). The
/// caller owns the input EDGE (one call per press edge — the
/// `jump_pressed` discipline). Validation order (fail-early,
/// deadlines untouched): origin → aim → the cooldown gate → the
/// profile → the spread parameters → the ray derivation → the
/// per-ray `resolve_attack` (combat §1 unchanged, per-ray
/// independent, nearest-wins with lowest-id ties). D applies damage
/// with D's policy — the engine never mutates `Health` here.
pub fn try_fire(arsenal: &mut Arsenal, set: &WeaponSet, context: &FireContext) -> FireOutcome {
    let FireContext {
        attacker,
        origin,
        aim,
        targets,
        world,
        now,
    } = *context;
    if !origin.iter().all(|v| v.is_finite()) {
        return FireOutcome::Refused(FireRefusal::NonFiniteOrigin);
    }
    if !aim.iter().all(|v| v.is_finite()) {
        return FireOutcome::Refused(FireRefusal::NonFiniteAim);
    }
    if aim == [0.0, 0.0, 0.0] {
        return FireOutcome::Refused(FireRefusal::ZeroAim);
    }
    let weapon = arsenal.selected;
    let spec = set.spec(weapon);
    let ready_at = arsenal.deadlines[weapon.slot()];
    if now.0 < ready_at {
        return FireOutcome::Refused(FireRefusal::Cooldown { ready_at });
    }
    if !spec.profile.range.is_finite() || spec.profile.range < 0.0 {
        return FireOutcome::Refused(FireRefusal::InvalidProfile(ProfileField::Range));
    }
    if !spec.profile.occlusion_margin.is_finite() || spec.profile.occlusion_margin < 0.0 {
        return FireOutcome::Refused(FireRefusal::InvalidProfile(ProfileField::OcclusionMargin));
    }
    let directions: Vec<[f32; 3]> = match spec.pattern {
        FiringPattern::Precision => vec![aim],
        FiringPattern::Spread { rays, lateral_step } => {
            if rays == 0 || rays > MAX_SPREAD_RAYS {
                return FireOutcome::Refused(FireRefusal::InvalidSpread(SpreadField::Rays));
            }
            if !lateral_step.is_finite() || lateral_step < 0.0 {
                return FireOutcome::Refused(FireRefusal::InvalidSpread(SpreadField::LateralStep));
            }
            match horizontal_perpendicular(aim) {
                // The vertical collapse: no horizontal perpendicular
                // exists — the fan degenerates to aim copies.
                None => vec![aim; rays as usize],
                Some(perpendicular) => (0..rays)
                    .map(|k| {
                        let offset = (k as f32 - (rays - 1) as f32 / 2.0) * lateral_step;
                        [
                            aim[0] + offset * perpendicular[0],
                            aim[1] + offset * perpendicular[1],
                            aim[2] + offset * perpendicular[2],
                        ]
                    })
                    .collect(),
            }
        }
    };
    let shots = directions
        .into_iter()
        .map(|direction| {
            let request = AttackRequest {
                attacker,
                origin,
                direction,
                profile: spec.profile,
            };
            Shot {
                direction,
                resolution: resolve_attack(world, &request, targets),
            }
        })
        .collect();
    // The deadline is set ONLY on a successful fire (the C3-R2
    // discipline — saturating so a huge configured cooldown never
    // wraps into instant re-eligibility).
    arsenal.deadlines[weapon.slot()] = now.saturating_add(spec.cooldown_ticks).0;
    FireOutcome::Fired { weapon, shots }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::{Combatant, Health, MissKind};
    use crate::player::StaticCollider;

    /// One laterally placed target slab: a tall thin box at lateral
    /// `x` (±), spanning z 9..11 — hit by a fan ray whose lateral
    /// offset lands inside it, missed by the center ray.
    fn slab_target(id: CombatantId, x: f32) -> Combatant {
        Combatant {
            id,
            hit_box: StaticCollider {
                min: [x - 0.2, 0.0, 9.0],
                max: [x + 0.2, 3.0, 11.0],
            },
        }
    }

    /// The per-call fire context builder (the public surface's
    /// grouping — every test drives the REAL `try_fire` signature).
    fn context<'a>(
        attacker: CombatantId,
        origin: [f32; 3],
        aim: [f32; 3],
        targets: &'a [Combatant],
        world: &'a CollisionWorld,
        now: Tick,
    ) -> FireContext<'a> {
        FireContext {
            attacker,
            origin,
            aim,
            targets,
            world,
            now,
        }
    }

    /// The precision compatibility invariant: `try_fire` with the
    /// precision pattern is BIT-IDENTICAL to the direct combat
    /// contract call with the same request — the existing
    /// single-shot behavior migrates onto this module unchanged.
    #[test]
    fn precision_shot_is_bit_identical_to_the_direct_contract_call() {
        let mut arsenal = Arsenal::new(WeaponId::Precision);
        let set = WeaponSet::placeholders();
        let origin = [0.0, 1.5, 0.0];
        let aim = [0.3, -0.1, 1.0];
        let targets = [slab_target(1, 3.0)];
        let world = CollisionWorld::new(0.0);
        let outcome = try_fire(
            &mut arsenal,
            &set,
            &context(7, origin, aim, &targets, &world, Tick(5)),
        );
        let FireOutcome::Fired { weapon, shots } = outcome else {
            panic!("the precision shot fires: {outcome:?}");
        };
        assert_eq!(weapon, WeaponId::Precision);
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].direction, aim, "the ray is the aim exactly");
        let request = AttackRequest {
            attacker: 7,
            origin,
            direction: aim,
            profile: set.spec(WeaponId::Precision).profile,
        };
        assert_eq!(
            shots[0].resolution,
            resolve_attack(&world, &request, &targets),
            "bit-identical to the direct contract call"
        );
        assert_eq!(arsenal.ready_at(WeaponId::Precision), 23, "5 + 18");
        assert_eq!(arsenal.ready_at(WeaponId::Spread), 0, "untouched");
    }

    /// The spread fan's ray directions are EXACT (the contract §1
    /// formula, hand-derived at exact-f32 literals): aim (0,0,1) →
    /// perpendicular (1,0,0); step 0.125 and 5 rays → lateral
    /// offsets −0.25, −0.125, 0, +0.125, +0.25 (all exact f32); the
    /// center ray is the aim bit-identically. The directions are
    /// UNNORMALIZED (the resolver normalizes — the line is what
    /// matters).
    #[test]
    fn spread_fan_directions_are_exact() {
        let mut arsenal = Arsenal::new(WeaponId::Spread);
        let set = WeaponSet {
            specs: [
                WeaponSpec::placeholder_precision(),
                WeaponSpec {
                    pattern: FiringPattern::Spread {
                        rays: 5,
                        lateral_step: 0.125,
                    },
                    profile: AttackProfile {
                        range: 30.0,
                        occlusion_margin: 0.0,
                    },
                    cooldown_ticks: 1,
                },
            ],
        };
        let origin = [0.0, 1.5, 0.0];
        let aim = [0.0, 0.0, 1.0];
        let targets = [];
        let world = CollisionWorld::new(0.0);
        let outcome = try_fire(
            &mut arsenal,
            &set,
            &context(7, origin, aim, &targets, &world, Tick(1)),
        );
        let FireOutcome::Fired { shots, .. } = outcome else {
            panic!("the spread fires: {outcome:?}");
        };
        let expected = [
            [-0.25, 0.0, 1.0],
            [-0.125, 0.0, 1.0],
            [0.0, 0.0, 1.0],
            [0.125, 0.0, 1.0],
            [0.25, 0.0, 1.0],
        ];
        assert_eq!(shots.len(), 5);
        for (shot, direction) in shots.iter().zip(expected) {
            assert_eq!(shot.direction, direction, "the exact fan geometry");
        }
        assert_eq!(shots[2].direction, aim, "the center ray is the aim exactly");
        assert!(shots.iter().all(|shot| {
            shot.resolution
                == HitResolution::Miss {
                    kind: MissKind::Clear,
                }
        }));
    }

    /// Multiple-target interaction: two laterally placed slabs — the
    /// center ray passes BETWEEN them (Clear), each offset ray hits
    /// a DISTINCT target (per-ray independence, nearest-wins per
    /// ray). Cover handling: a wall covering one offset ray's line
    /// turns that ray Occluded while the others resolve unchanged.
    #[test]
    fn spread_hits_multiple_targets_and_respects_cover() {
        let spread = WeaponSpec {
            pattern: FiringPattern::Spread {
                rays: 3,
                lateral_step: 0.07,
            },
            profile: AttackProfile {
                range: 30.0,
                occlusion_margin: 0.0,
            },
            cooldown_ticks: 1,
        };
        let set = WeaponSet {
            specs: [WeaponSpec::placeholder_precision(), spread],
        };
        let origin = [0.0, 1.5, 0.0];
        let aim = [0.0, 0.0, 1.0];
        let targets = [slab_target(1, 0.7), slab_target(2, -0.7)];
        let world = CollisionWorld::new(0.0);

        // The open field: ray 0 (offset −0.07) hits target 2, the
        // center ray is Clear between the slabs, ray 2 hits target 1.
        let mut arsenal = Arsenal::new(WeaponId::Spread);
        let outcome = try_fire(
            &mut arsenal,
            &set,
            &context(7, origin, aim, &targets, &world, Tick(1)),
        );
        let FireOutcome::Fired { shots, .. } = outcome else {
            panic!("the spread fires: {outcome:?}")
        };
        assert_eq!(shots.len(), 3);
        assert!(matches!(
            shots[0].resolution,
            HitResolution::Hit { target: 2, .. }
        ));
        assert_eq!(
            shots[1].resolution,
            HitResolution::Miss {
                kind: MissKind::Clear
            }
        );
        assert!(matches!(
            shots[2].resolution,
            HitResolution::Hit { target: 1, .. }
        ));

        // The cover: a wall on the +x ray's line (x ≈ 0.315 at z 4.5)
        // turns ray 2 Occluded; rays 0 and 1 are UNCHANGED.
        let walled = CollisionWorld::new(0.0).with_collider(StaticCollider {
            min: [0.25, 0.0, 4.0],
            max: [0.4, 3.0, 5.0],
        });
        let mut arsenal = Arsenal::new(WeaponId::Spread);
        let outcome = try_fire(
            &mut arsenal,
            &set,
            &context(7, origin, aim, &targets, &walled, Tick(1)),
        );
        let FireOutcome::Fired { shots, .. } = outcome else {
            panic!("the spread fires into cover: {outcome:?}")
        };
        assert!(matches!(
            shots[0].resolution,
            HitResolution::Hit { target: 2, .. }
        ));
        assert_eq!(
            shots[1].resolution,
            HitResolution::Miss {
                kind: MissKind::Clear
            }
        );
        assert!(matches!(
            shots[2].resolution,
            HitResolution::Miss {
                kind: MissKind::Occluded { .. }
            }
        ));
    }

    /// The cooldown is CANONICAL LOGICAL TICKS, per weapon, never
    /// reset by switching (the C3-R2 discipline at the weapon
    /// level): a fire at tick 5 with an 18-tick cooldown is refused
    /// until tick 23; the OTHER weapon fires on its own deadline
    /// mid-wait; switching back leaves the original deadline in
    /// force; refused presses spend NOTHING.
    #[test]
    fn cooldown_is_canonical_per_weapon_and_switch_never_resets() {
        let mut arsenal = Arsenal::new(WeaponId::Precision);
        let set = WeaponSet::placeholders();
        let origin = [0.0, 1.5, 0.0];
        let aim = [0.0, 0.0, 1.0];
        let targets = [];
        let world = CollisionWorld::new(0.0);
        let fire = |arsenal: &mut Arsenal, now: Tick| {
            try_fire(
                arsenal,
                &set,
                &context(7, origin, aim, &targets, &world, now),
            )
        };

        let outcome = fire(&mut arsenal, Tick(5));
        assert!(matches!(outcome, FireOutcome::Fired { .. }));
        assert_eq!(arsenal.ready_at(WeaponId::Precision), 23, "5 + 18");

        // Refused one tick early, with the canonical deadline carried.
        assert_eq!(
            fire(&mut arsenal, Tick(22)),
            FireOutcome::Refused(FireRefusal::Cooldown { ready_at: 23 })
        );
        assert_eq!(
            arsenal.ready_at(WeaponId::Precision),
            23,
            "a refused press spends nothing"
        );

        // The other weapon fires mid-wait on its OWN deadline.
        assert!(arsenal.select(WeaponId::Spread));
        let outcome = fire(&mut arsenal, Tick(22));
        assert!(
            matches!(outcome, FireOutcome::Fired { .. }),
            "the spread is ready"
        );
        assert_eq!(arsenal.ready_at(WeaponId::Spread), 52, "22 + 30");

        // Switching back: the ORIGINAL precision deadline still governs.
        assert!(arsenal.select(WeaponId::Precision));
        assert_eq!(
            fire(&mut arsenal, Tick(22)),
            FireOutcome::Refused(FireRefusal::Cooldown { ready_at: 23 }),
            "switching never resets a deadline"
        );
        assert!(matches!(
            fire(&mut arsenal, Tick(23)),
            FireOutcome::Fired { .. }
        ));
        assert_eq!(arsenal.ready_at(WeaponId::Precision), 41, "23 + 18");
    }

    /// FP/TP parity and determinism by construction: the module is a
    /// pure function of caller-supplied origin/aim (camera-agnostic)
    /// — equal inputs give BITWISE-equal volleys (the double-run).
    #[test]
    fn equal_inputs_give_bitwise_equal_volleys() {
        let set = WeaponSet::placeholders();
        let origin = [1.0, 1.6, -2.0];
        let aim = [0.2, -0.05, 1.0];
        let targets = [slab_target(1, 2.0), slab_target(2, -2.0)];
        let world = CollisionWorld::new(0.0);
        let volley = |now: Tick| {
            let mut arsenal = Arsenal::new(WeaponId::Spread);
            try_fire(
                &mut arsenal,
                &set,
                &context(7, origin, aim, &targets, &world, now),
            )
        };
        assert_eq!(
            volley(Tick(9)),
            volley(Tick(9)),
            "the double-run is bitwise"
        );
        // The same volley through "both camera modes": the identical
        // caller-supplied origin/aim (the recommended derivation is
        // the player eye + look direction in BOTH modes).
        let mut fp = Arsenal::new(WeaponId::Spread);
        let mut tp = Arsenal::new(WeaponId::Spread);
        assert_eq!(
            try_fire(
                &mut fp,
                &set,
                &context(7, origin, aim, &targets, &world, Tick(9))
            ),
            try_fire(
                &mut tp,
                &set,
                &context(7, origin, aim, &targets, &world, Tick(9))
            ),
            "FP/TP parity by construction"
        );
    }

    /// The refusal taxonomy (fail-early, deadlines bit-unchanged):
    /// non-finite origin/aim, the zero aim, the cooldown, invalid
    /// profile fields, invalid spread parameters (rays 0, rays above
    /// the cap, non-finite/negative step).
    #[test]
    fn refusal_taxonomy_leaves_deadlines_unchanged() {
        let mut arsenal = Arsenal::new(WeaponId::Spread);
        let set = WeaponSet::placeholders();
        let origin = [0.0, 1.5, 0.0];
        let aim = [0.0, 0.0, 1.0];
        let targets = [];
        let world = CollisionWorld::new(0.0);

        let refused =
            |arsenal: &mut Arsenal, set: &WeaponSet, origin: [f32; 3], aim: [f32; 3], now: Tick| {
                let before = arsenal.deadlines;
                let outcome = try_fire(
                    arsenal,
                    set,
                    &context(7, origin, aim, &targets, &world, now),
                );
                assert_eq!(arsenal.deadlines, before, "a refusal spends no cooldown");
                outcome
            };

        assert_eq!(
            refused(&mut arsenal, &set, [f32::NAN, 0.0, 0.0], aim, Tick(1)),
            FireOutcome::Refused(FireRefusal::NonFiniteOrigin)
        );
        assert_eq!(
            refused(
                &mut arsenal,
                &set,
                origin,
                [0.0, f32::INFINITY, 0.0],
                Tick(1)
            ),
            FireOutcome::Refused(FireRefusal::NonFiniteAim)
        );
        assert_eq!(
            refused(&mut arsenal, &set, origin, [0.0, 0.0, 0.0], Tick(1)),
            FireOutcome::Refused(FireRefusal::ZeroAim)
        );

        let bad_range = WeaponSet {
            specs: [
                WeaponSpec::placeholder_precision(),
                WeaponSpec {
                    pattern: FiringPattern::Spread {
                        rays: 3,
                        lateral_step: 0.07,
                    },
                    profile: AttackProfile {
                        range: -1.0,
                        occlusion_margin: 0.0,
                    },
                    cooldown_ticks: 1,
                },
            ],
        };
        assert_eq!(
            refused(&mut arsenal, &bad_range, origin, aim, Tick(1)),
            FireOutcome::Refused(FireRefusal::InvalidProfile(ProfileField::Range))
        );
        let bad_margin = WeaponSet {
            specs: [
                WeaponSpec::placeholder_precision(),
                WeaponSpec {
                    pattern: FiringPattern::Spread {
                        rays: 3,
                        lateral_step: 0.07,
                    },
                    profile: AttackProfile {
                        range: 30.0,
                        occlusion_margin: f32::NAN,
                    },
                    cooldown_ticks: 1,
                },
            ],
        };
        assert_eq!(
            refused(&mut arsenal, &bad_margin, origin, aim, Tick(1)),
            FireOutcome::Refused(FireRefusal::InvalidProfile(ProfileField::OcclusionMargin))
        );

        let zero_rays = WeaponSet {
            specs: [
                WeaponSpec::placeholder_precision(),
                WeaponSpec {
                    pattern: FiringPattern::Spread {
                        rays: 0,
                        lateral_step: 0.07,
                    },
                    profile: AttackProfile {
                        range: 30.0,
                        occlusion_margin: 0.0,
                    },
                    cooldown_ticks: 1,
                },
            ],
        };
        assert_eq!(
            refused(&mut arsenal, &zero_rays, origin, aim, Tick(1)),
            FireOutcome::Refused(FireRefusal::InvalidSpread(SpreadField::Rays))
        );
        let huge_rays = WeaponSet {
            specs: [
                WeaponSpec::placeholder_precision(),
                WeaponSpec {
                    pattern: FiringPattern::Spread {
                        rays: MAX_SPREAD_RAYS + 1,
                        lateral_step: 0.07,
                    },
                    profile: AttackProfile {
                        range: 30.0,
                        occlusion_margin: 0.0,
                    },
                    cooldown_ticks: 1,
                },
            ],
        };
        assert_eq!(
            refused(&mut arsenal, &huge_rays, origin, aim, Tick(1)),
            FireOutcome::Refused(FireRefusal::InvalidSpread(SpreadField::Rays)),
            "the fan width is capped (bounded work)"
        );
        let bad_step = WeaponSet {
            specs: [
                WeaponSpec::placeholder_precision(),
                WeaponSpec {
                    pattern: FiringPattern::Spread {
                        rays: 3,
                        lateral_step: -0.07,
                    },
                    profile: AttackProfile {
                        range: 30.0,
                        occlusion_margin: 0.0,
                    },
                    cooldown_ticks: 1,
                },
            ],
        };
        assert_eq!(
            refused(&mut arsenal, &bad_step, origin, aim, Tick(1)),
            FireOutcome::Refused(FireRefusal::InvalidSpread(SpreadField::LateralStep))
        );
    }

    /// The vertical-aim collapse: a (near-)vertical aim has no
    /// horizontal perpendicular — the fan degenerates to aim copies
    /// (documented, deterministic; every ray resolves identically).
    #[test]
    fn vertical_aim_collapses_the_fan() {
        let mut arsenal = Arsenal::new(WeaponId::Spread);
        let set = WeaponSet {
            specs: [
                WeaponSpec::placeholder_precision(),
                WeaponSpec {
                    pattern: FiringPattern::Spread {
                        rays: 4,
                        lateral_step: 0.125,
                    },
                    profile: AttackProfile {
                        range: 30.0,
                        occlusion_margin: 0.0,
                    },
                    cooldown_ticks: 1,
                },
            ],
        };
        let origin = [0.0, 0.0, 0.0];
        let aim = [0.0, 1.0, 0.0];
        let targets = [];
        let world = CollisionWorld::new(0.0);
        let outcome = try_fire(
            &mut arsenal,
            &set,
            &context(7, origin, aim, &targets, &world, Tick(1)),
        );
        let FireOutcome::Fired { shots, .. } = outcome else {
            panic!("the vertical spread fires (collapsed): {outcome:?}")
        };
        assert_eq!(shots.len(), 4);
        assert!(
            shots.iter().all(|shot| shot.direction == aim),
            "the collapsed fan is aim copies"
        );
    }

    /// A one-ray spread equals the precision pattern bit-for-bit
    /// (offset 0 → the aim exactly), and a huge configured cooldown
    /// saturates the deadline (never wraps into re-eligibility — the
    /// C3-R2 large-value discipline).
    #[test]
    fn single_ray_spread_equals_precision_and_large_cooldowns_saturate() {
        let one_ray = WeaponSet {
            specs: [
                WeaponSpec::placeholder_precision(),
                WeaponSpec {
                    pattern: FiringPattern::Spread {
                        rays: 1,
                        lateral_step: 0.5,
                    },
                    profile: AttackProfile {
                        range: 30.0,
                        occlusion_margin: 0.0,
                    },
                    cooldown_ticks: 1,
                },
            ],
        };
        let origin = [0.0, 1.5, 0.0];
        let aim = [0.4, -0.2, 1.0];
        let targets = [slab_target(1, 4.0)];
        let world = CollisionWorld::new(0.0);

        let mut spread_arsenal = Arsenal::new(WeaponId::Spread);
        let spread_outcome = try_fire(
            &mut spread_arsenal,
            &one_ray,
            &context(7, origin, aim, &targets, &world, Tick(3)),
        );
        let mut precision_arsenal = Arsenal::new(WeaponId::Precision);
        let precision_outcome = try_fire(
            &mut precision_arsenal,
            &one_ray,
            &context(7, origin, aim, &targets, &world, Tick(3)),
        );
        let FireOutcome::Fired {
            shots: spread_shots,
            ..
        } = &spread_outcome
        else {
            panic!("the one-ray spread fires: {spread_outcome:?}")
        };
        let FireOutcome::Fired {
            shots: precision_shots,
            ..
        } = &precision_outcome
        else {
            panic!("the precision fires: {precision_outcome:?}")
        };
        assert_eq!(spread_shots, precision_shots, "rays == 1 equals precision");
        assert_eq!(spread_shots[0].direction, aim);

        // The saturating deadline: u64::MAX never wraps.
        let saturating = WeaponSet {
            specs: [
                WeaponSpec::placeholder_precision(),
                WeaponSpec {
                    pattern: FiringPattern::Spread {
                        rays: 1,
                        lateral_step: 0.0,
                    },
                    profile: AttackProfile {
                        range: 30.0,
                        occlusion_margin: 0.0,
                    },
                    cooldown_ticks: u64::MAX,
                },
            ],
        };
        let mut arsenal = Arsenal::new(WeaponId::Spread);
        assert!(matches!(
            try_fire(
                &mut arsenal,
                &saturating,
                &context(7, origin, aim, &targets, &world, Tick(3))
            ),
            FireOutcome::Fired { .. }
        ));
        assert_eq!(arsenal.ready_at(WeaponId::Spread), u64::MAX, "saturated");
        // Every tick BEFORE the saturated deadline is refused; at
        // exactly the last representable tick the `now >= deadline`
        // gate is eligible BY RULE (the consistent C3-R2 semantics —
        // the deadline never PASSES u64::MAX, it can only BE it).
        assert_eq!(
            try_fire(
                &mut arsenal,
                &saturating,
                &context(7, origin, aim, &targets, &world, Tick(u64::MAX - 1))
            ),
            FireOutcome::Refused(FireRefusal::Cooldown { ready_at: u64::MAX }),
            "the saturated deadline refuses every tick before it"
        );
    }

    /// The placeholder specs are the documented values (the
    /// migration surface D overrides with encounter policy).
    #[test]
    fn placeholder_specs_are_the_documented_values() {
        let set = WeaponSet::placeholders();
        let precision = set.spec(WeaponId::Precision);
        assert_eq!(precision.pattern, FiringPattern::Precision);
        assert_eq!(precision.profile.range, 30.0);
        assert_eq!(precision.profile.occlusion_margin, 0.0);
        assert_eq!(precision.cooldown_ticks, 18);
        let spread = set.spec(WeaponId::Spread);
        assert_eq!(
            spread.pattern,
            FiringPattern::Spread {
                rays: 5,
                lateral_step: 0.08,
            }
        );
        assert_eq!(spread.profile.range, 12.0);
        assert_eq!(spread.cooldown_ticks, 30);
        // Sanity: the placeholder spread actually resolves over a
        // target in range (the fan exists in the world, not just in
        // the types).
        let mut arsenal = Arsenal::new(WeaponId::Spread);
        let targets = [slab_target(1, 0.8)];
        let outcome = try_fire(
            &mut arsenal,
            &set,
            &context(
                7,
                [0.0, 1.5, 0.0],
                [0.0, 0.0, 1.0],
                &targets,
                &CollisionWorld::new(0.0),
                Tick(1),
            ),
        );
        let FireOutcome::Fired { shots, .. } = outcome else {
            panic!("the placeholder spread fires: {outcome:?}")
        };
        assert_eq!(shots.len(), 5);
        assert!(
            shots
                .iter()
                .any(|shot| matches!(shot.resolution, HitResolution::Hit { .. })),
            "the placeholder fan reaches a slab at lateral 0.8, z 10"
        );
    }

    /// A compile-surface sanity import (the contract's re-export
    /// consumers rely on the types being nameable together).
    #[test]
    fn contract_types_are_nameable_together() {
        let arsenal = Arsenal::new(WeaponId::Spread);
        let set = WeaponSet::placeholders();
        let spec: &WeaponSpec = set.spec(arsenal.selected());
        let _profile: &AttackProfile = &spec.profile;
        let _health = Health {
            current: 100.0,
            maximum: 100.0,
        };
        assert!(spec.cooldown_ticks > 0);
    }
}
