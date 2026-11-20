//! The shared live, capture and benchmark presentation path for the courtyard.
use engine_core::{
    AttackProfile, AttackRequest, CameraMode, CameraPose, Combatant, WeaponId, look_direction,
    resolve_attack,
};
use engine_renderer::{FrameResult, Renderer, RendererError, SceneLayer, scene::SceneVertex};
use engine_shell::Cue;
use std::time::Duration;

use crate::encounter_demo::{
    EncounterDemo, EncounterOutcome, EncounterPhase, PLAYER_ID, build_arena, encounter_targets,
};
use crate::encounter_events::{PresentationEvent, RayImpact};
use crate::encounter_hud::CombatHud;
use crate::encounter_visuals::EncounterVisuals;
use crate::math;
use crate::player_demo::DEMO_TICK_HZ;

#[derive(Default)]
pub struct EncounterFrame {
    pub visuals: EncounterVisuals,
    hud: CombatHud,
    world: Vec<SceneVertex>,
    foreground: Vec<SceneVertex>,
    projection: math::Mat4,
    world_matrix: math::Mat4,
    base_only: bool,
    loss_age: Option<Duration>,
    round: u32,
    sight: Option<(SightKey, [f32; 3])>,
    targets: Vec<Combatant>,
}

#[derive(PartialEq)]
struct SightKey {
    tick: u64,
    round: u32,
    weapon: WeaponId,
    origin: [f32; 3],
    aim: [f32; 3],
}

impl EncounterFrame {
    /// Consume each ordered fact once for all three consumers. Aging proceeds
    /// even on zero-tick, minimized, and terminal frames.
    pub fn observe(&mut self, demo: &mut EncounterDemo, dt: Duration, mut play: impl FnMut(Cue)) {
        if self.round != demo.encounter.rounds {
            self.visuals.clear();
            self.hud.advance(Duration::MAX);
            self.sight = None;
            self.round = demo.encounter.rounds;
        }
        self.visuals.advance(dt);
        self.hud.advance(dt);
        self.loss_age = match (demo.encounter.phase, self.loss_age) {
            (EncounterPhase::Lost { .. }, Some(age)) => Some(age.saturating_add(dt)),
            (EncounterPhase::Lost { .. }, None) => Some(Duration::ZERO),
            _ => None,
        };
        let now = demo.tick_index();
        let round = demo.encounter.rounds;
        demo.encounter.presentation.drain(now, |tick, event| {
            let age = Duration::from_secs_f64(
                (now.saturating_sub(tick)) as f64 / f64::from(DEMO_TICK_HZ),
            );
            if matches!(event, PresentationEvent::RoundStarted) {
                self.sight = None;
            }
            if let PresentationEvent::Fired(v) = event
                && let Some(center) = v.rays.iter().find(|ray| ray.direction == v.aim)
            {
                self.sight = Some((
                    SightKey {
                        tick,
                        round,
                        weapon: v.weapon,
                        origin: v.origin,
                        aim: v.aim,
                    },
                    center.endpoint,
                ));
            }
            self.visuals.observe(event, age);
            self.hud.observe(event, age);
            cues_for(event, &mut play);
        });
    }

