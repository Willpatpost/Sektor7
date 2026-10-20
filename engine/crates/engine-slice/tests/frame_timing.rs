//! Stage 3 corrective finding A regression tests: the render loop's
//! actual timing policy (`FrameTiming`) feeding the actual driver
//! (`SimDriver`). Simulation time is never silently discarded —
//! rendering stalls and minimized periods accrue in full; only camera
//! movement is capped.

use std::time::{Duration, Instant};

use engine_slice::frame_timing::{CAMERA_DT_CAP, FrameSplit, FrameTiming};
use engine_slice::npc::NpcConfig;
use engine_slice::sim::{SimConfig, SimDriver};

/// Drives the timing policy through a frame plan of
/// `(interval_ms, rendering)` pairs on a synthetic clock, returning
/// the per-frame splits and the total simulation time advanced.
fn run_plan(plan: &[(u64, bool)]) -> (Vec<FrameSplit>, Duration) {
    let origin = Instant::now();
    let mut timing = FrameTiming::new_at(CAMERA_DT_CAP, origin);
    let mut offset = Duration::ZERO;
    let mut splits = Vec::new();
    for &(interval_ms, rendering) in plan {
        offset += Duration::from_millis(interval_ms);
        splits.push(timing.begin_frame(origin + offset, rendering));
    }
    (splits, timing.total_sim_advanced())
}

#[test]
fn rendering_stall_feeds_the_full_interval_to_the_simulation() {
    // The original defect: the frame delta was capped at 100 ms before
    // reaching the simulation, so a 500 ms rendering stall contributed
    // only one of its five 10 Hz ticks — four silently discarded.
    let (splits, total) = run_plan(&[(16, true), (17, true), (500, true), (16, true), (17, true)]);
    assert_eq!(
        splits[2].sim_advance,
        Duration::from_millis(500),
        "the stall contributes its full interval to the simulation"
    );
    assert_eq!(
        splits[2].camera_dt,
        Some(CAMERA_DT_CAP),
        "camera movement alone is capped against stalls"
    );
    assert_eq!(
        splits[0].camera_dt,
        Some(Duration::from_millis(16)),
        "normal frames pass through uncapped"
    );
    assert_eq!(
        total,
        Duration::from_millis(16 + 17 + 500 + 16 + 17),
        "every elapsed nanosecond reaches the simulation"
    );
}

#[test]
fn minimized_frames_keep_the_campaign_clock_advancing() {
    // While the window is minimized the app renders nothing, but the
    // campaign clock keeps accruing elapsed time — no silent pause
    // policy. (The minimized branch polls at ~50 ms.)
    let (splits, total) = run_plan(&[(50, false); 40]);
    assert_eq!(
        total,
        Duration::from_secs(2),
        "two seconds minimized = two seconds of campaign time"
    );
    assert!(
        splits.iter().all(|split| split.camera_dt.is_none()),
        "no camera step without presentation"
    );
    assert!(
        splits
            .iter()
            .all(|split| split.sim_advance == Duration::from_millis(50)),
        "the simulation advances by the full poll interval"
    );
}

#[test]
fn no_simulation_time_discarded_across_mixed_frames() {
    // Any mix of rendering frames, stalls, and minimized periods: the
    // simulation total equals the wall total exactly.
    let plan = [
        (16u64, true),
        (17, true),
        (500, true),
        (16, false),
        (50, false),
        (50, false),
        (2000, false),
        (16, true),
        (17, true),
    ];
    let expected: u64 = plan.iter().map(|&(interval_ms, _)| interval_ms).sum();
    let (_, total) = run_plan(&plan);
    assert_eq!(total, Duration::from_millis(expected));
}

#[test]
fn timing_policy_with_driver_costs_no_simulation_time() {
    // The integration the render loop performs: FrameTiming splits the
    // interval, SimDriver advances by it. A rendering stall (500 ms —
    // five due ticks, inside the catch-up bound) plus a minimized
    // period (2 s — twenty due ticks, EXCEEDING the bound) plus normal
    // frames: every tick due from advanced time is executed or
    // outstanding, the retained obligation drains without loss, and
    // the logical result equals an unstalled run at the same campaign
    // tick.
    let config = SimConfig::default();
    let plan = [
        (16u64, true),
        (17, true),
        (16, true),
        (17, true),
        (500, true),
        (16, true),
        (17, true),
        (2000, false),
    ];
    let (splits, total) = run_plan(&plan);
    let mut driver = SimDriver::new(NpcConfig::default(), config);
    for split in &splits {
        driver.advance(split.sim_advance);
    }
    assert!(
        driver.outstanding_ticks() > 0,
        "the plan ends with retained obligation"
    );
    assert_eq!(
        driver.spine().clock().as_u64() + u64::from(driver.outstanding_ticks()),
        driver.ticks_due_total(),
        "campaign + outstanding == ticks due — nothing discarded"
    );
    let period_ns = 1_000_000_000u128 / u128::from(config.tick_rate_hz);
    assert_eq!(
        driver.ticks_due_total(),
        (total.as_nanos() / period_ns) as u64,
        "the driver's accounting matches the policy's advanced time"
    );
    while driver.outstanding_ticks() > 0 {
        driver.advance(Duration::ZERO);
    }
    let target = driver.spine().clock().as_u64();
    let mut clean = SimDriver::new(NpcConfig::default(), config);
    clean.run_headless(target);
    assert_eq!(
        driver.spine().lane_state_fingerprint(),
        clean.spine().lane_state_fingerprint(),
        "stalls and minimized periods change when ticks are due, never the logical result"
    );
}
