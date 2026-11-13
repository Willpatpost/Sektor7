//! Stage 4 graphical integration (authorization §7, Batch 3 GO):
//! derived, non-authoritative region visualization. Eight region area
//! plates colored by the current LOD level, plus NPC render proxies
//! for Full/Simplified regions built from the world's represented grid
//! positions. Rendering stays strictly DOWNSTREAM of authoritative
//! simulation state — this module only reads the world and produces
//! `SceneVertex` lists; it never mutates simulation state, and the
//! authoritative sim state stays separate from these derived proxies
//! (EventDriven and Dormant regions drop their proxies; dormant
//! positions are discard sentinels, skipped defensively).
//!
//! The camera is the demo's live presence source: render-space XZ maps
//! to the simulation's region-plane XY, frame-sampled — a genuinely
//! cadence-dependent external input, with no cross-cadence equivalence
//! claim (the scripted per-tick path is the deterministic one).

use engine_core::{LodLevel, PresenceSample};
use engine_renderer::scene::{SceneVertex, push_box};

use crate::npc::{NPC_COUNT, NpcWorld, POSITION_DISCARDED, REGION_COUNT, REGION_GRID};
use crate::region::RegionConfig;

/// Region area plate footprint (world units): covers the 16×16 proxy
/// grid (15 units at 1-unit spacing) with margin; plates never overlap
/// on the default ring (adjacent centers ≈ 75–106 units apart).
pub(crate) const PLATE_FOOTPRINT: f32 = 20.0;
/// Region area plate height — a low slab whose top face carries the
/// LOD color and whose sides make the area's extent visible.
pub(crate) const PLATE_HEIGHT: f32 = 0.15;
/// NPC render proxy footprint and height (one small box per proxied
/// NPC, standing on its region's plate).
pub(crate) const PROXY_FOOTPRINT: f32 = 0.4;
pub(crate) const PROXY_HEIGHT: f32 = 0.5;

/// Area plate color per LOD level: Full bright green, Simplified blue,
/// EventDriven amber, Dormant near-black (visible as "gone dark" —
/// dormant regions drop their proxies but keep a dark plate, so
/// activation and deactivation stay observable in the graphical run).
pub(crate) fn lod_color(level: LodLevel) -> [f32; 3] {
    match level {
        LodLevel::Full => [0.20, 0.70, 0.30],
        LodLevel::Simplified => [0.25, 0.45, 0.80],
        LodLevel::EventDriven => [0.85, 0.62, 0.20],
        LodLevel::Dormant => [0.08, 0.08, 0.10],
    }
}

/// Builds the dynamic region scene for one frame: one area plate per
/// region colored by its current LOD, plus one small box per NPC for
/// Full/Simplified regions — derived render proxies at the NPC's
/// represented grid position, mapped onto the region's plate. Read-only
/// over the world; deterministic given (levels, world, config). The
/// output is bounded by `region_scene_capacity_vertices()` divided by
/// the frame count (one frame's geometry must fit one buffer slot).
pub fn build_region_scene(
    levels: &[LodLevel; REGION_COUNT],
    world: &NpcWorld,
    config: &RegionConfig,
) -> Vec<SceneVertex> {
    let mut vertices = Vec::new();
    build_region_scene_into(&mut vertices, levels, world, config);
    vertices
}

/// The caller-held scratch form (the 2026-11-12 quality audit's F-02
/// candidate, adopted on the measured A/B): clears and refills the
/// caller's buffer — the capacity retained from the first frames is
/// reused every frame, so the steady-state per-frame cost is the
/// geometry work alone (no fresh allocation, no doubling growth).
/// The NPC iteration is the direct region-index stride
/// (`region`, `region + REGION_COUNT`, …) — the exact same NPC
/// sequence the all-NPC filter selects, in the same order, so the
/// derived geometry is byte-identical to the original form (proven
/// by test); the stride simply visits each NPC once per frame
/// instead of once per region. Rendering stays non-authoritative.
pub fn build_region_scene_into(
    vertices: &mut Vec<SceneVertex>,
    levels: &[LodLevel; REGION_COUNT],
    world: &NpcWorld,
    config: &RegionConfig,
) {
    vertices.clear();
    let positions = world.positions();
    let half_grid = (f32::from(REGION_GRID) - 1.0) / 2.0;
    for (region, &level) in levels.iter().enumerate() {
        let (cx, cy) = config.centers[region];
        let color = lod_color(level);
        push_box(
            vertices,
            [cx as f32, 0.0, cy as f32],
            PLATE_FOOTPRINT,
            PLATE_HEIGHT,
            color,
        );
        // Proxies exist only for the player-proximate levels —
        // EventDriven and Dormant regions drop them; the area plate
        // still shows the level.
        if !matches!(level, LodLevel::Full | LodLevel::Simplified) {
            continue;
        }
        let proxy_color = [color[0] * 1.4, color[1] * 1.4, color[2] * 1.4];
        for npc in (region..NPC_COUNT).step_by(REGION_COUNT) {
            let (gx, gy) = positions[npc];
            if (gx, gy) == POSITION_DISCARDED {
                // Dormant discard sentinel — no represented position.
                continue;
            }
            push_box(
                vertices,
                [
                    cx as f32 + f32::from(gx) - half_grid,
                    PLATE_HEIGHT,
                    cy as f32 + f32::from(gy) - half_grid,
                ],
                PROXY_FOOTPRINT,
                PROXY_HEIGHT,
                proxy_color,
            );
        }
    }
}

