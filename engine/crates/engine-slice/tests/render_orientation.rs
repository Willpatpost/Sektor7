//! The orientation corrective's A6 regression: the REAL renderer
//! (instance, swapchain, pipelines, capture readback) under a software
//! Vulkan ICD, driven through the EXACT mvp chain the demos use
//! (`math::camera_view` → `math::perspective` → `math::mul`) and the
//! REAL production box emitter (`push_box`) — pixel-level proof that
//! the world renders right-side up: ground colors in the LOWER frame
//! region, an above-camera marker in the UPPER region, a visible TOP
//! surface, and the overlay glyphs still in the top-left corner (the
//! overlay's chain is untouched by the corrective).
//!
//! `#[ignore]`d like the other graphical smokes: it requires
//! `SDL_VIDEODRIVER=offscreen` plus a software Vulkan ICD (lavapipe).
//! The dev pod needs the user-local ICD setup (see render_overflow's
//! doc comment); CI-Linux runners install mesa-vulkan-drivers. Run
//! explicitly:
//!
//! ```sh
//! SDL_VIDEODRIVER=offscreen cargo test -p engine-slice --test render_orientation -- \
//!     --ignored --nocapture
//! ```

use engine_renderer::scene::push_box;
use engine_renderer::{
    CaptureFormat, PresentModePreference, RenderConfig, Renderer, ValidationPreference,
};
use engine_shell::{DriverPreference, ShellApp, ShellConfig};
use engine_slice::math;

/// Counts pixels in the horizontal band `y0..y1` (fractions of the
/// frame height, 0 = top row) matching `pixel`.
fn band_matches(
    frame: &engine_renderer::CapturedFrame,
    y0: f64,
    y1: f64,
    pixel: impl Fn([u8; 4]) -> bool,
) -> usize {
    let height = frame.height as usize;
    let width = frame.width as usize;
    let first = (y0 * height as f64) as usize;
    let last = (y1 * height as f64) as usize;
    let mut count = 0;
    for y in first..last.max(first + 1).min(height) {
        for x in 0..width {
            if pixel(frame.pixel_rgba(x as u32, y as u32)) {
                count += 1;
            }
        }
    }
    count
}

