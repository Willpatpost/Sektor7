//! Stage 1 soak measurement (Stage 1 authorization, item 5).
//!
//! Ignored by default — run explicitly:
//!
//! ```sh
//! cargo test -p engine-shell --test soak -- --ignored --nocapture
//! ```

mod common;

use std::time::{Duration, Instant};

use engine_shell::{LoopControl, QuitReason};

fn lock() -> std::sync::MutexGuard<'static, ()> {
    common::sdl_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
#[ignore = "long-running measurement (60 s); run explicitly with --ignored"]
fn application_loop_soak_60_seconds() {
    let _guard = lock();
    let mut app = common::dummy_app("soak-60s");
    let deadline = Instant::now() + Duration::from_secs(60);
    let summary = app.run(|_, snap| {
        if snap.quit_requested || Instant::now() >= deadline {
            LoopControl::Quit
        } else {
            LoopControl::Continue
        }
    });
    let stats = summary.frame_stats;
    println!(
        "soak: {} frames over {:.1}s — interval avg {:.2} ms · p50 {:.2} · p95 {:.2} · p99 {:.2} · max {:.2} · rss {:?} -> {:?} KiB",
        summary.frames,
        summary.wall_duration.as_secs_f64(),
        stats.avg_ms,
        stats.p50_ms,
        stats.p95_ms,
        stats.p99_ms,
        stats.max_ms,
        summary.rss_start_kib,
        summary.rss_end_kib
    );
    assert_eq!(summary.quit_reason, QuitReason::Callback);
    assert!(
        summary.frames >= 55 * 60,
        "expected ≈60 fps average under the 60 fps target, got {} frames",
        summary.frames
    );
}