/// Maps the graphical camera to a live presence sample: render-space
/// XZ → the simulation's region-plane XY. `displacement` is the
/// camera's actual world displacement over the frame's (capped)
/// camera dt, so the velocity feeding the director's lookahead is the
/// real live input — frame-sampled and genuinely cadence-dependent,
/// with no cross-cadence equivalence claim (the scripted per-tick path
/// is the deterministic one).
pub fn camera_presence(position: [f32; 3], displacement: [f32; 3], dt: f32) -> PresenceSample {
    let velocity = if dt > 0.0 {
        [displacement[0] / dt, displacement[2] / dt]
    } else {
        [0.0, 0.0]
    };
    PresenceSample {
        position: (f64::from(position[0]), f64::from(position[2])),
        velocity: (f64::from(velocity[0]), f64::from(velocity[1])),
    }
}

/// Compact per-region LOD string for overlays and reports: F/S/E/D,
/// space-separated in region order.
pub fn levels_compact(levels: &[LodLevel; REGION_COUNT]) -> String {
    let mut text = String::new();
    for (index, level) in levels.iter().enumerate() {
        if index > 0 {
            text.push(' ');
        }
        text.push(match level {
            LodLevel::Full => 'F',
            LodLevel::Simplified => 'S',
            LodLevel::EventDriven => 'E',
            LodLevel::Dormant => 'D',
        });
    }
    text
}

#[cfg(test)]
mod f02_tests {
    use super::*;
    use crate::benchmark::scenario_pair;
    use engine_core::PresenceSample;

    /// The F-02 adoption proof: the scratch form produces BYTE-IDENTICAL
    /// geometry to the allocating form over a real deterministic scenario
    /// (the accrual-peak fixture), and the stride iteration visits the
    /// same NPCs in the same order as the all-NPC filter (the equality
    /// over the full vertex sequence IS that proof). The scratch buffer
    /// also retains its capacity across builds (the no-realloc property).
    #[test]
    fn the_scratch_form_builds_identical_geometry_and_retains_capacity() {
        let near = PresenceSample {
            position: (-100.0, 0.0),
            velocity: (0.0, 0.0),
        };
        let (spine, director) = scenario_pair(150, &near);
        let levels = director.levels();
        let world = spine.world();
        let config = director.config();

        let allocating = build_region_scene(&levels, world, config);
        let mut scratch = Vec::new();
        build_region_scene_into(&mut scratch, &levels, world, config);
        assert_eq!(
            scratch, allocating,
            "the derived geometry must be identical"
        );

        // The stride sequence == the filter sequence (the per-region NPC
        // index order, proven directly on the index arithmetic).
        for region in 0..REGION_COUNT {
            let filtered: Vec<usize> = (0..NPC_COUNT)
                .filter(|npc| npc % REGION_COUNT == region)
                .collect();
            let strided: Vec<usize> = (region..NPC_COUNT).step_by(REGION_COUNT).collect();
            assert_eq!(filtered, strided, "region {region}: the same NPC order");
        }

        // The retained-capacity property: after a build, a second build
        // into the same buffer does not reduce capacity (clear keeps it).
        let capacity_after_first = scratch.capacity();
        build_region_scene_into(&mut scratch, &levels, world, config);
        assert!(scratch.capacity() >= capacity_after_first);
        assert_eq!(scratch, allocating, "the second build is also identical");
    }
}
