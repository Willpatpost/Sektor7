//! Cycle 4 C — the rotated-box collision capability (owner GO
//! 2026-11-18 §4: "properly supported rotated box geometry and
//! collision").
//!
//! The representation is the **yaw-rotated vertical prism** — an
//! oriented bounding box whose rotation is about the world Y axis
//! through its center, so the horizontal cross-section is a rotated
//! rectangle while every top and bottom face stays HORIZONTAL. This
//! is a deliberate scope decision (documented fully in
//! `cycle4/interfaces/COLLISION-INTERFACE.md` §1 and the acceptance
//! matrix): the engine's proven vertical model — the ground plane,
//! gravity, terminal clamp, step-up, standing on tops — carries over
//! unchanged, and only the horizontal narrow phase gains rotation.
//! Arbitrary 3D orientations (slanted tops) would require a
//! contact-manifold resolution model: the physics-engine rewrite the
//! owner's GO explicitly prefers against. A conservative world-AABB
//! stand-in per rotated box (over-blocking gaps beside the true
//! faces) is the "rotated visual over an axis-aligned hitbox" trap
//! the GO forbids — the narrow phase here is exact.
//!
//! Determinism rules (binding, mirroring the player module's):
//! pure closed-form arithmetic — no sampling, so no aliasing at any
//! speed or thinness; stable iteration order; corner ties resolve by
//! axis order; the segment parameter `t` is invariant under the
//! rigid local-frame transform, so the rotated slab result is the
//! exact analog of the AABB slab method; no wall-clock, no RNG. The
//! rotation frame (`cos`/`sin` of the yaw) is computed ONCE at
//! construction and cached — per-query work is adds and multiplies
//! only.
//!
//! Coexistence with the axis-aligned world (the byte-identity
//! guarantee): a [`CollisionWorld`] whose rotated list is EMPTY takes
//! the exact existing code paths — not one additional floating-point
//! operation — so every existing test and frozen trace is unchanged
//! by construction. A world that DOES carry rotated colliders gets
//! every query rotated-aware together (the all-or-nothing rule): the
//! movement wiring lands with the swept implementation, and until it
//! does the world queries REFUSE rotated worlds loudly (never the
//! forbidden half-rotated state where line-of-sight sees a shape
//! movement passes through).

use crate::player::StaticCollider;

/// One yaw-rotated static box collider: a vertical prism whose
/// horizontal cross-section is a rectangle of half-extents
/// `(half[0], half[2])` rotated by `yaw` about the vertical axis
/// through `center`, with vertical half-extent `half[1]`.
///
/// The rotation follows the engine's yaw convention (the same
/// `R_y(yaw)` as `look_direction`/`step_player`: at `yaw = 0` the box
/// is axis-aligned with its local +X toward world +X; a positive yaw
/// rotates local +X toward world −Z... consistent with the player's
/// facing math — see `to_world`). The frame (`cos_yaw`, `sin_yaw`) is
/// derived once at construction and cached; construct only through
/// [`RotatedCollider::new`] or [`RotatedCollider::from_base_yaw`],
/// which validate and refuse degenerate/non-finite inputs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RotatedCollider {
    /// The box center in world coordinates.
    pub center: [f32; 3],
    /// The half-extents in the LOCAL frame (`half[axis] > 0` on every
    /// axis — degenerate extents are refused at construction).
    pub half: [f32; 3],
    /// The rotation about the world Y axis (radians). Any finite value
    /// is accepted; the frame is cached, so normalization is never
    /// needed (the trigonometry is periodic).
    pub yaw: f32,
    /// The cached rotation frame — derived in the constructors, never
    /// recomputed per query.
    cos_yaw: f32,
    sin_yaw: f32,
}

