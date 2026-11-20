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
    write_ppm(&image, label);
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

const FULL: [f32; 4] = [0., 0., 1., 1.];

/// The LEGACY green player-box proxy's facet-lit signature (GO §5: its
/// absence from the live encounter is an explicit assertion, not a
/// side effect of other counts). The proxy color is `[0.20, 0.80,
/// 0.35]`; under the box builder's 0.39..1.0 facet lighting its pixels
/// land at r 20..51, g 80..204, b 35..89 — green-dominant with low red
/// AND low blue, and (the ratio discriminator) a FIXED g:b ratio of
/// 0.80:0.35 ≈ 2.29 under the uniform facet lighting, so g > 2·b holds
/// on every proxy facet. The operator model's teal dorsal stripe
/// (`[0.25, 0.71, 0.59]`, g:b ≈ 1.20) is excluded by the blue bound
/// (brightest facet b 150 > 105), the green bound (darkest facet
/// g 71 < 75), AND the ratio (no stripe/suit anti-alias blend can
/// exceed g:b ≈ 1.2 — the measured blend pixel (43,123,102) that
/// motivated the ratio is excluded three ways). The courtyard's teal
/// activation markings are blue-dominant. A positive control below
/// proves the detector finds the box when a composition draws it.
fn legacy_box_green([r, g, b, _]: [u8; 4]) -> bool {
    u32::from(g) > 75
        && u32::from(r) < 70
        && u32::from(b) < 105
        && u32::from(g) * 5 > u32::from(r) * 11
        && u32::from(g) > 2 * u32::from(b)
}

/// Pixels where two same-size captures disagree materially (a channel
/// sum delta above 30) inside `region` — the silhouette-difference key
/// that severe occlusion cannot fake: the two weapons must present
/// DIFFERENT geometry in the SAME screen region.
fn region_diff(a: &CapturedFrame, b: &CapturedFrame, region: [f32; 4]) -> usize {
    let [x0, y0, x1, y1] = region;
    assert_eq!((a.width, a.height), (b.width, b.height));
    let mut n = 0;
    for y in (y0 * a.height as f32) as u32..(y1 * a.height as f32) as u32 {
        for x in (x0 * a.width as f32) as u32..(x1 * a.width as f32) as u32 {
            let p = a.pixel_rgba(x, y);
            let q = b.pixel_rgba(x, y);
            let delta: u32 = p.iter().zip(q).map(|(u, v)| u32::from(u.abs_diff(v))).sum();
            n += usize::from(delta > 30);
        }
    }
    n
}

/// How many cells of a `grid`×`grid` tiling of `region` contain at
/// least one matching pixel — spatial spread, so a handful of
/// scattered pixels can never satisfy a presence claim the way a raw
/// low pixel count can.
fn cell_coverage(
    image: &CapturedFrame,
    region: [f32; 4],
    is: impl Fn([u8; 4]) -> bool,
    grid: u32,
) -> u32 {
    let [x0, y0, x1, y1] = region;
    let (rx0, rx1) = (
        (x0 * image.width as f32) as u32,
        (x1 * image.width as f32) as u32,
    );
    let (ry0, ry1) = (
        (y0 * image.height as f32) as u32,
        (y1 * image.height as f32) as u32,
    );
    let mut covered = 0;
    for gy in 0..grid {
        for gx in 0..grid {
            let (cx0, cx1) = (
                rx0 + (rx1 - rx0) * gx / grid,
                rx0 + (rx1 - rx0) * (gx + 1) / grid,
            );
            let (cy0, cy1) = (
                ry0 + (ry1 - ry0) * gy / grid,
                ry0 + (ry1 - ry0) * (gy + 1) / grid,
            );
            'cell: for y in cy0..cy1 {
                for x in cx0..cx1 {
                    if is(image.pixel_rgba(x, y)) {
                        covered += 1;
                        break 'cell;
                    }
                }
            }
        }
    }
    covered
}

fn write_ppm(image: &CapturedFrame, label: &str) {
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
}

