use super::*;
use crate::encounter_events::Volley;
use engine_core::MAX_SPREAD_RAYS;

fn fire(id: u32, weapon: WeaponId, impacts: &[RayImpact]) -> PresentationEvent {
    let origin = [0., 1.6, 8.];
    let rays = impacts
        .iter()
        .enumerate()
        .map(|(i, impact)| {
            let direction = [
                (i as f32 - (impacts.len() - 1) as f32 * 0.5) * 0.08,
                0.,
                -1.,
            ];
            let endpoint = add(origin, mul(direction, 6. + i as f32));
            RayFact {
                direction,
                endpoint,
                impact: *impact,
            }
        })
        .collect();
    PresentationEvent::Fired(Volley {
        id,
        weapon,
        origin,
        aim: [0., 0., -1.],
        rays,
    })
}

fn draw(
    v: &EncounterVisuals,
    demo: &EncounterDemo,
    aspect: f32,
) -> (Vec<SceneVertex>, Vec<SceneVertex>) {
    let mut world = Vec::new();
    let mut front = Vec::new();
    v.append(&mut world, &mut front, demo, demo.camera_pose(), aspect);
    (world, front)
}

#[test]
fn first_person_selection_stays_on_screen_and_right_handed_after_look_and_resize() {
    let mut demo = EncounterDemo::new();
    let v = EncounterVisuals::default();
    let (world, precision) = draw(&v, &demo, 16. / 9.);
    assert!(world.is_empty());
    let s = demo.driver.state_mut();
    s.yaw = 1.2;
    s.pitch = -0.48;
    s.position = [12., 1., -4.];
    assert_eq!(
        draw(&v, &demo, 16. / 9.).1,
        precision,
        "camera-local mesh must follow the eye exactly"
    );
    demo.arsenal.select(WeaponId::Spread);
    let (_, spread) = draw(&v, &demo, 16. / 9.);
    assert_ne!(precision, spread);
    let bounds = |vs: &[SceneVertex], axis: usize| {
        vs.iter()
            .map(|v| v.pos[axis])
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), x| {
                (lo.min(x), hi.max(x))
            })
    };
    assert!(
        bounds(&spread, 0).1 - bounds(&spread, 0).0
            > 1.35 * (bounds(&precision, 0).1 - bounds(&precision, 0).0)
    );
    assert!(bounds(&precision, 2).0 < bounds(&spread, 2).0 - 0.15);
    for aspect in [0.72, 1., 16. / 9., 2.4] {
        for id in [WeaponId::Precision, WeaponId::Spread] {
            demo.arsenal.select(id);
            let (_, mesh) = draw(&v, &demo, aspect);
            let muzzle = first_person_mount(aspect, 0.).point(model::tip(id));
            let x = muzzle[0] * 3f32.sqrt() / (-muzzle[2] * aspect);
            let y = muzzle[1] * 3f32.sqrt() / -muzzle[2];
            assert!((0.0..0.55).contains(&x) && (-0.65..0.).contains(&y));
            assert!(mesh.iter().all(|v| v.pos[2] < -0.1));
        }
    }
}

#[test]
fn chase_model_is_attached_to_the_aiming_player_independent_of_camera_pull_in() {
    let mut demo = EncounterDemo::new();
    let v = EncounterVisuals::default();
    let height = demo.driver.config().eye_height;
    let state = demo.driver.state_mut();
    state.camera_mode = CameraMode::ThirdPerson;
    state.position = [-3., 1., 4.];
    state.yaw = 0.7;
    state.pitch = 0.36;
    let aim = look_direction(state.yaw, state.pitch);
    let eye = [
        state.position[0],
        state.position[1] + height,
        state.position[2],
    ];
    let basis = Frame::looking(eye, aim);
    let (world, front) = draw(&v, &demo, 16. / 9.);
    assert!(front.is_empty());
    let mut obstructed = demo.camera_pose();
    obstructed.position = add(eye, [0., 0.1, 0.15]);
    let mut close = Vec::new();
    v.append(&mut close, &mut Vec::new(), &demo, obstructed, 1.);
    assert_eq!(close, world);
    let grip = third_person_mount(basis, 0.);
    let muzzle = grip.point(model::tip(WeaponId::Precision));
    assert!(dot(sub(grip.origin, eye), basis.right) > 0.35);
    assert!(dot(sub(muzzle, grip.origin), aim) > 0.7);
    let local = model::build(WeaponId::Precision);
    // The weapon is the final mesh in this unfired TP layer. Both forearms
    // terminate on its local grips even when pitch differs from body yaw.
    let (operator, weapon) = world.split_at(world.len() - local.len());
    for grip_point in [[0., -0.110, 0.0], [-0.035, -0.058, -0.23]] {
        let hand = grip.point(grip_point);
        assert!(
            operator
                .iter()
                .any(|v| dot(sub(v.pos, hand), sub(v.pos, hand)) < 0.073_f32.powi(2))
        );
    }
    let bottom = operator
        .iter()
        .map(|v| v.pos[1])
        .fold(f32::INFINITY, f32::min);
    let top = operator
        .iter()
        .map(|v| v.pos[1])
        .fold(f32::NEG_INFINITY, f32::max);
    assert!((bottom - demo.driver.state().position[1]).abs() < 0.001);
    assert!(top > eye[1] + 0.095 && top < eye[1] + 0.11);
    for (vertex, expected) in weapon.iter().zip(local.iter()) {
        assert_eq!(vertex.pos, grip.point(expected.pos));
    }
    demo.arsenal.select(WeaponId::Spread);
    assert_ne!(draw(&v, &demo, 1.).0, world);
}

