//! Stage 3 headless simulation demo (`--sim`): the same `SimDriver` +
//! `NpcWorld` logic the graphical application runs, stepped without a
//! window or GPU (Stage 3 authorization §5). Deterministic by
//! construction: the synthetic frame cadence uses computed durations —
//! never real sleeping — and the report prints only logical state, so
//! repeated runs with the same arguments produce identical output.

use std::time::Duration;

use crate::npc::NpcConfig;
use crate::sim::{SimConfig, SimDriver};

/// Runs the simulation to `target_ticks` campaign ticks under a synthetic
/// frame cadence, then prints the logical-state report. The cadence
/// exercises the same elapsed-time accrual the graphical loop performs;
/// `cadence_hz` of 0 selects an irregular pattern including a stall, to
/// exercise bounded catch-up headlessly.
pub fn run_sim_demo(
    npc_config: NpcConfig,
    sim_config: SimConfig,
    target_ticks: u64,
    cadence_hz: u32,
) -> Result<(), String> {
    let rate = sim_config.tick_rate_hz;
    let mut driver = SimDriver::new(npc_config.clone(), sim_config);
    let period_ns = 1_000_000_000u64 / u64::from(rate.max(1));
    let mut frame_index = 0u64;
    loop {
        let clock = driver.spine().clock().as_u64();
        if clock >= target_ticks {
            break;
        }
        let outstanding = driver.outstanding_ticks();
        let remaining = (target_ticks - clock) as u32;
        if outstanding >= remaining {
            // Enough accrued work to finish: land exactly on target.
            driver.run_headless(u64::from(remaining));
            break;
        }
        let frame = match cadence_hz {
            0 => irregular_frame(frame_index),
            hz => Duration::from_secs_f64(1.0 / f64::from(hz.max(1))),
        };
        // Cap the frame so it cannot accrue more due ticks than remain —
        // every cadence reports at exactly the same campaign tick, which
        // is what makes cadence fingerprints comparable.
        let cap = Duration::from_nanos(u64::from(remaining - outstanding) * period_ns);
        driver.advance(frame.min(cap));
        frame_index += 1;
    }

    let spine = driver.spine();
    let stats = spine.stats();
    let totals = spine.lane_totals();
    println!("Sektor7 Stage 3 — headless simulation");
    println!(
        "  config: {} Hz tick rate · budget {} · near reserve {} · seed {} · cadence {}",
        rate,
        spine.budget(),
        spine.near_reserve(),
        npc_config.seed,
        if cadence_hz == 0 {
            "irregular".to_string()
        } else {
            format!("{cadence_hz} Hz")
        },
    );
    println!(
        "  campaign: tick {} · executed {} · late arrivals {} · late executions {}",
        spine.clock(),
        stats.executed,
        stats.late_arrivals,
        stats.late_executions,
    );
    println!(
        "  backlog: pending {} (near {} / far {}) · blocked {} · budget-exhausted ticks {}",
        spine.pending_count(),
        spine.near_pending_count(),
        spine.far_pending_count(),
        spine.blocked_count(),
        stats.budget_exhausted_ticks,
    );
    println!(
        "  lanes: near executed {} (backlogged ticks {}) · far executed {} (backlogged ticks {})",
        totals.near_executed,
        totals.near_backlogged_ticks,
        totals.far_executed,
        totals.far_backlogged_ticks,
    );
    println!(
        "  watermark: max_executed_due {} · processed_through {}",
        spine
            .max_executed_due()
            .map(|tick| tick.as_u64().to_string())
            .unwrap_or_else(|| "none".into()),
        spine
            .processed_through()
            .map(|tick| tick.as_u64().to_string())
            .unwrap_or_else(|| "none".into()),
    );
    println!(
        "  npc: total activity steps {}",
        spine.world().total_steps()
    );
    println!(
        "  fingerprints: engine-lanes={} state-lanes={}",
        spine.lane_fingerprint_hex(),
        spine.lane_state_fingerprint_hex(),
    );
    Ok(())
}

/// Irregular synthetic cadence: a repeating pattern of short frames, a
/// 500 ms stall (five due ticks at once — bounded catch-up), and more
/// short frames. Computed constants only, so runs stay identical.
fn irregular_frame(index: u64) -> Duration {
    match index % 12 {
        0..=4 => Duration::from_secs_f64(1.0 / 60.0),
        5 => Duration::from_millis(500),
        6..=10 => Duration::from_secs_f64(1.0 / 240.0),
        _ => Duration::from_secs_f64(1.0 / 120.0),
    }
}
