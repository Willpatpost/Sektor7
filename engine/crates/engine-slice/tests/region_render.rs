//! Stage 4 graphical integration contracts (Batch 3 GO): the derived,
//! non-authoritative region visualization — LOD-colored area plates,
//! NPC render proxies for Full/Simplified regions only, dormant
//! discard sentinels skipped, per-frame capacity bounds, and the
//! camera→presence mapping (render XZ → region-plane XY, live
//! frame-sampled input).

use engine_core::LodLevel;
use engine_renderer::scene::region_scene_capacity_vertices;
use engine_slice::npc::{NpcConfig, NpcWorld, REGION_COUNT};
use engine_slice::region::RegionConfig;
use engine_slice::region_render::{build_region_scene, camera_presence, levels_compact};

fn lod_world() -> NpcWorld {
    NpcWorld::new(NpcConfig {
        region_lods_enabled: true,
        ..NpcConfig::default()
    })
}

#[test]
fn region_scene_reflects_levels_and_proxy_policy() {
    // Authorization §7: eight visible region areas colored by LOD;
    // NPC render proxies for Full/Simplified regions ONLY (EventDriven
    // and Dormant drop them); deterministic; and one frame's geometry
    // fits one buffer slot (the capacity divided by the frame count).
    let config = RegionConfig::default();
    let world = lod_world();
    let mut levels = [LodLevel::Dormant; REGION_COUNT];
    levels[0] = LodLevel::Full;
    levels[3] = LodLevel::Simplified;
    levels[5] = LodLevel::EventDriven;
    let vertices = build_region_scene(&levels, &world, &config);
    // 8 plates (36 non-indexed verts each) + 125 Full + 125 Simplified
    // proxies — EventDriven and Dormant regions contribute plates only.
    assert_eq!(
        vertices.len(),
        (8 + 125 + 125) * 36,
        "plates for all regions, proxies for Full+Simplified only"
    );
    // The worst case — every region Full — still fits one slot.
    let slot_capacity = region_scene_capacity_vertices() / 2; // FRAMES_IN_FLIGHT = 2
    let all_full = build_region_scene(&[LodLevel::Full; REGION_COUNT], &world, &config);
    assert_eq!(all_full.len(), (8 + 1000) * 36);
    assert!(
        all_full.len() as u32 <= slot_capacity,
        "the worst-case frame fits one slot ({} <= {})",
        all_full.len(),
        slot_capacity
    );
    // Deterministic: the same inputs build the same vertices (compared
    // field-wise — the render vertex type intentionally carries no
    // PartialEq derive).
    let rebuilt = build_region_scene(&levels, &world, &config);
    assert_eq!(vertices.len(), rebuilt.len());
    for (a, b) in vertices.iter().zip(&rebuilt) {
        assert_eq!(a.pos, b.pos);
        assert_eq!(a.col, b.col);
    }
}

#[test]
fn dormant_sentinel_positions_produce_no_proxies() {
    // A dormant region's positions are discard sentinels; even if the
    // level were (defensively) Full, no proxies are built from them —
    // the sentinel check is independent of the level policy.
    let config = RegionConfig::default();
    let mut world = lod_world();
    world.discard_region_routine(1);
    let mut levels = [LodLevel::Dormant; REGION_COUNT];
    levels[1] = LodLevel::Full;
    let vertices = build_region_scene(&levels, &world, &config);
    assert_eq!(
        vertices.len(),
        8 * 36,
        "eight plates, no proxies — region 1's positions are sentinels"
    );
}

#[test]
fn lod_levels_have_distinct_plate_colors() {
    // Activation and deactivation stay observable in the graphical run:
    // distinct plate colors per level (checked via the top-face vertex
    // color, which carries the unshaded LOD color).
    let config = RegionConfig::default();
    let world = lod_world();
    let top_color = |levels: &[LodLevel; REGION_COUNT]| -> [f32; 3] {
        build_region_scene(levels, &world, &config)[0].col
    };
    let full = top_color(&[LodLevel::Full; REGION_COUNT]);
    let simplified = top_color(&[LodLevel::Simplified; REGION_COUNT]);
    let event = top_color(&[LodLevel::EventDriven; REGION_COUNT]);
    let dormant = top_color(&[LodLevel::Dormant; REGION_COUNT]);
    assert_ne!(full, simplified);
    assert_ne!(full, event);
    assert_ne!(full, dormant);
    assert_ne!(simplified, event);
    assert_ne!(event, dormant);
}

#[test]
fn camera_presence_maps_render_xz_to_region_xy() {
    // Render-space XZ maps to the simulation's region-plane XY; the
    // live velocity derives from the actual displacement over the
    // camera dt; a zero dt yields zero velocity (no division blowups —
    // the minimized-frame path).
    let sample = camera_presence([100.0, 2.0, 9.0], [4.0, 1.0, -2.0], 0.5);
    assert_eq!(sample.position, (100.0, 9.0));
    assert_eq!(sample.velocity, (8.0, -4.0));
    let zero_dt = camera_presence([1.0, 2.0, 3.0], [1.0, 1.0, 1.0], 0.0);
    assert_eq!(zero_dt.velocity, (0.0, 0.0));
}

#[test]
fn levels_compact_renders_one_char_per_region() {
    let levels = [
        LodLevel::Full,
        LodLevel::Simplified,
        LodLevel::EventDriven,
        LodLevel::Dormant,
        LodLevel::Dormant,
        LodLevel::Dormant,
        LodLevel::Dormant,
        LodLevel::EventDriven,
    ];
    assert_eq!(levels_compact(&levels), "F S E D D D D E");
}
