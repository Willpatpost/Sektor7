//! Cycle 1 P3 — the player demo (application layer).
//!
//! Game-side policy over the reusable engine-core mechanics
//! (P5's engine/game boundary, as with the free-flight camera): the
//! demo owns tuning values, key bindings, the world CONTENT (the
//! collision boxes mirror the rendered cube field — collide-what-you-
//! see), the input mapping from shell snapshots to `MoveIntent`s, and
//! presentation. The engine/game boundary is exact: speeds, bindings,
//! and world content live here and only here.
//!
//! Two paths:
//!
//! * `--player` — the interactive Vulkan demo (the `render_demo`
//!   skeleton with the free-flight camera replaced by the player:
//!   movement advances on the fixed-step `PlayerDriver` over the FULL
//!   raw frame interval — the SimDriver no-discard discipline — never
//!   on the render-paced `camera_dt`; camera poses are DERIVED from
//!   player state, first- and third-person from the same state, and
//!   toggling never teleports or resets the player).
//! * `--player-headless` — the deterministic headless path (the
//!   `--sim` pattern): scripted per-tick intents, a printed summary,
//!   and the trace fingerprint. Cross-platform bitwise reproducibility
//!   holds for these scripted runs because every script keeps yaw and
//!   pitch at exactly zero (the libm-free class; see the matrix
//!   amendment) — the same-binary claim covers everything else.
//!
//! Persistence limitation (explicit, per charter): the player state is
//! TRANSIENT — nothing here is saved, no canonical save format or
//! historical fingerprint is touched, and a restart resets the player.

use std::time::{Duration, Instant};

use engine_core::{
    CameraMode, CollisionWorld, MoveIntent, PlayerConfig, PlayerDriver, PlayerState, camera_pose,
};
use engine_renderer::scene::{SceneVertex, push_box};
use engine_renderer::{PresentModePreference, RenderConfig, Renderer, ValidationPreference};
use engine_shell::{DriverPreference, InputSnapshot, Keycode, ShellApp, ShellConfig};

use crate::frame_timing::FrameTiming;
use crate::math;

/// Demo movement tick rate (a demo policy value, not an engine-wide
/// rate — the same reviewer clarification `sim.rs` records).
pub const DEMO_TICK_HZ: u32 = 60;

/// Mouse-look sensitivity (radians per pixel) — the free-flight
/// camera's value, kept so the two modes feel the same.
pub const DEMO_LOOK_SENSITIVITY: f32 = 0.0025;

/// Game-side tuning: every gameplay value the demo sets (the
/// engine-core defaults, written out here so the boundary is visible
/// and the demo can diverge without touching the engine).
pub fn demo_player_config() -> PlayerConfig {
    PlayerConfig {
        walk_speed: 4.0,
        sprint_speed: 12.0,
        gravity: 25.0,
        jump_speed: 8.0,
        terminal_fall: 50.0,
        height: 1.7,
        radius: 0.35,
        eye_height: 1.6,
        tp_distance: 4.5,
        tp_lift: 1.2,
        tp_min_distance: 1.2,
        pitch_limit: 1.54,
        // 0.0 keeps the Cycle-1 demo path bit-identical (the frozen
        // trace's guarantee); the C2-P1 playable demo sets its own
        // step height explicitly.
        step_height: 0.0,
    }
}

/// The demo collision world: the rendered scene's ground plate, its
/// 5×5 cube field, and the three depth-demonstrator cubes — box for
/// box what `engine_renderer::scene::build_scene` draws, plus bounds
/// matching the visible 40×40 plate (regression-tested against the
/// scene's own vertices; see `tests/player_movement.rs`).
pub fn demo_collision_world() -> CollisionWorld {
    let mut world = CollisionWorld::new(0.0);
    for gx in -2..=2i32 {
        for gz in -2..=2i32 {
            // scene.rs's own arithmetic, verbatim (as-cast floats) —
            // the collision mirror must not drift from the renderer's
            // field by even a rounding step.
            let height = 0.6 + ((gx + gz).unsigned_abs() as f32) * 0.45;
            world = world.with_box([(gx as f32) * 3.0, 0.0, (gz as f32) * 3.0], 0.9, height);
        }
    }
    // The scene's depth demonstrators (the third floats at head
    // height — the player walks under it).
    world = world.with_box([0.0, 0.0, -2.0], 1.4, 1.4);
    world = world.with_box([0.6, 0.3, 0.8], 1.4, 1.4);
    world = world.with_box([-0.5, 2.2, -0.6], 0.8, 0.8);
    world.with_bounds([-20.0, 0.0, -20.0], [20.0, 50.0, 20.0])
}

