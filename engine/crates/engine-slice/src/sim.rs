//! Stage 3 fixed-timestep simulation driver — application-level policy
//! (Stage 3 authorization §1). Monotonic real time decides **when**
//! simulation ticks are due; logical state ever sees only fixed ticks:
//! wall-clock timestamps never reach authoritative calculations. Render
//! cadence stays fully independent of simulation cadence.

use std::time::Duration;

use engine_core::{Spine, Tick};

use crate::npc::{NpcConfig, NpcWorld, install_initial_schedules};

/// Default simulation tick rate for the Stage 3 NPC scheduling
/// demonstration: 10 Hz. This is a *demo default, not an engine-wide
/// rate* (reviewer clarification 2) — E-10's synchrony results inform
/// event-budget sizing, not the universal simulation frequency, and
/// future latency-sensitive systems (physics, player movement, combat)
/// will choose their own rates. Configurable in [`SimConfig`].
pub const DEFAULT_TICK_RATE_HZ: u32 = 10;

/// Default per-tick budget: sized to the workload's sustainable average
/// with cluster headroom (E-10: budgets are sized to cluster peaks, not
/// average rates). With 1,000 NPCs at period 6: near demand ≈ 42/tick
/// (250 near-region NPCs) against a 60 reservation, far demand ≈ 125
/// against 140 nonreserved — sustainable, with the seeded jitter
/// spreading synchrony clusters.
pub const DEFAULT_BUDGET: u32 = 200;
pub const DEFAULT_NEAR_RESERVE: u32 = 60;

/// Simulation driver configuration — policy values, all configurable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimConfig {
    pub tick_rate_hz: u32,
    pub budget: u32,
    pub near_reserve: u32,
    /// Per-frame catch-up bound: keeps the frame loop responsive during
    /// stalls. Due-but-unexecuted ticks stay accrued in the accumulator
    /// and are reported as outstanding — never silently discarded
    /// (Stage 3 §1); they drain when capacity allows.
    pub max_catchup_per_frame: u32,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            tick_rate_hz: DEFAULT_TICK_RATE_HZ,
            budget: DEFAULT_BUDGET,
            near_reserve: DEFAULT_NEAR_RESERVE,
            max_catchup_per_frame: 8,
        }
    }
}

/// One advance step's outcome (observational reporting for §1 and §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimStep {
    pub executed_ticks: u32,
    /// Due ticks accrued but not yet executed — a retained obligation
    /// that drains when catch-up capacity allows.
    pub outstanding_ticks: u32,
    pub campaign_tick: Tick,
}

/// Fixed-timestep driver over the NPC spine.
pub struct SimDriver {
    spine: Spine<NpcWorld>,
    config: SimConfig,
    period_ns: u128,
    accumulator_ns: u128,
    /// Bounded history of per-tick execution durations in milliseconds —
    /// observational only, never fingerprinted.
    tick_ms_history: Vec<f64>,
}

impl SimDriver {
    pub fn new(npc_config: NpcConfig, config: SimConfig) -> Self {
        let mut spine = Spine::with_reservation(
            NpcWorld::new(npc_config.clone()),
            config.budget,
            config.near_reserve,
        );
        install_initial_schedules(&mut spine, &npc_config);
        let rate = config.tick_rate_hz.max(1);
        SimDriver {
            spine,
            config,
            period_ns: u128::from(1_000_000_000u64) / u128::from(rate),
            accumulator_ns: 0,
            tick_ms_history: Vec::new(),
        }
    }

    pub fn spine(&self) -> &Spine<NpcWorld> {
        &self.spine
    }

    pub fn spine_mut(&mut self) -> &mut Spine<NpcWorld> {
        &mut self.spine
    }

    pub fn config(&self) -> SimConfig {
        self.config
    }

    /// Accrued due-but-unexecuted ticks right now (retained obligation).
    pub fn outstanding_ticks(&self) -> u32 {
        (self.accumulator_ns / self.period_ns) as u32
    }

    /// Advances the simulation by elapsed monotonic time. The elapsed
    /// duration only accrues due ticks — it never enters logical state.
    /// When more ticks are due than the catch-up bound allows, the
    /// remainder stays accrued and is reported as outstanding.
    pub fn advance(&mut self, elapsed: Duration) -> SimStep {
        self.accumulator_ns += elapsed.as_nanos();
        let due = (self.accumulator_ns / self.period_ns) as u32;
        let executed = due.min(self.config.max_catchup_per_frame);
        self.run_ticks(executed);
        // Only executed ticks consume the accumulator: the unexecuted
        // obligation is retained, drains later, and is always reported.
        self.accumulator_ns -= u128::from(executed) * self.period_ns;
        SimStep {
            executed_ticks: executed,
            outstanding_ticks: due - executed,
            campaign_tick: self.spine.clock(),
        }
    }

    /// Runs exactly `ticks` fixed ticks — the headless deterministic
    /// path (same spine logic; no real-time input at all).
    pub fn run_headless(&mut self, ticks: u64) {
        self.run_ticks(ticks as u32);
    }

    fn run_ticks(&mut self, ticks: u32) {
        if ticks == 0 {
            return;
        }
        let start = std::time::Instant::now();
        self.spine.run_ticks(u64::from(ticks));
        let per_tick_ms = start.elapsed().as_secs_f64() * 1000.0 / f64::from(ticks);
        self.tick_ms_history.push(per_tick_ms);
        let len = self.tick_ms_history.len();
        if len > 600 {
            self.tick_ms_history.drain(0..len - 600);
        }
    }

    /// Observational per-tick duration percentiles (milliseconds).
    pub fn tick_ms_percentile(&self, quantile: f64) -> f64 {
        if self.tick_ms_history.is_empty() {
            return 0.0;
        }
        let mut sorted = self.tick_ms_history.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let index = ((quantile * sorted.len() as f64).ceil() as usize)
            .saturating_sub(1)
            .min(sorted.len() - 1);
        sorted[index]
    }

    pub fn tick_sample_count(&self) -> usize {
        self.tick_ms_history.len()
    }
}
