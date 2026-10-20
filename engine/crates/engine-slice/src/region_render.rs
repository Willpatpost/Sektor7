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
const PLATE_FOOTPRINT: f32 = 20.0;
/// Region area plate height — a low slab whose top face carries the
/// LOD color and whose sides make the area's extent visible.
const PLATE_HEIGHT: f32 = 0.15;
/// NPC render proxy footprint and height (one small box per proxied
/// NPC, standing on its region's plate).
const PROXY_FOOTPRINT: f32 = 0.4;
const PROXY_HEIGHT: f32 = 0.5;

/// Area plate color per LOD level: Full bright green, Simplified blue,
/// EventDriven amber, Dormant near-black (visible as "gone dark" —
/// dormant regions drop their proxies but keep a dark plate, so
/// activation and deactivation stay observable in the graphical run).
fn lod_color(level: LodLevel) -> [f32; 3] {
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
    let positions = world.positions();
    let half_grid = (f32::from(REGION_GRID) - 1.0) / 2.0;
    for (region, &level) in levels.iter().enumerate() {
        let (cx, cy) = config.centers[region];
        let color = lod_color(level);
        push_box(
            &mut vertices,
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
        for npc in (0..NPC_COUNT).filter(|npc| npc % REGION_COUNT == region) {
            let (gx, gy) = positions[npc];
            if (gx, gy) == POSITION_DISCARDED {
                // Dormant discard sentinel — no represented position.
                continue;
            }
            push_box(
                &mut vertices,
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
    vertices
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
