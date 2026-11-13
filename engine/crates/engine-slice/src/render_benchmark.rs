//! The render-path CPU benchmark (the 2026-11-12 independent quality
//! audit's F-02/F-05 assignment — the renderer/performance
//! infrastructure): the per-frame CPU costs of the DERIVED render
//! path measured in isolation over the deterministic scenario, no GPU
//! required. **Pod results are CPU-side A/B evidence only — NOT
//! physical-rig or real-GPU evidence** (the owner's Windows
//! 5070 Ti / 9800X3D rig remains the authoritative graphical
//! benchmark; CI's llvipe runners are software rasterization and do
//! not represent it either).
//!
//! The measured operations (each a distribution over N iterations,
//! reported by the shared `report` helper):
//! - **Region scene build** (`build_region_scene` — the per-frame
//!   derived geometry: the fresh `Vec` allocation, the per-region
//!   all-NPC filter loop, and the box pushes; F-02).
//! - **Overlay text formatting** (the frame loop's `format!` chain
//!   shape — the F-05 instrumentation text cost).
//! - **Overlay percentile pair** (`math::percentile` at p50 and p95
//!   over the 600-sample frame-time window — the clone+sort the
//!   audit flags; F-05).
//! - **Overlay vertex rebuild** (`build_overlay_vertices` over the
//!   formatted text — the glyph quad regeneration; F-05).
//!
//! The results are interpreted against the 60/144/240 FPS CPU frame
//! budgets (16.7 / 6.9 / 4.2 ms) — the per-frame instrumentation
//! share, not the total frame cost (presentation, GPU waits, and the
//! simulation itself are outside this module's scope).

use std::time::{Duration, Instant};

use engine_renderer::overlay::build_overlay_vertices;

use crate::benchmark::{report, scenario_pair};
use crate::math;
use crate::npc::{NPC_COUNT, NpcWorld, POSITION_DISCARDED, REGION_COUNT, REGION_GRID};
use crate::region::RegionConfig;
use crate::region_render::build_region_scene_into;
use engine_core::LodLevel;
use engine_renderer::scene::{SceneVertex, push_box};

/// The preserved F-02 baseline — the EXACT original algorithm (the
/// all-NPC filter per region over a fresh `Vec::new()` with the
/// doubling growth): kept here as the A/B reference record; the
/// production frame loops now use the caller-held scratch form.
fn baseline_build_region_scene(
    levels: &[LodLevel; REGION_COUNT],
    world: &NpcWorld,
    config: &RegionConfig,
) -> Vec<SceneVertex> {
    let mut vertices = Vec::new();
    let positions = world.positions();
    let half_grid = (f32::from(REGION_GRID) - 1.0) / 2.0;
    for (region, &level) in levels.iter().enumerate() {
        let (cx, cy) = config.centers[region];
        let color = crate::region_render::lod_color(level);
        push_box(
            &mut vertices,
            [cx as f32, 0.0, cy as f32],
            crate::region_render::PLATE_FOOTPRINT,
            crate::region_render::PLATE_HEIGHT,
            color,
        );
        if !matches!(level, LodLevel::Full | LodLevel::Simplified) {
            continue;
        }
        let proxy_color = [color[0] * 1.4, color[1] * 1.4, color[2] * 1.4];
        for npc in (0..NPC_COUNT).filter(|npc| npc % REGION_COUNT == region) {
            let (gx, gy) = positions[npc];
            if (gx, gy) == POSITION_DISCARDED {
                continue;
            }
            push_box(
                &mut vertices,
                [
                    cx as f32 + f32::from(gx) - half_grid,
                    crate::region_render::PLATE_HEIGHT,
                    cy as f32 + f32::from(gy) - half_grid,
                ],
                crate::region_render::PROXY_FOOTPRINT,
                crate::region_render::PROXY_HEIGHT,
                proxy_color,
            );
        }
    }
    vertices
}

