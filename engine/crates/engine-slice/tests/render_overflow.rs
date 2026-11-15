//! F-03 Step 1 oversize smoke: the REAL render path under a software
//! Vulkan ICD — the actual `Renderer` (instance, swapchain,
//! persistently-mapped slot buffers, command buffers) driven exactly as
//! `render_demo` drives it, synthetic ONLY in input sizes.
//!
//! `#[ignore]`d like the s6 graphical smoke: it requires
//! `SDL_VIDEODRIVER=offscreen` plus a software Vulkan ICD (lavapipe).
//! The dev pod currently has NO Vulkan loader (verified 2026-11-14), so
//! local execution needs the user-local ICD setup; CI-Linux runners
//! install mesa-vulkan-drivers (see the workflow's render smokes).
//! The Cycle-1 corrective R1 (the ChatGPT review 2026-11-15) adds an
//! ENFORCED Linux CI step that executes this test with `--ignored
//! --nocapture` — the per-stage `println!` evidence below is what the
//! CI log must show (actual execution with exact counter values, not
//! a compile or an ignored-skip). Run explicitly:
//!
//! ```sh
//! SDL_VIDEODRIVER=offscreen cargo test -p engine-slice --test render_overflow -- \
//!     --ignored --nocapture
//! ```

use engine_renderer::scene::{SceneVertex, push_box};
use engine_renderer::{PresentModePreference, RenderConfig, Renderer, ValidationPreference};
use engine_shell::{DriverPreference, ShellApp, ShellConfig};

use engine_slice::render_demo::{TruncationReport, truncation_report_lines};

/// Any valid view-projection works — the clamps sit upstream of
/// rasterization, so the matrix content is irrelevant to the capacity
/// decision.
const IDENTITY_VIEW_PROJ: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// The oversize region scene: 1,050 `push_box` boxes × 36 verts =
/// 37,800 supplied vs the 36,800 per-slot capacity → exactly 1,000
/// omitted per frame. The mod-36 derivation is auditable (the P1
/// challenge F-1): `push_box` emits 36 verts per call and 36,800 ≡ 8
/// (mod 36), so every box-built supply omits ≡ 28 (mod 36) — 1,050
/// boxes is the round 37,800 that omits the round 1,000.
fn oversize_region_scene() -> Vec<SceneVertex> {
    let mut vertices = Vec::new();
    for index in 0..1_050_u32 {
        let x = -32.0 + (index % 35) as f32 * 2.0;
        let z = -32.0 + (index / 35) as f32 * 2.0;
        push_box(&mut vertices, [x, 0.0, z], 0.5, 1.0, [0.85, 0.45, 0.20]);
    }
    assert_eq!(vertices.len(), 1_050 * 36, "36 verts per push_box call");
    vertices
}

/// An in-capacity control scene (100 boxes = 3,600 verts — far below
/// the 36,800 slot capacity).
fn in_capacity_region_scene() -> Vec<SceneVertex> {
    let mut vertices = Vec::new();
    for index in 0..100_u32 {
        let x = -10.0 + (index % 10) as f32 * 2.0;
        let z = -10.0 + (index / 10) as f32 * 2.0;
        push_box(&mut vertices, [x, 0.0, z], 0.5, 1.0, [0.20, 0.45, 0.85]);
    }
    vertices
}

