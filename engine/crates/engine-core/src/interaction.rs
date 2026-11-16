//! Cycle 2 C2-P1 — the minimal reusable interaction framework.
//!
//! Engine MECHANICS only: a spatial/facing query over interaction
//! targets and a deterministic selection. What interactions DO —
//! the target kinds, their effects, their feedback — is game policy
//! and lives in the application layer (`engine-slice`'s playable
//! demo), which owns the `InteractionTarget` inventory and reacts to
//! the selected id. This mirrors the engine/game boundary the player
//! module established (mechanics in core, policy in the demo).
//!
//! Determinism: selection is a pure function of (targets, eye,
//! forward, query) — nearest surface distance wins, ties break by
//! the lowest id, iteration order is the caller's slice. Input
//! validity (the R4/R4b fail-visible discipline extended, challenge
//! F5): a non-finite eye or forward REFUSES selection (`None`), and
//! targets with non-finite positions are skipped — never an
//! unpredictable comparison outcome.

/// One interactable world entity, as the engine sees it: an identity,
/// a position, and an interaction extent (the radius around the
/// position within which the entity is considered "at" its surface).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InteractionTarget {
    pub id: u32,
    pub position: [f32; 3],
    pub radius: f32,
}

/// The selection query: the player's reach and the facing cone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InteractionQuery {
    /// Maximum surface distance (target extent discounted) at which a
    /// target is offered (m).
    pub range: f32,
    /// The facing cone as the cosine of its half angle: a target is
    /// offered only when the direction to it dots against the look
    /// direction by at least this (1.0 = dead ahead only; 0.0 =
    /// the full forward hemisphere).
    pub cone_half_angle_cos: f32,
}

impl Default for InteractionQuery {
    fn default() -> Self {
        // The playable demo's policy values (a 2.5 m reach and a
        // 45° half-angle cone), kept here only as a neutral default.
        InteractionQuery {
            range: 2.5,
            cone_half_angle_cos: std::f32::consts::FRAC_1_SQRT_2,
        }
    }
}

