//! Local meshes for the two encounter tools. Dimensions are in metres. Each
//! faceted section joins two independently sized octagons along the bore (-Z).
//! Meshes are baked once and reused in both camera modes.

use super::{Frame, Point, add, mul, sub, unit};
use engine_core::WeaponId;
use engine_renderer::scene::SceneVertex;

const CERAMIC: Point = [0.74, 0.84, 0.85];
const GRAPHITE: Point = [0.15, 0.21, 0.26];
const STEEL: Point = [0.38, 0.46, 0.50];
const CYAN: Point = [0.12, 0.94, 1.0];
const AMBER: Point = [1.0, 0.42, 0.08];
const ORANGE: Point = [0.79, 0.31, 0.10];

pub(super) const fn tip(weapon: WeaponId) -> Point {
    match weapon {
        WeaponId::Precision => [0.0, 0.025, -0.75],
        WeaponId::Spread => [0.0, 0.010, -0.48],
    }
}

pub(super) fn build(weapon: WeaponId) -> Vec<SceneVertex> {
    let mut mesh = Vec::new();
    match weapon {
        WeaponId::Precision => {
            // Long stepped spine, raised split sights and an exposed cyan coil
            // between ivory field jackets. The narrower rear tapers to a stock.
            section(
                &mut mesh,
                [0., -0.032, 0.28],
                [0.042, 0.068],
                [0., 0., 0.07],
                [0.067, 0.060],
                GRAPHITE,
            );
            section(
                &mut mesh,
                [0., 0., 0.12],
                [0.075, 0.070],
                [0., 0.016, -0.34],
                [0.064, 0.056],
                CERAMIC,
            );
            section(
                &mut mesh,
                [0., -0.055, 0.01],
                [0.052, 0.130],
                [0., -0.039, -0.09],
                [0.050, 0.090],
                GRAPHITE,
            );
            section(
                &mut mesh,
                [0., 0.025, -0.31],
                [0.043, 0.042],
                [0., 0.025, -0.72],
                [0.033, 0.032],
                CYAN,
            );
            for z in [-0.40, -0.50, -0.60] {
                section(
                    &mut mesh,
                    [0., 0.025, z],
                    [0.052, 0.051],
                    [0., 0.025, z - 0.035],
                    [0.049, 0.048],
                    STEEL,
                );
            }
            section(
                &mut mesh,
                [0., 0.025, -0.68],
                [0.044, 0.044],
                tip(weapon),
                [0.042, 0.040],
                GRAPHITE,
            );
            for x in [-0.047, 0.047] {
                section(
                    &mut mesh,
                    [x, 0.084, 0.047],
                    [0.012, 0.030],
                    [x, 0.075, -0.11],
                    [0.011, 0.014],
                    STEEL,
                );
            }
            section(
                &mut mesh,
                [0., 0.075, -0.14],
                [0.023, 0.007],
                [0., 0.075, -0.29],
                [0.015, 0.005],
                CYAN,
            );
        }
        WeaponId::Spread => {
            // Short, wide industrial projector. Twin orange side pods surround
            // three distinct dark bore sleeves and a transverse cooling comb.
            section(
                &mut mesh,
                [0., -0.027, 0.24],
                [0.064, 0.073],
                [0., -0.015, -0.05],
                [0.082, 0.079],
                GRAPHITE,
            );
            section(
                &mut mesh,
                [0., -0.018, 0.095],
                [0.088, 0.088],
                [0., -0.005, -0.31],
                [0.122, 0.077],
                STEEL,
            );
            section(
                &mut mesh,
                [0., -0.083, 0.035],
                [0.060, 0.11],
                [0., -0.075, -0.10],
                [0.052, 0.085],
                GRAPHITE,
            );
            for x in [-0.106, 0.106] {
                section(
                    &mut mesh,
                    [x * 0.80, -0.003, 0.075],
                    [0.029, 0.063],
                    [x, 0.011, -0.36],
                    [0.043, 0.071],
                    ORANGE,
                );
                section(
                    &mut mesh,
                    [x, 0.079, -0.15],
                    [0.018, 0.013],
                    [x, 0.079, -0.32],
                    [0.016, 0.010],
                    AMBER,
                );
            }
            for z in [-0.01, -0.09, -0.17] {
                section(
                    &mut mesh,
                    [0., 0.067, z],
                    [0.082, 0.019],
                    [0., 0.067, z - 0.027],
                    [0.079, 0.014],
                    GRAPHITE,
                );
            }
            for x in [-0.095_f32, 0., 0.095] {
                let z = tip(weapon)[2] + x.abs() * 0.38;
                section(
                    &mut mesh,
                    [x, 0.010, -0.30],
                    [0.047, 0.052],
                    [x, 0.010, z],
                    [0.042, 0.046],
                    GRAPHITE,
                );
                section(
                    &mut mesh,
                    [x, 0.010, z + 0.035],
                    [0.046, 0.050],
                    [x, 0.010, z + 0.016],
                    [0.046, 0.050],
                    AMBER,
                );
            }
        }
    }
    mesh
}