#[test]
#[ignore = "requires SDL_VIDEODRIVER=offscreen and a software Vulkan ICD (lavapipe); the dev pod needs the user-local ICD setup (see render_overflow)"]
fn world_renders_right_side_up_offscreen() {
    if std::env::var("SDL_VIDEODRIVER").is_err() {
        panic!(
            "SDL_VIDEODRIVER is not set — run this smoke under the offscreen driver \
             with a software Vulkan ICD (see the test's doc comment)"
        );
    }

    // The purpose-built orientation scene through the REAL emitter,
    // with colors chosen unique against the static demo scene (the
    // indexed scene always draws beneath the region geometry): strong
    // RED ground, a BLUE marker well above camera height, and a
    // MAGENTA cube whose TOP face (full-shade) is the only magenta in
    // the frame — the sides shade below the predicate.
    let mut region = Vec::new();
    // Ground: top surface at y = 0.004, above the static plate and
    // grid lines (their own 0.002 offset discipline) — no z-fighting.
    push_box(
        &mut region,
        [0.0, -1.0, 0.0],
        40.0,
        1.004,
        [0.85, 0.15, 0.10],
    );
    // The above-camera marker: y 4..7 at 8 m ahead (camera at y = 2).
    push_box(&mut region, [0.0, 4.0, -8.0], 1.0, 3.0, [0.10, 0.15, 0.85]);
    // The top-surface cube: clear of the static cube field (which
    // spans x −6.45..6.45), base 0.05 above the ground.
    push_box(
        &mut region,
        [-8.0, 0.05, -6.0],
        2.0,
        1.5,
        [0.95, 0.15, 0.85],
    );

    // The REAL mvp chain, exactly as the playable/player demos build
    // it: a level camera 2 m above the ground, looking −z with +y up.
    let eye = [0.0, 2.0, 6.0];
    let view = math::camera_view(eye, 0.0, 0.0, 0.0);
    let aspect = 16.0 / 9.0;
    let projection = math::perspective(std::f32::consts::FRAC_PI_3, aspect, 0.1, 200.0);
    let view_proj = math::mul(&projection, &view);

    // Renderer configuration, stated for checkability (the offscreen
    // smokes' discipline): FIFO via Auto, validation ON, overlay
    // enabled (the glyph-position assertions depend on it).
    let config = RenderConfig {
        present_mode: PresentModePreference::Auto,
        validation: ValidationPreference::On,
        replace_pipelines: false,
    };
    let mut app = ShellApp::new(ShellConfig {
        title: "Sektor7 orientation smoke".to_string(),
        width: 1280,
        height: 720,
        target_fps: 60,
        driver: DriverPreference::Real,
    })
    .expect("the offscreen shell initializes");
    let mut renderer = Renderer::new(app.window(), config)
        .expect("the renderer constructs under the software ICD");
    renderer.set_overlay_enabled(true);

    let mut frame = |renderer: &mut Renderer, capture: bool| {
        if capture {
            renderer.request_frame_capture();
        }
        let _snapshot = app.frame();
        renderer
            .render_frame(&view_proj, &region, Some("ORIENTATION"), app.window_size())
            .expect("the frame renders");
    };

    // Two warm-up frames (lazy allocations settle), then the capture.
    frame(&mut renderer, false);
    frame(&mut renderer, false);
    frame(&mut renderer, true);
    let captured = renderer
        .take_frame_capture()
        .expect("the capture readback completes")
        .expect("a capture completed for the captured frame");
    assert!(
        captured.width >= 640 && captured.height >= 360,
        "a real drawable"
    );
    assert_eq!(
        captured.format,
        CaptureFormat::Bgra8,
        "the lvp surface picks the renderer's preferred BGRA8 format (got {:?})",
        captured.format
    );
    let total = captured.width as usize * captured.height as usize;

    // 1. GROUND IN THE LOWER REGION: the bottom band is dominated by
    //    the red ground (a level camera puts the ground plane below
    //    the horizon — the mid-screen line — everywhere).
    let red = |p: [u8; 4]| {
        (p[0] as u16) > 150
            && (p[0] as u16) > (p[1] as u16) * 2 + 20
            && (p[0] as u16) > (p[2] as u16) * 2 + 20
    };
    let lower_red = band_matches(&captured, 0.70, 0.98, red);
    let lower_pixels = band_matches(&captured, 0.70, 0.98, |_| true);
    assert!(
        lower_red > lower_pixels / 2,
        "the ground colors the LOWER frame region: {lower_red} of {lower_pixels}"
    );
    println!(
        "[orientation-smoke] ground: {lower_red} red-dominant of {lower_pixels} pixels in the lower band"
    );

    // 2. NO GROUND IN THE UPPER REGION: above the horizon only the
    //    clear color and the marker — the upside-down defect put the
    //    ground HERE. The band reaches down to 35% (the marker sits
    //    at 20–31% for this camera geometry) but stays well above the
    //    mid-screen horizon of a level camera.
    let upper_red = band_matches(&captured, 0.02, 0.35, red);
    assert!(
        upper_red == 0,
        "the UPPER region carries no ground colors (the defect's signature): {upper_red}"
    );
    println!("[orientation-smoke] upper region ground colors: {upper_red} (must be 0)");

    // 3. THE ABOVE-CAMERA MARKER IN THE UPPER REGION: the blue box at
    //    y 4..7 projects above the screen center — the jump-moved-
    //    downward symptom's converse.
    let blue = |p: [u8; 4]| {
        (p[2] as u16) > 150
            && (p[2] as u16) > (p[0] as u16) + 40
            && (p[2] as u16) > (p[1] as u16) + 40
    };
    let upper_blue = band_matches(&captured, 0.02, 0.35, blue);
    assert!(
        upper_blue > total / 1000,
        "the above-camera marker colors the UPPER region: {upper_blue}"
    );
    println!("[orientation-smoke] marker: {upper_blue} blue-dominant pixels in the upper band");

    // 4. THE TOP SURFACE IS VISIBLE (winding/culling correct): a
    //    second captured frame with the camera ABOVE the cube looking
    //    down at it — the top face is face-on there, and only its
    //    full-shade magenta passes the predicate (the shaded sides
    //    fall below it). Under reversed winding this face is culled
    //    and the predicate finds nothing.
    let down_view = math::camera_view([-8.0, 5.0, -2.0], 0.0, -0.6, 0.0);
    let down_view_proj = math::mul(&projection, &down_view);
    renderer.request_frame_capture();
    let _snapshot = app.frame();
    renderer
        .render_frame(&down_view_proj, &region, None, app.window_size())
        .expect("the down-looking frame renders");
    let down = renderer
        .take_frame_capture()
        .expect("the capture readback completes")
        .expect("a capture completed for the down-looking frame");
    let magenta_top = |p: [u8; 4]| (p[0] as u16) > 200 && (p[2] as u16) > 150 && (p[1] as u16) < 80;
    let down_magenta = band_matches(&down, 0.0, 1.0, magenta_top);
    assert!(
        down_magenta > total / 200,
        "the cube's TOP surface renders face-on from above (winding/culling \
         correct): {down_magenta}",
    );
    println!(
        "[orientation-smoke] top surface: {down_magenta} magenta-top pixels in the down-looking frame"
    );

    // 5. THE OVERLAY IS UNCHANGED: the glyphs still land in the
    //    top-left corner (authored y-up NDC + the shader flip — the
    //    corrective never touches the overlay chain), and no glyph
    //    pixels appear below the first lines.
    let green = |p: [u8; 4]| {
        (p[1] as u16) > 200
            && (p[1] as u16) > (p[0] as u16) + 40
            && (p[1] as u16) > (p[2] as u16) + 30
    };
    let overlay_region = {
        let height = captured.height as usize;
        let width = captured.width as usize;
        let mut count = 0;
        for y in 0..((0.05 * height as f64) as usize).max(1) {
            for x in 0..width / 2 {
                if green(captured.pixel_rgba(x as u32, y as u32)) {
                    count += 1;
                }
            }
        }
        count
    };
    assert!(
        overlay_region > total / 4000,
        "the overlay glyphs render in the top-left region: {overlay_region}"
    );
    let lower_green = band_matches(&captured, 0.30, 1.0, green);
    assert!(
        lower_green == 0,
        "no overlay glyphs below the first lines: {lower_green}"
    );
    println!(
        "[orientation-smoke] overlay: {overlay_region} glyph pixels top-left, {lower_green} below (unchanged)"
    );

    // 6. The capture path itself is validation-clean (the barrier
    //    pair and the copy are ordinary Vulkan usage).
    let stats = renderer.stats();
    assert_eq!(
        stats.validation_errors, 0,
        "the captured frame is validation-clean"
    );
    println!(
        "[orientation-smoke] validation errors={} (clean) — EXECUTED through the real \
         render path with pixel readback, all assertions held",
        stats.validation_errors
    );
}
