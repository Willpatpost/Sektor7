//! Real swapchain/readback regression. Run this test on a graphical desktop, or with
//! SDL_VIDEODRIVER=offscreen on Linux. CYCLE5_CAPTURE_DIR optionally retains PPMs.
use engine_core::{MoveIntent, WeaponId};
use engine_renderer::{CapturedFrame, FrameResult, RenderConfig, Renderer, ValidationPreference};
use engine_shell::{DriverPreference, ShellApp, ShellConfig};
use engine_slice::encounter_demo::EncounterDemo;
use engine_slice::encounter_frame::EncounterFrame;
use std::{
    fs::File,
    io::{BufWriter, Write},
    time::Duration,
};

fn capture(
    app: &mut ShellApp,
    renderer: &mut Renderer,
    frame: &mut EncounterFrame,
    demo: &EncounterDemo,
    label: &str,
) -> (CapturedFrame, FrameResult) {
    let size = app.window().size_in_pixels();
    frame.prepare(demo, size);
    for _ in 0..2 {
        let _ = app.frame();
        frame.draw(renderer, None, size).unwrap();
    }
    renderer.request_frame_capture().unwrap();
    let result = frame.draw(renderer, None, size).unwrap();
    let image = renderer.take_frame_capture().unwrap().unwrap();
    assert_eq!((image.width, image.height), size);
    assert_eq!(
        (
            result.geometry.scene_omitted,
            result.geometry.overlay_omitted
        ),
        (0, 0)
    );
    if let Some(path) = std::env::var_os("CYCLE5_CAPTURE_DIR") {
        let mut file = BufWriter::new(
            File::create(std::path::PathBuf::from(path).join(format!("{label}.ppm"))).unwrap(),
        );
        write!(file, "P6\n{} {}\n255\n", image.width, image.height).unwrap();
        for y in 0..image.height {
            for x in 0..image.width {
                file.write_all(&image.pixel_rgba(x, y)[..3]).unwrap();
            }
        }
    }
    (image, result)
}

fn count(image: &CapturedFrame, region: [f32; 4], is: impl Fn([u8; 4]) -> bool) -> usize {
    let [x0, y0, x1, y1] = region;
    let mut n = 0;
    for y in (y0 * image.height as f32) as u32..(y1 * image.height as f32) as u32 {
        for x in (x0 * image.width as f32) as u32..(x1 * image.width as f32) as u32 {
            n += usize::from(is(image.pixel_rgba(x, y)));
        }
    }
    n
}

