//! Stage 3 headless simulation demo (`--sim`): the same `SimDriver` +
//! `NpcWorld` logic the graphical application runs, stepped without a
//! window or GPU (Stage 3 authorization §5). Deterministic by
//! construction: the synthetic frame cadence uses computed durations —
//! never real sleeping — and the report prints only logical state, so
//! repeated runs with the same arguments produce identical output.

use std::time::Duration;

use crate::npc::NpcConfig;
use crate::region::{PresenceScenario, RegionConfig, stage4_state_fingerprint_hex};
use crate::sim::{SimConfig, SimDriver};

/// Runs the simulation to `target_ticks` campaign ticks under a synthetic
/// frame cadence, then prints the logical-state report. The cadence
/// exercises the same elapsed-time accrual the graphical loop performs;
/// `cadence_hz` of 0 selects an irregular pattern including a stall that
/// exceeds the per-frame catch-up bound, exercising retained-obligation
/// drain headlessly. With `presence`, runs the Stage 4 region director
/// under a deterministic scripted presence (authorization §5) — the
/// report stays logical-only (no wall-time), so repeated runs with the
/// same arguments print identical output.
pub fn run_sim_demo(
    npc_config: NpcConfig,
    sim_config: SimConfig,
    target_ticks: u64,
    cadence_hz: u32,
    presence: Option<PresenceScenario>,
) -> Result<(), String> {
    let rate = sim_config.tick_rate_hz;
    let region_config = RegionConfig::default();
    let mut driver = match &presence {
        Some(scenario) => SimDriver::with_regions(
            npc_config.clone(),
            sim_config,
            region_config.clone(),
            &[scenario.sample_at(0, &region_config.centers)],
        )?,
        None => SimDriver::new(npc_config.clone(), sim_config),
    };
    let period_ns = 1_000_000_000u64 / u64::from(rate.max(1));
    let mut frame_index = 0u64;
    loop {
        let clock = driver.spine().clock().as_u64();
        if clock >= target_ticks {
            break;
        }
        // Stage 4: feed the scripted presence — a function of the
        // logical tick, so the input is deterministic (never
        // wall-clock).
        if let Some(scenario) = &presence {
            driver.set_presence(vec![scenario.sample_at(clock, &region_config.centers)]);
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
        sim_config.near_reserve_description(),
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
    if let Some(director) = driver.director() {
        // Logical-only region report (no wall-time latencies — the
        // double-run diff discipline requires identical output).
        let levels = director.levels();
        let diagnostics = director.diagnostics();
        let stats = spine.stats();
        println!("--- regions ---");
        println!("  levels: {levels:?}");
        println!(
            "  transitions: activations {} (replay {} / synthesis {}) · deactivations {} · invalid {}",
            diagnostics.activations,
            diagnostics.replay_activations,
            diagnostics.synthesis_activations,
            diagnostics.deactivations,
            diagnostics.invalid_transitions,
        );
        println!(
            "  reconstruction: activities {} · captured {} · reapplied {}",
            diagnostics.reconstructed_activities,
            diagnostics.captured_deltas,
            diagnostics.reapplied_deltas,
        );
        println!(
            "  obligations: queued {} · blocked {} · held {} · conservation {}",
            spine.pending_count(),
            spine.blocked_count(),
            spine.held_count(),
            stats.scheduled
                == stats.executed
                    + spine.pending_count() as u64
                    + spine.blocked_count() as u64
                    + spine.held_count() as u64,
        );
        println!(
            "  fingerprints: stage4={} · held={}",
            stage4_state_fingerprint_hex(spine, director),
            spine.held_fingerprint_hex(),
        );
    }
    Ok(())
}

/// Irregular synthetic cadence: a repeating pattern of short frames, a
/// 2 s stall (twenty due ticks at 10 Hz — EXCEEDING the eight-tick
/// per-frame catch-up bound, so the retained obligation drains across
/// subsequent frames; corrective finding B), and more short frames.
/// Computed constants only, so runs stay identical. Public so the
/// determinism tests exercise the demo's actual cadence.
pub fn irregular_frame(index: u64) -> Duration {
    match index % 12 {
        0..=4 => Duration::from_secs_f64(1.0 / 60.0),
        5 => Duration::from_millis(2000),
        6..=10 => Duration::from_secs_f64(1.0 / 240.0),
        _ => Duration::from_secs_f64(1.0 / 120.0),
    }
}
