//! The Cycle-3 P3 scene-separation regression: the REAL renderer
//! (instance, swapchain, pipelines, capture readback) under a software
//! Vulkan ICD, pixel-level proof that the reusable renderer does not
//! impose the Stage-2 demonstration content on a game's scene —
//!
//! - **D1** a renderer constructed with the DEFAULT demo-scene
//!   preference and presented an EMPTY dynamic region renders exactly
//!   the clear color in every pixel — no ground plate, no grid, no
//!   cube field;
//! - **D2** the playable demo's own frame — the demo's own vertex
//!   builder and mvp chain, from the settled spawn pose — carries
//!   ZERO legacy signatures (the teal grid lines, the blue/magenta
//!   cube field the owner saw in the gatehouse);
//! - **D3** a second renderer constructed with the EXPLICIT opt-in
//!   (`demo_scene: Included`) renders the original demonstration —
//!   the legacy cube field is back, per-instance (no global state);
//! - **D4** the pipeline-REPLACEMENT lifecycle (the
//!   `--replace-pipelines` verification flag plus a one-frame size
//!   report — exactly how a real resize reaches the renderer) is
//!   built with the LIVE preference in BOTH directions: an omitted
//!   renderer stays omitted across the replacement, and an opted-in
//!   renderer keeps the demonstration.
//!
//! Fail-first discipline: the D1/D2 assertions were run against the
//! baseline renderer (the static scene uploaded and drawn
//! unconditionally) and FAILED there (D1: 500,162 of 921,600 clear
//! pixels; D2: 6,333 grid-teal and 199,723 cube-field blue) — the
//! separation makes them pass without weakening either assertion.
//! The D3 direction is corroborated by the render_orientation /
//! render_overflow smokes, whose pinned evidence the opt-in keeps
//! byte-identical.
//!
//! `#[ignore]`d like the other graphical smokes: it requires
//! `SDL_VIDEODRIVER=offscreen` plus a software Vulkan ICD (lavapipe).
//! The dev pod needs the user-local ICD setup (see render_overflow's
//! doc comment); CI-Linux runners install mesa-vulkan-drivers. Run
//! explicitly:
//!
//! ```sh
//! SDL_VIDEODRIVER=offscreen cargo test -p engine-slice --test render_scene_separation -- \
//!     --ignored --nocapture
//! ```

use engine_core::MoveIntent;
use engine_renderer::scene::SceneVertex;
use engine_renderer::{
    CaptureFormat, DemoScenePreference, PresentModePreference, RenderConfig, Renderer,
    ValidationPreference,
};
use engine_shell::{DriverPreference, ShellApp, ShellConfig};
use engine_slice::math;
use engine_slice::playable_demo::{PlayableDemo, build_scene_vertices_into};

/// Whole-frame pixel counter (the orientation smoke's band helper,
/// full-band).
fn frame_matches(frame: &engine_renderer::CapturedFrame, pixel: impl Fn([u8; 4]) -> bool) -> usize {
    let mut count = 0;
    for y in 0..frame.height {
        for x in 0..frame.width {
            if pixel(frame.pixel_rgba(x, y)) {
                count += 1;
            }
        }
    }
    count
}

/// The clear color `[0.02, 0.02, 0.04]` quantized to BGRA8 — the
/// surface is UNORM (the SRGB_NONLINEAR tag names the color space,
/// it does not re-encode the stored bytes), so the bytes are
/// `[5, 5, 10, 255]` with ±3 rounding tolerance.
fn is_clear_color(p: [u8; 4]) -> bool {
    p[0].abs_diff(5) <= 3 && p[1].abs_diff(5) <= 3 && p[2].abs_diff(10) <= 3 && p[3] > 250
}

/// The legacy Stage-2 grid signature: the teal lines
/// `[0.10, 0.45, 0.55]` → `[26, 115, 140]`. Unique against every
/// gatehouse and floor color (none carries green ≈ 115 with blue
/// 13+ above it at this red level).
fn is_legacy_grid_teal(p: [u8; 4]) -> bool {
    let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
    (13..=40).contains(&r) && (103..=127).contains(&g) && (128..=152).contains(&b) && b >= g + 13
}

