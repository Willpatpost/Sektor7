//! Stage 3 deterministic verification (authorization §5): double-run
//! fingerprints, render-cadence independence at equivalent campaign
//! ticks, the 200-event synchrony cluster, backlog-under-constrained-
//! budget without silent loss, near-field starvation-freedom under
//! far-field pressure, and catch-up-bound overflow retention (corrective
//! finding B). Workload-level policy tests — the lane mechanism
//! contracts live in `engine-core/tests/lanes.rs`; the application
//! timing integration lives in `tests/frame_timing.rs`.

use std::collections::HashSet;
use std::time::Duration;

use engine_core::{EventClass, EventSpec, Field, Schedule, Tick};
use engine_slice::npc::{NPC_COUNT, NpcConfig};
use engine_slice::sim::{SimConfig, SimDriver};

/// Advances a fresh driver to exactly `target` campaign ticks under a
/// synthetic frame cadence (computed durations only — no real sleeping),
/// then returns the lane-domain state fingerprint. The frame is capped
/// so no cadence can overshoot the target: every cadence lands on the
/// same campaign tick, which is what makes the fingerprints comparable.
fn advance_to<F>(target: u64, config: SimConfig, frame_at: F) -> u64
where
    F: Fn(u64) -> Duration,
{
    let rate = config.tick_rate_hz;
    let mut driver = SimDriver::new(NpcConfig::default(), config);
    let period_ns = 1_000_000_000u64 / u64::from(rate.max(1));
    let mut index = 0u64;
    loop {
        let clock = driver.spine().clock().as_u64();
        if clock >= target {
            break;
        }
        let outstanding = driver.outstanding_ticks();
        let remaining = (target - clock) as u32;
        if outstanding >= remaining {
            driver.run_headless(u64::from(remaining));
            break;
        }
        let cap = Duration::from_nanos(u64::from(remaining - outstanding) * period_ns);
        driver.advance(frame_at(index).min(cap));
        index += 1;
    }
    driver.spine().lane_state_fingerprint()
}

#[test]
fn identical_configuration_and_ticks_produce_identical_fingerprints() {
    let config = SimConfig::default();
    let frame = Duration::from_secs_f64(1.0 / 60.0);
    let first = advance_to(300, config, |_| frame);
    let second = advance_to(300, config, |_| frame);
    assert_eq!(
        first, second,
        "double-run determinism (E-07…E-10 tradition)"
    );
}

#[test]
fn render_cadence_does_not_change_logical_results() {
    // Acceptance requirement 2: 60 Hz, 240 Hz, and an irregular pattern
    // (with a 500 ms stall exercising bounded catch-up) all reach the
    // same campaign tick with identical logical state.
    let config = SimConfig::default();
    let at_60 = advance_to(300, config, |_| Duration::from_secs_f64(1.0 / 60.0));
    let at_240 = advance_to(300, config, |_| Duration::from_secs_f64(1.0 / 240.0));
    assert_eq!(
        at_60, at_240,
        "render rate must not change logical results per completed tick"
    );
    // Irregular cadence — the demo's actual pattern (shared with
    // `--sim --cadence irregular`): short frames, one 2 s stall per
    // dozen frames (twenty due ticks at once — EXCEEDING the eight-tick
    // catch-up bound, so the retained obligation drains across the
    // following frames; corrective finding B), then faster frames.
    let irregular = advance_to(300, config, engine_slice::sim_demo::irregular_frame);
    assert_eq!(
        irregular, at_60,
        "stalls and irregular frames must not change logical results"
    );
}

