//! Stage 2 render demo (`--render`): Vulkan presentation with a 6-DoF
//! camera and the instrumentation overlay. Camera/navigation policy lives
//! here in the application, not in the renderer (Stage 2 authorization,
//! item 2).

use std::time::{Duration, Instant};

use engine_renderer::{PresentModePreference, RenderConfig, Renderer, ValidationPreference};
use engine_shell::{DriverPreference, InputSnapshot, Keycode, ShellApp, ShellConfig};

use crate::frame_timing::FrameTiming;
use crate::math;
use crate::npc::NpcConfig;
use crate::sim::{SimConfig, SimDriver};

/// Demo options; CLI parsing lives in the binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    /// CPU pacing target. 0 = uncapped measurement mode (real rendering
    /// every iteration — never an empty loop).
    pub fps: u32,
    pub present: PresentModePreference,
    /// Auto-quit after N presented frames (smoke tests).
    pub frames: Option<u64>,
    pub overlay: bool,
    pub validation: ValidationPreference,
    /// Verification scenario (corrective follow-up): frame 11 reports a
    /// different window size for one frame, forcing the swapchain
    /// recreation path headlessly — the app owns window state, so this is
    /// exactly how a real resize reaches the renderer.
    pub resize_test: bool,
    /// Stage 3: artificially constrained event budget for the integrated
    /// simulation (verification — measurable backlog, never silent loss).
    pub sim_budget: Option<u32>,
    /// Stage 3: explicit near-lane reservation for the integrated
    /// simulation. `None` keeps the documented default, which scales
    /// proportionally when the budget is reduced (corrective finding C).
    pub sim_near_reserve: Option<u32>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            fps: 60,
            present: PresentModePreference::Auto,
            frames: None,
            overlay: true,
            validation: ValidationPreference::Auto,
            resize_test: false,
            sim_budget: None,
            sim_near_reserve: None,
        }
    }
}

struct Camera {
    position: [f32; 3],
    yaw: f32,
    pitch: f32,
    roll: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            position: [0.0, 2.0, 9.0],
            yaw: 0.0,
            pitch: -0.12,
            roll: 0.0,
        }
    }
}

impl Camera {
    fn update(&mut self, snapshot: &InputSnapshot, app: &ShellApp, dt: f32) {
        let look = 0.0025;
        self.yaw += snapshot.look_delta.0 * look;
        self.pitch = (self.pitch - snapshot.look_delta.1 * look).clamp(-1.54, 1.54);
        let roll_input = i8::from(app.is_held(Keycode::E)) - i8::from(app.is_held(Keycode::Q));
        self.roll += roll_input as f32 * 1.5 * dt;
        let speed = if app.is_held(Keycode::LShift) {
            12.0
        } else {
            4.0
        };
        let forward = math::camera_forward(self.yaw, self.pitch, self.roll);
        let right = math::camera_right(self.yaw, self.pitch, self.roll);
        for axis in 0..3 {
            self.position[axis] += forward[axis] * f32::from(snapshot.move_forward) * speed * dt
                + right[axis] * f32::from(snapshot.move_strafe) * speed * dt;
        }
        let vertical = i8::from(app.is_held(Keycode::R)) - i8::from(app.is_held(Keycode::F));
        self.position[1] += vertical as f32 * speed * dt;
    }
}