impl RotatedCollider {
    /// Constructs a rotated collider from its center, half-extents,
    /// and yaw. Returns `None` — refusing, never panicking — when any
    /// component is non-finite, or any half-extent is non-positive
    /// (a degenerate box: the GO §4 adversarial "degenerate" class is
    /// refused at the representation boundary, exactly like the
    /// player config's validation discipline).
    pub fn new(center: [f32; 3], half: [f32; 3], yaw: f32) -> Option<RotatedCollider> {
        let valid = center.iter().all(|v| v.is_finite())
            && half.iter().all(|v| v.is_finite() && *v > 0.0)
            && yaw.is_finite();
        if !valid {
            return None;
        }
        Some(RotatedCollider {
            center,
            half,
            yaw,
            cos_yaw: yaw.cos(),
            sin_yaw: yaw.sin(),
        })
    }

    /// A rotated box from a center-BOTTOM base point, independent
    /// footprint extents along the local X and Z axes, a height, and
    /// a yaw — the rotated analog of
    /// [`StaticCollider::from_base`], so a rotated cover piece
    /// mirrors its visible geometry parameter for parameter (the
    /// renderer's box builders use the same shape).
    pub fn from_base_yaw(
        base: [f32; 3],
        footprint_x: f32,
        footprint_z: f32,
        height: f32,
        yaw: f32,
    ) -> Option<RotatedCollider> {
        RotatedCollider::new(
            [base[0], base[1] + height / 2.0, base[2]],
            [footprint_x / 2.0, height / 2.0, footprint_z / 2.0],
            yaw,
        )
    }

    /// The cached rotation frame `(cos(yaw), sin(yaw))`.
    pub fn frame(&self) -> (f32, f32) {
        (self.cos_yaw, self.sin_yaw)
    }

    /// Maps a world point into the collider's local frame (translate
    /// by the center, rotate by −yaw). The inverse of
    /// [`RotatedCollider::to_world`].
    pub fn to_local(&self, point: [f32; 3]) -> [f32; 3] {
        let dx = point[0] - self.center[0];
        let dy = point[1] - self.center[1];
        let dz = point[2] - self.center[2];
        [
            dx * self.cos_yaw - dz * self.sin_yaw,
            dy,
            dx * self.sin_yaw + dz * self.cos_yaw,
        ]
    }

    /// Maps a local-frame point back to world coordinates (rotate by
    /// +yaw, translate by the center). The inverse of
    /// [`RotatedCollider::to_local`].
    pub fn to_world(&self, point: [f32; 3]) -> [f32; 3] {
        [
            self.center[0] + point[0] * self.cos_yaw + point[2] * self.sin_yaw,
            self.center[1] + point[1],
            self.center[2] - point[0] * self.sin_yaw + point[2] * self.cos_yaw,
        ]
    }

    /// Rotates a local-frame direction (e.g. a face normal) into world
    /// coordinates — the direction half of [`to_world`] (no
    /// translation).
    pub fn normal_to_world(&self, normal: [f32; 3]) -> [f32; 3] {
        [
            normal[0] * self.cos_yaw + normal[2] * self.sin_yaw,
            normal[1],
            -normal[0] * self.sin_yaw + normal[2] * self.cos_yaw,
        ]
    }

    /// The world-axis-aligned bounding box of this collider — the
    /// BROAD-PHASE rejection volume: exact (the four transformed
    /// horizontal corners, minimized/maximized; the vertical interval
    /// is rotation-invariant). Every rotated narrow-phase query
    /// pretests against this box first (six compares), so distant
    /// colliders cost one cheap rejection, never the SAT.
    pub fn world_aabb(&self) -> StaticCollider {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for corner_x in [-self.half[0], self.half[0]] {
            for corner_z in [-self.half[2], self.half[2]] {
                let world_x = self.center[0] + corner_x * self.cos_yaw + corner_z * self.sin_yaw;
                let world_z = self.center[2] - corner_x * self.sin_yaw + corner_z * self.cos_yaw;
                min[0] = min[0].min(world_x);
                max[0] = max[0].max(world_x);
                min[2] = min[2].min(world_z);
                max[2] = max[2].max(world_z);
            }
        }
        min[1] = self.center[1] - self.half[1];
        max[1] = self.center[1] + self.half[1];
        StaticCollider { min, max }
    }