/// One frame's mapped input from the shell snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MappedInput {
    pub intent: MoveIntent,
    /// A camera-mode toggle was requested this frame (key edge).
    pub toggle_camera: bool,
}

/// Maps a shell input snapshot onto a movement intent. `sprint_held`
/// is the caller's held-key query (Shift) — the snapshot itself only
/// carries edges and axes. Focus loss and minimization map to the
/// neutral intent: no stuck movement across focus changes (the
/// shell's own anti-stuck discipline, mirrored here).
pub fn map_intent(snapshot: &InputSnapshot, sprint_held: bool) -> MappedInput {
    let neutral = snapshot.focus_lost || snapshot.minimized;
    let intent = if neutral {
        MoveIntent::default()
    } else {
        MoveIntent {
            forward: snapshot.move_forward,
            strafe: snapshot.move_strafe,
            jump_pressed: snapshot.keys_pressed.contains(&Keycode::Space),
            sprint: sprint_held,
            look: (
                // The post-orientation hardening: mouse-X is negated
                // at the mapping — this engine's +yaw turns toward
                // world-LEFT (look_direction = [−sin(yaw), …]), so a
                // rightward mouse delta must produce a NEGATIVE yaw
                // delta to turn the view right. The vertical mapping
                // below is unchanged.
                -snapshot.look_delta.0 * DEMO_LOOK_SENSITIVITY,
                -snapshot.look_delta.1 * DEMO_LOOK_SENSITIVITY,
            ),
        }
    };
    MappedInput {
        intent,
        toggle_camera: !neutral && snapshot.keys_pressed.contains(&Keycode::C),
    }
}

/// Demo options; CLI parsing lives in the binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerOptions {
    /// CPU pacing target. 0 = uncapped measurement mode.
    pub fps: u32,
    pub present: PresentModePreference,
    /// Auto-quit after N presented frames (smoke tests).
    pub frames: Option<u64>,
    pub overlay: bool,
    pub validation: ValidationPreference,
}

impl Default for PlayerOptions {
    fn default() -> Self {
        PlayerOptions {
            fps: 60,
            present: PresentModePreference::Auto,
            frames: None,
            overlay: true,
            validation: ValidationPreference::Auto,
        }
    }
}

/// The scripted headless scenarios — deterministic per-tick intent
/// functions of the LOGICAL tick index (cadence-independent, the
/// scripted-presence precedent). Every script keeps yaw/pitch at zero:
/// the libm-free class whose traces are cross-platform bit-stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadlessScript {
    Settle,
    Walk,
    WalkJump,
    Tour,
}

impl HeadlessScript {
    pub fn parse(value: &str) -> Option<HeadlessScript> {
        match value {
            "settle" => Some(HeadlessScript::Settle),
            "walk" => Some(HeadlessScript::Walk),
            "jump" => Some(HeadlessScript::WalkJump),
            "tour" => Some(HeadlessScript::Tour),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            HeadlessScript::Settle => "settle",
            HeadlessScript::Walk => "walk",
            HeadlessScript::WalkJump => "jump",
            HeadlessScript::Tour => "tour",
        }
    }

    fn intent_at(self, tick: u64) -> MoveIntent {
        let mut intent = MoveIntent::default();
        match self {
            HeadlessScript::Settle => {}
            HeadlessScript::Walk => {
                if tick >= 30 {
                    intent.forward = 1;
                }
            }
            HeadlessScript::WalkJump => {
                if (30..150).contains(&tick) {
                    intent.forward = 1;
                }
                if tick == 90 {
                    intent.jump_pressed = true;
                }
            }
            HeadlessScript::Tour => {
                if (30..90).contains(&tick) || (150..180).contains(&tick) || tick >= 210 {
                    intent.forward = 1;
                }
                if (90..180).contains(&tick) {
                    intent.strafe = 1;
                }
                if tick == 190 {
                    intent.jump_pressed = true;
                }
            }
        }
        intent
    }
}