#[test]
fn catch_up_bound_retains_reports_and_eventually_executes_overflow() {
    // Corrective finding B: a stall producing more due ticks than the
    // eight-tick catch-up bound must execute exactly the bound, retain
    // the remainder as reported outstanding obligation, and drain it
    // later — every due tick eventually executes, nothing is lost.
    let config = SimConfig::default();
    assert_eq!(config.max_catchup_per_frame, 8);
    let mut driver = SimDriver::new(NpcConfig::default(), config);
    // 5 s at 10 Hz = 50 due ticks in one frame.
    let step = driver.advance(Duration::from_secs(5));
    assert_eq!(step.executed_ticks, 8, "the bound caps per-frame execution");
    assert_eq!(
        step.outstanding_ticks, 42,
        "the remainder is retained and reported"
    );
    // Zero-time advances keep draining the retained obligation,
    // bounded per advance, until nothing is owed.
    let mut executed_ticks = u64::from(step.executed_ticks);
    let mut drains = 0;
    while driver.outstanding_ticks() > 0 {
        let step = driver.advance(Duration::ZERO);
        assert!(step.executed_ticks <= 8, "the bound holds while draining");
        executed_ticks += u64::from(step.executed_ticks);
        drains += 1;
        assert!(drains < 100, "the drain must terminate");
    }
    assert_eq!(
        executed_ticks, 50,
        "every due tick eventually executes — no loss"
    );
    assert_eq!(driver.spine().clock().as_u64(), 50);
    assert_eq!(
        driver.spine().clock().as_u64() + u64::from(driver.outstanding_ticks()),
        driver.ticks_due_total(),
        "the accounting invariant holds end-to-end"
    );
    let spine = driver.spine();
    let stats = spine.stats();
    assert_eq!(stats.invalid_dropped, 0, "nothing is silently dropped");
    let accounted = stats.executed + spine.pending_count() as u64 + spine.blocked_count() as u64;
    assert_eq!(accounted, stats.scheduled, "conservation: nothing vanishes");
}

#[test]
fn tick_rate_does_not_change_logical_results_at_equal_ticks() {
    // The tick RATE is timing policy (when ticks become due in real
    // time), not logical state: at equal completed ticks the
    // fingerprints agree. This is the explicit test that the real-time
    // accumulator, rate configuration, and catch-up diagnostics are
    // excluded from the fingerprint domains (authorization §3: real-time
    // accumulator and catch-up state must not contaminate logical
    // fingerprints).
    let slow = advance_to(300, SimConfig::default(), |_| {
        Duration::from_secs_f64(1.0 / 60.0)
    });
    let fast = advance_to(
        300,
        SimConfig {
            tick_rate_hz: 30,
            ..SimConfig::default()
        },
        |_| Duration::from_secs_f64(1.0 / 60.0),
    );
    assert_eq!(
        slow, fast,
        "tick rate is timing policy, not authoritative logical state"
    );
}

#[test]
fn artificially_low_budget_produces_measurable_backlog_never_silent_loss() {
    // Budget 30 is far below the ~154 events/tick natural demand. The
    // chain workload is self-limiting — each NPC holds at most one
    // pending activity, so the standing queue sits near NPC_COUNT and
    // overload manifests as lateness, exhausted ticks, and slowed
    // progress rather than unbounded queue growth (execution-relative
    // chains throttle to the available budget). Every one of those
    // signals must be measurable, and no event may vanish.
    let config = SimConfig {
        budget: 30,
        near_reserve: Some(10),
        ..SimConfig::default()
    };
    let mut driver = SimDriver::new(NpcConfig::default(), config);
    driver.run_headless(300);
    let spine = driver.spine();
    let stats = spine.stats();
    let totals = spine.lane_totals();
    assert!(
        stats.budget_exhausted_ticks > 250,
        "exhaustion is reported every tick, not hidden"
    );
    assert!(
        totals.near_backlogged_ticks + totals.far_backlogged_ticks > 250,
        "backlogged ticks are measurable"
    );
    assert!(
        stats.late_executions > 8000,
        "overload deferral is recorded as lateness ({})",
        stats.late_executions
    );
    assert!(
        spine.world().total_steps() < 15_000,
        "progress measurably slows under constraint ({}, vs ~45k sustainable)",
        spine.world().total_steps()
    );
    assert_eq!(stats.invalid_dropped, 0, "nothing is silently dropped");
    // No silent loss: everything scheduled is executed, queued, or
    // blocked — the spine never discards.
    let accounted = stats.executed + spine.pending_count() as u64 + spine.blocked_count() as u64;
    assert_eq!(accounted, stats.scheduled);
}

