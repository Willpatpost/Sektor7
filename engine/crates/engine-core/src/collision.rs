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
//! the exact existing code paths — the rotated query arms are
//! zero-iteration loops that add no floating-point operations — so
//! every existing test and frozen trace is unchanged by construction.
//! A world that DOES carry rotated colliders gets every query
//! rotated-aware together (the all-or-nothing rule, now wired): the
//! axis-aligned list is consulted first with byte-identical behavior,
//! then the rotated list through the exact narrow phase here.

use crate::player::{SKIN, StaticCollider};

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
    /// The cached world AABB (the broad-phase rejection volume) —
    /// derived in the constructor from the same corner transform
    /// [`RotatedCollider::world_aabb`] returns, so every pretest is
    /// six compares with zero per-query trigonometry.
    aabb_min: [f32; 3],
    aabb_max: [f32; 3],
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
        let cos_yaw = yaw.cos();
        let sin_yaw = yaw.sin();
        let mut aabb_min = [f32::INFINITY; 3];
        let mut aabb_max = [f32::NEG_INFINITY; 3];
        for corner_x in [-half[0], half[0]] {
            for corner_z in [-half[2], half[2]] {
                let world_x = center[0] + corner_x * cos_yaw + corner_z * sin_yaw;
                let world_z = center[2] - corner_x * sin_yaw + corner_z * cos_yaw;
                aabb_min[0] = aabb_min[0].min(world_x);
                aabb_max[0] = aabb_max[0].max(world_x);
                aabb_min[2] = aabb_min[2].min(world_z);
                aabb_max[2] = aabb_max[2].max(world_z);
            }
        }
        aabb_min[1] = center[1] - half[1];
        aabb_max[1] = center[1] + half[1];
        Some(RotatedCollider {
            center,
            half,
            yaw,
            cos_yaw,
            sin_yaw,
            aabb_min,
            aabb_max,
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
    /// is rotation-invariant), computed once at construction and
    /// cached. Every rotated narrow-phase query pretests against this
    /// box (six compares), so distant colliders cost one cheap
    /// rejection, never the SAT.
    pub fn world_aabb(&self) -> StaticCollider {
        StaticCollider {
            min: self.aabb_min,
            max: self.aabb_max,
        }
    }

    /// Whether the given axis-aligned box overlaps this rotated
    /// collider (strict overlap — contact at the skin margin is
    /// clear, mirroring [`CollisionWorld::box_overlaps`]).
    ///
    /// The broad phase (the cached world AABB, six compares) rejects
    /// first; the narrow phase is the exact 2D separating-axis test
    /// between the box's horizontal rectangle (axis-aligned) and the
    /// rotated rectangle over the FOUR separating axes (world X,
    /// world Z, the collider's local X, its local Z), gated by the
    /// vertical interval overlap (both shapes are vertical prisms,
    /// so vertical overlap is a 1D interval test). Deterministic:
    /// fixed axis order, pure arithmetic.
    pub fn overlaps_box(&self, min: [f32; 3], max: [f32; 3]) -> bool {
        if (0..3).any(|a| min[a] >= self.aabb_max[a] || max[a] <= self.aabb_min[a]) {
            return false;
        }
        if min[1] >= self.center[1] + self.half[1] || max[1] <= self.center[1] - self.half[1] {
            return false;
        }
        let (projection, extent_sum) = self.horizontal_sat(min, max);
        (0..4).all(|u| projection[u].abs() < extent_sum[u])
    }

    /// The minimal push that clears an overlapping axis-aligned box
    /// from this rotated collider, as `(push_vector, magnitude)`: the
    /// smallest-magnitude translation along ONE candidate face
    /// direction — the AWAY side of each horizontal face axis (the
    /// rotated analog of the AABB depenetration's minimal-axis push:
    /// the push direction is a face normal, not a world axis) or the
    /// two vertical faces (down through the bottom, up onto the top).
    /// Every push lands [`SKIN`]-clear of the face it exits through,
    /// never merely re-aligned with it. `ground_y` carries the
    /// world's ground plane for the AABB path's validity rule: a
    /// downward push that would cross it is not a candidate (the
    /// ground clamp would push the box straight back in).
    /// Deterministic: strictly-smaller magnitudes win; mathematical
    /// ties resolve by the fixed candidate order (local X, local Z,
    /// down, up) — identically on every run and platform. `None`
    /// when the box does not overlap this collider (that overlap is
    /// this query's own gate).
    pub fn push_out_box(
        &self,
        min: [f32; 3],
        max: [f32; 3],
        ground_y: f32,
    ) -> Option<([f32; 3], f32)> {
        if !self.overlaps_box(min, max) {
            return None;
        }
        let (projection, extent_sum) = self.horizontal_sat(min, max);
        let (c, s) = (self.cos_yaw, self.sin_yaw);
        let mut best: Option<([f32; 3], f32)> = None;
        // The horizontal face axes, in the fixed candidate order: the
        // collider's local X, then its local Z. The push runs along
        // the AWAY side of the axis — the sign of the projection,
        // which side of the box the player center sits on.
        for (u, axis_x, axis_z) in [(2, c, -s), (3, s, c)] {
            let depth = extent_sum[u] - projection[u].abs() + SKIN;
            let sign = if projection[u] >= 0.0 { 1.0 } else { -1.0 };
            let push = [axis_x * sign * depth, 0.0, axis_z * sign * depth];
            keep_smaller(&mut best, push, depth);
        }
        // The vertical faces: down through the bottom (the ground
        // rule mirrors the AABB path's `neg_valid`), then up onto the
        // top.
        let bottom = self.center[1] - self.half[1];
        let top = self.center[1] + self.half[1];
        let down = max[1] - bottom + SKIN;
        if min[1] - down >= ground_y {
            keep_smaller(&mut best, [0.0, -down, 0.0], down);
        }
        let up = top - min[1] + SKIN;
        keep_smaller(&mut best, [0.0, up, 0.0], up);
        best
    }

    /// Swept movement along one world axis against this collider: the
    /// [`SKIN`]-short contact displacement (signed — positive along
    /// `+axis`, negative along `−axis`) such that moving the box by
    /// up to that value keeps it out of this collider. The exact
    /// per-axis time-of-impact against the rotated faces — the swept
    /// separating-axis form: for each of the four horizontal axes,
    /// the closing rate of the projected gap gives the displacement
    /// at which that axis closes, and the LAST axis to close governs
    /// the contact. Swept, not sampled: tunneling is impossible at
    /// any speed, exactly like the AABB `sweep_axis`. `None` when
    /// this movement cannot contact the collider — a broad-phase
    /// miss, a perpendicular axis whose gap never closes within the
    /// skin (resting contact does not block), a trajectory that opens
    /// a gapped axis, or an existing SAT overlap (the
    /// depenetration's to resolve). A contact that lies BEYOND this
    /// move also returns `None` — the broad phase pretests the SWEPT
    /// box (the move's own extent), so a farther contact is not this
    /// sweep's business; the caller re-queries with a larger delta
    /// when it needs one. The returned value may sit slightly
    /// negative (resting within [`SKIN`] of the face — the tiny
    /// back-push the AABB path also produces).
    pub fn sweep_axis_box(
        &self,
        min: [f32; 3],
        max: [f32; 3],
        axis: usize,
        delta: f32,
    ) -> Option<f32> {
        if delta == 0.0 {
            return None;
        }
        // The broad phase: the SWEPT box vs the cached world AABB —
        // strictly-disjoint rejection (an exact-touch approach is a
        // contact the AABB path also clamps, via its `<=`/`>=` face
        // conditions; the pretest must not be stricter than the query
        // it guards).
        let mut swept_min = min;
        let mut swept_max = max;
        if delta > 0.0 {
            swept_max[axis] += delta;
        } else {
            swept_min[axis] += delta;
        }
        if (0..3).any(|a| swept_min[a] > self.aabb_max[a] || swept_max[a] < self.aabb_min[a]) {
            return None;
        }
        let bottom = self.center[1] - self.half[1];
        let top = self.center[1] + self.half[1];
        if axis == 1 {
            // The vertical sweep: the horizontal SAT is the
            // perpendicular gate (each axis SKIN-relaxed, mirroring
            // the AABB path's perpendicular SKIN margins — resting
            // contact against a rotated wall never blocks a fall),
            // and the faces are the horizontal top/bottom.
            let (projection, extent_sum) = self.horizontal_sat(min, max);
            if (0..4).any(|u| projection[u].abs() >= extent_sum[u] - SKIN) {
                return None;
            }
            if delta < 0.0 {
                if min[1] >= top {
                    return Some(top + SKIN - min[1]);
                }
            } else if max[1] <= bottom {
                return Some(bottom - SKIN - max[1]);
            }
            return None;
        }
        // A horizontal sweep: the vertical interval is the
        // perpendicular gate (the AABB path's SKIN margins).
        if min[1] >= top - SKIN || max[1] <= bottom + SKIN {
            return None;
        }
        // The swept SAT over the four horizontal axes. The velocity
        // per unit displacement along each axis: world X = (1, 0),
        // world Z = (0, 1), local X = (c, −s), local Z = (s, c).
        let (projection, extent_sum) = self.horizontal_sat(min, max);
        let (c, s) = (self.cos_yaw, self.sin_yaw);
        let velocity: [f32; 4] = if axis == 0 {
            if delta > 0.0 {
                [1.0, 0.0, c, s]
            } else {
                [-1.0, 0.0, -c, -s]
            }
        } else if delta > 0.0 {
            [0.0, 1.0, -s, c]
        } else {
            [0.0, -1.0, s, -c]
        };
        let mut contact = 0.0f32;
        let mut constrained = false;
        for u in 0..4 {
            let separation = projection[u].abs() - extent_sum[u];
            let velocity_along = velocity[u];
            if velocity_along == 0.0 {
                // The perpendicular world axis: a gap at or beyond
                // the skin means this collider is never reached;
                // resting contact (within it) imposes no constraint.
                if separation >= SKIN {
                    return None;
                }
                continue;
            }
            if separation <= 0.0 {
                continue;
            }
            if projection[u] * velocity_along >= 0.0 {
                // This gapped axis only opens along the trajectory:
                // it can never close, so contact is impossible.
                return None;
            }
            let displacement = separation / velocity_along.abs();
            if displacement > contact {
                contact = displacement;
            }
            constrained = true;
        }
        if !constrained {
            // No axis closes a gap: the boxes already overlap (the
            // depenetration's to resolve) or rest in contact — never
            // a sweep clamp from this collider.
            return None;
        }
        if delta > 0.0 {
            Some(contact - SKIN)
        } else {
            Some(SKIN - contact)
        }
    }

    /// The 2D separating-axis data between the given axis-aligned box
    /// and this collider's rotated rectangle: for each of the four
    /// axes (world X, world Z, the collider's local X, its local Z)
    /// the SIGNED projection of the box center onto the axis and the
    /// sum of the two rectangles' projected half-extents. The
    /// rectangles overlap horizontally iff every axis satisfies
    /// `|projection| < extent_sum` (the separating-axis theorem for
    /// two rectangles — exact, closed-form, fixed axis order).
    fn horizontal_sat(&self, min: [f32; 3], max: [f32; 3]) -> ([f32; 4], [f32; 4]) {
        let player_center_x = (min[0] + max[0]) / 2.0;
        let player_center_z = (min[2] + max[2]) / 2.0;
        let player_half_x = (max[0] - min[0]) / 2.0;
        let player_half_z = (max[2] - min[2]) / 2.0;
        let dx = player_center_x - self.center[0];
        let dz = player_center_z - self.center[2];
        let (c, s) = (self.cos_yaw, self.sin_yaw);
        let (absolute_c, absolute_s) = (c.abs(), s.abs());
        let (hx, hz) = (self.half[0], self.half[2]);
        (
            [dx, dz, dx * c - dz * s, dx * s + dz * c],
            [
                player_half_x + hx * absolute_c + hz * absolute_s,
                player_half_z + hx * absolute_s + hz * absolute_c,
                player_half_x * absolute_c + player_half_z * absolute_s + hx,
                player_half_x * absolute_s + player_half_z * absolute_c + hz,
            ],
        )
    }
}

/// The depenetration candidate rule: strictly-smaller magnitudes win,
/// so the fixed candidate order resolves mathematical ties — the
/// first candidate achieving the minimum is kept (deterministic on
/// every run and platform, mirroring the AABB path's rule).
fn keep_smaller(best: &mut Option<([f32; 3], f32)>, push: [f32; 3], magnitude: f32) {
    if best.is_none_or(|(_, best_magnitude)| magnitude < best_magnitude) {
        *best = Some((push, magnitude));
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
    // The broad phase: the segment's margin-expanded bounding box vs
    // the cached world AABB (six compares after six min/max) —
    // STRICTLY-DISJOINT rejection only, so a segment whose range
    // merely TOUCHES the box (the exact corner/boundary-contact class
    // the AABB slab path reports) still reaches the narrow phase: the
    // pretest must never be stricter than the query it guards.
    let lo = [
        from[0].min(to[0]) - margin,
        from[1].min(to[1]) - margin,
        from[2].min(to[2]) - margin,
    ];
    let hi = [
        from[0].max(to[0]) + margin,
        from[1].max(to[1]) + margin,
        from[2].max(to[2]) + margin,
    ];
    if (0..3).any(|a| hi[a] < collider.aabb_min[a] || lo[a] > collider.aabb_max[a]) {
        return None;
    }
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

    /// The tangent class (§8): exact face contact is CLEAR (the
    /// strict-overlap contract — contact at the skin margin never
    /// counts as an overlap), a hair inside overlaps, and the same
    /// holds at the diagonal face within named float error.
    #[test]
    fn tangent_contact_is_clear_and_a_hair_inside_overlaps() {
        let axis_aligned = RotatedCollider::new([2.0, 1.0, 2.0], [1.0, 1.0, 1.0], 0.0).unwrap();
        // Resting exactly ON the +X face (x = 3.0): clear.
        assert!(!axis_aligned.overlaps_box([3.0, 0.5, 1.5], [3.4, 1.5, 2.5]));
        // A hair inside (x = 2.99): overlaps.
        assert!(axis_aligned.overlaps_box([2.99, 0.5, 1.5], [3.4, 1.5, 2.5]));
        // Exactly ON the top (y = 2.0): clear; a hair below: overlaps.
        assert!(!axis_aligned.overlaps_box([1.5, 2.0, 1.5], [2.5, 3.0, 2.5]));
        assert!(axis_aligned.overlaps_box([1.5, 1.99, 1.5], [2.5, 3.0, 2.5]));

        // The diagonal face: a small box whose SAT projection on the
        // local-X axis is 0.1·(|cos|+|sin|) — the world-axis-aligned
        // box's extents PROJECT onto the local axes, they do not map
        // 1:1. The center sits at local x = −(2 + projection ∓ 1e-6):
        // just beyond the face is clear, just inside overlaps.
        let diagonal = RotatedCollider::new(
            [0.0, 1.0, 0.0],
            [2.0, 1.0, 2.0],
            std::f32::consts::FRAC_PI_4,
        )
        .unwrap();
        let (c, s) = diagonal.frame();
        let projected_half = 0.1 * (c.abs() + s.abs());
        for (offset, expected) in [(-1e-6f32, false), (1e-6, true)] {
            let center = diagonal.to_world([-(2.0 + projected_half) + offset, 0.0, 0.0]);
            let min = [center[0] - 0.1, 0.5, center[2] - 0.1];
            let max = [center[0] + 0.1, 1.5, center[2] + 0.1];
            assert_eq!(
                diagonal.overlaps_box(min, max),
                expected,
                "local-x offset {offset}"
            );
        }
    }

    /// The near-parallel and perpendicular-gate sweep classes (§8): a
    /// box outside the z-band never contacts along a pure X sweep
    /// (the perpendicular SKIN gate); a box in the band approaching
    /// the true face contacts SKIN-short; a box already SAT-overlapping
    /// gets NO sweep clamp (the depenetration's domain); zero-delta is
    /// refused; and an opening trajectory never contacts.
    #[test]
    fn sweep_axis_gates_near_parallel_and_perpendicular_cases() {
        let diagonal = RotatedCollider::new(
            [0.0, 1.0, 0.0],
            [2.0, 1.0, 2.0],
            std::f32::consts::FRAC_PI_4,
        )
        .unwrap();
        // (a) z-clear of the whole shape (world z = 4.0): the +X sweep
        // never reaches it — the perpendicular world-Z gate.
        let far = [0.0, 0.5, 3.75];
        let far_max = [0.5, 1.5, 4.25];
        assert_eq!(diagonal.sweep_axis_box(far, far_max, 0, 10.0), None);
        assert_eq!(diagonal.sweep_axis_box(far, far_max, 0, -10.0), None);
        // (b) in the z-band, approaching from the left along +X: a
        // real SKIN-short contact against the true shape.
        let near = [-6.0, 0.5, -0.25];
        let near_max = [-5.5, 1.5, 0.25];
        let face = diagonal.sweep_axis_box(near, near_max, 0, 10.0).unwrap();
        assert!(face > 0.0 && face < 10.0, "a bounded contact {face}");
        // Applying exactly the clamp leaves the box SKIN-clear.
        let mut clamped = [near[0] + face, near[1], near[2]];
        let mut clamped_max = [near_max[0] + face, near_max[1], near_max[2]];
        clamped[0] += 0.5 * SKIN;
        clamped_max[0] += 0.5 * SKIN;
        assert!(!diagonal.overlaps_box(clamped, clamped_max));
        // A full SKIN PAST the contact (net +2·SKIN from the
        // SKIN-short clamp) re-overlaps: the clamp is exactly
        // SKIN-short of the true contact, neither generous nor early.
        clamped[0] += 1.5 * SKIN;
        clamped_max[0] += 1.5 * SKIN;
        assert!(
            diagonal.overlaps_box(clamped, clamped_max),
            "a full SKIN past the clamp re-overlaps (the clamp is SKIN-short, not generous)"
        );
        // (c) already SAT-overlapping (centered inside): no clamp.
        let inside = [-0.25, 0.5, -0.25];
        let inside_max = [0.25, 1.5, 0.25];
        assert!(diagonal.overlaps_box(inside, inside_max));
        assert_eq!(diagonal.sweep_axis_box(inside, inside_max, 0, 10.0), None);
        // (d) zero delta: refused.
        assert_eq!(diagonal.sweep_axis_box(near, near_max, 0, 0.0), None);
        // (e) an opening trajectory (moving +X from beyond the +X
        // local face, in the band): never contacts.
        let past = diagonal.to_world([2.5, 0.0, 0.0]);
        let opening = [past[0] - 0.25, 0.5, past[2] - 0.25];
        let opening_max = [past[0] + 0.25, 1.5, past[2] + 0.25];
        assert_eq!(diagonal.sweep_axis_box(opening, opening_max, 0, 10.0), None);
    }

    /// The vertical sweep (§8): falling onto a rotated top lands
    /// SKIN-short of it, rising into a rotated bottom is clamped, and
    /// resting horizontal contact within the skin never blocks a fall
    /// (the perpendicular SKIN gates mirror the AABB path's).
    #[test]
    fn vertical_sweep_lands_skin_short_and_resting_never_blocks() {
        let diagonal = RotatedCollider::new(
            [0.0, 1.0, 0.0],
            [2.0, 1.0, 2.0],
            std::f32::consts::FRAC_PI_4,
        )
        .unwrap();
        // Falling from above the top (top = 2.0): clamp SKIN-short.
        let above = [-0.25, 2.5, -0.25];
        let above_max = [0.25, 3.0, 0.25];
        let floor = diagonal.sweep_axis_box(above, above_max, 1, -5.0).unwrap();
        assert!(
            (floor - (-0.5 + SKIN)).abs() < 1e-6,
            "lands SKIN-short {floor}"
        );
        // Resting horizontal contact: the same fall with the box
        // nudged into the diagonal face band (within SKIN of the face
        // on the perpendicular axes) must NOT block — the relaxed
        // horizontal gate treats skin-contact as clear.
        let resting = [-0.25, 2.5, 1.35];
        let resting_max = [0.25, 3.0, 1.85];
        // (the resting z sits the box against the upper-right edge
        // region; the exact face position is irrelevant — the fall
        // must clamp ONLY on the top, at the same SKIN-short value.)
        let floor_rest = diagonal.sweep_axis_box(resting, resting_max, 1, -5.0);
        let expected_floor = -0.5 + SKIN;
        assert!(
            matches!(floor_rest, Some(value) if (value - expected_floor).abs() < 1e-6),
            "the resting fall clamps only on the top, SKIN-short: {floor_rest:?}"
        );
        // Rising into the bottom (bottom = 0.0) from below: clamped.
        let below = [-0.25, -1.5, -0.25];
        let below_max = [0.25, -1.0, 0.25];
        let ceiling = diagonal.sweep_axis_box(below, below_max, 1, 5.0).unwrap();
        assert!(
            (ceiling - (1.0 - SKIN)).abs() < 1e-6,
            "rises SKIN-short {ceiling}"
        );
        // Past the top already (falling from inside the vertical
        // span): no vertical clamp from this collider.
        let beside = [-0.25, 1.0, -0.25];
        let beside_max = [0.25, 1.5, 0.25];
        assert!(diagonal.overlaps_box(beside, beside_max));
        assert_eq!(diagonal.sweep_axis_box(beside, beside_max, 1, -5.0), None);
    }

    /// The overlapping class (§8): a deep interpenetration resolves
    /// through the NEAR face along its true normal, landing SKIN-clear
    /// (never merely re-aligned, never through to the far side), and
    /// the ground rule excludes a downward push that would cross the
    /// ground plane (the AABB path's `neg_valid` mirrored).
    #[test]
    fn push_out_box_exits_through_the_near_face_skin_clear() {
        let diagonal = RotatedCollider::new(
            [0.0, 1.0, 0.0],
            [2.0, 1.0, 2.0],
            std::f32::consts::FRAC_PI_4,
        )
        .unwrap();
        // A player box centered at local (−1.5, 0, 0) — deep in the
        // −X-local side.
        let center = diagonal.to_world([-1.5, 0.0, 0.0]);
        let min = [center[0] - 0.3, 0.2, center[2] - 0.3];
        let max = [center[0] + 0.3, 1.8, center[2] + 0.3];
        assert!(diagonal.overlaps_box(min, max));
        let (push, magnitude) = diagonal.push_out_box(min, max, 0.0).unwrap();
        // The expected direction: the AWAY side of the local X axis,
        // (−cos45, 0, +sin45) — a face normal, never a world axis.
        let (c, s) = diagonal.frame();
        let length = (push[0] * push[0] + push[2] * push[2]).sqrt();
        assert!((push[0] / length + c).abs() < 1e-5, "the −X face normal");
        assert!((push[2] / length - s).abs() < 1e-5, "the −X face normal");
        assert!(push[1] == 0.0);
        // The magnitude: the SAT depth on the local-X axis + SKIN.
        let expected = 2.0 + 0.3 * (c + s) - 1.5 + SKIN;
        assert!(
            (magnitude - expected).abs() < 1e-3,
            "{magnitude} vs {expected}"
        );
        // Landing SKIN-clear: the pushed box no longer overlaps.
        let pushed_min = [min[0] + push[0], min[1] + push[1], min[2] + push[2]];
        let pushed_max = [max[0] + push[0], max[1] + push[1], max[2] + push[2]];
        assert!(!diagonal.overlaps_box(pushed_min, pushed_max));

        // The ground rule: a box barely poking into a face resolves
        // through the SMALLEST valid push — DOWN through a raised
        // bottom face (the poke box straddles y = 1.0, the raised
        // box's bottom) — but the same poke geometry against a box
        // sitting ON the ground refuses the down push (crossing the
        // ground plane) and resolves upward instead.
        let raised = RotatedCollider::new(
            [0.0, 2.0, 0.0],
            [2.0, 1.0, 2.0],
            std::f32::consts::FRAC_PI_4,
        )
        .unwrap();
        let poke_min = [-0.25, 0.9, -0.25];
        let poke_max = [0.25, 1.05, 0.25];
        assert!(raised.overlaps_box(poke_min, poke_max));
        let (push, _) = raised.push_out_box(poke_min, poke_max, 0.0).unwrap();
        assert!(
            push[1] < 0.0 && push[0] == 0.0 && push[2] == 0.0,
            "down through the raised bottom (the smallest valid push)"
        );
        let grounded = RotatedCollider::new(
            [0.0, 0.0, 0.0],
            [2.0, 1.0, 2.0],
            std::f32::consts::FRAC_PI_4,
        )
        .unwrap();
        let ground_poke_min = [-0.25, 0.9, -0.25];
        let ground_poke_max = [0.25, 1.05, 0.25];
        assert!(grounded.overlaps_box(ground_poke_min, ground_poke_max));
        let (push, _) = grounded
            .push_out_box(ground_poke_min, ground_poke_max, 0.0)
            .unwrap();
        assert!(
            push[1] > 0.0 && push[0] == 0.0 && push[2] == 0.0,
            "the down push would cross the ground plane — the up push wins instead"
        );
    }

    /// The corner-tie class (§8): a segment entering exactly at a
    /// corner (two faces closing at the identical t) resolves by axis
    /// order X → Y → Z — the X face's normal, deterministically.
    #[test]
    fn segment_corner_tie_resolves_by_axis_order() {
        // The yaw-0 box [1..3]×[0..2]×[1..3]: a segment aimed exactly
        // at the (min-x, max-y) corner (1, 2) in the z=2 plane — both
        // slabs close at t = 1.0.
        let collider = RotatedCollider::new([2.0, 1.0, 2.0], [1.0, 1.0, 1.0], 0.0).unwrap();
        let (t, normal) = segment_rotated_hit([-1.0, 3.0, 2.0], [1.0, 1.0, 2.0], &collider, 0.0)
            .expect("the corner hit");
        assert_eq!(t, 1.0);
        assert_eq!(normal, [-1.0, 0.0, 0.0], "the X face wins the tie");
    }

    /// The degenerate segment class (§8): a zero-extent segment (from
    /// == to) reports inside-start occlusion or a clean miss — never
    /// a panic, never a synthetic entry.
    #[test]
    fn zero_extent_segments_report_inside_or_miss() {
        let collider = RotatedCollider::new([0.0, 1.0, 0.0], [1.0, 1.0, 1.0], 0.785).unwrap();
        let (t, normal) = segment_rotated_hit([0.0, 1.0, 0.0], [0.0, 1.0, 0.0], &collider, 0.0)
            .expect("a point inside the box is occluded");
        assert_eq!((t, normal), (0.0, [0.0, 0.0, 0.0]));
        assert_eq!(
            segment_rotated_hit([5.0, 1.0, 5.0], [5.0, 1.0, 5.0], &collider, 0.0),
            None
        );
    }

    /// The 30°/45°/60° discriminators (§8): at every diagonal yaw, a
    /// probe in the corner band (inside the world AABB, OUTSIDE the
    /// true rotated shape) is CLEAR under the true queries and BLOCKED
    /// under the AABB stand-in — the anti-"rotated visual over an
    /// axis-aligned hitbox" regression, and its over-blocking mirror.
    #[test]
    fn the_true_shape_discriminates_against_the_stand_in_at_three_yaws() {
        use crate::player::CollisionWorld;
        for yaw_deg in [30.0f32, 45.0, 60.0] {
            let yaw = yaw_deg.to_radians();
            let collider = RotatedCollider::new([0.0, 1.0, 0.0], [2.0, 1.0, 2.0], yaw).unwrap();
            let stand_in = CollisionWorld::new(0.0).with_collider(collider.world_aabb());
            // The corner-band probe: centered at local (2.5, 0, 0) —
            // beyond the +X face (|local x| = 2.5 > 2), inside the
            // world AABB band (its x-extent along the local-X
            // direction is 2·(|cos|+|sin|) ≥ 2.5 at these yaws... the
            // band center world position:
            let probe_center = collider.to_world([2.5, 0.0, 0.0]);
            let min = [probe_center[0] - 0.1, 0.5, probe_center[2] - 0.1];
            let max = [probe_center[0] + 0.1, 1.5, probe_center[2] + 0.1];
            assert!(
                !collider.overlaps_box(min, max),
                "the true shape is clear at {yaw_deg}°"
            );
            assert!(
                stand_in.box_overlaps(min, max),
                "the stand-in blocks the corner band at {yaw_deg}°"
            );
            // The segment discriminator: a ray along the local +Z
            // direction at local x = 2.5 — beyond the face for its
            // entire length, crossing the world AABB.
            let (c, s) = collider.frame();
            let start = collider.to_world([2.5, 0.0, -20.0]);
            let end = [start[0] + s * 40.0, start[1], start[2] + c * 40.0];
            assert_eq!(
                segment_rotated_hit(start, end, &collider, 0.0),
                None,
                "the true shape is clear at {yaw_deg}°"
            );
            assert!(
                stand_in.segment_hit(start, end, 0.0).is_some(),
                "the stand-in occludes at {yaw_deg}°"
            );
            // The control arm at local x = 1.5 (inside the true
            // shape's x extent): BOTH block — the discriminator is
            // the offset, not the direction.
            let inside_start = collider.to_world([1.5, 0.0, 0.0]);
            let inside_end = [
                inside_start[0] + s * 40.0,
                inside_start[1],
                inside_start[2] + c * 40.0,
            ];
            assert!(
                stand_in
                    .segment_hit(inside_start, inside_end, 0.0)
                    .is_some()
            );
            assert_eq!(
                segment_rotated_hit(inside_start, inside_end, &collider, 0.0).map(|(t, _)| t),
                Some(0.0),
                "the control ray starts inside the true shape"
            );
        }
    }

    /// The yaw-0 parity bridges (§8): on exactly-representable values
    /// (quarters), the rotated sweep and depenetration evaluate the
    /// SAME mathematical quantities as the accepted AABB path over
    /// the equivalent StaticCollider. The results agree to within one
    /// f32 ulp — NOT always bit-identically, because the swept-SAT
    /// computes `(|center projection| − extent sum) − SKIN` while the
    /// AABB path computes `(face − SKIN) − player extent`: identical
    /// algebra, a different operation association, occasionally one
    /// rounding apart (measured here: 0.4999 vs 0.49989998). The
    /// load-bearing byte-identity guarantee is structural and
    /// separate — an EMPTY rotated list takes the exact existing code
    /// paths, which every pre-existing passing test proves.
    #[test]
    fn sweep_and_push_parity_with_the_aabb_path_at_yaw_zero() {
        use crate::player::CollisionWorld;
        fn parity(a: f32, b: f32) -> bool {
            (a - b).abs() < 1e-6
        }
        let center = [2.0, 1.0, 2.0];
        let half = [1.0, 1.0, 1.0];
        let rotated = RotatedCollider::new(center, half, 0.0).unwrap();
        let world = CollisionWorld::new(0.0).with_collider(StaticCollider {
            min: [1.0, 0.0, 1.0],
            max: [3.0, 2.0, 3.0],
        });
        let rotated_world = CollisionWorld::new(0.0).with_rotated_collider(Some(rotated));

        // Sweeps: approaching +X (hits), the same move short of the
        // face (no contact within the move), approaching −X from the
        // right, falling onto the top, rising into the bottom, moving
        // away, and z-clear (the perpendicular gate).
        let cases: [([f32; 3], [f32; 3], usize, f32); 7] = [
            ([0.0, 0.5, 1.5], [0.5, 1.5, 2.5], 0, 5.0), // hits the −X face
            ([0.0, 0.5, 1.5], [0.5, 1.5, 2.5], 0, 0.2), // short of the face
            ([3.5, 0.5, 1.5], [4.0, 1.5, 2.5], 0, -5.0), // hits the +X face
            ([1.5, 2.5, 1.5], [2.5, 3.0, 2.5], 1, -5.0), // lands on the top
            ([1.5, -1.5, 1.5], [2.5, -1.0, 2.5], 1, 5.0), // rises into the bottom
            ([3.5, 0.5, 1.5], [4.0, 1.5, 2.5], 0, 5.0), // moving away: clear
            ([0.0, 0.5, 3.5], [0.5, 1.5, 4.0], 0, 5.0), // z-clear: gate
        ];
        for (min, max, axis, delta) in cases {
            let rotated_face = rotated.sweep_axis_box(min, max, axis, delta);
            let (aabb_allowed, aabb_hit) = world.sweep_axis(min, max, axis, delta);
            let (rotated_allowed, rotated_hit) = rotated_world.sweep_axis(min, max, axis, delta);
            // The world-level results agree (same geometry, both
            // worlds, within the one-ulp association tolerance).
            assert_eq!(rotated_hit, aabb_hit);
            assert!(
                parity(rotated_allowed, aabb_allowed),
                "rotated {rotated_allowed} vs aabb {aabb_allowed}"
            );
            // The collider-level face agrees with the world's clamp
            // whenever the collider reports one (cases 1, 3, 4, 5 —
            // the same geometry clamps both worlds).
            if let Some(face) = rotated_face {
                assert!(
                    aabb_hit,
                    "the rotated collider clamps where the AABB path does not"
                );
                assert!(
                    parity(face, aabb_allowed),
                    "the collider face {face} matches the clamp {aabb_allowed}"
                );
            }
            // Cases 2/6/7: no box clamp within the move — case 2's
            // contact lies beyond delta (the broad phase pretests the
            // SWEPT box, so the collider correctly reports None; the
            // world-level behavior matches the AABB path's
            // no-clamp); cases 6/7 cannot contact at all.
            match (min, max, axis, delta) {
                ([0.0, 0.5, 1.5], [0.5, 1.5, 2.5], 0, 0.2)
                | ([3.5, 0.5, 1.5], [4.0, 1.5, 2.5], 0, 5.0)
                | ([0.0, 0.5, 3.5], [0.5, 1.5, 4.0], 0, 5.0) => {
                    assert_eq!(rotated_face, None);
                }
                _ => {}
            }
        }

        // Depenetration: an unambiguous +X overlap resolves through
        // the near face — the same push vector, within the same
        // one-ulp association tolerance.
        let overlap_min = [2.5, 0.5, 1.5];
        let overlap_max = [3.0, 1.5, 2.5];
        let expected_push = [3.0 + SKIN - 2.5, 0.0, 0.0];
        let aabb_push = world
            .depenetration(overlap_min, overlap_max)
            .expect("the AABB push");
        let rotated_push = rotated_world
            .depenetration(overlap_min, overlap_max)
            .expect("the rotated push");
        for axis in 0..3 {
            assert!(
                parity(rotated_push[axis], aabb_push[axis]),
                "axis {axis}: rotated {} vs aabb {}",
                rotated_push[axis],
                aabb_push[axis]
            );
            assert!(parity(aabb_push[axis], expected_push[axis]));
        }
        let (push, magnitude) = rotated.push_out_box(overlap_min, overlap_max, 0.0).unwrap();
        for axis in 0..3 {
            assert!(parity(push[axis], expected_push[axis]));
        }
        assert!(parity(magnitude, 3.0 + SKIN - 2.5));
    }

    /// The measurement note's own-run evidence (the GO §4
    /// measured-performance requirement): the query-path costs at
    /// Q = 512 on this pod, against SEAT E's Phase-0 collide-Q
    /// baseline (the AABB path measured LINEAR with no broad-phase
    /// rejection: segment_hit p50 ≈ 41/102/383/3195/22821 ns and
    /// sweep_axis ≈ 32/42/149/1301/10214 ns at Q = 1/8/64/512/4096).
    /// This measures THIS branch's paths on the same query class: the
    /// AABB-only world (the byte-identity path — the existing loops
    /// plus the empty rotated arm) and the rotated-extended world
    /// (the six-compare cached-AABB pretest per collider, then the
    /// local-frame narrow phase for the colliders whose band the
    /// query crosses). The numbers PRINT as the delivery report's
    /// evidence; the only assertions are generous sanity bounds —
    /// characterization, not thresholds (the F-06 rule: thresholds
    /// only after variance is measured).
    #[test]
    fn rotated_query_cost_characterization_at_q512() {
        use crate::player::CollisionWorld;
        const QUERIES: usize = 20_000;
        // Two 512-collider grids (16 columns × 32 rows, 3 m spacing,
        // spanning ±45 m): axis-aligned, and the same grid yaw-rotated.
        let mut aabb_world = CollisionWorld::new(0.0);
        let mut rotated_world = CollisionWorld::new(0.0);
        for column in 0..16u32 {
            for row in 0..32u32 {
                let base = [-45.0 + column as f32 * 3.0, 0.0, -45.0 + row as f32 * 3.0];
                aabb_world = aabb_world.with_box(base, 1.0, 2.0);
                rotated_world = rotated_world.with_rotated_collider(
                    RotatedCollider::from_base_yaw(base, 1.0, 1.0, 2.0, 0.785),
                );
            }
        }
        assert_eq!(aabb_world.colliders().len(), 512);
        assert_eq!(rotated_world.rotated().len(), 512);

        // The crossing ray: a long diagonal through the whole grid —
        // every collider's broad phase runs, most narrow phases too.
        let from = [-60.0, 1.0, -60.0];
        let to = [60.0, 1.0, 60.0];
        // The far ray: entirely beyond every collider's AABB — the
        // pure broad-phase rejection cost per collider.
        let far_from = [500.0, 1.0, 500.0];
        let far_to = [510.0, 1.0, 510.0];

        let start = std::time::Instant::now();
        for _ in 0..QUERIES {
            let _ = aabb_world.segment_hit(from, to, 0.0);
        }
        let aabb_cross_ns = start.elapsed().as_nanos() / QUERIES as u128;
        let start = std::time::Instant::now();
        for _ in 0..QUERIES {
            let _ = rotated_world.segment_hit(from, to, 0.0);
        }
        let rotated_cross_ns = start.elapsed().as_nanos() / QUERIES as u128;
        let start = std::time::Instant::now();
        for _ in 0..QUERIES {
            let _ = aabb_world.segment_hit(far_from, far_to, 0.0);
        }
        let aabb_far_ns = start.elapsed().as_nanos() / QUERIES as u128;
        let start = std::time::Instant::now();
        for _ in 0..QUERIES {
            let _ = rotated_world.segment_hit(far_from, far_to, 0.0);
        }
        let rotated_far_ns = start.elapsed().as_nanos() / QUERIES as u128;

        // The sweep: a player-sized box moving +X across the whole
        // grid, from LEFT of its first column (the grid spans
        // x ∈ [−45, 0] — starting left makes the move cross every
        // column, engaging the clamps as well as the rejections).
        let (min, max) = ([-49.4, 0.5, -0.4], [-48.6, 1.5, 0.4]);
        let start = std::time::Instant::now();
        for _ in 0..QUERIES {
            let _ = aabb_world.sweep_axis(min, max, 0, 90.0);
        }
        let aabb_sweep_ns = start.elapsed().as_nanos() / QUERIES as u128;
        let start = std::time::Instant::now();
        for _ in 0..QUERIES {
            let _ = rotated_world.sweep_axis(min, max, 0, 90.0);
        }
        let rotated_sweep_ns = start.elapsed().as_nanos() / QUERIES as u128;

        println!(
            "[collide-q512-characterization] segment_hit crossing: aabb {aabb_cross_ns} ns vs rotated {rotated_cross_ns} ns · \
             far: aabb {aabb_far_ns} ns vs rotated {rotated_far_ns} ns · \
             sweep_axis: aabb {aabb_sweep_ns} ns vs rotated {rotated_sweep_ns} ns"
        );
        // Generous sanity bounds only (characterization, not a gate).
        for nanos in [aabb_cross_ns, rotated_cross_ns, aabb_far_ns, rotated_far_ns] {
            assert!(
                nanos < 500_000,
                "the segment query stays bounded: {nanos} ns"
            );
        }
        for nanos in [aabb_sweep_ns, rotated_sweep_ns] {
            assert!(nanos < 500_000, "the sweep query stays bounded: {nanos} ns");
        }
        // The correctness cross-check: the rotated grid still resolves
        // a bounded clamp.
        let (allowed, hit) = rotated_world.sweep_axis(min, max, 0, 90.0);
        println!("[collide-q512-cross-check] hit={hit} allowed={allowed}");
        assert!(
            hit && (0.0..90.0).contains(&allowed),
            "the rotated grid resolves a bounded clamp: hit={hit} allowed={allowed}"
        );
    }
}