#[test]
fn every_live_tracer_ends_at_its_resolved_point_and_only_contacts_leave_impacts() {
    let impacts = [
        RayImpact::Cover,
        RayImpact::Adversary {
            target: 11,
            damaged: true,
            defeated: false,
        },
        RayImpact::Dormant { target: 12 },
        RayImpact::Miss,
        RayImpact::OutOfRange,
    ];
    let event = fire(3, WeaponId::Spread, &impacts);
    let PresentationEvent::Fired(volley) = &event else {
        unreachable!()
    };
    let mut v = EncounterVisuals::default();
    let mut demo = EncounterDemo::new();
    demo.driver.state_mut().position = [0., 0., 8.];
    v.observe(&event, Duration::ZERO);
    let (world, _) = draw(&v, &demo, 16. / 9.);
    for ray in &volley.rays {
        // Ribbon's far cross-section straddles, and is centered on, the exact
        // resolved point. Test emitted vertices, including the two miss rays.
        let near_tip: Vec<_> = world
            .iter()
            .filter(|v| {
                let d = sub(v.pos, ray.endpoint);
                let length2 = dot(d, d);
                (length2 - 0.016_f32.powi(2)).abs() < 1e-7
            })
            .map(|v| v.pos)
            .collect();
        assert!(
            near_tip.iter().any(|a| near_tip.iter().any(|b| {
                let d = sub(mul(add(*a, *b), 0.5), ray.endpoint);
                dot(d, d) < 1e-9
            })),
            "missing resolved end cap at {:?}",
            ray.endpoint
        );
    }
    // A point-blank shot with a viewmodel muzzle beyond the contact still
    // has visible projected area and a single path ending on the near face.
    let eye = [0., 1.6, 8.];
    let hit = [0., 1.6, 7.5];
    let mut strip = Vec::new();
    tracer_strip(
        &mut strip,
        [eye, [0.24, 1.31, 6.8], hit],
        eye,
        [1., 0., 0.],
        0.021,
        [1.; 3],
    );
    assert_eq!(strip.len(), 24);
    assert!(
        strip
            .iter()
            .all(|v| v.pos != eye && v.pos[2] >= hit[2] && v.pos[2] < eye[2] - 0.1)
    );
    let first_ring = &strip[..12];
    let last_ring = &strip[12..];
    assert_eq!(first_ring[2].pos, last_ring[1].pos);
    assert_eq!(first_ring[8].pos, last_ring[0].pos);
    let project = |p: Point| {
        [
            (p[0] - eye[0]) / (eye[2] - p[2]),
            (p[1] - eye[1]) / (eye[2] - p[2]),
        ]
    };
    assert!(first_ring.as_chunks::<3>().0.iter().any(|tri| {
        let [a, b, c] = [
            project(tri[0].pos),
            project(tri[1].pos),
            project(tri[2].pos),
        ];
        ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() > 1e-4
    }));
    v.advance(Duration::from_millis(130));
    let (after_tracers, _) = draw(&v, &demo, 16. / 9.);
    for (ray, impact) in volley.rays.iter().zip(impacts) {
        let sample = after_tracers.iter().find(|v| {
            let d = sub(v.pos, ray.endpoint);
            dot(d, d) < 0.1
        });
        assert_eq!(sample.is_some(), is_impact(impact));
        match impact {
            RayImpact::Cover => assert!(sample.unwrap().col[1] > sample.unwrap().col[2]),
            RayImpact::Adversary { .. } => assert!(sample.unwrap().col[2] > sample.unwrap().col[1]),
            _ => {}
        }
    }
    v.advance(Duration::from_millis(90));
    assert!(draw(&v, &demo, 16. / 9.).0.is_empty());
    assert_eq!(v.stats().expired_volleys, 1);
}