/// The formatted-overlay fixture: the frame loop's exact template
/// shape (the main stats block plus the Stage 4 regions block) with
/// representative fixed values — the measurement target is the
/// formatting machinery cost, which the values do not change.
fn representative_overlay_text(
    fps: f64,
    cpu_p95: f64,
    gpu_p95: f64,
    act_p50: f64,
    act_p95: f64,
) -> String {
    format!(
        "Sektor7 Stage 2 - Vulkan\nFPS {fps:.0} (cpu work p95 {cpu_p95:.2} ms / gpu p95 {gpu_p95:.2} ms)\npresent fifo / 1280x720 / 3 images / recreations 1\nregion ring / Vulkan 1.3\nvalidation on (err 0 / warn 0)\nsim tick 150 @ 60 Hz · out 42 · ev/s 62\nnear/far 118/216 · pend 3 · blk 0 · late 0 · bud-exh 0\nwm 12 / 4\ncam (12.3, 45.6, 78.9) yaw 1.57 pitch 0.12\nregions F S S E D D E D\nact 18 · deact 16 · invalid 0 · recon 2\ncaptured 1400 · reapplied 1320 · act-lat p50 {act_p50:.2} / p95 {act_p95:.2} ms",
    )
}

/// Runs the render-path CPU benchmark and prints the report.
pub fn run() {
    println!(
        "Sektor7 render-path CPU benchmark (F-02/F-05 — POD CPU evidence only, NOT physical-rig or real-GPU evidence)"
    );
    println!(
        "workload: the deterministic 1000-NPC / 8-region Lod scenario at 150 ticks (the accrual peak), the fixed frame state"
    );

    // The fixed deterministic frame state: the scenario's levels,
    // world, and config — build_region_scene is a pure function of
    // these, so every iteration measures the identical work.
    let near = engine_core::PresenceSample {
        position: (-100.0, 0.0),
        velocity: (0.0, 0.0),
    };
    let (spine, director) = scenario_pair(150, &near);
    let levels = director.levels();
    let world = spine.world();
    let config = director.config();

    // A. F-02 baseline — the PRESERVED ORIGINAL algorithm (the
    //    all-NPC filter per region + the fresh Vec::new() with the
    //    doubling growth), the A/B reference record. 200 warmup +
    //    2000 measured iterations.
    let mut region_build = Vec::new();
    let mut region_vertices = 0usize;
    let mut region_capacity = 0usize;
    for i in 0..2200 {
        let start = Instant::now();
        let vertices = baseline_build_region_scene(&levels, world, config);
        region_build.push(start.elapsed());
        region_vertices = vertices.len();
        region_capacity = vertices.capacity();
        if i < 200 {
            region_build.pop();
        }
    }
    report(
        "region scene build (the F-02 BASELINE: the filter + fresh Vec, per frame)",
        &mut region_build,
    );
    println!(
        "  geometry: {region_vertices} vertices, the Vec grown to capacity {region_capacity} (a fresh allocation + the doubling growth from empty every frame)",
    );

    // A2. The F-02 A/B — the ADOPTED scratch form (the caller-held
    //     buffer with the retained capacity + the direct region-index
    //     stride), with the byte-identical-geometry assertion between
    //     the two forms.
    let mut scratch: Vec<SceneVertex> = Vec::new();
    let mut region_scratch_build = Vec::new();
    for i in 0..2200 {
        let start = Instant::now();
        build_region_scene_into(&mut scratch, &levels, world, config);
        region_scratch_build.push(start.elapsed());
        if i < 200 {
            region_scratch_build.pop();
        }
    }
    report(
        "region scene build (the F-02 SCRATCH form: retained buffer + stride, per frame)",
        &mut region_scratch_build,
    );
    let baseline_geometry = baseline_build_region_scene(&levels, world, config);
    assert_eq!(
        scratch, baseline_geometry,
        "the A/B forms must produce identical geometry"
    );
    println!(
        "  A/B equality: the scratch form's {} vertices (retained capacity {}) are byte-identical to the baseline's",
        scratch.len(),
        scratch.capacity()
    );

    // B. F-05 — the overlay text formatting (the frame loop's
    //    format! chain shape over fixed representative values).
    let mut overlay_format = Vec::new();
    for i in 0..2200 {
        let start = Instant::now();
        let _ = representative_overlay_text(59.7, 1.234, 0.987, 0.456, 0.789);
        overlay_format.push(start.elapsed());
        if i < 200 {
            overlay_format.pop();
        }
    }
    report(
        "overlay text formatting (the format! chain, per frame)",
        &mut overlay_format,
    );

    // C. F-05 — the overlay percentile pair: math::percentile at p95
    //    and p50 over the 600-sample frame-time window (the clone +
    //    sort the audit flags), exactly as the frame loop calls them.
    let samples: Vec<f64> = (0..600).map(|i| 1.0 + f64::from(i % 97) * 0.01).collect();
    let mut percentile_pair = Vec::new();
    for i in 0..2200 {
        let start = Instant::now();
        let p95 = math::percentile(&samples, 0.95);
        let p50 = math::percentile(&samples, 0.50);
        percentile_pair.push(start.elapsed());
        if i < 200 {
            percentile_pair.pop();
        }
        if i == 2199 {
            println!(
                "  (percentile fixture check: p50 {p50:.3} / p95 {p95:.3} over {} samples)",
                samples.len()
            );
        }
    }
    report(
        "overlay percentile pair (math::percentile p95+p50, per frame)",
        &mut percentile_pair,
    );

    // D. F-05 — the overlay vertex rebuild over the formatted text
    //    (the glyph quad regeneration, the per-frame Vec).
    let text = representative_overlay_text(59.7, 1.234, 0.987, 0.456, 0.789);
    let mut overlay_vertices = Vec::new();
    let mut overlay_vertex_count = 0usize;
    for i in 0..2200 {
        let start = Instant::now();
        let vertices = build_overlay_vertices(&text, (1280, 720), [1.0, 1.0, 1.0, 1.0]);
        overlay_vertices.push(start.elapsed());
        overlay_vertex_count = vertices.len();
        if i < 200 {
            overlay_vertices.pop();
        }
    }
    report(
        "overlay vertex rebuild (build_overlay_vertices, per frame)",
        &mut overlay_vertices,
    );
    println!(
        "  glyph geometry: {overlay_vertex_count} overlay vertices for the {}-byte text",
        text.len()
    );

    // The budget interpretation: the measured overlay-on
    // instrumentation share against the CPU frame budgets. (The
    // region build runs regardless of the overlay; the three F-05
    // components run only when the overlay is enabled.)
    let median_ms = |v: &[Duration]| {
        let mut s = v.to_vec();
        s.sort_unstable();
        s[s.len() / 2].as_secs_f64() * 1e3
    };
    let region_ms = median_ms(&region_build);
    let overlay_ms = median_ms(&overlay_format) + median_ms(&percentile_pair);
    // The vertex rebuild is a renderer-side cost of the overlay text
    // (it runs inside the renderer's draw path when the overlay is
    // enabled) — counted in the overlay-on share.
    let overlay_on_extra = overlay_ms + median_ms(&overlay_vertices);
    println!(
        "frame-budget share (medians): region scene {region_ms:.3} ms/frame; overlay-on extra {overlay_on_extra:.3} ms/frame (format {:.3} + percentiles {:.3} + vertices {:.3})",
        median_ms(&overlay_format),
        median_ms(&percentile_pair),
        median_ms(&overlay_vertices),
    );
    for (fps, budget) in [(60u32, 16.7f64), (144, 6.9), (240, 4.2)] {
        let share = (region_ms + overlay_on_extra) / budget * 100.0;
        let share_off = region_ms / budget * 100.0;
        println!(
            "  @{fps} FPS ({budget:.1} ms budget): overlay-on CPU share ~{share:.2}% (overlay-off ~{share_off:.2}% — the overlay costs ~{overlay_on_extra:.3} ms/frame of the budget)",
        );
    }
    println!(
        "NOTE: these are the POD's CPU-side costs of the derived render path in isolation — the full frame cost (presentation, GPU waits, simulation, durability I/O) is separate; the real-GPU evidence is the owner's Windows rig per the profiling kit"
    );
}