#[test]
#[ignore = "requires SDL_VIDEODRIVER=offscreen and a software Vulkan ICD (lavapipe); the dev pod has no Vulkan loader (verified 2026-11-14)"]
fn oversized_region_and_overlay_report_exact_truncation() {
    if std::env::var("SDL_VIDEODRIVER").is_err() {
        panic!(
            "SDL_VIDEODRIVER is not set — run this smoke under the offscreen driver \
             with a software Vulkan ICD (see the test's doc comment)"
        );
    }
    // Renderer configuration, stated for checkability (the offscreen
    // smokes' discipline — challenge F-5): FIFO via Auto (always
    // supported, works offscreen), validation requested ON (an absent
    // layer is an environment limitation, not an error), overlay
    // enabled (the frame-4 assertions depend on it), no
    // pipeline-replacement scenario.
    let config = RenderConfig {
        present_mode: PresentModePreference::Auto,
        validation: ValidationPreference::On,
        replace_pipelines: false,
    };
    let mut app = ShellApp::new(ShellConfig {
        title: "Sektor7 F-03 Step 1 oversize smoke".to_string(),
        width: 1280,
        height: 720,
        target_fps: 60,
        driver: DriverPreference::Real,
    })
    .expect("the offscreen shell initializes");
    let mut renderer = Renderer::new(app.window(), config)
        .expect("the renderer constructs under the software ICD");
    renderer.set_overlay_enabled(true);

    let mut frame = |renderer: &mut Renderer, region: &[SceneVertex], overlay: Option<&str>| {
        let _snapshot = app.frame();
        renderer
            .render_frame(&IDENTITY_VIEW_PROJ, region, overlay, app.window_size())
            .expect("the frame renders");
    };

    // Frame 1 (control): empty region, short overlay — the lazy region
    // buffer must stay unallocated and nothing may be counted.
    let short_text = "F-03 Step 1 oversize smoke";
    frame(&mut renderer, &[], Some(short_text));
    let stats = renderer.stats();
    assert_eq!(stats.region_truncated_vertices, 0);
    assert_eq!(
        stats.region_capacity_vertices, 0,
        "lazy region buffer unallocated"
    );
    assert_eq!(stats.overlay_truncated_vertices, 0);
    println!(
        "[overflow-smoke] stage 1 control: region_truncated=0 region_capacity=0 \
         (lazy buffer unallocated) overlay_truncated=0"
    );

    // Frame 2: the oversize region — exactly 1,000 omitted after ONE
    // frame. The double-count guard: one physical truncation must read
    // 1,000, NOT 2,000 (the two historical min() clamps now consume
    // one decision).
    let oversize = oversize_region_scene();
    frame(&mut renderer, &oversize, Some(short_text));
    let stats = renderer.stats();
    assert_eq!(stats.region_truncated_vertices, 1_000);
    assert_eq!(stats.region_supplied_peak_vertices, 37_800);
    assert_eq!(stats.region_capacity_vertices, 36_800);
    assert_eq!(stats.overlay_truncated_vertices, 0, "overlay untouched");
    println!(
        "[overflow-smoke] stage 2 region oversize (ONE frame): \
         region_truncated={} (double-count guard: exactly 1000, not 2000) \
         region_peak={} region_capacity={} overlay_truncated=0",
        stats.region_truncated_vertices,
        stats.region_supplied_peak_vertices,
        stats.region_capacity_vertices
    );

    // Frame 3: the same oversize again (the second frame slot) — the
    // counters accumulate per recorded frame.
    frame(&mut renderer, &oversize, Some(short_text));
    let stats = renderer.stats();
    assert_eq!(stats.region_truncated_vertices, 2_000);
    println!(
        "[overflow-smoke] stage 3 second oversize frame (second slot): \
         region_truncated={} (per-frame accumulation)",
        stats.region_truncated_vertices
    );

    // Frame 4: the oversize overlay — 2,000 'A' glyphs × 168 verts =
    // 336,000 supplied vs the 300,000 capacity → exactly 36,000
    // omitted. 2,000 glyphs sit below MAX_GLYPHS (6,000), so the
    // builder's own glyph cap cannot confound the measurement; the
    // region is in-capacity, so its counters must not move
    // (cross-contamination guard).
    let big_text = "A".repeat(2_000);
    let in_capacity = in_capacity_region_scene();
    frame(&mut renderer, &in_capacity, Some(&big_text));
    let stats = renderer.stats();
    assert_eq!(stats.overlay_truncated_vertices, 36_000);
    assert_eq!(stats.overlay_supplied_peak_vertices, 336_000);
    assert_eq!(stats.overlay_capacity_vertices, 300_000);
    assert_eq!(
        stats.region_truncated_vertices, 2_000,
        "region untouched (cross-contamination guard)"
    );
    println!(
        "[overflow-smoke] stage 4 overlay oversize: overlay_truncated={} \
         overlay_peak={} overlay_capacity={} region_truncated={} \
         (cross-contamination guard: region unchanged)",
        stats.overlay_truncated_vertices,
        stats.overlay_supplied_peak_vertices,
        stats.overlay_capacity_vertices,
        stats.region_truncated_vertices
    );

    // Frame 5 (control): in-capacity inputs only — the counters must
    // not move (in-capacity geometry is never counted).
    frame(&mut renderer, &in_capacity, Some(short_text));
    let stats = renderer.stats();
    assert_eq!(stats.region_truncated_vertices, 2_000);
    assert_eq!(stats.overlay_truncated_vertices, 36_000);
    println!(
        "[overflow-smoke] stage 5 in-capacity control: counters unchanged \
         (region_truncated={} overlay_truncated={})",
        stats.region_truncated_vertices, stats.overlay_truncated_vertices
    );

    // The REPORTED requirement: the run-summary lines carry the exact
    // numbers through the same formatter the demos print.
    let report = TruncationReport::from_stats(renderer.stats());
    let lines = truncation_report_lines(&report);
    assert!(lines.contains(
        &"REGION TRUNCATION: 2000 vertices omitted (peak 37800 / capacity 36800)".to_string()
    ));
    assert!(lines.contains(
        &"OVERLAY TRUNCATION: 36000 vertices omitted (peak 336000 / capacity 300000)".to_string()
    ));
    for line in &lines {
        println!("[overflow-smoke] report line: {line}");
    }

    // Validation corroboration, scoped per the P1 challenge F-2: a
    // clean validation run corroborates no whole-buffer overflow and
    // no other Vulkan misuse on the oversize path; the per-slot
    // bounds are guarded structurally (the single decision) and by
    // the clamp unit tests — this assertion is corroboration, not the
    // primary guard.
    assert_eq!(renderer.stats().validation_errors, 0);
    println!(
        "[overflow-smoke] validation errors={} (clean run corroborated) — \
         EXECUTED through the real render path, all assertions held",
        renderer.stats().validation_errors
    );
}
