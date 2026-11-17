//! Minimal column-major 4×4 matrix math for the Stage 2 camera.
//!
//! Camera/navigation **policy** lives in the application (Stage 2
//! authorization, item 2) — these are pure numeric helpers, not reusable
//! engine mechanisms.

/// Column-major 4×4 matrix (matches GLSL `mat4` memory layout and the
/// renderer's push-constant bytes).
pub type Mat4 = [[f32; 4]; 4];

pub fn identity() -> Mat4 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// Matrix product `a · b`.
pub fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [[0.0; 4]; 4];
    for (column, out_column) in out.iter_mut().enumerate() {
        for row in 0..4 {
            out_column[row] = a[0][row] * b[column][0]
                + a[1][row] * b[column][1]
                + a[2][row] * b[column][2]
                + a[3][row] * b[column][3];
        }
    }
    out
}

/// Right-handed perspective projection authoring a y-UP NDC (the
/// GLSL/WebGPU convention the shaders are written in), with depth
/// mapped to [0, 1] for `LESS` depth comparison. Vulkan's y-DOWN NDC
/// conversion happens exactly ONCE, at the shader boundary: the
/// build-time naga compilation emits the ADJUST_COORDINATE_SPACE
/// epilogue (an OpFNegate on `gl_Position.y` — pinned by
/// engine-renderer's
/// `emitted_vertex_shaders_carry_the_position_y_adjustment`). This
/// matrix must NOT negate Y itself: composing the two flips rendered
/// the world upside down (the owner-reported 2026-11-17 defect — a
/// world point above the camera landed at the BOTTOM of the screen).
pub fn perspective(fov_y_radians: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let focal = 1.0 / (fov_y_radians / 2.0).tan();
    let mut matrix = [[0.0; 4]; 4];
    matrix[0][0] = focal / aspect;
    matrix[1][1] = focal;
    matrix[2][2] = far / (near - far);
    matrix[2][3] = -1.0;
    matrix[3][2] = near * far / (near - far);
    matrix
}

