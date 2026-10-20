//! The Stage 2 demo scene: a ground grid and a field of shaded cubes with
//! deliberately overlapping depth demonstrators. Scene *content* is first
//! application data, not an engine mechanism — the renderer only knows
//! the vertex format.

use crate::config::FRAMES_IN_FLIGHT;

/// Scene vertex: position + color (24 bytes, matches `scene.vert`).
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct SceneVertex {
    pub pos: [f32; 3],
    pub col: [f32; 3],
}

/// Builds the scene. Depth correctness is exercised by: (a) a 40×40
/// ground quad at y=0; (b) a 5×5 grid of height-varying cubes (the camera
/// can move among them); (c) three overlapping cubes near the origin at
/// different depths and heights, plus one floating cube — from any angle,
/// near geometry must occlude far geometry.
pub fn build_scene() -> (Vec<SceneVertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    let quad = |vertices: &mut Vec<SceneVertex>,
                indices: &mut Vec<u32>,
                a: [f32; 3],
                b: [f32; 3],
                c: [f32; 3],
                d: [f32; 3],
                color: [f32; 3]| {
        let base = vertices.len() as u32;
        for pos in [a, b, c, d] {
            vertices.push(SceneVertex { pos, col: color });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };

    // Ground: dark plate plus a grid of thin teal lines every 2 units.
    quad(
        &mut vertices,
        &mut indices,
        [-20.0, 0.0, -20.0],
        [20.0, 0.0, -20.0],
        [20.0, 0.0, 20.0],
        [-20.0, 0.0, 20.0],
        [0.05, 0.05, 0.08],
    );
    let line_color = [0.10, 0.45, 0.55];
    for i in -10..=10i32 {
        let x = i as f32 * 2.0;
        quad(
            &mut vertices,
            &mut indices,
            [x - 0.02, 0.002, -20.0],
            [x + 0.02, 0.002, -20.0],
            [x + 0.02, 0.002, 20.0],
            [x - 0.02, 0.002, 20.0],
            line_color,
        );
        quad(
            &mut vertices,
            &mut indices,
            [-20.0, 0.002, x - 0.02],
            [20.0, 0.002, x - 0.02],
            [20.0, 0.002, x + 0.02],
            [-20.0, 0.002, x + 0.02],
            line_color,
        );
    }

    // Cube field: 5×5, height by distance pattern, blue→magenta ramp.
    for gx in -2..=2i32 {
        for gz in -2..=2i32 {
            let cx = gx as f32 * 3.0;
            let cz = gz as f32 * 3.0;
            let height = 0.6 + ((gx + gz).unsigned_abs() as f32) * 0.45;
            let t = height / 2.4;
            let color = [0.15 + 0.35 * t, 0.20 + 0.15 * t, 0.55 + 0.40 * t];
            push_cube(
                &mut vertices,
                &mut indices,
                [cx, 0.0, cz],
                0.9,
                height,
                color,
            );
        }
    }

    // Depth demonstrators near the origin: near cube must occlude the far
    // one, and the floating cube must sort correctly against both.
    push_cube(
        &mut vertices,
        &mut indices,
        [0.0, 0.0, -2.0],
        1.4,
        1.4,
        [0.85, 0.25, 0.20],
    );
    push_cube(
        &mut vertices,
        &mut indices,
        [0.6, 0.3, 0.8],
        1.4,
        1.4,
        [0.20, 0.35, 0.85],
    );
    push_cube(
        &mut vertices,
        &mut indices,
        [-0.5, 2.2, -0.6],
        0.8,
        0.8,
        [0.90, 0.75, 0.20],
    );

    (vertices, indices)
}

/// Appends an axis-aligned box from (x,z) with the given footprint and
/// height. Per-face shading multipliers fake lighting so depth ordering is
/// visible without a material system.
fn push_cube(
    vertices: &mut Vec<SceneVertex>,
    indices: &mut Vec<u32>,
    base: [f32; 3],
    footprint: f32,
    height: f32,
    color: [f32; 3],
) {
    let [x, y, z] = base;
    let h = footprint / 2.0;
    let top = y + height;
    let shade = |mult: f32| [color[0] * mult, color[1] * mult, color[2] * mult];

    let base_index = vertices.len() as u32;
    let push_face = |vertices: &mut Vec<SceneVertex>,
                     indices: &mut Vec<u32>,
                     quad: [[f32; 3]; 4],
                     color: [f32; 3]| {
        let start = vertices.len() as u32;
        for pos in quad {
            vertices.push(SceneVertex { pos, col: color });
        }
        indices.extend_from_slice(&[start, start + 1, start + 2, start, start + 2, start + 3]);
    };

    // Top (CCW from above) and bottom.
    push_face(
        vertices,
        indices,
        [
            [x - h, top, z + h],
            [x - h, top, z - h],
            [x + h, top, z - h],
            [x + h, top, z + h],
        ],
        shade(1.0),
    );
    push_face(
        vertices,
        indices,
        [
            [x - h, y, z - h],
            [x - h, y, z + h],
            [x + h, y, z + h],
            [x + h, y, z - h],
        ],
        shade(0.45),
    );
    // Sides.
    push_face(
        vertices,
        indices,
        [
            [x - h, y, z - h],
            [x - h, top, z - h],
            [x + h, top, z - h],
            [x + h, y, z - h],
        ],
        shade(0.80),
    );
    push_face(
        vertices,
        indices,
        [
            [x + h, y, z + h],
            [x + h, top, z + h],
            [x - h, top, z + h],
            [x - h, y, z + h],
        ],
        shade(0.80),
    );
    push_face(
        vertices,
        indices,
        [
            [x - h, y, z + h],
            [x - h, top, z + h],
            [x - h, top, z - h],
            [x - h, y, z - h],
        ],
        shade(0.65),
    );
    push_face(
        vertices,
        indices,
        [
            [x + h, y, z - h],
            [x + h, top, z - h],
            [x + h, top, z + h],
            [x + h, y, z + h],
        ],
        shade(0.65),
    );
    debug_assert_eq!(vertices.len() as u32 - base_index, 24);
}

/// Appends an axis-aligned box as NON-INDEXED triangle vertices (36 per
/// box — six per face, two triangles each). The Stage 4 graphical
/// integration (region areas + NPC render proxies) draws dynamic
/// per-frame geometry through the scene pipeline without an index
/// buffer, mirroring the overlay's per-slot dynamic path. Same face
/// order, winding, and shading convention as [`push_cube`].
pub fn push_box(
    vertices: &mut Vec<SceneVertex>,
    base: [f32; 3],
    footprint: f32,
    height: f32,
    color: [f32; 3],
) {
    let [x, y, z] = base;
    let h = footprint / 2.0;
    let top = y + height;
    let shade = |mult: f32| [color[0] * mult, color[1] * mult, color[2] * mult];
    let face = |vertices: &mut Vec<SceneVertex>, quad: [[f32; 3]; 4], color: [f32; 3]| {
        // The indexed convention flattened: (a, b, c) and (a, c, d).
        for pos in [quad[0], quad[1], quad[2], quad[0], quad[2], quad[3]] {
            vertices.push(SceneVertex { pos, col: color });
        }
    };
    // Top (CCW from above) and bottom.
    face(
        vertices,
        [
            [x - h, top, z + h],
            [x - h, top, z - h],
            [x + h, top, z - h],
            [x + h, top, z + h],
        ],
        shade(1.0),
    );
    face(
        vertices,
        [
            [x - h, y, z - h],
            [x - h, y, z + h],
            [x + h, y, z + h],
            [x + h, y, z - h],
        ],
        shade(0.45),
    );
    // Sides.
    face(
        vertices,
        [
            [x - h, y, z - h],
            [x - h, top, z - h],
            [x + h, top, z - h],
            [x + h, y, z - h],
        ],
        shade(0.80),
    );
    face(
        vertices,
        [
            [x + h, y, z + h],
            [x + h, top, z + h],
            [x - h, top, z + h],
            [x - h, y, z + h],
        ],
        shade(0.80),
    );
    face(
        vertices,
        [
            [x - h, y, z + h],
            [x - h, top, z + h],
            [x - h, top, z - h],
            [x - h, y, z - h],
        ],
        shade(0.65),
    );
    face(
        vertices,
        [
            [x + h, y, z - h],
            [x + h, top, z - h],
            [x + h, top, z + h],
            [x + h, y, z + h],
        ],
        shade(0.65),
    );
}

/// Dynamic region-scene vertex budget (Stage 4 graphical integration):
/// the worst-case single frame is all eight regions Full — eight area
/// plates plus 1000 NPC proxy boxes, 36 non-indexed vertices each —
/// plus margin. The disjoint per-slot pattern divides this total by
/// `FRAMES_IN_FLIGHT`, so the total carries that factor; one frame's
/// output must fit one slot.
pub fn region_scene_capacity_vertices() -> u32 {
    ((8 + 1000) * 36 + 512) * FRAMES_IN_FLIGHT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_is_nonempty_and_consistent() {
        let (vertices, indices) = build_scene();
        // 25-cube field (600 verts) plus ground, grid, depth-demo and
        // floating cubes; the bar guards against an accidentally empty
        // scene, not scene richness.
        assert!(
            vertices.len() >= 700,
            "substantial scene: {}",
            vertices.len()
        );
        assert_eq!(vertices.len() % 4, 0, "faces are quads");
        assert_eq!(indices.len() % 6, 0, "two triangles per quad");
        assert!(
            indices.iter().all(|i| (*i as usize) < vertices.len()),
            "indices in range"
        );
        // 25 field cubes + 3 demonstrators = 28 cubes × 24 verts.
        let cube_vertices = 28 * 24;
        assert!(vertices.len() > cube_vertices, "plus ground and grid");
    }
}
