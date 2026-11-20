//! Paired GPU workload for the four-adversary encounter. Each half uses the
//! same input script and simulation in this binary. The control draws only
//! the existing arena; the treatment goes through the playable presentation.

use std::time::{Duration, Instant};

use engine_core::{CameraMode, MoveIntent, WeaponId};
use engine_renderer::config::FRAMES_IN_FLIGHT;
use engine_renderer::{
    GeometryStats, PresentModePreference, RenderConfig, Renderer, ValidationPreference,
};
use engine_shell::{DriverPreference, ShellApp, ShellConfig};

use crate::encounter_demo::{
    ADVERSARY_HALF, EncounterConfig, EncounterCounters, EncounterDemo, EncounterPhase, cover_slab,
    encounter_config, encounter_fingerprint_hex,
};
use crate::encounter_frame::EncounterFrame;
use crate::math::percentile;

const EXTENT: (u32, u32) = (1280, 720);
const WARMUP: usize = 120;
const SAMPLES: usize = 480;
const STEP: Duration = Duration::from_nanos(1_000_000_000 / 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Workload {
    Rest,
    Precision,
    Spread,
    Stress,
}

impl Workload {
    fn fixture(self, camera: CameraMode) -> Result<EncounterDemo, String> {
        // Keep all four targets present throughout both halves. Combat
        // resolution, wave activation and ray/cover classification stay live.
        let config = EncounterConfig {
            player_hp: 100_000.0,
            adversary_hp: 100_000.0,
            adversary_speed: 0.35,
            adversary_spawn: [0.0, 0.0, -10.0],
            spread_cooldown_ticks: if self == Self::Stress {
                1
            } else {
                encounter_config().spread_cooldown_ticks
            },
            ..encounter_config()
        };
        let mut demo = EncounterDemo::new_with_config(config);
        demo.driver.set_trace_recording(false);
        demo.driver.state_mut().camera_mode = camera;
        // Walk through both real activation lines. A stationary sample starts
        // here as well, so rest and firing have identical initial geometry.
        for _ in 0..600 {
            if demo.encounter.wave2_activated {
                break;
            }
            demo.tick(
                MoveIntent {
                    forward: 1,
                    ..MoveIntent::default()
                },
                false,
            );
        }
        if !demo.encounter.wave2_activated || demo.spine.world().count() != 4 {
            return Err("benchmark fixture failed to reach both activation waves".into());
        }
        if matches!(self, Self::Spread | Self::Stress) {
            demo.apply_selection(WeaponId::Spread);
        }
        let now = demo.tick_index();
        demo.encounter.presentation.drain(now, |_, _| {});
        Ok(demo)
    }

    fn inputs(self, frame: usize, demo: &EncounterDemo) -> (MoveIntent, bool) {
        if self == Self::Rest {
            return (MoveIntent::default(), false);
        }
        // Rotate through all four opponents and the slab. Stress changes
        // aim every two ticks, leaving simultaneous effects in the world.
        let dwell = if self == Self::Stress { 2 } else { 37 };
        let index = (frame / dwell) % 5;
        let target = if index == 4 {
            cover_slab().center
        } else {
            let [x, y, z] = demo.spine.world().adversary(index).position;
            [x, y + ADVERSARY_HALF[1], z]
        };
        let [x, y, z] = demo.eye();
        let [dx, dy, dz] = [target[0] - x, target[1] - y, target[2] - z];
        let player = demo.driver.state();
        let intent = MoveIntent {
            look: (
                (-dx).atan2(-dz) - player.yaw,
                dy.atan2(dx.hypot(dz)) - player.pitch,
            ),
            ..MoveIntent::default()
        };
        // Input is accepted on the next logical tick.
        let fire = demo.arsenal.ready_at(demo.arsenal.selected()) <= demo.tick_index() + 1;
        (intent, fire)
    }
}

#[derive(Default)]
struct GeometryTotals {
    scene: u64,
    overlay: u64,
    omitted: u64,
    draws: u64,
    bytes: u64,
    peak_scene: u32,
    peak_overlay: u32,
}

impl GeometryTotals {
    fn add(&mut self, frame: GeometryStats) {
        self.scene += u64::from(frame.scene_drawn);
        self.overlay += u64::from(frame.overlay_drawn);
        self.omitted += u64::from(frame.scene_omitted) + u64::from(frame.overlay_omitted);
        self.draws += u64::from(frame.dynamic_draw_calls);
        self.bytes += frame.uploaded_bytes;
        self.peak_scene = self.peak_scene.max(frame.scene_drawn);
        self.peak_overlay = self.peak_overlay.max(frame.overlay_drawn);
    }
}

fn distribution(samples: &[f64]) -> String {
    if samples.is_empty() {
        return "unavailable".to_string();
    }
    format!(
        "{:.6}/{:.6}/{:.6}",
        percentile(samples, 0.50),
        percentile(samples, 0.95),
        percentile(samples, 0.99)
    )
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    tick: u64,
    digest: String,
    counters: EncounterCounters,
}

fn measure(
    app: &mut ShellApp,
    renderer: &mut Renderer,
    workload: Workload,
    camera: CameraMode,
    integrated: bool,
) -> Result<Outcome, String> {
    let mut demo = workload.fixture(camera)?;
    let mut presentation = EncounterFrame::default();
    let mut cpu = Vec::with_capacity(SAMPLES);
    let mut tick_cpu = Vec::with_capacity(SAMPLES);
    let mut preparation = Vec::with_capacity(SAMPLES);
    let mut gpu = Vec::with_capacity(SAMPLES);
    let mut geometry = GeometryTotals::default();
    let mut warm_bytes = 0;
    let mut growths = 0;
    let mut peak_volleys = 0;
    let mut peak_rays = 0;
    let mut previous_bytes = 0;
    let mut warm_counters = EncounterCounters::default();
    let lag = FRAMES_IN_FLIGHT as usize;
    // Read the two outstanding timestamps after the last measured submission.
    // The drain submits the final prepared frame without advancing simulation.
    for index in 0..WARMUP + SAMPLES + lag {
        let recording = (WARMUP..WARMUP + SAMPLES).contains(&index);
        let start = Instant::now();
        let snapshot = app.frame();
        if snapshot.quit_requested || snapshot.minimized || app.window_size() != EXTENT {
            return Err("benchmark interrupted by close, minimize, or resize".into());
        }
        if index < WARMUP + SAMPLES {
            let (intent, fire) = workload.inputs(index, &demo);
            let tick_start = Instant::now();
            let step = demo.advance_frame(STEP, intent, fire);
            let tick_ms = tick_start.elapsed().as_secs_f64() * 1_000.0;
            if step.executed_ticks != 1
                || step.outstanding_ticks != 0
                || demo.encounter.phase != EncounterPhase::Active
            {
                return Err(format!(
                    "benchmark fixture diverged at {index}: {step:?} {:?}",
                    demo.encounter.phase
                ));
            }
            let prepare_start = Instant::now();
            if integrated {
                presentation.observe(&mut demo, STEP, |_| {});
                presentation.prepare(&demo, EXTENT);
            } else {
                let now = demo.tick_index();
                demo.encounter.presentation.drain(now, |_, _| {});
                presentation.prepare_base(&demo, EXTENT);
            }
            let prepare_ms = prepare_start.elapsed().as_secs_f64() * 1_000.0;
            let bytes = presentation.scratch_bytes();
            if index + 1 == WARMUP {
                warm_bytes = bytes;
                warm_counters = demo.encounter.counters;
            }
            if recording {
                tick_cpu.push(tick_ms);
                preparation.push(prepare_ms);
                growths += usize::from(bytes > previous_bytes);
                let effects = presentation.visuals.stats();
                peak_volleys = peak_volleys.max(effects.live_volleys);
                peak_rays = peak_rays.max(effects.live_rays);
            }
            previous_bytes = bytes;
        }
        let frame = presentation
            .draw(renderer, None, EXTENT)
            .map_err(|e| e.to_string())?;
        let cpu_ms = start.elapsed().as_secs_f64() * 1_000.0;
        if !frame.presented || frame.recreated || frame.suboptimal {
            return Err(format!(
                "benchmark presentation interrupted at {index}: {frame:?}"
            ));
        }
        if recording {
            cpu.push(cpu_ms);
            geometry.add(frame.geometry);
        }
        if (WARMUP + lag..WARMUP + SAMPLES + lag).contains(&index)
            && let Some(ms) = frame.gpu_render_ms
        {
            gpu.push(ms);
        }
    }
    if !gpu.is_empty() && gpu.len() != SAMPLES {
        return Err(format!(
            "incomplete GPU timestamps: {} of {SAMPLES}",
            gpu.len()
        ));
    }
    let outcome = Outcome {
        tick: demo.tick_index(),
        digest: encounter_fingerprint_hex(&demo),
        counters: demo.encounter.counters,
    };
    let fires = outcome.counters.fires - warm_counters.fires;
    let hits = outcome.counters.hits - warm_counters.hits;
    let cover = outcome.counters.blocked - warm_counters.blocked;
    if workload != Workload::Rest && (fires == 0 || hits == 0 || cover == 0) {
        return Err(format!(
            "workload did not exercise firing, hit, and cover: {outcome:?}"
        ));
    }
    let n = SAMPLES as f64;
    let half = if integrated { "integrated" } else { "arena" };
    println!(
        "case={workload:?} camera={camera:?} presentation={half} frames={SAMPLES} fires={fires} hits={hits} cover={cover} fingerprint={}",
        outcome.digest
    );
    println!(
        "  ms p50/p95/p99: frame_cpu={} tick_cpu={} prepare_cpu={} gpu={} gpu_samples={}",
        distribution(&cpu),
        distribution(&tick_cpu),
        distribution(&preparation),
        distribution(&gpu),
        gpu.len()
    );
    println!(
        "  per frame: scene_vertices={:.1} overlay_vertices={:.1} draws={:.2} upload_bytes={:.0} peak_scene={} peak_overlay={} omitted_total={}",
        geometry.scene as f64 / n,
        geometry.overlay as f64 / n,
        geometry.draws as f64 / n,
        geometry.bytes as f64 / n,
        geometry.peak_scene,
        geometry.peak_overlay,
        geometry.omitted
    );
    println!(
        "  retained_vertex_bytes warm={warm_bytes} final={previous_bytes} growth_frames={growths} peak_volleys={peak_volleys} peak_rays={peak_rays} effects_including_warmup={:?} stream={:?}",
        presentation.visuals.stats(),
        demo.encounter.presentation.stats()
    );
    if geometry.omitted > 0 {
        return Err(format!(
            "renderer omitted {} vertices under benchmark workload",
            geometry.omitted
        ));
    }
    Ok(outcome)
}

pub fn run() -> Result<(), String> {
    let mut app = ShellApp::new(ShellConfig {
        title: "Encounter presentation benchmark".into(),
        width: EXTENT.0,
        height: EXTENT.1,
        target_fps: 60,
        driver: DriverPreference::Real,
    })?;
    let mut renderer = Renderer::new(
        app.window(),
        RenderConfig {
            present_mode: PresentModePreference::Immediate,
            validation: ValidationPreference::Off,
            ..RenderConfig::default()
        },
    )
    .map_err(|e| e.to_string())?;
    renderer.set_overlay_enabled(false);
    let stats = renderer.stats();
    if stats.extent != EXTENT {
        return Err(format!(
            "benchmark needs 1280x720, acquired {:?}",
            stats.extent
        ));
    }
    println!(
        "Encounter paired GPU benchmark | {} | Vulkan {} | present={} | {:?} | debug_assertions={} | validation={}",
        stats.gpu_name,
        stats.api_version,
        stats.present_mode,
        stats.extent,
        cfg!(debug_assertions),
        stats.validation_active
    );
    println!(
        "Same-binary arena control and playable presentation; one fixed 60 Hz tick per frame; {WARMUP} warmup + {SAMPLES} measured per half. No pacing sleep, audio or debug text. Frame CPU includes SDL poll, scripted aim, simulation, preparation, Vulkan wait/submit/present. GPU timestamps cover the render pass. Retained bytes count CPU vertex capacity; allocator traffic is uninstrumented."
    );
    println!(
        "Fixture: four long-lived opponents, 0.35 m/s movement, both waves active; ordinary weapon cooldowns, stress uses one five-ray volley per tick."
    );
    for (camera_index, camera) in [CameraMode::FirstPerson, CameraMode::ThirdPerson]
        .into_iter()
        .enumerate()
    {
        for (index, workload) in [
            Workload::Rest,
            Workload::Precision,
            Workload::Spread,
            Workload::Stress,
        ]
        .into_iter()
        .enumerate()
        {
            // Counterbalance order across workloads and cameras to reduce
            // systematic warm-clock bias.
            let integrated_first = (camera_index + index) % 2 == 1;
            let first = measure(&mut app, &mut renderer, workload, camera, integrated_first)?;
            let second = measure(&mut app, &mut renderer, workload, camera, !integrated_first)?;
            if first != second {
                return Err(format!(
                    "paired simulation mismatch in {camera:?} {workload:?}: {first:?} vs {second:?}"
                ));
            }
            println!("  matched logical outcomes at tick {}", first.tick);
        }
    }
    Ok(())
}