fn rotation_x(radians: f32) -> Mat4 {
    let (cos, sin) = (radians.cos(), radians.sin());
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, cos, sin, 0.0],
        [0.0, -sin, cos, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn rotation_y(radians: f32) -> Mat4 {
    let (cos, sin) = (radians.cos(), radians.sin());
    [
        [cos, 0.0, -sin, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [sin, 0.0, cos, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn rotation_z(radians: f32) -> Mat4 {
    let (cos, sin) = (radians.cos(), radians.sin());
    [
        [cos, sin, 0.0, 0.0],
        [-sin, cos, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// Camera rotation R = Ry(yaw) · Rx(pitch) · Rz(roll), column-major.
/// Zero angles look down −Z with +Y up.
pub fn camera_rotation(yaw: f32, pitch: f32, roll: f32) -> Mat4 {
    mul(
        &mul(&rotation_y(yaw), &rotation_x(pitch)),
        &rotation_z(roll),
    )
}

/// Camera forward vector = R · (0, 0, −1).
pub fn camera_forward(yaw: f32, pitch: f32, roll: f32) -> [f32; 3] {
    let rotation = camera_rotation(yaw, pitch, roll);
    [-rotation[2][0], -rotation[2][1], -rotation[2][2]]
}

/// Camera right vector = R · (1, 0, 0).
pub fn camera_right(yaw: f32, pitch: f32, roll: f32) -> [f32; 3] {
    let rotation = camera_rotation(yaw, pitch, roll);
    [rotation[0][0], rotation[0][1], rotation[0][2]]
}

/// Camera view matrix = (T(position) · R)⁻¹ = Rᵀ · T(−position): the
/// inverse of the camera's world transform (rotate, then translate).
pub fn camera_view(position: [f32; 3], yaw: f32, pitch: f32, roll: f32) -> Mat4 {
    let rotation = camera_rotation(yaw, pitch, roll);
    let mut view = [[0.0; 4]; 4];
    // Upper-left 3×3 transpose.
    for column in 0..3 {
        for row in 0..3 {
            view[column][row] = rotation[row][column];
        }
    }
    // Translation: Rᵀ · (−position) — transposed indices relative to the
    // rotation read above (this is the term the round-trip test guards).
    for row in 0..3 {
        view[3][row] = -(rotation[row][0] * position[0]
            + rotation[row][1] * position[1]
            + rotation[row][2] * position[2]);
    }
    view[3][3] = 1.0;
    view
}

/// Percentile over unsorted samples (mirrors the renderer's stats helper).
pub fn percentile(samples: &[f64], quantile: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let index = ((quantile * sorted.len() as f64).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[index]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_multiplication_is_identity() {
        let matrix = identity();
        assert_eq!(mul(&matrix, &matrix), matrix);
    }

    #[test]
    fn vulkan_perspective_maps_mid_depth_into_unit_range() {
        // Camera at origin looking down −Z; a point 5 units away with
        // near 0.1 / far 100 must land inside (0, 1) after w-divide.
        let projection = perspective(std::f32::consts::FRAC_PI_3, 16.0 / 9.0, 0.1, 100.0);
        let point = [0.0, 0.0, -5.0, 1.0];
        let mut clip = [0.0f32; 4];
        for row in 0..4 {
            clip[row] = projection[0][row] * point[0]
                + projection[1][row] * point[1]
                + projection[2][row] * point[2]
                + projection[3][row] * point[3];
        }
        assert!(clip[3] > 0.0);
        let depth = clip[2] / clip[3];
        assert!(
            (0.0..=1.0).contains(&depth),
            "Vulkan depth must map to [0,1]: {depth}"
        );
    }

    #[test]
    fn camera_view_inverts_camera_transform() {
        // Camera world transform: p_world = R · p_local + position
        // (i.e. T(position) · R). The view matrix must be its inverse:
        // view · p_world = p_local for any local point.
        let position = [1.0, 2.0, 3.0];
        let view = camera_view(position, 0.2, -0.1, 0.05);
        let rotation = camera_rotation(0.2, -0.1, 0.05);
        // Local point: 0.5 right, 1.0 up, 2.0 forward (−Z is forward).
        let local = [0.5, 1.0, -2.0, 1.0];
        let mut world = [0.0f32; 4];
        for row in 0..4 {
            world[row] = rotation[0][row] * local[0]
                + rotation[1][row] * local[1]
                + rotation[2][row] * local[2];
        }
        world[0] += position[0];
        world[1] += position[1];
        world[2] += position[2];
        world[3] = 1.0;
        let mut transformed = [0.0f32; 4];
        for row in 0..4 {
            transformed[row] = view[0][row] * world[0]
                + view[1][row] * world[1]
                + view[2][row] * world[2]
                + view[3][row] * world[3];
        }
        assert!((transformed[0] - local[0]).abs() < 1e-5);
        assert!((transformed[1] - local[1]).abs() < 1e-5);
        assert!((transformed[2] - local[2]).abs() < 1e-5);
        assert!((transformed[3] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn zero_orientation_looks_down_negative_z() {
        assert_eq!(camera_forward(0.0, 0.0, 0.0), [0.0, 0.0, -1.0]);
        assert_eq!(camera_right(0.0, 0.0, 0.0), [1.0, 0.0, 0.0]);
    }

    /// Mat4 · vec4 (column-major convention, matching `mul`).
    fn transform(matrix: &Mat4, point: [f32; 4]) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        for row in 0..4 {
            out[row] = matrix[0][row] * point[0]
                + matrix[1][row] * point[1]
                + matrix[2][row] * point[2]
                + matrix[3][row] * point[3];
        }
        out
    }

    /// The orientation corrective's A3 regression: a world point ABOVE
    /// a level camera projects toward the TOP of the screen through
    /// the REAL render chain, and a point below projects downward.
    /// The chain modeled here is the one the GPU executes:
    /// `view_proj = perspective · view` (the mvp push constant), the
    /// w-divide, and the compiler-generated shader-boundary Y flip —
    /// naga's ADJUST_COORDINATE_SPACE epilogue, an OpFNegate on
    /// `gl_Position.y` whose presence in the build-emitted SPIR-V is
    /// pinned by engine-renderer's
    /// `emitted_vertex_shaders_carry_the_position_y_adjustment`. The
    /// projection therefore authors a y-UP NDC (the GLSL/WebGPU
    /// convention the shaders are written in) and the flip converts
    /// it to Vulkan's y-DOWN NDC, where +1 is the BOTTOM of the
    /// screen and a positive-height viewport maps NDC −1 to pixel row
    /// 0 (the top). The defect this test was written fail-first
    /// against: `perspective()` used to negate Y itself (a SECOND
    /// flip), so world-up landed at the bottom of the screen — the
    /// owner-observed upside-down world.
    #[test]
    fn world_up_projects_to_screen_top_through_the_shader_flip_chain() {
        let eye = [0.0, 1.6, 0.0];
        let view = camera_view(eye, 0.0, 0.0, 0.0); // level, looking −z, +y up
        let projection = perspective(std::f32::consts::FRAC_PI_3, 16.0 / 9.0, 0.1, 200.0);
        let view_proj = mul(&projection, &view);
        let (width, height) = (1280.0f32, 720.0f32);

        // The full chain: clip → w-divide → the shader's Y flip →
        // Vulkan NDC → the positive-height viewport (row 0 = top).
        let screen = |world: [f32; 3]| -> (f32, f32) {
            let clip = transform(&view_proj, [world[0], world[1], world[2], 1.0]);
            let ndc_y_authored = clip[1] / clip[3];
            let vulkan_ndc_y = -ndc_y_authored; // the shader-boundary flip
            let pixel_x = (clip[0] / clip[3] + 1.0) / 2.0 * width;
            let pixel_y = (vulkan_ndc_y + 1.0) / 2.0 * height;
            (pixel_x, pixel_y)
        };

        // A point 1.6 m ABOVE the eye, 6 m ahead — top of the screen.
        let (x, y) = screen([0.0, 3.2, -6.0]);
        assert!(
            y < height / 2.0,
            "a world point above the camera projects to the TOP half: y={y}"
        );
        assert!((0.0..width).contains(&x), "on-screen horizontally: x={x}");

        // A point 1.6 m BELOW the eye — bottom of the screen.
        let (x, y) = screen([0.0, 0.0, -6.0]);
        assert!(
            y > height / 2.0,
            "a world point below the camera projects to the BOTTOM half: y={y}"
        );
        assert!((0.0..width).contains(&x), "on-screen horizontally: x={x}");

        // Symmetric distances land symmetrically: the same magnitude
        // above and below mirrors around the screen center.
        let above = screen([0.0, 3.2, -6.0]).1;
        let below = screen([0.0, 0.0, -6.0]).1;
        assert!(
            (above + below - height).abs() < 1.0,
            "equal up/down offsets mirror around the screen center: {above} + {below} vs {height}"
        );
    }
}