/// The legacy cube-field signature: the blue/magenta field cubes and
/// the blue depth demonstrator — on every shaded face the blue
/// dominates red by 40+ and green by 60+ u8 counts. The gatehouse's
/// own blue (the goal pad) sits behind the CLOSED gate at the spawn
/// pose; the floor and every wall color fail by wide margins.
fn is_legacy_cube_blue(p: [u8; 4]) -> bool {
    let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
    b >= r + 40 && b >= g + 60
}

#[test]
#[ignore = "requires SDL_VIDEODRIVER=offscreen and a software Vulkan ICD (lavapipe); the dev pod needs the user-local ICD setup (see render_overflow)"]
fn the_default_omits_and_the_explicit_opt_in_preserves_the_demo_scene() {
    if std::env::var("SDL_VIDEODRIVER").is_err() {
        panic!(
            "SDL_VIDEODRIVER is not set — run this smoke under the offscreen driver \
             with a software Vulkan ICD (see the test's doc comment)"
        );
    }

    // The DEFAULT demo-scene preference — the `..RenderConfig::default()`
    // spread states the policy under test: whatever the renderer's
    // defaults are, they must not impose demonstration content — with
    // the offscreen smokes' checkability discipline (FIFO via Auto,
    // validation ON) and the D4 verification flag ON (orthogonal to
    // the preference under test: it only forces the pipeline
    // replacement on every recreation). Overlay disabled — the D1
    // frame must be pure clear color, no glyphs.
    let config = RenderConfig {
        present_mode: PresentModePreference::Auto,
        validation: ValidationPreference::On,
        replace_pipelines: true,
        ..RenderConfig::default()
    };
    let mut app = ShellApp::new(ShellConfig {
        title: "Sektor7 scene-separation smoke".to_string(),
        width: 1280,
        height: 720,
        target_fps: 60,
        driver: DriverPreference::Real,
    })
    .expect("the offscreen shell initializes");
    let mut renderer = Renderer::new(app.window(), config)
        .expect("the renderer constructs under the software ICD");
    renderer.set_overlay_enabled(false);

    // The orientation smoke's proven camera: a level eye 2 m above
    // the origin ground looking −z — from there the legacy field
    // fills the lower frame (the fail-first shape).
    let view = math::camera_view([0.0, 2.0, 6.0], 0.0, 0.0, 0.0);
    let projection = math::perspective(std::f32::consts::FRAC_PI_3, 16.0 / 9.0, 0.1, 200.0);
    let view_proj = math::mul(&projection, &view);

    // One render helper for the whole test (SDL's main-thread rule
    // also means this binary runs exactly one SDL-initializing test).
    // `app` is a parameter, not a capture — the window stays borrowable
    // between calls (the second renderer's construction needs it).
    // `window_size` is the caller's report — the app owns window
    // state, so a differing report is exactly how a real resize
    // reaches the renderer.
    let render_with = |app: &mut ShellApp,
                       renderer: &mut Renderer,
                       frame_view_proj: &[[f32; 4]; 4],
                       scene: &[SceneVertex],
                       window_size: (u32, u32)| {
        let _snapshot = app.frame();
        renderer
            .render_frame(frame_view_proj, scene, None, window_size)
            .expect("the frame renders");
    };
    let real_size = app.window_size();
    // The resize-test pattern (render_demo's `--simulate-resize`): one
    // frame reports a different size, forcing the swapchain
    // recreation; with `replace_pipelines` that recreation replaces
    // the pipeline state.
    let fake_size = (real_size.0 + 128, real_size.1 + 72);

    // D1: the default renderer + an EMPTY dynamic region — every
    // pixel must be exactly the clear color. Two warm-up frames (the
    // lazy allocations settle), then the capture.
    render_with(&mut app, &mut renderer, &view_proj, &[], real_size);
    render_with(&mut app, &mut renderer, &view_proj, &[], real_size);
    renderer
        .request_frame_capture()
        .expect("the D1 capture request is accepted");
    render_with(&mut app, &mut renderer, &view_proj, &[], real_size);
    let captured = renderer
        .take_frame_capture()
        .expect("the D1 readback completes")
        .expect("a D1 capture completed");
    assert!(
        captured.width >= 640 && captured.height >= 360,
        "a real drawable"
    );
    assert_eq!(
        captured.format,
        CaptureFormat::Bgra8,
        "the channel order the predicates assume"
    );
    let total = captured.width as usize * captured.height as usize;
    let clear = frame_matches(&captured, is_clear_color);
    println!("[separation-smoke] D1 default + empty region: {clear}/{total} clear-color pixels");

    // D2: the playable demo's own frame — the demo's own vertex
    // builder (the gatehouse + the courtyard floor), the demo's own
    // mvp chain, from the settled spawn pose. ZERO legacy
    // signatures: no teal grid, no cube-field blue.
    let mut demo = PlayableDemo::new();
    for _ in 0..200 {
        if demo.driver.state().on_ground {
            break;
        }
        demo.tick(MoveIntent::default(), false);
    }
    assert!(
        demo.driver.state().on_ground,
        "the spawn settles on the courtyard ground"
    );
    let pose = demo.camera_pose();
    let playable_view = math::camera_view(pose.position, pose.yaw, pose.pitch, pose.roll);
    let playable_view_proj = math::mul(&projection, &playable_view);
    let mut playable_scene = Vec::new();
    build_scene_vertices_into(&mut playable_scene, &demo);
    assert!(
        !playable_scene.is_empty(),
        "the demo's own geometry is non-empty"
    );

    render_with(
        &mut app,
        &mut renderer,
        &playable_view_proj,
        &playable_scene,
        real_size,
    );
    render_with(
        &mut app,
        &mut renderer,
        &playable_view_proj,
        &playable_scene,
        real_size,
    );
    renderer
        .request_frame_capture()
        .expect("the D2 capture request is accepted");
    render_with(
        &mut app,
        &mut renderer,
        &playable_view_proj,
        &playable_scene,
        real_size,
    );
    let playable = renderer
        .take_frame_capture()
        .expect("the D2 readback completes")
        .expect("a D2 capture completed");
    let playable_total = playable.width as usize * playable.height as usize;
    let teal = frame_matches(&playable, is_legacy_grid_teal);
    let cube_blue = frame_matches(&playable, is_legacy_cube_blue);
    println!(
        "[separation-smoke] D2 playable own-geometry frame: legacy grid-teal {teal}, \
         legacy cube-blue {cube_blue} of {playable_total} pixels (both must be 0)"
    );

    // D4, omitted direction: force the pipeline-REPLACEMENT path (the
    // fake-size frame recreates and replaces; the real-size frame
    // recreates and replaces back) — the replacement must rebuild
    // with the LIVE preference, so the omitted renderer must still
    // render exactly the clear color on an empty region.
    render_with(&mut app, &mut renderer, &view_proj, &[], fake_size);
    render_with(&mut app, &mut renderer, &view_proj, &[], real_size);
    renderer
        .request_frame_capture()
        .expect("the D4-omitted capture request is accepted");
    render_with(&mut app, &mut renderer, &view_proj, &[], real_size);
    let replaced = renderer
        .take_frame_capture()
        .expect("the D4-omitted readback completes")
        .expect("a D4-omitted capture completed");
    let replaced_total = replaced.width as usize * replaced.height as usize;
    let clear_after_replacement = frame_matches(&replaced, is_clear_color);
    println!(
        "[separation-smoke] D4 omitted across pipeline replacement: \
         {clear_after_replacement}/{replaced_total} clear-color pixels (recreations: {})",
        renderer.stats().swapchain_recreations
    );
    assert_eq!(
        renderer.stats().validation_errors,
        0,
        "the omitted-direction run is validation-clean"
    );

    // The per-instance opt-in: the omitted renderer is dropped and a
    // SECOND renderer on the same window constructs with the explicit
    // `Included` preference — no global state can carry the omission
    // over (D3), and the replacement keeps the demonstration (D4,
    // included direction).
    drop(renderer);
    let opt_in_config = RenderConfig {
        present_mode: PresentModePreference::Auto,
        validation: ValidationPreference::On,
        replace_pipelines: true,
        demo_scene: DemoScenePreference::Included,
    };
    let mut opt_in_renderer = Renderer::new(app.window(), opt_in_config)
        .expect("the opt-in renderer constructs under the software ICD");
    opt_in_renderer.set_overlay_enabled(false);

    // D3: the explicit opt-in renders the ORIGINAL demonstration —
    // the legacy cube field stands in the frame again (the same
    // camera, the same empty region: everything visible is the
    // opt-in's own content).
    render_with(&mut app, &mut opt_in_renderer, &view_proj, &[], real_size);
    render_with(&mut app, &mut opt_in_renderer, &view_proj, &[], real_size);
    opt_in_renderer
        .request_frame_capture()
        .expect("the D3 capture request is accepted");
    render_with(&mut app, &mut opt_in_renderer, &view_proj, &[], real_size);
    let opt_in = opt_in_renderer
        .take_frame_capture()
        .expect("the D3 readback completes")
        .expect("a D3 capture completed");
    let opt_in_total = opt_in.width as usize * opt_in.height as usize;
    let opt_in_cube_blue = frame_matches(&opt_in, is_legacy_cube_blue);
    println!(
        "[separation-smoke] D3 explicit opt-in + empty region: {opt_in_cube_blue} \
         legacy cube-blue of {opt_in_total} pixels (the demonstration is back)"
    );

    // D4, included direction: the same replacement forcing — the
    // opted-in renderer must KEEP the demonstration across the
    // replacement.
    render_with(&mut app, &mut opt_in_renderer, &view_proj, &[], fake_size);
    render_with(&mut app, &mut opt_in_renderer, &view_proj, &[], real_size);
    opt_in_renderer
        .request_frame_capture()
        .expect("the D4-included capture request is accepted");
    render_with(&mut app, &mut opt_in_renderer, &view_proj, &[], real_size);
    let opt_in_replaced = opt_in_renderer
        .take_frame_capture()
        .expect("the D4-included readback completes")
        .expect("a D4-included capture completed");
    let opt_in_replaced_total = opt_in_replaced.width as usize * opt_in_replaced.height as usize;
    let opt_in_cube_blue_after_replacement = frame_matches(&opt_in_replaced, is_legacy_cube_blue);
    println!(
        "[separation-smoke] D4 included across pipeline replacement: \
         {opt_in_cube_blue_after_replacement} legacy cube-blue of \
         {opt_in_replaced_total} pixels (recreations: {})",
        opt_in_renderer.stats().swapchain_recreations
    );
    assert_eq!(
        opt_in_renderer.stats().validation_errors,
        0,
        "the opt-in run is validation-clean"
    );
    println!(
        "[separation-smoke] validation errors 0 on both renderers — EXECUTED through \
         the real render path with pixel readback"
    );

    // The assertions, AFTER all the evidence is printed (one failing
    // run reports every probe's counts).
    assert!(
        clear == total,
        "D1 — the default renderer imposes NO demo content: {clear} of {total} \
         pixels are the clear color (the rest is imposed scene content)"
    );
    assert!(
        teal == 0,
        "D2a — the playable frame carries no legacy grid-teal: {teal} pixels"
    );
    assert!(
        cube_blue == 0,
        "D2b — the playable frame carries no legacy cube-field blue: {cube_blue} pixels"
    );
    assert!(
        clear_after_replacement == replaced_total,
        "D4 (omitted) — the pipeline replacement rebuilds with the LIVE preference: \
         {clear_after_replacement} of {replaced_total} clear-color pixels after the \
         replacement (an omitted renderer must not gain demo content by recreating)"
    );
    assert!(
        opt_in_cube_blue > opt_in_total / 1000,
        "D3 — the explicit opt-in renders the original demonstration: \
         {opt_in_cube_blue} legacy cube-blue pixels (expected a substantial field)"
    );
    assert!(
        opt_in_cube_blue_after_replacement > opt_in_replaced_total / 1000,
        "D4 (included) — the pipeline replacement keeps the demonstration: \
         {opt_in_cube_blue_after_replacement} legacy cube-blue pixels after the \
         replacement (an opted-in renderer must not lose it by recreating)"
    );
}