    fn camera(&mut self, demo: &EncounterDemo) -> CameraPose {
        let state = demo.driver.state();
        let config = demo.driver.config();
        let mut pose = engine_core::camera_pose(state, config);
        if demo.driver.state().camera_mode == CameraMode::ThirdPerson {
            // The lifted chase camera has parallax, especially near cover. Point
            // it at the eye ray's actual contact so the center reticle stays true.
            // A trigger already supplies this contact. Pre-fire sighting uses the
            // same pure geometry query once per changed aim/weapon/logical tick.
            // View from the firing shoulder. Apply the cinematic descent to the
            // desired position before constraining it against walls.
            let right = math::camera_right(pose.yaw, 0., 0.);
            pose.position[0] += 1.05 * right[0];
            pose.position[2] += 1.05 * right[2];
            let loss = self
                .loss_age
                .map_or(0., |age| (age.as_secs_f32() / 2.).min(1.));
            pose.position[1] = (pose.position[1] - 1.2 * loss)
                .max(demo.driver.world().ground_y() + engine_core::player::CAMERA_MARGIN);
            pose = engine_core::player::constrain_camera_pose(
                pose,
                state,
                config,
                demo.driver.world(),
            );
            let origin = demo.eye();
            let aim = look_direction(pose.yaw, pose.pitch);
            let weapon = demo.arsenal.selected();
            let key = SightKey {
                tick: demo.tick_index(),
                round: demo.encounter.rounds,
                weapon,
                origin,
                aim,
            };
            if self.sight.as_ref().is_none_or(|(prev, _)| *prev != key) {
                encounter_targets(&mut self.targets, demo.spine.world());
                let range = match weapon {
                    WeaponId::Precision => demo.config().weapon_range,
                    WeaponId::Spread => demo.config().spread_range,
                };
                let request = AttackRequest {
                    attacker: PLAYER_ID,
                    origin,
                    direction: aim,
                    profile: AttackProfile {
                        range,
                        occlusion_margin: 0.,
                    },
                };
                let hit = resolve_attack(demo.driver.world(), &request, &self.targets);
                self.sight = hit.endpoint(origin, aim, range).map(|point| (key, point));
            }
            if let Some((_, p)) = &self.sight {
                let [x, y, z] = std::array::from_fn(|i| p[i] - pose.position[i]);
                let horizontal = x.hypot(z);
                if horizontal > 1.0e-5 {
                    pose.yaw = (-x).atan2(-z);
                }
                if horizontal > 1.0e-5 || y.abs() > 1.0e-5 {
                    pose.pitch = y.atan2(horizontal);
                }
            }
            pose.pitch += 0.35 * loss;
        }
        pose
    }

    fn arena(
        &mut self,
        demo: &EncounterDemo,
        extent: (u32, u32),
        box_proxy: bool,
    ) -> (CameraPose, f32) {
        let pose = self.camera(demo);
        let aspect = extent.0.max(1) as f32 / extent.1.max(1) as f32;
        self.projection = math::perspective(std::f32::consts::FRAC_PI_3, aspect, 0.1, 200.);
        self.world_matrix = math::mul(
            &self.projection,
            &math::camera_view(pose.position, pose.yaw, pose.pitch, pose.roll),
        );
        build_arena(&mut self.world, demo, box_proxy);
        self.foreground.clear();
        (pose, aspect)
    }

    /// Arena-only reference workload for paired measurements.
    pub fn prepare_base(&mut self, demo: &EncounterDemo, extent: (u32, u32)) {
        self.arena(demo, extent, true);
        self.base_only = true;
    }

    pub fn prepare(&mut self, demo: &EncounterDemo, extent: (u32, u32)) {
        let (pose, aspect) = self.arena(demo, extent, false);
        courtyard_markings(&mut self.world);
        self.base_only = false;
        self.visuals
            .append(&mut self.world, &mut self.foreground, demo, pose, aspect);
        self.hud.build(demo, extent);
    }

    pub fn draw(
        &self,
        renderer: &mut Renderer,
        debug: Option<&str>,
        size: (u32, u32),
    ) -> Result<FrameResult, RendererError> {
        let foreground = (!self.foreground.is_empty()).then_some(SceneLayer {
            vertices: &self.foreground,
            view_proj: &self.projection,
        });
        renderer.render_layers(
            &self.world_matrix,
            &self.world,
            foreground,
            if self.base_only {
                &[]
            } else {
                &self.hud.vertices
            },
            debug,
            size,
        )
    }

    /// Retained capacity of the CPU vertex scratch buffers. Excludes event and GPU storage.
    pub fn scratch_bytes(&self) -> usize {
        (self.world.capacity() + self.foreground.capacity()) * std::mem::size_of::<SceneVertex>()
            + self.hud.vertices.capacity()
                * std::mem::size_of::<engine_renderer::overlay::OverlayVertex>()
    }
}

/// Marks the two actual wake lines and supplies low-contrast ground references.
/// The ground plane supports movement; the accepted hitscan world includes only
/// the courtyard colliders. Open markings leave clear rays visible below it.
fn courtyard_markings(out: &mut Vec<SceneVertex>) {
    static MARKINGS: std::sync::OnceLock<Vec<SceneVertex>> = std::sync::OnceLock::new();
    out.extend_from_slice(MARKINGS.get_or_init(|| {
        let mut mesh = Vec::new();
        let mut strip = |x0: f32, z0: f32, x1: f32, z1: f32, col| {
            for pos in [
                [x0, 0., z1],
                [x0, 0., z0],
                [x1, 0., z0],
                [x0, 0., z1],
                [x1, 0., z0],
                [x1, 0., z1],
            ] {
                mesh.push(SceneVertex { pos, col });
            }
        };
        let gray = [0.15, 0.18, 0.20];
        for x in (-12..=12).step_by(4) {
            strip(x as f32, -15.7, x as f32 + 0.028, 9.7, gray);
        }
        for z in (-12..=8).step_by(4) {
            strip(-12.7, z as f32, 12.7, z as f32 + 0.028, gray);
        }
        for (z, color) in [
            (crate::encounter_demo::ACTIVATION_Z, [0.23, 0.50, 0.51]),
            (
                crate::encounter_demo::ACTIVATION_LINE_B_Z,
                [0.49, 0.34, 0.16],
            ),
        ] {
            for x in (-12..12).step_by(2) {
                strip(x as f32, z, x as f32 + 1., z + 0.08, color);
            }
        }
        mesh
    }));
}