// Clockwise winding as seen from outside, matching the renderer's tested
// ground/top face convention. Dark bevels separate facets without normals.
fn section(
    out: &mut Vec<SceneVertex>,
    rear: Point,
    rear_size: [f32; 2],
    front: Point,
    front_size: [f32; 2],
    color: Point,
) {
    const RIM: [[f32; 2]; 8] = [
        [-0.65, 1.0],
        [0.65, 1.0],
        [1.0, 0.58],
        [1.0, -0.58],
        [0.65, -1.0],
        [-0.65, -1.0],
        [-1.0, -0.58],
        [-1.0, 0.58],
    ];
    const LIGHT: [f32; 8] = [1.0, 0.84, 0.68, 0.49, 0.39, 0.51, 0.71, 0.91];
    let ring =
        |center, size: [f32; 2]| RIM.map(|[x, y]| add(center, [x * size[0], y * size[1], 0.0]));
    let a = ring(rear, rear_size);
    let b = ring(front, front_size);
    let mut triangle = |p, q, r, light| {
        let col = mul(color, light);
        out.extend([p, q, r].map(|pos| SceneVertex { pos, col }));
    };
    for i in 0..8 {
        let j = (i + 1) % 8;
        triangle(a[i], b[i], b[j], LIGHT[i]);
        triangle(a[i], b[j], a[j], LIGHT[i]);
    }
    for i in 1..7 {
        triangle(a[0], a[i], a[i + 1], 0.75);
        triangle(b[0], b[i + 1], b[i], 0.68);
    }
}

/// A standing operator in yaw-local eye space. Torso, helmet and legs are
/// immutable; only the four arm segments follow the grip and fore-end.
pub(super) struct OperatorMesh {
    body: Vec<SceneVertex>,
    sleeve: Vec<SceneVertex>,
    gauntlet: Vec<SceneVertex>,
}

const CLOTH: Point = [0.16, 0.23, 0.26];
const ARMOR: Point = [0.40, 0.53, 0.55];
const STRIPE: Point = [0.25, 0.71, 0.59];

/// Bake an upright tapered band with independent x/z radii. This rotation
/// preserves the octagonal weapon section's winding.
fn band(
    out: &mut Vec<SceneVertex>,
    top: Point,
    top_size: [f32; 2],
    bottom: Point,
    bottom_size: [f32; 2],
    color: Point,
) {
    let first = out.len();
    let local = |[x, y, z]: Point| [x, -z, y];
    section(out, local(top), top_size, local(bottom), bottom_size, color);
    for v in &mut out[first..] {
        let [x, y, z] = v.pos;
        v.pos = [x, z, -y];
    }
}