/// Selects the interaction target: the nearest (surface distance,
/// extent discounted) target within `range` and inside the facing
/// cone; ties break to the lowest id. Returns `None` when nothing is
/// offered, when the eye or forward is non-finite (the F5 refusal),
/// or when the forward vector is degenerate.
pub fn select_target(
    targets: &[InteractionTarget],
    eye: [f32; 3],
    forward: [f32; 3],
    query: &InteractionQuery,
) -> Option<u32> {
    if !eye.iter().all(|v| v.is_finite()) || !forward.iter().all(|v| v.is_finite()) {
        return None;
    }
    let forward_len =
        (forward[0] * forward[0] + forward[1] * forward[1] + forward[2] * forward[2]).sqrt();
    if forward_len == 0.0 || !forward_len.is_finite() {
        return None;
    }
    let look = [
        forward[0] / forward_len,
        forward[1] / forward_len,
        forward[2] / forward_len,
    ];
    let mut best: Option<(u32, f32)> = None;
    for target in targets {
        if !target.position.iter().all(|v| v.is_finite()) {
            continue;
        }
        let delta = [
            target.position[0] - eye[0],
            target.position[1] - eye[1],
            target.position[2] - eye[2],
        ];
        let distance = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
        if distance == 0.0 {
            // The eye is inside the target's extent: dead-ahead by
            // definition.
            let candidate = (target.id, 0.0f32);
            if best.is_none_or(|(_, best_d)| candidate.1 < best_d) {
                best = Some(candidate);
            }
            continue;
        }
        let direction = [
            delta[0] / distance,
            delta[1] / distance,
            delta[2] / distance,
        ];
        let dot = direction[0] * look[0] + direction[1] * look[1] + direction[2] * look[2];
        if dot < query.cone_half_angle_cos {
            continue;
        }
        let surface = (distance - target.radius).max(0.0);
        if surface > query.range {
            continue;
        }
        let better = match best {
            None => true,
            Some((best_id, best_surface)) => {
                surface < best_surface || (surface == best_surface && target.id < best_id)
            }
        };
        if better {
            best = Some((target.id, surface));
        }
    }
    best.map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: u32, position: [f32; 3]) -> InteractionTarget {
        InteractionTarget {
            id,
            position,
            radius: 0.0,
        }
    }

    fn eye_forward() -> ([f32; 3], [f32; 3]) {
        ([0.0, 1.6, 0.0], [0.0, 0.0, -1.0])
    }

    #[test]
    fn selects_nearest_target_in_range() {
        let (eye, forward) = eye_forward();
        let query = InteractionQuery {
            range: 5.0,
            ..InteractionQuery::default()
        };
        let targets = [target(1, [0.0, 1.0, -2.0]), target(2, [0.0, 1.0, -4.0])];
        assert_eq!(select_target(&targets, eye, forward, &query), Some(1));

        // A large extent discounts the surface distance: the "far"
        // target with a 3 m radius is nearer by surface.
        let targets = [
            target(1, [0.0, 1.0, -2.0]),
            InteractionTarget {
                id: 2,
                position: [0.0, 1.0, -4.0],
                radius: 3.0,
            },
        ];
        assert_eq!(select_target(&targets, eye, forward, &query), Some(2));
    }

    #[test]
    fn cone_rejects_behind_and_beside() {
        let (eye, forward) = eye_forward();
        let query = InteractionQuery::default(); // 45° half angle
        let behind = [target(1, [0.0, 1.0, 2.0])];
        assert_eq!(
            select_target(&behind, eye, forward, &query),
            None,
            "a target behind the look direction is never offered"
        );
        let beside = [target(1, [2.0, 1.6, -0.0])]; // ~90° off-axis
        assert_eq!(
            select_target(&beside, eye, forward, &query),
            None,
            "a target beside the cone is not offered"
        );
        let ahead = [target(1, [2.0, 1.6, -0.0]), target(2, [0.5, 1.6, -2.0])];
        assert_eq!(
            select_target(&ahead, eye, forward, &query),
            Some(2),
            "the in-cone target is offered even beside an off-cone one"
        );
    }

    #[test]
    fn tie_breaks_by_id_stable() {
        let (eye, forward) = eye_forward();
        let query = InteractionQuery::default();
        // Two targets mirrored across the path, equidistant.
        let targets = [target(7, [1.0, 1.6, -2.0]), target(3, [-1.0, 1.6, -2.0])];
        let first = select_target(&targets, eye, forward, &query);
        let second = select_target(&targets, eye, forward, &query);
        assert_eq!(
            first,
            Some(3),
            "equal surface distance breaks to the lower id"
        );
        assert_eq!(first, second, "the tie-break is stable across calls");
    }

    #[test]
    fn empty_and_out_of_range_return_none() {
        let (eye, forward) = eye_forward();
        let query = InteractionQuery {
            range: 2.0,
            ..InteractionQuery::default()
        };
        assert_eq!(select_target(&[], eye, forward, &query), None);
        let far = [target(1, [0.0, 1.0, -10.0])];
        assert_eq!(select_target(&far, eye, forward, &query), None);
    }

    #[test]
    fn selection_is_order_independent() {
        let (eye, forward) = eye_forward();
        let query = InteractionQuery {
            range: 5.0,
            ..InteractionQuery::default()
        };
        let a = [target(1, [0.0, 1.0, -3.0]), target(2, [0.4, 1.4, -2.0])];
        let b = [a[1], a[0]];
        assert_eq!(
            select_target(&a, eye, forward, &query),
            select_target(&b, eye, forward, &query),
            "slice order never changes the selection"
        );
    }

    #[test]
    fn nonfinite_inputs_refused_or_skipped() {
        let (eye, forward) = eye_forward();
        let query = InteractionQuery::default();
        // A non-finite eye or forward refuses selection outright.
        assert_eq!(
            select_target(
                &[target(1, [0.0, 1.0, -2.0])],
                [f32::NAN, 1.6, 0.0],
                forward,
                &query
            ),
            None
        );
        assert_eq!(
            select_target(
                &[target(1, [0.0, 1.0, -2.0])],
                eye,
                [0.0, f32::INFINITY, 0.0],
                &query
            ),
            None
        );
        // A non-finite target position is skipped, never an
        // unpredictable comparison: the valid target still selects.
        let targets = [target(2, [f32::NAN, 0.0, 0.0]), target(1, [0.0, 1.0, -2.0])];
        assert_eq!(select_target(&targets, eye, forward, &query), Some(1));
        let all_bad = [target(1, [f32::NAN, 0.0, 0.0])];
        assert_eq!(select_target(&all_bad, eye, forward, &query), None);
    }
}
