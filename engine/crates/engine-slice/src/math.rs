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

/// Right-handed perspective projection for Vulkan: NDC y points down
/// (y flipped) and depth is mapped to [0, 1] for `LESS` depth comparison.
pub fn perspective(fov_y_radians: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let focal = 1.0 / (fov_y_radians / 2.0).tan();
    let mut matrix = [[0.0; 4]; 4];
    matrix[0][0] = focal / aspect;
    matrix[1][1] = -focal;
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
}