#[test]
fn old_shots_expire_and_switch_and_reset_clear_active_muzzle_without_replay() {
    let demo = EncounterDemo::new();
    let mut v = EncounterVisuals::default();
    let base = draw(&v, &demo, 1.).1;
    let fire = fire(1, WeaponId::Precision, &[RayImpact::Cover]);
    v.observe(&fire, Duration::from_secs(5));
    assert_eq!(v.stats().stale_volleys, 1);
    assert_eq!(v.stats().live_rays, 0);
    v.observe(&fire, Duration::ZERO);
    assert!(draw(&v, &demo, 1.).1.len() > base.len());
    v.advance(FLASH_TIME);
    let recoiling = draw(&v, &demo, 1.).1;
    assert_eq!(recoiling.len(), base.len());
    assert_ne!(recoiling, base);
    v.observe(
        &PresentationEvent::Switched(WeaponId::Spread),
        Duration::ZERO,
    );
    assert_eq!(draw(&v, &demo, 1.).1, base);
    assert_eq!(v.stats().live_rays, 1);
    v.observe(&PresentationEvent::RoundStarted, Duration::ZERO);
    assert_eq!(v.stats(), VisualStats::default());
    assert!(draw(&v, &demo, 1.).0.is_empty());
}

#[test]
fn worst_case_pool_reuses_storage_and_stays_below_dynamic_slot_budget() {
    let mut v = EncounterVisuals::default();
    let before: Vec<_> = v
        .slots
        .iter()
        .map(|s| (s.rays.as_ptr(), s.rays.capacity()))
        .collect();
    let contacts = vec![
        RayImpact::Adversary {
            target: 12,
            damaged: true,
            defeated: false
        };
        MAX_SPREAD_RAYS as usize
    ];
    for id in 1..=96 {
        v.observe(&fire(id, WeaponId::Spread, &contacts), Duration::ZERO);
    }
    let s = v.stats();
    assert_eq!(s.live_volleys, VOLLEY_CAPACITY);
    assert_eq!(s.live_rays, VOLLEY_CAPACITY * contacts.len());
    assert_eq!(s.evicted_volleys, 96 - VOLLEY_CAPACITY as u64);
    assert!(v.slots.iter().all(|s| s.id > 96 - VOLLEY_CAPACITY as u32));
    let mut demo = EncounterDemo::new();
    demo.driver.state_mut().camera_mode = CameraMode::ThirdPerson;
    demo.arsenal.select(WeaponId::Spread);
    let (mut world, _) = draw(&v, &demo, 1.);
    let effects = world.len();
    crate::encounter_demo::build_encounter_vertices_into(&mut world, &demo);
    let slot = engine_renderer::scene::region_scene_capacity_vertices()
        / engine_renderer::config::FRAMES_IN_FLIGHT;
    assert!(world.len() + effects < slot as usize);
    v.advance(IMPACT_TIME);
    assert_eq!(v.stats().expired_volleys, VOLLEY_CAPACITY as u64);
    assert_eq!(v.stats().live_rays, 0);
    assert_eq!(
        before,
        v.slots
            .iter()
            .map(|s| (s.rays.as_ptr(), s.rays.capacity()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn malformed_facts_never_submit_partial_or_nonfinite_geometry() {
    let mut v = EncounterVisuals::default();
    let demo = EncounterDemo::new();
    let mut event = fire(1, WeaponId::Spread, &[RayImpact::Cover, RayImpact::Cover]);
    let PresentationEvent::Fired(volley) = &mut event else {
        unreachable!()
    };
    volley.rays[1].endpoint[2] = f32::NAN;
    v.observe(&event, Duration::ZERO);
    assert_eq!(v.stats().invalid_volleys, 1);
    assert_eq!(v.stats().live_rays, 0);
    let (world, front) = draw(&v, &demo, f32::NAN);
    assert!(world.is_empty() && front.iter().all(|v| v.pos.into_iter().all(f32::is_finite)));
}