/// The arena-only reference capture (the benchmark's `prepare_base`
/// path) — used as the POSITIVE CONTROL for the legacy-box detector:
/// this composition still draws the green player-box proxy in third
/// person, so the detector's live-path zero is proven absence, never
/// blindness.
fn capture_base(
    app: &mut ShellApp,
    renderer: &mut Renderer,
    frame: &mut EncounterFrame,
    demo: &EncounterDemo,
    label: &str,
) -> CapturedFrame {
    let size = app.window().size_in_pixels();
    frame.prepare_base(demo, size);
    for _ in 0..2 {
        let _ = app.frame();
        frame.draw(renderer, None, size).unwrap();
    }
    renderer.request_frame_capture().unwrap();
    frame.draw(renderer, None, size).unwrap();
    let image = renderer.take_frame_capture().unwrap().unwrap();
    assert_eq!((image.width, image.height), size);
    write_ppm(&image, label);
    image
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
    // GO §5 — the legacy green player-box proxy is EXPLICITLY absent
    // from the live first-person presentation (the structural
    // correction: `prepare` renders without the box proxy).
    assert_eq!(
        count(&precision, FULL, legacy_box_green),
        0,
        "the legacy green box is absent from the live FP precision view"
    );
    demo.apply_selection(WeaponId::Spread);
    frame.observe(&mut demo, Duration::ZERO, |_| {});
    let (spread, _) = capture(&mut app, &mut renderer, &mut frame, &demo, "spread");
    assert!(
        count(&spread, near, orange) > 100,
        "visible orange spread housing"
    );
    assert_eq!(
        count(&spread, FULL, legacy_box_green),
        0,
        "the legacy green box is absent from the live FP spread view"
    );
    // GO §5 — DISCERNIBLY DIFFERENT silhouettes: the two weapons occupy
    // the same screen region with different geometry, so a large
    // fraction of that region's pixels must differ between the two
    // captures (severe occlusion of one weapon shrinks this toward
    // zero — a low pixel count cannot fake it). Measured 68220 at the
    // deterministic capture; the threshold keeps an order of
    // magnitude of headroom.
    let near_silhouette = region_diff(&precision, &spread, near);
    eprintln!("[visuals] FP weapon-region silhouette diff: {near_silhouette}");
    assert!(
        near_silhouette > 8000,
        "the two weapons present discernibly different FP silhouettes \
         ({near_silhouette} differing pixels in the weapon region)"
    );
    // The weapon occupies a real REGION, not a few scattered pixels:
    // the orange housing must reach a broad fraction of a 6x6 cell
    // tiling of the weapon region.
    let near_cells = cell_coverage(&spread, near, orange, 6);
    eprintln!("[visuals] FP spread housing cell coverage: {near_cells}/36");
    assert!(
        near_cells >= 12,
        "the spread housing fills a broad region ({near_cells}/36 cells)"
    );
    // The HUD names the selected weapon: the bottom-right label region
    // must change between the two selections ("01 PRECISION" vs
    // "02 SPREAD").
    let label_region = [0.78, 0.88, 0.999, 0.999];
    let label_silhouette = region_diff(&precision, &spread, label_region);
    eprintln!("[visuals] HUD weapon-label diff: {label_silhouette}");
    assert!(
        label_silhouette > 400,
        "the HUD label reflects the selected weapon ({label_silhouette} \
         differing pixels; measured 2118)"
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
    // GO §5 — the operator geometry must not obscure the weapon, and
    // the legacy green box must be absent from the live third-person
    // view (the reported Codex-era defect; the structural correction
    // renders the live path without the box proxy).
    assert_eq!(
        count(&chase, FULL, legacy_box_green),
        0,
        "the legacy green box is absent from the live TP spread view"
    );
    let person_cells = cell_coverage(&chase, person, orange, 6);
    eprintln!("[visuals] TP spread pods cell coverage: {person_cells}/36");
    assert!(
        person_cells >= 3,
        "the held spread weapon forms a compact visible cluster \
         ({person_cells}/36 cells; the pods are ~37x24 px at chase \
         distance — a scatter of stray pixels cannot satisfy this)"
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
    assert_eq!(
        count(&tp_precision, FULL, legacy_box_green),
        0,
        "the legacy green box is absent from the live TP precision view"
    );
    // GO §5 — the two held weapons present discernibly different
    // third-person silhouettes in the operator region (measured 4118;
    // the threshold keeps 5x headroom — occlusion of either weapon
    // shrinks the diff toward the shared operator-only background).
    let person_silhouette = region_diff(&chase, &tp_precision, person);
    eprintln!("[visuals] TP weapon silhouette diff: {person_silhouette}");
    assert!(
        person_silhouette > 800,
        "the two weapons present discernibly different TP silhouettes \
         ({person_silhouette} differing pixels in the operator region)"
    );
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
    // GO §5 — the views at different yaw/pitch orientations: a yawed
    // and pitched third-person view must keep the attached weapon
    // visible and the legacy box absent (the attachment follows the
    // operator through orientation changes, not just the default pose).
    demo.driver.state_mut().toggle_camera_mode();
    demo.driver.state_mut().yaw = std::f32::consts::FRAC_PI_2;
    demo.driver.state_mut().pitch = 0.25;
    frame.observe(&mut demo, Duration::ZERO, |_| {});
    let (yawed, _) = capture(&mut app, &mut renderer, &mut frame, &demo, "third-yawed");
    let yawed_weapon = count(&yawed, FULL, cyan);
    eprintln!("[visuals] yawed TP precision coil pixels: {yawed_weapon}");
    assert!(
        yawed_weapon > 60,
        "the precision weapon stays attached and visible at the yawed \
         and pitched third-person view ({yawed_weapon} coil pixels)"
    );
    assert_eq!(
        count(&yawed, FULL, legacy_box_green),
        0,
        "the legacy green box is absent at the yawed third-person view"
    );
    // THE POSITIVE CONTROL — the legacy-box detector must FIND the box
    // when a composition actually draws it: the benchmark's arena-only
    // reference path (`prepare_base`) still renders the green
    // player-box proxy in third person. This proves the live-path
    // zeros above are genuine absence, never detector blindness.
    let proxy = capture_base(
        &mut app,
        &mut renderer,
        &mut frame,
        &demo,
        "box-proxy-control",
    );
    let proxy_green = count(&proxy, FULL, legacy_box_green);
    eprintln!("[visuals] box-proxy control green pixels: {proxy_green}");
    assert!(
        proxy_green > 300,
        "the detector finds the drawn proxy box ({proxy_green} green \
         pixels) — the live-path zeros are proven absence"
    );
    assert_eq!(renderer.stats().validation_errors, 0);
}