    /// Whether the given axis-aligned box overlaps this rotated
    /// collider (strict overlap — contact at the skin margin is
    /// clear, mirroring [`CollisionWorld::box_overlaps`]).
    ///
    /// Contract: the world-AABB pretest rejects first; the narrow
    /// phase is the exact 2D separating-axis test between the player
    /// footprint rectangle (axis-aligned) and the rotated rectangle,
    /// gated by the vertical interval overlap (both shapes are
    /// vertical prisms, so vertical overlap is a 1D interval test).
    ///
    /// **Skeleton stub** — the exact SAT lands with the movement
    /// implementation (the interface contract §2 fixes the
    /// semantics; the world wiring refuses rotated worlds until it
    /// lands, so this is never silently wrong).
    pub fn overlaps_box(&self, _min: [f32; 3], _max: [f32; 3]) -> bool {
        todo!("the Cycle-4 C horizontal SAT narrow phase (COLLISION-INTERFACE §2)")
    }

    /// The minimal push that clears an overlapping axis-aligned box
    /// from this rotated collider: the smallest-magnitude translation
    /// along ONE of the four world-frame face normals (the rotated
    /// analog of the AABB depenetration's minimal-axis push — the
    /// push direction is the face normal, not a world axis).
    /// Deterministic: strictly-smaller magnitudes win; mathematical
    /// ties resolve by the fixed face order (local −X, +X, −Z, +Z).
    ///
    /// **Skeleton stub** — lands with the movement implementation.
    pub fn push_out_box(&self, _min: [f32; 3], _max: [f32; 3]) -> Option<([f32; 3], f32)> {
        todo!("the Cycle-4 C minimal-normal depenetration (COLLISION-INTERFACE §2)")
    }

    /// Swept movement along one world axis: the largest displacement
    /// not exceeding `delta` along `axis` that keeps the given
    /// axis-aligned box out of this rotated collider — the exact
    /// per-axis time-of-impact against the rotated faces (swept, not
    /// sampled: tunneling is impossible at any speed, exactly like
    /// the AABB `sweep_axis`). Returns `None` when the movement along
    /// this axis cannot contact the collider at all.
    ///
    /// Contract: the world-AABB pretest rejects first; the narrow
    /// phase is the closed-form swept SAT along the axis direction.
    ///
    /// **Skeleton stub** — lands with the movement implementation.
    pub fn sweep_axis_box(
        &self,
        _min: [f32; 3],
        _max: [f32; 3],
        _axis: usize,
        _delta: f32,
    ) -> Option<f32> {
        todo!("the Cycle-4 C per-axis swept TOI (COLLISION-INTERFACE §2)")
    }
}

