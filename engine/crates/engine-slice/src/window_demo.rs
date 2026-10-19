//! Stage 1 windowed demo (`--window`) and soak harness (`--soak`) for the
//! interactive slice.
//!
//! Evidence separation (Stage 1 authorization, item 5): a real-driver run
//! on a graphical session is GUI evidence; the dummy driver
//! (`--window --dummy`) is a headless smoke only. The soak prints frame
//! pacing and memory-trend measurements.

use std::time::{Duration, Instant};

use engine_shell::{
    DriverPreference, Keycode, Level, LoopControl, MouseButton, RunSummary, ShellApp, ShellConfig,
    resident_set_kib,
};

/// Runs the Stage 1 shell window. With `dummy`, uses SDL's dummy driver
/// (headless smoke — no window is displayed) and exits after `frame_limit`
/// frames; otherwise a real window runs until Esc or window close.
pub fn run_window_demo(dummy: bool, frame_limit: Option<u64>) -> Result<(), String> {
    let driver = if dummy {
        DriverPreference::Dummy
    } else {
        DriverPreference::Real
    };
    let config = ShellConfig {
        title: "Sektor7 original engine — Stage 1 shell".to_string(),
        width: 1280,
        height: 720,
        target_fps: 60,
        driver,
    };
    let mut app = ShellApp::new(config)?;
    app.set_diag_level(Level::Info);
    let version = engine_shell::linked_sdl_version();
    if dummy {
        println!(
            "Sektor7 Stage 1 shell — SDL {version}, dummy driver \
             (HEADLESS smoke: no window is displayed; dispatch-level evidence only)"
        );
    } else {
        println!("Sektor7 Stage 1 shell — SDL {version}, real driver (GUI evidence)");
    }
    println!(
        "controls: WASD = movement intent · mouse while captured = look intent · \
         left click = toggle capture · Esc or window close = quit"
    );

    let mut captured = false;
    let mut frames: u64 = 0;
    let summary = app.run(|app, snap| {
        if snap.quit_requested {
            return LoopControl::Quit;
        }
        if snap.keys_pressed.contains(&Keycode::Escape) {
            return LoopControl::Quit;
        }
        if snap.mouse_pressed.contains(&MouseButton::Left) {
            captured = !captured;
            app.set_relative_mouse(captured);
            println!("relative mouse capture: {captured}");
        }
        if let Some((w, h)) = snap.resized {
            println!("window resized: {w}x{h}");
        }
        if snap.focus_lost {
            println!("diagnostics: keyboard focus lost — held input cleared");
        }
        if snap.focus_gained {
            println!("diagnostics: keyboard focus regained");
        }
        if frames.is_multiple_of(60) {
            println!(
                "frame {frames}: move=({}, {}) look=({:.1}, {:.1})",
                snap.move_forward, snap.move_strafe, snap.look_delta.0, snap.look_delta.1
            );
        }
        frames += 1;
        if let Some(limit) = frame_limit
            && frames >= limit
        {
            return LoopControl::Quit;
        }
        LoopControl::Continue
    });

    print_summary(&summary);
    println!(
        "evidence note: this run is {} evidence; the other kind requires {}.",
        if dummy { "headless" } else { "GUI" },
        if dummy {
            "a graphical session (no --dummy)"
        } else {
            "the dummy driver (--dummy)"
        }
    );
    Ok(())
}

/// Runs the paced application loop for `seconds`, then reports frame
/// pacing and memory trends. Dummy driver by default; `--real` for a GUI
/// soak on a graphical session.
pub fn run_soak(seconds: u64, real: bool) -> Result<(), String> {
    let driver = if real {
        DriverPreference::Real
    } else {
        DriverPreference::Dummy
    };
    let config = ShellConfig {
        title: "Sektor7 Stage 1 soak".to_string(),
        width: 1280,
        height: 720,
        target_fps: 60,
        driver,
    };
    let mut app = ShellApp::new(config)?;
    println!(
        "soak: application loop for {seconds}s ({} driver), 60 fps target ...",
        if real { "real" } else { "dummy" }
    );
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut frames: u64 = 0;
    let mut rss_samples: Vec<Option<u64>> = Vec::new();
    let summary = app.run(|_, snap| {
        frames += 1;
        if frames.is_multiple_of(60) {
            rss_samples.push(resident_set_kib());
        }
        if snap.quit_requested || Instant::now() >= deadline {
            LoopControl::Quit
        } else {
            LoopControl::Continue
        }
    });
    print_summary(&summary);
    if let (Some(first), Some(last)) = (
        rss_samples.first().copied().flatten(),
        rss_samples.last().copied().flatten(),
    ) {
        println!(
            "resident memory trend: {first} KiB -> {last} KiB over {} samples (1 per 60 frames)",
            rss_samples.len()
        );
    }
    println!(
        "note (Stage 1 authorization): a soak with no observed memory growth \
         is not proof that all memory leaks are impossible."
    );
    Ok(())
}

fn print_summary(summary: &RunSummary) {
    let stats = summary.frame_stats;
    println!(
        "loop summary: {} frames over {:.1}s — frame interval avg {:.2} ms · \
         p50 {:.2} · p95 {:.2} · p99 {:.2} · max {:.2} ms (quit: {:?})",
        summary.frames,
        summary.wall_duration.as_secs_f64(),
        stats.avg_ms,
        stats.p50_ms,
        stats.p95_ms,
        stats.p99_ms,
        stats.max_ms,
        summary.quit_reason
    );
    if let (Some(start), Some(end)) = (summary.rss_start_kib, summary.rss_end_kib) {
        println!(
            "resident memory: {start} KiB -> {end} KiB ({:+} KiB)",
            end as i64 - start as i64
        );
    }
}