pub fn run_render_demo(options: RenderOptions) -> Result<(), String> {
    let shell_config = ShellConfig {
        title: "Sektor7 original engine — Stage 2 render".to_string(),
        width: 1280,
        height: 720,
        target_fps: 60,
        driver: DriverPreference::Real,
    };
    let mut app = ShellApp::new(shell_config)?;
    app.set_relative_mouse(true);

    let render_config = RenderConfig {
        present_mode: options.present,
        validation: options.validation,
    };
    let mut renderer =
        Renderer::new(app.window(), render_config).map_err(|error| error.to_string())?;
    renderer.set_overlay_enabled(options.overlay);

    // Stage 3: the campaign simulation runs beside rendering — render
    // cadence stays independent of the fixed simulation cadence (the
    // renderer imposes no ceiling; the sim accrues its own ticks from
    // elapsed real time, which never enters logical state).
    let mut sim_config = SimConfig::default();
    if let Some(budget) = options.sim_budget {
        sim_config.budget = budget;
    }
    sim_config.near_reserve = options.sim_near_reserve;
    let mut sim = SimDriver::new(NpcConfig::default(), sim_config);

    {
        let stats = renderer.stats();
        println!("Sektor7 Stage 2 — Vulkan first render");
        println!(
            "  device: {} {} (Vulkan {})",
            stats.device_type, stats.gpu_name, stats.api_version
        );
        println!(
            "  present: {} (supported: {}) · {} swapchain images · {}x{} · validation: {}",
            stats.present_mode,
            stats.supported_present_modes.join("/"),
            stats.swapchain_images,
            stats.extent.0,
            stats.extent.1,
            stats.validation_active
        );
        println!(
            "  controls: WASD move · mouse look · Q/E roll · R/F up/down · \
             Shift fast · O overlay · Esc/close quit"
        );
    }

    let mut camera = Camera::default();
    let mut cpu_work_ms: Vec<f64> = Vec::new();
    let mut frame_intervals_ms: Vec<f64> = Vec::new();
    let mut presented: u64 = 0;
    let start = Instant::now();
    let frame_interval = if options.fps == 0 {
        None
    } else {
        Some(Duration::from_secs_f64(
            1.0 / f64::from(options.fps.clamp(1, 1000)),
        ))
    };
    let mut timing = FrameTiming::new();

    loop {
        let frame_start = Instant::now();
        let snapshot = app.frame();
        if snapshot.quit_requested || snapshot.keys_pressed.contains(&Keycode::Escape) {
            break;
        }
        if snapshot.keys_pressed.contains(&Keycode::O) {
            renderer.set_overlay_enabled(!renderer.overlay_enabled());
        }
        // Frame-boundary timing (corrective finding A): the simulation
        // accrues the FULL raw interval — rendering stalls and minimized
        // periods included, never silently discarded; camera movement is
        // capped against post-stall view jumps. `rendering` is false
        // while the window is minimized, but the campaign clock keeps
        // advancing while the application runs.
        let split = timing.begin_frame(frame_start, !snapshot.minimized);
        // Full frame interval — includes the previous iteration's
        // pacing sleep; distinct from the CPU work measurement below.
        // Capped at 100 ms in the readout (it measures pacing quality,
        // not stall length).
        frame_intervals_ms.push((split.sim_advance.as_secs_f64() * 1000.0).min(100.0));
        let len = frame_intervals_ms.len();
        if len > 600 {
            frame_intervals_ms.drain(0..len - 600);
        }

        if snapshot.minimized {
            // Rendering is unavailable, but the application is still
            // running: the campaign clock advances by the full interval
            // — no silent pause policy (corrective finding A). Poll
            // quietly; the next frame's interval includes this sleep.
            sim.advance(split.sim_advance);
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        camera.update(
            &snapshot,
            &app,
            split.camera_dt.unwrap_or_default().as_secs_f32(),
        );

        // Fixed-step simulation advance: the full raw elapsed interval
        // accrues due ticks (bounded per-frame catch-up; outstanding
        // obligations are retained and reported, never silently
        // discarded).
        sim.advance(split.sim_advance);

        let (width, height) = renderer.stats().extent;
        let aspect = if height == 0 {
            16.0 / 9.0
        } else {
            width as f32 / height as f32
        };
        let view = math::camera_view(camera.position, camera.yaw, camera.pitch, camera.roll);
        let projection = math::perspective(std::f32::consts::FRAC_PI_3, aspect, 0.1, 200.0);
        let view_proj = math::mul(&projection, &view);

        let overlay_text = renderer.overlay_enabled().then(|| {
            let elapsed = start.elapsed().as_secs_f64().max(0.001);
            let stats = renderer.stats();
            format!(
                "Sektor7 Stage 2 - Vulkan\nFPS {:.0} (cpu work p95 {:.2} ms / gpu p95 {:.2} ms)\npresent {} / {}x{} / {} images / recreations {}\n{} {} / Vulkan {}\nvalidation {} (err {} / warn {})\nsim tick {} @ {} Hz · out {} · ev/s {:.0}\nnear/far {}/{} · pend {} · blk {} · late {} · bud-exh {}\nwm {} / {}\ncam ({:.1}, {:.1}, {:.1}) yaw {:.2} pitch {:.2}",
                presented as f64 / elapsed,
                math::percentile(&cpu_work_ms, 0.95),
                stats.gpu_ms_p95(),
                stats.present_mode,
                width,
                height,
                stats.swapchain_images,
                stats.swapchain_recreations,
                stats.device_type,
                stats.gpu_name,
                stats.api_version,
                stats.validation_active,
                stats.validation_errors,
                stats.validation_warnings,
                sim.spine().clock(),
                sim.config().tick_rate_hz,
                sim.outstanding_ticks(),
                sim.spine().stats().executed as f64 / elapsed,
                sim.spine().near_pending_count(),
                sim.spine().far_pending_count(),
                sim.spine().pending_count(),
                sim.spine().blocked_count(),
                sim.spine().stats().late_executions,
                sim.spine().stats().budget_exhausted_ticks,
                sim.spine()
                    .processed_through()
                    .map(|tick| tick.as_u64().to_string())
                    .unwrap_or_else(|| "none".into()),
                sim.spine()
                    .max_executed_due()
                    .map(|tick| tick.as_u64().to_string())
                    .unwrap_or_else(|| "none".into()),
                camera.position[0],
                camera.position[1],
                camera.position[2],
                camera.yaw,
                camera.pitch,
            )
        });

        let window_size = if options.resize_test && presented == 10 {
            let (width, height) = app.window_size();
            (width + 128, height + 72)
        } else {
            app.window_size()
        };
        let frame_result = renderer
            .render_frame(&view_proj, overlay_text.as_deref(), window_size)
            .map_err(|error| format!("{error}"))?;

        if frame_result.deferred {
            // The drawable is temporarily unavailable (minimized or
            // mid-transition): poll quietly, present nothing, count
            // nothing, and retry next frame without a recreation storm.
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        if frame_result.presented {
            presented += 1;
            // CPU work duration: poll → camera → overlay build → render
            // → present call, excluding the pacing sleep (distinct from
            // the paced frame interval above).
            cpu_work_ms.push((frame_start.elapsed().as_secs_f64() * 1000.0).min(1000.0));
            let len = cpu_work_ms.len();
            if len > 600 {
                cpu_work_ms.drain(0..len - 600);
            }
        }
        if let Some(limit) = options.frames
            && presented >= limit
        {
            break;
        }
        if let Some(interval) = frame_interval {
            let elapsed = frame_start.elapsed();
            if elapsed < interval {
                std::thread::sleep(interval - elapsed);
            }
        }
    }

    let elapsed = start.elapsed();
    let stats = renderer.stats();
    println!("--- run summary ---");
    println!(
        "presented {} frames over {:.1}s ({:.1} fps avg)",
        presented,
        elapsed.as_secs_f64(),
        presented as f64 / elapsed.as_secs_f64().max(0.001)
    );
    println!(
        "cpu work (poll→present, excl. pacing sleep): p50 {:.2} ms · p95 {:.2} ms · p99 {:.2} ms",
        math::percentile(&cpu_work_ms, 0.50),
        math::percentile(&cpu_work_ms, 0.95),
        math::percentile(&cpu_work_ms, 0.99)
    );
    println!(
        "paced frame interval (incl. pacing sleep; target {}): p50 {:.2} ms · p95 {:.2} ms · p99 {:.2} ms",
        match options.fps {
            0 => "uncapped".to_string(),
            fps => format!("{fps} fps"),
        },
        math::percentile(&frame_intervals_ms, 0.50),
        math::percentile(&frame_intervals_ms, 0.95),
        math::percentile(&frame_intervals_ms, 0.99)
    );
    println!(
        "gpu render (timestamps, {} samples): p50 {:.2} ms · p95 {:.2} ms · p99 {:.2} ms",
        stats.gpu_sample_count(),
        stats.gpu_ms_p50(),
        stats.gpu_ms_p95(),
        stats.gpu_ms_p99()
    );
    println!(
        "present mode {} · swapchain recreations {} · validation err/warn {}/{}",
        stats.present_mode,
        stats.swapchain_recreations,
        stats.validation_errors,
        stats.validation_warnings
    );
    println!("--- simulation summary ---");
    let spine = sim.spine();
    let sim_stats = spine.stats();
    let totals = spine.lane_totals();
    println!(
        "config: {} Hz tick rate · budget {} · near reserve {} · catch-up {}/frame",
        sim.config().tick_rate_hz,
        spine.budget(),
        sim.config().near_reserve_description(),
        sim.config().max_catchup_per_frame
    );
    println!(
        "sim time: advanced {:.1} ms · ticks due {} = campaign {} + outstanding {} (accounted, never discarded)",
        timing.total_sim_advanced().as_secs_f64() * 1000.0,
        sim.ticks_due_total(),
        spine.clock(),
        sim.outstanding_ticks()
    );
    println!(
        "campaign tick {} · executed {} · late arrivals {} · late executions {}",
        spine.clock(),
        sim_stats.executed,
        sim_stats.late_arrivals,
        sim_stats.late_executions
    );
    println!(
        "backlog pending {} (near {} / far {}) · blocked {} · budget-exhausted ticks {}",
        spine.pending_count(),
        spine.near_pending_count(),
        spine.far_pending_count(),
        spine.blocked_count(),
        sim_stats.budget_exhausted_ticks
    );
    println!(
        "lanes near {} (backlogged ticks {}) · far {} (backlogged ticks {})",
        totals.near_executed,
        totals.near_backlogged_ticks,
        totals.far_executed,
        totals.far_backlogged_ticks
    );
    println!(
        "sim tick time: p50 {:.3} ms · p95 {:.3} ms (per-tick, {} samples)",
        sim.tick_ms_percentile(0.50),
        sim.tick_ms_percentile(0.95),
        sim.tick_sample_count()
    );
    println!(
        "watermark max_executed_due {} · processed_through {}",
        spine
            .max_executed_due()
            .map(|tick| tick.as_u64().to_string())
            .unwrap_or_else(|| "none".into()),
        spine
            .processed_through()
            .map(|tick| tick.as_u64().to_string())
            .unwrap_or_else(|| "none".into())
    );
    println!(
        "npc activity steps {} · fingerprints engine-lanes {} · state-lanes {}",
        spine.world().total_steps(),
        spine.lane_fingerprint_hex(),
        spine.lane_state_fingerprint_hex()
    );
    for message in &stats.recent_validation {
        println!("  validation: {message}");
    }
    // Verification requirement: with the validation layer ACTIVE, any
    // recorded error is a hard failure — this is what the CI validation
    // smoke gates on. An unavailable layer is an environment limitation,
    // not an application error, and is reported honestly above instead.
    if stats.validation_active && stats.validation_errors > 0 {
        return Err(format!(
            "validation layer reported {} error(s) while active — see the messages above",
            stats.validation_errors
        ));
    }
    println!(
        "note: instrument readings of the simple Stage 2 scene — not \
         first-game performance claims; no growth observed is not proof \
         that leaks are impossible."
    );
    println!(
        "evidence note: real-GPU GUI evidence requires a graphical session \
         with hardware Vulkan; software-ICD runs (lavapipe) are correctness \
         smoke only."
    );
    Ok(())
}