fn cues_for(event: &PresentationEvent, mut play: impl FnMut(Cue)) {
    match event {
        PresentationEvent::Fired(v) => {
            play(Cue::Fire);
            if v.rays
                .iter()
                .any(|r| matches!(r.impact, RayImpact::Adversary { damaged: true, .. }))
            {
                play(Cue::Impact);
            }
            if v.rays
                .iter()
                .any(|r| matches!(r.impact, RayImpact::Adversary { defeated: true, .. }))
            {
                play(Cue::AdversaryDefeated);
            }
        }
        PresentationEvent::Switched(_) => play(Cue::WeaponSwitch),
        PresentationEvent::PlayerDamaged => play(Cue::Damage),
        PresentationEvent::RoundStarted | PresentationEvent::Activated => play(Cue::Interact),
        PresentationEvent::Terminal(EncounterOutcome::Won) => play(Cue::EncounterComplete),
        PresentationEvent::Terminal(EncounterOutcome::Lost) => play(Cue::Defeat),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::MoveIntent;

    #[test]
    fn one_spread_trigger_plays_one_fire_and_reset_clears_without_a_tick() {
        let mut demo = EncounterDemo::new();
        let mut frame = EncounterFrame::default();
        demo.driver.state_mut().position = [-5.5, 0., 1.5];
        demo.tick(MoveIntent::default(), false);
        demo.apply_selection(WeaponId::Spread);
        demo.tick(MoveIntent::default(), true);
        let mut cues = Vec::new();
        frame.observe(&mut demo, Duration::ZERO, |cue| cues.push(cue));
        assert_eq!(cues.iter().filter(|&&cue| cue == Cue::Fire).count(), 1);
        assert!(cues.contains(&Cue::WeaponSwitch));
        assert!(!cues.contains(&Cue::Impact));
        assert_eq!(frame.visuals.stats().live_rays, 5);
        assert_eq!(demo.encounter.counters.blocked, 5);
        demo.reset();
        frame.observe(&mut demo, Duration::ZERO, |_| panic!("no fresh tick"));
        assert_eq!(frame.visuals.stats().live_rays, 0);
        assert_eq!(demo.tick_index(), 0);
    }

    #[test]
    fn shoulder_view_centers_real_contact_and_respects_rotated_cover() {
        let mut demo = EncounterDemo::new();
        let mut frame = EncounterFrame::default();
        demo.driver.state_mut().position = [-5.5, 0., 1.5];
        demo.driver.state_mut().toggle_camera_mode();
        demo.tick(MoveIntent::default(), true);
        frame.observe(&mut demo, Duration::ZERO, |_| {});
        let pose = frame.camera(&demo);
        let eye = demo.eye();
        let max = [eye[0], eye[1], eye[2] - 30.];
        let t = demo.driver.world().segment_hit(eye, max, 0.).unwrap();
        let contact = [eye[0], eye[1], eye[2] - 30. * t];
        let ray = look_direction(pose.yaw, pose.pitch);
        let delta: [f32; 3] = std::array::from_fn(|i| contact[i] - pose.position[i]);
        let length = delta.iter().map(|a| a * a).sum::<f32>().sqrt();
        assert!(pose.position[0] > eye[0] + 0.9);
        for i in 0..3 {
            assert!((delta[i] / length - ray[i]).abs() < 1e-5);
        }
        // Put the slab behind the player's shoulder, inside the desired camera boom.
        demo.driver.state_mut().position = [-5.5, 0., -7.];
        let constrained = frame.camera(&demo);
        assert!(constrained.position[2] < demo.eye()[2] + demo.driver.config().tp_distance - 0.2);
        assert!(
            demo.driver
                .world()
                .segment_hit(demo.eye(), constrained.position, 0.)
                .is_none()
        );
    }
}