/// The deterministic headless run: fixed ticks of the scripted
/// intents over the demo world, returning the printable summary
/// (final state, the no-discard accounting, and the trace
/// fingerprint). Byte-identical across identical invocations — the
/// CI selftest pattern.
pub fn run_player_headless(ticks: u64, script: HeadlessScript) -> String {
    let mut driver = PlayerDriver::new(
        PlayerState::spawn([0.0, 2.0, 9.0]),
        demo_player_config(),
        demo_collision_world(),
        DEMO_TICK_HZ,
    );
    driver.run_headless(ticks, |tick| script.intent_at(tick));
    let state = driver.state();
    format!(
        "Sektor7 Cycle 1 P3 - player headless run\n\
         script={} ticks={} rate={} Hz\n\
         player pos ({:.3}, {:.3}, {:.3}) vel ({:.3}, {:.3}, {:.3}) on_ground={} mode={}\n\
         movement ticks executed={} outstanding={} due={} refused={}\n\
         player-trace={}",
        script.name(),
        ticks,
        DEMO_TICK_HZ,
        state.position[0],
        state.position[1],
        state.position[2],
        state.velocity[0],
        state.velocity[1],
        state.velocity[2],
        state.on_ground,
        match state.camera_mode {
            CameraMode::FirstPerson => "first",
            CameraMode::ThirdPerson => "third",
        },
        driver.executed_ticks_total(),
        driver.outstanding_ticks(),
        driver.ticks_due_total(),
        driver.refused_steps(),
        driver.trace_fingerprint_hex(),
    )
}