#[test]
#[ignore = "requires Vulkan presentation, validation layer and swapchain readback"]
fn encounter_camera_weapons_cover_and_resize_reach_pixels() {
    let mut app = ShellApp::new(ShellConfig {
        title: "Courtyard capture".into(),
        width: 1280,
        height: 720,
        target_fps: 60,
        driver: DriverPreference::Real,
    })
    .unwrap();
    let mut renderer = Renderer::new(
        app.window(),
        RenderConfig {
            validation: ValidationPreference::On,
            replace_pipelines: true,
            ..Default::default()
        },
    )
    .unwrap();
    renderer.set_overlay_enabled(false);
    assert!(renderer.stats().validation_active);
    let mut demo = EncounterDemo::new();
    // A live activation tick, then aim down the left lane at the rotated slab.
    demo.driver.state_mut().position = [-5.5, 0., 1.5];
    demo.tick(MoveIntent::default(), false);
    let mut frame = EncounterFrame::default();
    frame.observe(&mut demo, Duration::ZERO, |_| {});
    let (precision, p) = capture(&mut app, &mut renderer, &mut frame, &demo, "precision");
    assert!(p.geometry.foreground_drawn > 100);
    assert!(
        p.geometry.overlay_drawn > 100,
        "combat HUD survives debug toggle"
    );
    let cyan = |[r, g, b, _]: [u8; 4]| g > 150 && b > 160 && r < 100;
    let orange = |[r, g, b, _]: [u8; 4]| r > 155 && g < 160 && b < 75;
    let amber = |[r, g, b, _]: [u8; 4]| r > 190 && g > 144 && b < 125;
    let near = [0.51, 0.53, 0.99, 0.88];
    assert!(
        count(&precision, near, cyan) > 30,
        "visible cyan precision emitter"
    );
    demo.apply_selection(WeaponId::Spread);
    frame.observe(&mut demo, Duration::ZERO, |_| {});
    let (spread, _) = capture(&mut app, &mut renderer, &mut frame, &demo, "spread");
    assert!(
        count(&spread, near, orange) > 100,
        "visible orange spread housing"
    );
    let aim = [0.38, 0.415, 0.62, 0.515];
    let idle_amber = count(&spread, aim, amber);
    demo.tick(MoveIntent::default(), true);
    frame.observe(&mut demo, Duration::ZERO, |_| {});
    assert_eq!(
        demo.encounter.counters.blocked, 5,
        "all five rays intercepted by slab"
    );
    assert_eq!(frame.visuals.stats().live_rays, 5);
    let (firing, _) = capture(&mut app, &mut renderer, &mut frame, &demo, "spread-cover");
    let shot_amber = count(&firing, aim, amber);
    assert!(
        shot_amber > idle_amber + 55,
        "visible beams and cover sparks {shot_amber} vs {idle_amber}"
    );
    demo.driver.state_mut().toggle_camera_mode();
    let (chase, tp) = capture(&mut app, &mut renderer, &mut frame, &demo, "third-person");
    assert_eq!(tp.geometry.foreground_drawn, 0);
    let person = [0.13, 0.40, 0.85, 0.91];
    let spread_color = count(&chase, person, orange);
    assert!(
        spread_color > 90,
        "spread side pods visible in third person: {spread_color}"
    );

    frame.observe(&mut demo, Duration::from_secs(1), |_| {});
    assert_eq!(frame.visuals.stats().live_rays, 0);
    demo.apply_selection(WeaponId::Precision);
    frame.observe(&mut demo, Duration::ZERO, |_| {});
    let (tp_precision, p) = capture(
        &mut app,
        &mut renderer,
        &mut frame,
        &demo,
        "third-precision",
    );
    assert_eq!(p.geometry.foreground_drawn, 0);
    let precision_color = count(&tp_precision, person, cyan);
    assert!(
        precision_color > 15,
        "precision coil visible in third person: {precision_color}"
    );
    assert!(count(&tp_precision, person, orange) < spread_color / 2);
    // A real precision hit into a live target, with a clear short sightline.
    demo.driver.state_mut().toggle_camera_mode();
    demo.driver.state_mut().position = [0.0, 0.0, -4.5];
    demo.tick(MoveIntent::default(), false);
    assert!(demo.encounter.wave2_activated);
    let target = demo.spine.world().adversary(0).position;
    let (dx, dz) = [(0.0_f32, 4.0_f32), (0., -4.), (4., 0.), (-4., 0.)]
        .into_iter()
        .find(|(dx, dz)| {
            demo.driver
                .world()
                .segment_hit(
                    [target[0] + dx, 1.6, target[2] + dz],
                    [target[0], 1.6, target[2]],
                    0.,
                )
                .is_none()
        })
        .expect("a clear lane to the live target");
    demo.driver.state_mut().position = [target[0] + dx, 0., target[2] + dz];
    demo.driver.state_mut().yaw = dx.atan2(dz);
    demo.driver.state_mut().pitch = 0.;
    demo.tick(MoveIntent::default(), true);
    frame.observe(&mut demo, Duration::ZERO, |_| {});
    assert_eq!(
        demo.encounter.counters.hits,
        1,
        "{:?} at {:?}, target {:?}, {:?}",
        demo.encounter.last_fire_line,
        demo.eye(),
        target,
        demo.encounter.phase
    );
    let (hit, _) = capture(&mut app, &mut renderer, &mut frame, &demo, "precision-hit");
    let pink = |[r, g, b, _]: [u8; 4]| r > 210 && g < 115 && b > 85 && b < 155;
    assert!(
        count(&hit, [0.44, 0.40, 0.56, 0.59], pink) > 45,
        "adversary impact and hit marker"
    );
    demo.reset();
    frame.observe(&mut demo, Duration::ZERO, |_| {});
    let mut window = app.window().clone();
    window.set_size(800, 600).unwrap();
    window.sync();
    let (resized, _) = capture(&mut app, &mut renderer, &mut frame, &demo, "resize-reset");
    assert_eq!((resized.width, resized.height), (800, 600));
    assert!(renderer.stats().swapchain_recreations > 0);
    assert!(count(&resized, near, cyan) > 10);
    assert_eq!(renderer.stats().validation_errors, 0);
}