impl OperatorMesh {
    pub fn new() -> Self {
        let mut body = Vec::new();
        // Body axis is relative to the encounter's 1.6 m eye. Leave air
        // between helmet and pauldrons, and between the staggered boots.
        band(
            &mut body,
            [0., 0.10, 0.],
            [0.083, 0.083],
            [0., 0.065, 0.],
            [0.114, 0.111],
            ARMOR,
        );
        band(
            &mut body,
            [0., 0.065, 0.],
            [0.114, 0.111],
            [0., -0.055, 0.],
            [0.11, 0.116],
            ARMOR,
        );
        band(
            &mut body,
            [0., -0.052, 0.],
            [0.11, 0.116],
            [0., -0.122, 0.],
            [0.076, 0.080],
            GRAPHITE,
        );
        section(
            &mut body,
            [0., 0.008, -0.085],
            [0.104, 0.036],
            [0., 0.009, -0.125],
            [0.086, 0.022],
            GRAPHITE,
        );
        band(
            &mut body,
            [0., -0.09, 0.],
            [0.071, 0.079],
            [0., -0.21, 0.],
            [0.09, 0.08],
            CLOTH,
        );
        band(
            &mut body,
            [0., -0.17, 0.],
            [0.150, 0.108],
            [0., -0.25, 0.01],
            [0.208, 0.133],
            ARMOR,
        );
        band(
            &mut body,
            [0., -0.25, 0.01],
            [0.208, 0.133],
            [0., -0.50, 0.01],
            [0.158, 0.117],
            ARMOR,
        );
        band(
            &mut body,
            [0., -0.50, 0.01],
            [0.158, 0.117],
            [0., -0.65, 0.],
            [0.143, 0.100],
            CLOTH,
        );
        band(
            &mut body,
            [0., -0.63, 0.],
            [0.15, 0.107],
            [0., -0.74, 0.],
            [0.17, 0.116],
            GRAPHITE,
        );
        // A small dorsal plate and a narrow identification stripe help read
        // the orientation of the torso independently of weapon selection.
        section(
            &mut body,
            [0., -0.34, 0.162],
            [0.089, 0.099],
            [0., -0.35, 0.105],
            [0.116, 0.13],
            GRAPHITE,
        );
        section(
            &mut body,
            [0., -0.305, 0.164],
            [0.056, 0.011],
            [0., -0.305, 0.157],
            [0.062, 0.011],
            STRIPE,
        );
        for side in [-1.0, 1.0] {
            let z = 0.056 * side;
            band(
                &mut body,
                [0.095 * side, -0.715, 0.],
                [0.091, 0.100],
                [0.123 * side, -1.07, z - 0.035],
                [0.073, 0.077],
                CLOTH,
            );
            band(
                &mut body,
                [0.123 * side, -1.05, z - 0.035],
                [0.077, 0.081],
                [0.128 * side, -1.165, z - 0.033],
                [0.070, 0.077],
                ARMOR,
            );
            band(
                &mut body,
                [0.128 * side, -1.16, z - 0.033],
                [0.067, 0.075],
                [0.145 * side, -1.47, z + 0.017],
                [0.052, 0.054],
                CLOTH,
            );
            band(
                &mut body,
                [0.145 * side, -1.44, z + 0.017],
                [0.058, 0.060],
                [0.145 * side, -1.60, z - 0.029],
                [0.067, 0.112],
                GRAPHITE,
            );
        }
        let mut sleeve = Vec::new();
        section(
            &mut sleeve,
            [0., 0., 0.04],
            [0.071, 0.075],
            [0., 0., -0.33],
            [0.080, 0.079],
            ARMOR,
        );
        section(
            &mut sleeve,
            [0., 0., -0.32],
            [0.080, 0.079],
            [0., 0., -1.0],
            [0.056, 0.059],
            CLOTH,
        );
        let mut gauntlet = Vec::new();
        section(
            &mut gauntlet,
            [0., 0., 0.04],
            [0.058, 0.060],
            [0., 0., -0.72],
            [0.053, 0.058],
            GRAPHITE,
        );
        section(
            &mut gauntlet,
            [0., 0., -0.70],
            [0.053, 0.058],
            [0., 0., -1.0],
            [0.045, 0.047],
            CLOTH,
        );
        Self {
            body,
            sleeve,
            gauntlet,
        }
    }

    /// Each forearm ends at a point on the equipped model. The elbow bends
    /// outward and down from the shoulder/wrist midpoint as aim changes.
    pub fn append(&self, out: &mut Vec<SceneVertex>, upright: Frame, mount: Frame) {
        upright.append(out, &self.body);
        for (side, grip) in [(1., [0., -0.110, 0.0]), (-1., [-0.035, -0.058, -0.23])] {
            let shoulder = upright.point([0.185 * side, -0.235, 0.0]);
            let wrist = mount.point(grip);
            let elbow = add(
                mul(add(shoulder, wrist), 0.5),
                upright.vector([0.085 * side, -0.18, 0.065]),
            );
            append_segment(out, &self.sleeve, shoulder, elbow);
            append_segment(out, &self.gauntlet, elbow, wrist);
        }
    }
}

fn append_segment(out: &mut Vec<SceneVertex>, mesh: &[SceneVertex], from: Point, to: Point) {
    let delta = sub(to, from);
    if let Some(direction) = unit(delta) {
        let mut basis = Frame::looking(from, direction);
        // Section vertices run from z=0 to z=-1. Scale length alone.
        basis.back = mul(delta, -1.);
        basis.append(out, mesh);
    }
}