#[test]
fn near_field_starvation_free_under_far_field_pressure() {
    // Budget 140 squeezes the far field (empirically: far backlogged
    // ~293 of 300 ticks) while the near reservation keeps every due
    // near event on time — the reservation's purpose (E-10). Near
    // demand (~38/tick, spiking with the initial synchrony cluster)
    // stays inside the 70 reservation; near's unused reserve flows to
    // far through the three-phase redistribution, so far pressure
    // requires a constrained TOTAL budget, not just a small nonreserved
    // share.
    let config = SimConfig {
        budget: 140,
        near_reserve: Some(70),
        ..SimConfig::default()
    };
    let mut driver = SimDriver::new(NpcConfig::default(), config);
    driver.run_headless(300);
    let spine = driver.spine();
    let totals = spine.lane_totals();
    assert_eq!(
        totals.near_backlogged_ticks, 0,
        "near demand sits inside its reservation"
    );
    assert!(
        totals.far_backlogged_ticks > 100,
        "far field is genuinely under pressure"
    );
    let near_late = spine
        .ledger()
        .iter()
        .filter(|record| record.field == Field::Near && record.late)
        .count();
    assert_eq!(
        near_late, 0,
        "zero near-field lateness under far-field pressure"
    );
}

#[test]
fn synchrony_burst_of_200_processes_completely_with_recorded_lateness() {
    // Acceptance requirement 7: 200 events due on one tick, processed
    // across ticks, deferral recorded as lateness, zero drops.
    let mut driver = SimDriver::new(NpcConfig::default(), SimConfig::default());
    // Warm up so the steady workload is running, then inject the burst.
    driver.run_headless(40);
    let burst_due = Tick(50);
    let mut burst_ids = HashSet::new();
    {
        let spine = driver.spine_mut();
        for index in 0..200u64 {
            // Payloads far outside the NPC identity space: the world
            // ignores them (no activity step), the spine executes them.
            let id = spine
                .schedule(
                    EventSpec::new(
                        EventClass::new(0),
                        Schedule::AtLogicalTime { due: burst_due },
                        0x8000_0000_0000_0000 | index,
                    )
                    .in_field(Field::Near),
                )
                .expect("burst schedule is dependency-free");
            burst_ids.insert(id.seq);
        }
    }
    driver.run_headless(200);
    let spine = driver.spine();
    let executed_burst = spine
        .ledger()
        .iter()
        .filter(|record| burst_ids.contains(&record.id.seq))
        .count();
    assert_eq!(executed_burst, 200, "the burst drains completely");
    let late_burst = spine
        .ledger()
        .iter()
        .filter(|record| burst_ids.contains(&record.id.seq) && record.late)
        .count();
    assert!(
        late_burst > 0,
        "budget-constrained deferral is recorded as lateness"
    );
    assert_eq!(spine.stats().invalid_dropped, 0, "zero dropped events");
    // The steady workload keeps its identity space intact.
    assert!(spine.world().total_steps() > 0);
    assert_eq!(spine.world().steps().len(), NPC_COUNT);
}

#[test]
fn different_seeds_produce_different_logical_states() {
    // Different meaningful configurations produce different
    // fingerprints (reviewer clarification 3B).
    let config = SimConfig::default();
    let frame = Duration::from_secs_f64(1.0 / 60.0);
    let baseline = advance_to(200, config, |_| frame);
    let other_config = SimConfig::default();
    let mut other_driver = SimDriver::new(
        NpcConfig {
            seed: 4242,
            ..NpcConfig::default()
        },
        other_config,
    );
    let mut index = 0u64;
    loop {
        let clock = other_driver.spine().clock().as_u64();
        if clock >= 200 {
            break;
        }
        let outstanding = other_driver.outstanding_ticks();
        let remaining = (200 - clock) as u32;
        if outstanding >= remaining {
            other_driver.run_headless(u64::from(remaining));
            break;
        }
        let rate = other_driver.config().tick_rate_hz;
        let period_ns = 1_000_000_000u64 / u64::from(rate.max(1));
        let cap = Duration::from_nanos(u64::from(remaining - outstanding) * period_ns);
        other_driver.advance(frame.min(cap));
        index += 1;
    }
    let _ = index;
    assert_ne!(
        baseline,
        other_driver.spine().lane_state_fingerprint(),
        "seed is authoritative state"
    );
}