/// Exact segment-vs-rotated-box intersection: the smallest `t` in
/// `[0, 1]` at which the segment `from → to` enters the collider
/// expanded by `margin` (per LOCAL axis — the exact frame-relative
/// analog of the AABB path's per-axis expansion), together with the
/// WORLD-FRAME surface normal of the entry face.
///
/// The method: transform both endpoints into the local frame, run the
/// same slab arithmetic as [`CollisionWorld::segment_hit`] on the
/// local box, and rotate the winning entry face's normal back. `t` is
/// invariant under the rigid transform, so the result is exactly the
/// slab method's — no sampling, no aliasing; a rotated face thinner
/// than any sample step still registers exactly. `Some((0.0,
/// [0.0, 0.0, 0.0]))` when the segment starts inside the expanded box
/// (fully occluded; no entry face exists — the documented contract);
/// `None` on a miss. Corner ties (two faces entered at the identical
/// `t`) resolve by axis order X → Y → Z — deterministic on every
/// platform and every run.
pub fn segment_rotated_hit(
    from: [f32; 3],
    to: [f32; 3],
    collider: &RotatedCollider,
    margin: f32,
) -> Option<(f32, [f32; 3])> {
    let local_from = collider.to_local(from);
    let local_to = collider.to_local(to);
    let direction = [
        local_to[0] - local_from[0],
        local_to[1] - local_from[1],
        local_to[2] - local_from[2],
    ];
    let mut t_enter = 0.0f32;
    let mut t_exit = 1.0f32;
    let mut missed = false;
    let mut entry_axis: Option<usize> = None;
    let mut entry_positive = false;
    for axis in 0..3 {
        let lo = -collider.half[axis] - margin;
        let hi = collider.half[axis] + margin;
        let d = direction[axis];
        let origin = local_from[axis];
        if d == 0.0 {
            if origin < lo || origin > hi {
                missed = true;
                break;
            }
        } else {
            let mut t1 = (lo - origin) / d;
            let mut t2 = (hi - origin) / d;
            // A negative direction swaps which slab face is entered
            // first: after the swap, `t1` crosses the POSITIVE face.
            let mut positive_face = false;
            if t1 > t2 {
                std::mem::swap(&mut t1, &mut t2);
                positive_face = true;
            }
            // Strictly-greater: the FIRST axis achieving the maximum
            // entry time wins the face (the corner-tie rule).
            if t1 > t_enter {
                t_enter = t1;
                entry_axis = Some(axis);
                entry_positive = positive_face;
            }
            t_exit = t_exit.min(t2);
            if t_enter > t_exit {
                missed = true;
                break;
            }
        }
    }
    if missed || t_exit < 0.0 || t_enter > 1.0 {
        return None;
    }
    let t = t_enter.max(0.0);
    if t == 0.0 {
        // The origin is inside the expanded box: occluded from the
        // start. There is no entry face — the zero normal is the
        // documented sentinel, and callers needing only `t` (the LOS
        // consumers) read full occlusion.
        return Some((0.0, [0.0, 0.0, 0.0]));
    }
    let Some(axis) = entry_axis else {
        // Parallel inside every slab: same inside-start contract.
        return Some((0.0, [0.0, 0.0, 0.0]));
    };
    let mut local_normal = [0.0f32; 3];
    local_normal[axis] = if entry_positive { 1.0 } else { -1.0 };
    Some((t, collider.normal_to_world(local_normal)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construction refuses the degenerate and non-finite classes at
    /// the representation boundary (the GO §4 "degenerate" test
    /// requirement, handled as an explicit refusal — never a panic,
    /// never a silently-accepted broken shape).
    #[test]
    fn construction_refuses_nonfinite_and_degenerate_shapes() {
        assert!(RotatedCollider::new([0.0, 0.0, 0.0], [1.0, 1.0, 1.0], 0.0).is_some());
        assert!(RotatedCollider::new([f32::NAN, 0.0, 0.0], [1.0, 1.0, 1.0], 0.0).is_none());
        assert!(RotatedCollider::new([0.0, 0.0, 0.0], [f32::INFINITY, 1.0, 1.0], 0.0).is_none());
        assert!(RotatedCollider::new([0.0, 0.0, 0.0], [1.0, 1.0, 1.0], f32::NAN).is_none());
        assert!(RotatedCollider::new([0.0, 0.0, 0.0], [0.0, 1.0, 1.0], 0.0).is_none());
        assert!(RotatedCollider::new([0.0, 0.0, 0.0], [1.0, -1.0, 1.0], 0.0).is_none());
        assert!(
            RotatedCollider::from_base_yaw([0.0, 0.0, 0.0], 0.0, 2.0, 1.0, 0.0).is_none(),
            "a zero footprint axis is degenerate"
        );
        assert!(
            RotatedCollider::from_base_yaw([0.0, 0.0, 0.0], 2.0, 2.0, 0.0, 0.0).is_none(),
            "zero height is degenerate"
        );
    }

    /// The local-frame transforms are exact inverses at yaw 0 (the
    /// only bit-exact frame: `sin(0)` and `cos(0)` are exact in f32)
    /// and preserve the round trip within named float error at every
    /// other angle — f32 trigonometry is not bit-exact at "cardinal"
    /// angles either (`sin(π_f32) ≈ 8.7e−8`), so exactness is claimed
    /// only where it is true (the honest-determinism rule: per-run
    /// determinism always; cross-value bit-identity only at exact
    /// frames).
    #[test]
    fn local_frame_round_trip_and_distance_preservation() {
        let point = [3.5, -1.25, 2.75];
        let identity = RotatedCollider::new([1.0, 0.0, -2.0], [1.0, 1.0, 1.0], 0.0).unwrap();
        assert_eq!(identity.to_world(identity.to_local(point)), point);

        for yaw_deg in [30.0f32, 45.0, 60.0, 90.0, 180.0] {
            let yaw = yaw_deg.to_radians();
            let collider = RotatedCollider::new([1.0, 0.0, -2.0], [1.0, 1.0, 1.0], yaw).unwrap();
            let local = collider.to_local(point);
            let round = collider.to_world(local);
            for axis in 0..3 {
                assert!(
                    (round[axis] - point[axis]).abs() < 1e-5,
                    "round trip within f32 trig error at {yaw_deg}°"
                );
            }
            // Distance preservation (a rigid transform): the local
            // offset's magnitude equals the world offset's.
            let world_offset = [
                point[0] - collider.center[0],
                point[1] - collider.center[1],
                point[2] - collider.center[2],
            ];
            let world_sq: f32 = world_offset.iter().map(|v| v * v).sum();
            let local_sq: f32 = local.iter().map(|v| v * v).sum();
            assert!(
                (world_sq - local_sq).abs() < 1e-4,
                "the local frame is rigid at {yaw_deg}°"
            );
        }
    }

    /// The world AABB exactly bounds the rotated corners: at the
    /// cardinal yaws it equals the axis-aligned box, and at 45° the
    /// four transformed corners sit inside it with the exact diagonal
    /// extent (a unit half-extent square at 45° spans
    /// ±(hx·cos45 + hz·sin45) on both world axes).
    #[test]
    fn world_aabb_exactly_bounds_the_rotated_corners() {
        let cardinal = RotatedCollider::new([2.0, 1.0, 3.0], [1.0, 2.0, 0.5], 0.0).unwrap();
        let aabb = cardinal.world_aabb();
        assert_eq!(aabb.min, [1.0, -1.0, 2.5]);
        assert_eq!(aabb.max, [3.0, 3.0, 3.5]);

        let diagonal = RotatedCollider::new(
            [0.0, 1.0, 0.0],
            [2.0, 1.0, 2.0],
            std::f32::consts::FRAC_PI_4,
        )
        .unwrap();
        let aabb = diagonal.world_aabb();
        let extent =
            2.0 * std::f32::consts::FRAC_PI_4.cos() + 2.0 * std::f32::consts::FRAC_PI_4.sin();
        for axis in [0, 2] {
            assert!(
                (aabb.min[axis] + extent).abs() < 1e-6,
                "min face at −extent"
            );
            assert!(
                (aabb.max[axis] - extent).abs() < 1e-6,
                "max face at +extent"
            );
        }
        assert_eq!(
            aabb.min[1], 0.0,
            "the vertical interval is rotation-invariant"
        );
        assert_eq!(aabb.max[1], 2.0);
        // Every transformed corner lies inside the AABB (the bounding
        // property itself, not just the computed extent).
        for corner_x in [-2.0f32, 2.0] {
            for corner_z in [-2.0f32, 2.0] {
                let corner = diagonal.to_world([corner_x, 0.0, corner_z]);
                assert!(
                    corner
                        .iter()
                        .zip(aabb.min)
                        .zip(aabb.max)
                        .all(|((value, lo), hi)| *value >= lo && *value <= hi),
                    "the corner {corner:?} lies inside the AABB"
                );
            }
        }
    }

    /// At yaw 0 the rotated segment query is EXACTLY the AABB slab
    /// method: the same `t` as `CollisionWorld::segment_hit` over the
    /// equivalent `StaticCollider` on exactly-representable test
    /// values (the coexistence bridge — bit-equal, not approximately).
    #[test]
    fn segment_query_agrees_exactly_with_the_aabb_slab_at_yaw_zero() {
        use crate::player::CollisionWorld;
        let center = [2.0, 1.0, 2.0];
        let half = [1.0, 1.0, 1.0];
        let rotated = RotatedCollider::new(center, half, 0.0).unwrap();
        let world = CollisionWorld::new(0.0).with_collider(StaticCollider {
            min: [
                center[0] - half[0],
                center[1] - half[1],
                center[2] - half[2],
            ],
            max: [
                center[0] + half[0],
                center[1] + half[1],
                center[2] + half[2],
            ],
        });
        for (from, to) in [
            ([0.0, 1.0, 2.0], [10.0, 1.0, 2.0]),  // straight through +X
            ([10.0, 1.0, 2.0], [0.0, 1.0, 2.0]),  // the reverse direction
            ([0.0, 5.0, 2.0], [10.0, 5.0, 2.0]),  // over the top: miss
            ([2.0, 1.0, 2.0], [10.0, 1.0, 2.0]),  // starting inside: t = 0
            ([0.0, 3.0, 0.0], [10.0, -1.0, 4.0]), // a diagonal crossing
        ] {
            let rotated_t = segment_rotated_hit(from, to, &rotated, 0.0).map(|(t, _)| t);
            let aabb_t = world.segment_hit(from, to, 0.0);
            assert_eq!(rotated_t, aabb_t, "from {from:?} to {to:?}");
        }
    }

    /// The 45° proof: a ray entering a diagonally-rotated face hits
    /// at the exact analytic `t`, the hit point lies ON the local
    /// face plane, and the returned normal is the rotated face
    /// normal — the surface-normal correctness the GO §4 requires.
    #[test]
    fn segment_query_hits_the_rotated_face_exactly_with_the_true_normal() {
        // A 2×2 (half-extent) square rotated 45°, centered at the
        // origin, base y 0: its local −X face is the world-space
        // plane through the corners rotated by 45°.
        let collider = RotatedCollider::new(
            [0.0, 1.0, 0.0],
            [2.0, 1.0, 2.0],
            std::f32::consts::FRAC_PI_4,
        )
        .unwrap();
        // A ray along world +X at the box's mid-height, from far left.
        let from = [-10.0, 1.0, 0.0];
        let to = [10.0, 1.0, 0.0];
        let (t, normal) = segment_rotated_hit(from, to, &collider, 0.0).unwrap();
        // The entry face at 45°: the local −X face's world plane is
        // x·cos45 − z·sin45 = −2 (the local-frame x of the hit point
        // is exactly −half_x). The hit point:
        let hit = [
            from[0] + (to[0] - from[0]) * t,
            from[1] + (to[1] - from[1]) * t,
            from[2] + (to[2] - from[2]) * t,
        ];
        let local_hit = collider.to_local(hit);
        assert!(
            (local_hit[0] + collider.half[0]).abs() < 1e-5,
            "the hit point sits on the local −X face ({local_hit:?})"
        );
        // The local −X face normal rotated to world:
        let expected = collider.normal_to_world([-1.0, 0.0, 0.0]);
        for axis in 0..3 {
            assert!(
                (normal[axis] - expected[axis]).abs() < 1e-6,
                "the world normal is the rotated face normal"
            );
        }
        assert!(t > 0.0 && t < 1.0);
        // The world normal of a 45°-rotated −X face is (−cos45, 0,
        // +sin45) — pointing up-left away from the box.
        assert!(normal[0] < 0.0 && normal[2] > 0.0);
    }

    /// The anti-"AABB-hitbox" regression: the rotated query sees the
    /// TRUE shape, not the world AABB. A ray running parallel to a
    /// rotated face, offset OUTSIDE the true box (local x = −2.5 vs
    /// half-extent 2) but INSIDE the collider's world AABB, must MISS
    /// — while the AABB stand-in (the world AABB as a
    /// `StaticCollider`, queried through the EXISTING accepted slab
    /// path) reports a hit from the very start. This is the exact
    /// failure mode the GO forbids ("rotated visual meshes while
    /// retaining unrelated axis-aligned hitboxes"); its over-blocking
    /// mirror (the stand-in blocking where the true shape is clear)
    /// is the same geometry read from the other side — pinned by the
    /// control arm at local x = −1.5, where BOTH queries hit.
    #[test]
    fn segment_query_uses_the_true_rotated_shape_not_the_world_aabb() {
        use crate::player::CollisionWorld;
        let collider = RotatedCollider::new(
            [0.0, 1.0, 0.0],
            [2.0, 1.0, 2.0],
            std::f32::consts::FRAC_PI_4,
        )
        .unwrap();
        // The discriminating ray: PARALLEL to the rotated −X face (it
        // runs along the local +Z direction — within the face's
        // plane), offset so its entire length sits at local x = −2.5 —
        // strictly beyond the true face, while its world path cuts
        // straight across the world AABB band.
        let direction = [
            std::f32::consts::FRAC_PI_4.sin(),
            0.0,
            std::f32::consts::FRAC_PI_4.cos(),
        ];
        let start = collider.to_world([-2.5, 0.0, -20.0]);
        let end = [
            start[0] + direction[0] * 40.0,
            start[1],
            start[2] + direction[2] * 40.0,
        ];
        let stand_in = CollisionWorld::new(0.0).with_collider(collider.world_aabb());
        assert!(
            matches!(stand_in.segment_hit(start, end, 0.0), Some(t) if t > 0.0),
            "the AABB stand-in hits mid-segment (the ray cuts across it)"
        );
        assert!(
            segment_rotated_hit(start, end, &collider, 0.0).is_none(),
            "the true rotated shape is clear (local x stays at −2.5, beyond the face)"
        );
        // The control arm: the same parallel ray at local x = −1.5 —
        // inside the true box's x extent — is occluded under BOTH
        // representations, so the discriminator is the offset, not
        // the direction.
        let inside_start = collider.to_world([-1.5, 0.0, 0.0]);
        let inside_end = [
            inside_start[0] + direction[0] * 40.0,
            inside_start[1],
            inside_start[2] + direction[2] * 40.0,
        ];
        assert_eq!(
            stand_in.segment_hit(inside_start, inside_end, 0.0),
            Some(0.0)
        );
        let (t, _) = segment_rotated_hit(inside_start, inside_end, &collider, 0.0)
            .expect("the parallel-inside ray is occluded by the true box");
        assert_eq!(t, 0.0, "it starts inside the true box");
    }

    /// Margin semantics: the local-frame expansion shifts the entry
    /// `t` outward exactly as the AABB path's world-axis expansion
    /// does at yaw 0 (the frame-relative analog), and a fully
    /// margin-blocked grazing ray reports the earlier `t`.
    #[test]
    fn margin_expands_the_local_slabs_like_the_aabb_path() {
        use crate::player::CollisionWorld;
        let center = [2.0, 1.0, 2.0];
        let half = [1.0, 1.0, 1.0];
        let rotated = RotatedCollider::new(center, half, 0.0).unwrap();
        let world = CollisionWorld::new(0.0).with_collider(StaticCollider {
            min: [1.0, 0.0, 1.0],
            max: [3.0, 2.0, 3.0],
        });
        let from = [0.0, 1.0, 2.0];
        let to = [10.0, 1.0, 2.0];
        for margin in [0.0f32, 0.25, 1.0] {
            let rotated_t = segment_rotated_hit(from, to, &rotated, margin).map(|(t, _)| t);
            let aabb_t = world.segment_hit(from, to, margin);
            assert_eq!(rotated_t, aabb_t, "margin {margin}");
        }
    }

    /// The inside-start contract: a segment beginning inside the
    /// expanded box reports `t = 0.0` with the zero-normal sentinel
    /// (full occlusion, no entry face) — matching the AABB path's
    /// `Some(0.0)`.
    #[test]
    fn inside_start_reports_zero_t_with_the_sentinel_normal() {
        let collider = RotatedCollider::new([0.0, 1.0, 0.0], [1.0, 1.0, 1.0], 0.785).unwrap();
        let (t, normal) =
            segment_rotated_hit([0.0, 1.0, 0.0], [10.0, 1.0, 0.0], &collider, 0.0).unwrap();
        assert_eq!(t, 0.0);
        assert_eq!(normal, [0.0, 0.0, 0.0]);
    }
}