/// The interactive player demo (`--player`). Rendering cadence stays
/// fully independent of the movement cadence: the driver advances on
/// the FULL raw frame interval (stalls and minimized periods
/// included — no silent pause), bounded catch-up per frame, retained
/// outstanding ticks reported in the summary; camera poses derive
/// from player state every presented frame.
pub fn run_player_demo(options: PlayerOptions) -> Result<(), String> {
    let shell_config = ShellConfig {
        title: "Sektor7 original engine — Cycle 1 P3 player prototype".to_string(),
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
        replace_pipelines: false,
    };
    let mut renderer =
        Renderer::new(app.window(), render_config).map_err(|error| error.to_string())?;
    renderer.set_overlay_enabled(options.overlay);

    let mut driver = PlayerDriver::new(
        PlayerState::spawn([0.0, 2.0, 9.0]),
        demo_player_config(),
        demo_collision_world(),
        DEMO_TICK_HZ,
    );
    // The interactive session's trace is cadence-dependent (live
    // input) — not recorded; determinism claims never route through
    // the live path.
    driver.set_trace_recording(false);

    {
        let stats = renderer.stats();
        println!("Sektor7 Cycle 1 P3 — player movement/collision prototype");
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
            "  controls: WASD move · Space jump · Shift sprint · C camera mode · \
             mouse look · O overlay · Esc/close quit"
        );
        println!(
            "  movement: fixed {} Hz tick · budgeted catch-up · no-discard accounting",
            DEMO_TICK_HZ
        );
    }

    let mut cpu_work_ms: Vec<f64> = Vec::new();
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
    // The retained third-person player-proxy buffer (the F-02
    // discipline: capacity is reused, no per-frame growth).
    let mut player_proxy: Vec<SceneVertex> = Vec::new();

    loop {
        let frame_start = Instant::now();
        let snapshot = app.frame();
        if snapshot.quit_requested || snapshot.keys_pressed.contains(&Keycode::Escape) {
            break;
        }
        if snapshot.keys_pressed.contains(&Keycode::O) {
            renderer.set_overlay_enabled(!renderer.overlay_enabled());
        }
        let split = timing.begin_frame(frame_start, !snapshot.minimized);

        // Movement advances on the full raw interval — render pacing
        // never gates the fixed-step tick (the charter requirement);
        // minimized frames accrue their ticks like everyone else's.
        let mapped = map_intent(&snapshot, app.is_held(Keycode::LShift));
        if mapped.toggle_camera {
            driver.state_mut().toggle_camera_mode();
        }
        driver.advance(split.sim_advance, mapped.intent);

        if snapshot.minimized {
            // No presentation, but the movement clock keeps its
            // obligation: due-but-unexecuted ticks stay accrued and
            // reported, never silently discarded.
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        // The camera pose derives from player state — first- and
        // third-person from the same state; toggling changes only
        // this derivation.
        let pose = camera_pose(driver.state(), driver.config());
        let (width, height) = renderer.stats().extent;
        let aspect = if height == 0 {
            16.0 / 9.0
        } else {
            width as f32 / height as f32
        };
        let view = math::camera_view(pose.position, pose.yaw, pose.pitch, pose.roll);
        let projection = math::perspective(std::f32::consts::FRAC_PI_3, aspect, 0.1, 200.0);
        let view_proj = math::mul(&projection, &view);

        // The player body is drawn in third person only (a dynamic
        // proxy box through the scene pipeline's non-indexed path).
        player_proxy.clear();
        if driver.state().camera_mode == CameraMode::ThirdPerson {
            let state = driver.state();
            push_box(
                &mut player_proxy,
                [
                    state.position[0] - driver.config().radius,
                    state.position[1],
                    state.position[2] - driver.config().radius,
                ],
                driver.config().radius * 2.0,
                driver.config().height,
                [0.20, 0.80, 0.35],
            );
        }

        let state = driver.state();
        let overlay_text = renderer.overlay_enabled().then(|| {
            let elapsed = start.elapsed().as_secs_f64().max(0.001);
            let stats = renderer.stats();
            format!(
                "Sektor7 P3 - player\nFPS {:.0} (cpu work p95 {:.2} ms)\n{} {} / Vulkan {}\n\
                 player ({:.2}, {:.2}, {:.2}) vel ({:.2}, {:.2}, {:.2}) ground {}\n\
                 move tick {} @ {} Hz · out {} · due {}\n\
                 cam {} ({:.1}, {:.1}, {:.1}) yaw {:.2} pitch {:.2}",
                presented as f64 / elapsed,
                math::percentile(&cpu_work_ms, 0.95),
                stats.device_type,
                stats.gpu_name,
                stats.api_version,
                state.position[0],
                state.position[1],
                state.position[2],
                state.velocity[0],
                state.velocity[1],
                state.velocity[2],
                state.on_ground,
                driver.executed_ticks_total(),
                DEMO_TICK_HZ,
                driver.outstanding_ticks(),
                driver.ticks_due_total(),
                match state.camera_mode {
                    CameraMode::FirstPerson => "first",
                    CameraMode::ThirdPerson => "third",
                },
                pose.position[0],
                pose.position[1],
                pose.position[2],
                pose.yaw,
                pose.pitch,
            )
        });

        let frame_result = renderer
            .render_frame(
                &view_proj,
                &player_proxy,
                overlay_text.as_deref(),
                app.window_size(),
            )
            .map_err(|error| format!("{error}"))?;

        if frame_result.deferred {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        if frame_result.presented {
            presented += 1;
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
    let state = driver.state();
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
        "present mode {} · swapchain recreations {} · validation err/warn {}/{}",
        stats.present_mode,
        stats.swapchain_recreations,
        stats.validation_errors,
        stats.validation_warnings
    );
    println!(
        "player final: pos ({:.3}, {:.3}, {:.3}) vel ({:.3}, {:.3}, {:.3}) ground {} mode {}",
        state.position[0],
        state.position[1],
        state.position[2],
        state.velocity[0],
        state.velocity[1],
        state.velocity[2],
        state.on_ground,
        match state.camera_mode {
            CameraMode::FirstPerson => "first",
            CameraMode::ThirdPerson => "third",
        }
    );
    println!(
        "movement ticks: executed {} · outstanding {} (retained, reported — never discarded) · due {}",
        driver.executed_ticks_total(),
        driver.outstanding_ticks(),
        driver.ticks_due_total()
    );
    println!(
        "controller state is transient (not persisted); offscreen CI runs are a \
         correctness smoke, not a substitute for a human-played run"
    );
    Ok(())
}
