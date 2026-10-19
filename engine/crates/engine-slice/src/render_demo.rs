//! Stage 2 render demo (`--render`): Vulkan presentation with a 6-DoF
//! camera and the instrumentation overlay. Camera/navigation policy lives
//! here in the application, not in the renderer (Stage 2 authorization,
//! item 2).

use std::time::{Duration, Instant};

use engine_renderer::{PresentModePreference, RenderConfig, Renderer, ValidationPreference};
use engine_shell::{DriverPreference, InputSnapshot, Keycode, ShellApp, ShellConfig};

use crate::math;

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
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            fps: 60,
            present: PresentModePreference::Auto,
            frames: None,
            overlay: true,
            validation: ValidationPreference::Auto,
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
    let mut cpu_intervals_ms: Vec<f64> = Vec::new();
    let mut presented: u64 = 0;
    let start = Instant::now();
    let frame_interval = if options.fps == 0 {
        None
    } else {
        Some(Duration::from_secs_f64(
            1.0 / f64::from(options.fps.clamp(1, 1000)),
        ))
    };
    let mut last_frame_start = Instant::now();

    loop {
        let frame_start = Instant::now();
        let dt = (frame_start - last_frame_start).as_secs_f64().min(0.1);
        last_frame_start = frame_start;

        let snapshot = app.frame();
        if snapshot.quit_requested || snapshot.keys_pressed.contains(&Keycode::Escape) {
            break;
        }
        if snapshot.keys_pressed.contains(&Keycode::O) {
            renderer.set_overlay_enabled(!renderer.overlay_enabled());
        }
        if snapshot.minimized {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        camera.update(&snapshot, &app, dt as f32);

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
                "Sektor7 Stage 2 - Vulkan\nFPS {:.0} (cpu p95 {:.2} ms / gpu p95 {:.2} ms)\npresent {} / {}x{} / {} images / recreations {}\n{} {} / Vulkan {}\nvalidation {} (err {} / warn {})\ncam ({:.1}, {:.1}, {:.1}) yaw {:.2} pitch {:.2}",
                presented as f64 / elapsed,
                math::percentile(&cpu_intervals_ms, 0.95),
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
                camera.position[0],
                camera.position[1],
                camera.position[2],
                camera.yaw,
                camera.pitch,
            )
        });

        let frame_result = renderer
            .render_frame(&view_proj, overlay_text.as_deref(), app.window_size())
            .map_err(|error| format!("{error}"))?;

        if frame_result.presented {
            presented += 1;
            cpu_intervals_ms.push((frame_start.elapsed().as_secs_f64() * 1000.0).min(1000.0));
            let len = cpu_intervals_ms.len();
            if len > 600 {
                cpu_intervals_ms.drain(0..len - 600);
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
        "cpu frame+present: p50 {:.2} ms · p95 {:.2} ms · p99 {:.2} ms",
        math::percentile(&cpu_intervals_ms, 0.50),
        math::percentile(&cpu_intervals_ms, 0.95),
        math::percentile(&cpu_intervals_ms, 0.99)
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
    for message in &stats.recent_validation {
        println!("  validation: {message}");
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
