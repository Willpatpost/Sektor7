//! The Cycle-4 capability-E performance sweep (owner GO §6): reproducible
//! adversary-population workloads with per-tick instrumentation, separating
//! the dormant floor from active combat regimes.
//!
//! Design contract: `cycle4/deliverables/E-workload-design.md` (the cells
//! are DATA, fixed before results — anti-manipulation). Timestamp
//! contract: `cycle4/interfaces/EVENT-TIMESTAMPS.md` — every sample is
//! taken AFTER the tick's `run_tick`, labeled with the spine clock
//! (1-based, `Tick::ZERO`-origin; "measured at tick N" = after N executed
//! logical ticks).
//!
//! Phase 0 (this module, at the single-adversary tree `c514911`): the
//! cells that do NOT depend on the unreconciled
//! `install_adversary_schedules` mixed-dormant semantics —
//! - `floor-<P>`: P adversaries, ALL dormant, never activated (the
//!   C3.1 dormant-start composition held forever) — the executed-work
//!   floor and the memory footprint vs population.
//! - `one-<P>`: P adversaries dormant, exactly ONE activated at a fixed
//!   tick via the C3.1-blessed `activation_spec` mechanism — the
//!   dormant-majority tax on one active adversary.
//! - `baseline-1`: the canonical winnable `CombatDemo` fight, end to
//!   end, repeated — the wall-clock comparison point for the
//!   spine-side cells.
//! - `collide-<Q>`: the collision query cost (segment + axis sweep)
//!   vs obstacle count — the current AABB baseline Seat C's rotated
//!   work is judged against.
//! - `queued-<D>`: per-tick spine overhead vs pending-queue depth at
//!   a fixed 1-execution-per-tick chain (synthetic obligations — the
//!   harness's own substrate, no adversary semantics).
//!
//! Phase 1 (SEAT A's reconciled multi-adversary contract) adds the
//! k-of-N activation cells through the same `PerfSweepSpec` parameters.
//!
//! The 200/60 budget is FIXED in every cell (the owner-gated default;
//! any budget recommendation is a separate labeled document, never a
//! cell parameter).

use std::time::Instant;

use engine_core::adversary::{AdversaryConfig, AdversaryWorld};
use engine_core::clock::Tick;
use engine_core::event::{EventClass, EventSpec, Field, Schedule};
use engine_core::player::{CollisionWorld, StaticCollider};
use engine_core::spine::Spine;
use engine_core::{Fingerprinter, World};

use crate::combat_demo::{CombatDemo, run_winnable_script};

/// The owner-gated default reservation — fixed in every cell.
const BUDGET: u32 = 200;
const NEAR_RESERVE: u32 = 60;

/// The placeholder perception geometry the demo uses (comparability:
/// the same numbers as `combat_demo::adversary_config`).
const AGGRO_RADIUS: f32 = 8.0;
const DEAGGRO_RADIUS: f32 = 10.0;
const PATROL_CADENCE: u64 = 12;
const AGGRO_CADENCE: u64 = 2;
const RECOVER_CADENCE: u64 = 6;
const ATTACK_COOLDOWN: u64 = 18;
const ATTACK_RANGE: f32 = 2.0;
const ADVERSARY_SPEED: f32 = 4.0;
const MAX_HEALTH: f32 = 100.0;
/// The cells' circuit radius (the empty collision world allows a wider
/// circuit than the demo's arena-constrained 2.5 m).
const WAYPOINT_RADIUS: f32 = 6.0;
/// The combat regime's player orbit radius: distances to the adversary
/// circuit stay within [3, 9] m — inside aggro (8), never beyond
/// de-aggro (10): a stable AGGRO/ATTACK regime once entered.
const COMBAT_ORBIT: f32 = 3.0;
/// The patrol regime's orbit: distances [14, 26] m — never aggro.
const PATROL_ORBIT: f32 = 20.0;

/// One population cell's specification (the workload-design axes).
#[derive(Debug, Clone, PartialEq)]
pub struct PerfSweepSpec {
    pub name: String,
    /// Axis P — the adversary population.
    pub population: usize,
    /// Axis A — the activity distribution (Phase 0: the floor and the
    /// single blessed activation; Phase 1 adds k-of-N on A's contract).
    pub activity: Activity,
    /// The player script's orbit radius (the regime knob: combat vs
    /// patrol pressure).
    pub player_orbit: f32,
    pub duration_ticks: u64,
    pub seed: u64,
}

/// The Phase-0 activity profiles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Activity {
    /// All adversaries dormant, never activated — nothing scheduled.
    DormantFloor,
    /// Exactly ONE adversary (index 0) activated at the given (1-based)
    /// tick via the C3.1-blessed `activation_spec` mechanism.
    OneActive { activation_tick: u64 },
}

impl PerfSweepSpec {
    /// The Phase-0 population cell list (the workload-design table).
    pub fn phase0_cells() -> Vec<PerfSweepSpec> {
        let populations = [1usize, 4, 32, 128, 256, 512];
        let mut cells = Vec::new();
        for population in populations {
            cells.push(PerfSweepSpec {
                name: format!("floor-{population}"),
                population,
                activity: Activity::DormantFloor,
                player_orbit: PATROL_ORBIT,
                duration_ticks: 600,
                seed: 0xE_CEEE,
            });
            cells.push(PerfSweepSpec {
                name: format!("one-{population}"),
                population,
                activity: Activity::OneActive {
                    activation_tick: 30,
                },
                player_orbit: COMBAT_ORBIT,
                duration_ticks: 1800,
                seed: 0xE_CEEE,
            });
        }
        cells
    }
}

/// One per-tick sample (taken after that tick's `run_tick`; the tick
/// label is the spine clock per the timestamp contract).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TickSample {
    pub tick: u64,
    /// The wall-clock duration of THIS tick's `run_tick` call.
    pub run_nanos: u64,
    /// Cumulative executed events (the spine's `Stats.executed`) after
    /// this tick.
    pub executed: u64,
    /// Cumulative near-lane executions after this tick.
    pub near_executed: u64,
    /// Cumulative far-lane executions after this tick.
    pub far_executed: u64,
    /// Pending obligations after this tick.
    pub pending: usize,
    /// Whether due work remained in the near lane when the budget was
    /// exhausted during this tick (the backlog flag).
    pub near_backlogged: bool,
    pub far_backlogged: bool,
}

/// A cell's measured outcome — the counters are deterministic
/// (profile-independent); the wall-clock series is hardware-dependent
/// and always reported with the environment header in the evidence
/// file, never without it.
#[derive(Debug, Clone, PartialEq)]
pub struct CellOutcome {
    pub spec: PerfSweepSpec,
    pub samples: Vec<TickSample>,
    /// The spine's cumulative counters at the end (fingerprinted
    /// logical state — the determinism evidence).
    pub scheduled: u64,
    pub executed: u64,
    pub late_arrivals: u64,
    pub late_executions: u64,
    pub blocked_transitions: u64,
    pub invalid_dropped: u64,
    pub budget_exhausted_ticks: u64,
    /// Near/far lane totals (observational).
    pub lane_near_executed: u64,
    pub lane_far_executed: u64,
    pub lane_near_backlogged_ticks: u64,
    pub lane_far_backlogged_ticks: u64,
    /// The adversaries actively threatening at the end.
    pub active_adversaries: usize,
    /// The final world fingerprint (the determinism comparator).
    pub world_fingerprint: String,
    /// RSS (kB) at cell start and end.
    pub rss_start_kb: u64,
    pub rss_end_kb: u64,
    /// Ledger length at the end.
    pub ledger_len: usize,
    /// Positive-lateness summary: the late count and the max lateness
    /// in ticks (the full per-event series is derivable from the
    /// spine's ledger at the same spec).
    pub late_count: u64,
    pub max_lateness: u64,
}

/// The empty collision world for the spine-side cells (obstacle cost is
/// the `collide-<Q>` axis's job, measured separately; an empty world
/// gives LOS-clear geometry so the regime is distance-controlled).
fn empty_collision_world() -> CollisionWorld {
    CollisionWorld::new(0.0).with_bounds([-64.0, 0.0, -64.0], [64.0, 64.0, 64.0])
}

/// The cell world configuration (the demo's placeholder numbers,
/// parameterized population and seed).
fn sweep_adversary_config(population: usize, seed: u64) -> AdversaryConfig {
    AdversaryConfig {
        seed,
        count: population,
        tick_seconds: 1.0 / 60.0,
        aggro_radius: AGGRO_RADIUS,
        deaggro_radius: DEAGGRO_RADIUS,
        los_loss_ticks: 24,
        patrol_cadence: PATROL_CADENCE,
        aggro_cadence: AGGRO_CADENCE,
        recover_cadence: RECOVER_CADENCE,
        attack_cooldown: ATTACK_COOLDOWN,
        attack_range: ATTACK_RANGE,
        speed: ADVERSARY_SPEED,
        waypoint_radius: WAYPOINT_RADIUS,
        spawn_center: [0.0, 0.0, 0.0],
        max_health: MAX_HEALTH,
        eye_height: 1.5,
        player: engine_core::player::PlayerConfig::default(),
        player_id: 1,
        adversary_id_base: 0,
    }
}

/// The deterministic player script: a seeded-phase orbit at the cell's
/// radius around the spawn center (one full circuit per 180 ticks).
/// The position is finite by construction (cos/sin of bounded angles).
fn orbit_position(seed: u64, tick: u64, radius: f32) -> [f32; 3] {
    let phase = (seed % 360) as f32;
    let angle = phase + (tick as f32) * (std::f32::consts::TAU / 180.0);
    [radius * angle.cos(), 0.0, radius * angle.sin()]
}

/// Reads this process's resident set size (kB) — the honest memory
/// instrument available under `unsafe_code = deny` (a counting global
/// allocator is forbidden; the C2-P2 F2 precedent).
fn rss_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .unwrap_or(0);
        }
    }
    0
}

/// Nearest-rank percentile over a sorted copy of the series.
fn percentile(series: &[u64], rank: u64) -> u64 {
    if series.is_empty() {
        return 0;
    }
    let mut sorted = series.to_vec();
    sorted.sort_unstable();
    let index = (rank as usize * sorted.len()).div_ceil(100);
    sorted[index.min(sorted.len() - 1)]
}

/// (min, p50, p90, p99, max, mean) over the series.
fn series_summary(series: &[u64]) -> (u64, u64, u64, u64, u64, f64) {
    if series.is_empty() {
        return (0, 0, 0, 0, 0, 0.0);
    }
    let sum: u128 = series.iter().map(|value| u128::from(*value)).sum();
    (
        *series.iter().min().unwrap(),
        percentile(series, 50),
        percentile(series, 90),
        percentile(series, 99),
        *series.iter().max().unwrap(),
        sum as f64 / series.len() as f64,
    )
}

/// The per-tick deltas of a cumulative sample series (the first tick's
/// delta is measured from zero — tick 0 is the construction state).
fn per_tick_deltas(samples: &[TickSample], field: fn(&TickSample) -> u64) -> Vec<u64> {
    let mut deltas = Vec::with_capacity(samples.len());
    let mut previous = 0u64;
    for sample in samples {
        let current = field(sample);
        deltas.push(current.saturating_sub(previous));
        previous = current;
    }
    deltas
}

/// Runs one population cell (floor-P / one-P) and returns the measured
/// outcome. Deterministic on the logical counters for a given spec.
pub fn run_population_cell(spec: &PerfSweepSpec) -> CellOutcome {
    let rss_start = rss_kb();
    let mut spine = Spine::with_reservation(
        AdversaryWorld::new(
            sweep_adversary_config(spec.population, spec.seed),
            empty_collision_world(),
        ),
        BUDGET,
        NEAR_RESERVE,
    );
    // The C3.1 dormant-start composition: every adversary dormant,
    // NOTHING scheduled at construction.
    for index in 0..spec.population {
        spine.world_mut().set_dormant(index);
    }
    let mut samples = Vec::with_capacity(spec.duration_ticks as usize);
    let activation_due_tick = match spec.activity {
        Activity::OneActive { activation_tick } => Some(activation_tick),
        Activity::DormantFloor => None,
    };
    for tick_index in 1..=spec.duration_ticks {
        // The once-per-tick player sample (this tick's position), the
        // same driver discipline as the demo.
        spine.world_mut().set_player_position(orbit_position(
            spec.seed,
            tick_index,
            spec.player_orbit,
        ));
        // The single blessed activation (the C3.1 mechanism), scheduled
        // at the NEXT spine tick exactly as the demo's activation line.
        if activation_due_tick == Some(tick_index) {
            let due = Tick(spine.run_ticks(0).0 + 1);
            if let Some(activation) = spine.world().activation_spec(0, due) {
                spine
                    .schedule(activation)
                    .expect("the activation schedule is dependency-free");
            }
        }
        let started = Instant::now();
        spine.run_tick();
        let run_nanos = started.elapsed().as_nanos() as u64;
        // Sampled AFTER the tick's run: the label is the spine clock
        // (the timestamp contract's T4 invariant).
        debug_assert_eq!(spine.clock().0, tick_index);
        let stats = spine.stats();
        let totals = spine.lane_totals();
        let report = spine.lane_report();
        samples.push(TickSample {
            tick: spine.clock().0,
            run_nanos,
            executed: stats.executed,
            near_executed: totals.near_executed,
            far_executed: totals.far_executed,
            pending: spine.pending_count(),
            near_backlogged: report.near_backlogged,
            far_backlogged: report.far_backlogged,
        });
    }
    let stats = spine.stats();
    let totals = spine.lane_totals();
    let mut late_count = 0u64;
    let mut max_lateness = 0u64;
    for record in spine.ledger() {
        if record.late {
            late_count += 1;
            let lateness = record.executed_tick.0.saturating_sub(record.intended_due.0);
            max_lateness = max_lateness.max(lateness);
        }
    }
    let mut fp = Fingerprinter::new();
    spine.world().fingerprint(&mut fp);
    CellOutcome {
        spec: spec.clone(),
        samples,
        scheduled: stats.scheduled,
        executed: stats.executed,
        late_arrivals: stats.late_arrivals,
        late_executions: stats.late_executions,
        blocked_transitions: stats.blocked_transitions,
        invalid_dropped: stats.invalid_dropped,
        budget_exhausted_ticks: stats.budget_exhausted_ticks,
        lane_near_executed: totals.near_executed,
        lane_far_executed: totals.far_executed,
        lane_near_backlogged_ticks: totals.near_backlogged_ticks,
        lane_far_backlogged_ticks: totals.far_backlogged_ticks,
        active_adversaries: spine.world().active_count(),
        world_fingerprint: fp.finish_hex(),
        rss_start_kb: rss_start,
        rss_end_kb: rss_kb(),
        ledger_len: spine.ledger().len(),
        late_count,
        max_lateness,
    }
}

/// Prints one cell's measured block (the evidence-file body; the run
/// wrapper prepends the environment header).
pub fn print_cell_outcome(outcome: &CellOutcome) {
    let spec = &outcome.spec;
    println!("=== cell {} ===", spec.name);
    println!(
        "spec: population={} activity={:?} orbit={} ticks={} seed={}",
        spec.population, spec.activity, spec.player_orbit, spec.duration_ticks, spec.seed
    );
    println!(
        "budget: {}/{} (fixed — the owner-gated default)",
        BUDGET, NEAR_RESERVE
    );
    let executed_series = per_tick_deltas(&outcome.samples, |sample| sample.executed);
    let (min, p50, p90, p99, max, mean) = series_summary(&executed_series);
    println!("executed/tick: min={min} p50={p50} p90={p90} p99={p99} max={max} mean={mean:.3}");
    let near_series = per_tick_deltas(&outcome.samples, |sample| sample.near_executed);
    let (_, near_p50, _, near_p99, near_max, near_mean) = series_summary(&near_series);
    println!("near/tick: p50={near_p50} p99={near_p99} max={near_max} mean={near_mean:.3}");
    let far_series = per_tick_deltas(&outcome.samples, |sample| sample.far_executed);
    let (_, far_p50, _, far_p99, far_max, far_mean) = series_summary(&far_series);
    println!("far/tick: p50={far_p50} p99={far_p99} max={far_max} mean={far_mean:.3}");
    let run_series: Vec<u64> = outcome
        .samples
        .iter()
        .map(|sample| sample.run_nanos)
        .collect();
    let (rmin, rp50, rp90, rp99, rmax, rmean) = series_summary(&run_series);
    println!(
        "run_tick wall (ns): min={rmin} p50={rp50} p90={rp90} p99={rp99} max={rmax} mean={rmean:.1}"
    );
    let warm: Vec<u64> = run_series.iter().skip(60).copied().collect();
    let (_, wp50, wp90, wp99, wmax, wmean) = series_summary(&warm);
    println!(
        "run_tick wall (ns, ticks 61+ post-warmup): p50={wp50} p90={wp90} p99={wp99} max={wmax} mean={wmean:.1}"
    );
    let pending_final = outcome.samples.last().map(|s| s.pending).unwrap_or(0);
    println!(
        "counters: scheduled={} executed={} late_arrivals={} late_executions={} blocked={} invalid_dropped={} budget_exhausted_ticks={}",
        outcome.scheduled,
        outcome.executed,
        outcome.late_arrivals,
        outcome.late_executions,
        outcome.blocked_transitions,
        outcome.invalid_dropped,
        outcome.budget_exhausted_ticks
    );
    println!(
        "lanes: near={} far={} near_backlog_ticks={} far_backlog_ticks={} pending_final={} ledger_len={}",
        outcome.lane_near_executed,
        outcome.lane_far_executed,
        outcome.lane_near_backlogged_ticks,
        outcome.lane_far_backlogged_ticks,
        pending_final,
        outcome.ledger_len
    );
    println!(
        "lateness: late_count={} max_lateness_ticks={}",
        outcome.late_count, outcome.max_lateness
    );
    println!(
        "active_adversaries_final={} world_fingerprint={}",
        outcome.active_adversaries, outcome.world_fingerprint
    );
    println!(
        "rss: start={}kB end={}kB delta={}kB",
        outcome.rss_start_kb,
        outcome.rss_end_kb,
        outcome.rss_end_kb.saturating_sub(outcome.rss_start_kb)
    );
    println!();
}

/// The baseline-1 cell: the canonical winnable `CombatDemo` fight, end
/// to end, `repeats` fresh rounds — the wall-clock comparison point
/// for the spine-side cells (the demo orchestration included; the
/// adaptive script's conditions read private demo state, so the honest
/// public-API measurement is the whole-fight duration distribution).
pub fn run_baseline_cell(repeats: usize) {
    println!("=== cell baseline-1 (CombatDemo path, winnable script) ===");
    println!("spec: population=1 repeats={repeats} script=run_winnable_script");
    let rss_start = rss_kb();
    let mut fight_nanos = Vec::with_capacity(repeats);
    let mut fight_ticks = Vec::with_capacity(repeats);
    for _ in 0..repeats {
        let mut demo = CombatDemo::new();
        let started = Instant::now();
        run_winnable_script(&mut demo);
        fight_nanos.push(started.elapsed().as_nanos() as u64);
        fight_ticks.push(demo.tick_index());
    }
    let (fmin, fp50, fp90, fp99, fmax, fmean) = series_summary(&fight_nanos);
    println!(
        "fight wall (ns): min={fmin} p50={fp50} p90={fp90} p99={fp99} max={fmax} mean={fmean:.1}"
    );
    let (tmin, tp50, _, _, tmax, tmean) = series_summary(&fight_ticks);
    println!("fight ticks: min={tmin} p50={tp50} max={tmax} mean={tmean:.1}");
    println!(
        "per-tick whole-path estimate (mean fight ns / mean ticks): {:.1} ns",
        fmean / tmean.max(1.0)
    );
    let rss_end = rss_kb();
    println!(
        "rss: start={rss_start}kB end={rss_end}kB delta={}kB",
        rss_end.saturating_sub(rss_start)
    );
    println!();
}

/// The collide-Q cell: the collision query cost vs obstacle count on
/// the CURRENT public AABB path (`segment_hit` + `sweep_axis`) — the
/// baseline Seat C's rotated-geometry work is judged against.
pub fn run_collide_cell(obstacles: usize, queries: usize, seed: u64) {
    println!("=== cell collide-{obstacles} ===");
    println!(
        "spec: obstacles={obstacles} queries={queries} seed={seed} path=segment_hit+sweep_axis"
    );
    // A seeded obstacle field: boxes on a jittered grid covering the
    // [-32, 32]^2 ground plane (deterministic positions from the seed).
    let mut world = CollisionWorld::new(0.0);
    let mut rng = spec_rng(seed);
    for index in 0..obstacles {
        let cell = index as f32;
        let x = -32.0 + (cell % 16.0) * 4.0 + (rng() % 100) as f32 / 100.0;
        let z = -32.0 + ((cell / 16.0) % 16.0) * 4.0 + (rng() % 100) as f32 / 100.0;
        let half = 0.8 + (rng() % 100) as f32 / 200.0;
        world = world.with_collider(StaticCollider {
            min: [x - half, 0.0, z - half],
            max: [x + half, 2.0, z + half],
        });
    }
    world = world.with_bounds([-40.0, 0.0, -40.0], [40.0, 64.0, 40.0]);
    // The query script: seeded origin/target pairs across the field
    // (segment LOS tests) plus horizontal axis sweeps (the movement
    // query) from the same origins.
    let mut segment_nanos = Vec::with_capacity(queries);
    let mut sweep_nanos = Vec::with_capacity(queries);
    for query in 0..queries {
        let angle = (rng() % 360) as f32 * (std::f32::consts::TAU / 360.0);
        let radius = (rng() % 3200) as f32 / 100.0;
        let origin = [radius * angle.cos(), 1.5, radius * angle.sin()];
        let target = [-origin[0], 1.5 + (query % 3) as f32 * 0.1, -origin[2]];
        let started = Instant::now();
        let _ = world.segment_hit(origin, target, 0.0);
        segment_nanos.push(started.elapsed().as_nanos() as u64);
        let min = [origin[0] - 0.3, 0.0, origin[2] - 0.3];
        let max = [origin[0] + 0.3, 1.7, origin[2] + 0.3];
        let started = Instant::now();
        let _ = world.sweep_axis(min, max, 0, 1.0);
        sweep_nanos.push(started.elapsed().as_nanos() as u64);
    }
    let (smin, sp50, sp90, sp99, smax, smean) = series_summary(&segment_nanos);
    println!(
        "segment_hit (ns): min={smin} p50={sp50} p90={sp90} p99={sp99} max={smax} mean={smean:.1}"
    );
    let (wmin, wp50, wp90, wp99, wmax, wmean) = series_summary(&sweep_nanos);
    println!(
        "sweep_axis (ns): min={wmin} p50={wp50} p90={wp90} p99={wp99} max={wmax} mean={wmean:.1}"
    );
    println!();
}

/// A tiny deterministic LCG for the seeded layout/query scripts (the
/// harness's own — never a time or entropy source).
fn spec_rng(seed: u64) -> impl FnMut() -> u64 {
    let mut state = seed | 1;
    move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        state
    }
}

/// The queued-D cell: per-tick spine overhead vs pending depth with a
/// fixed 1-execution-per-tick chain — synthetic far-lane obligations on
/// a no-op world (the harness's own substrate; no adversary semantics).
pub fn run_queued_cell(depth: usize, duration_ticks: u64, seed: u64) {
    println!("=== cell queued-{depth} ===");
    println!(
        "spec: pending_depth={depth} due_per_tick=1(chain) ticks={duration_ticks} seed={seed}"
    );
    let rss_start = rss_kb();
    let mut spine = Spine::with_reservation(NoopWorld::default(), BUDGET, NEAR_RESERVE);
    let mut rng = spec_rng(seed);
    // The depth fill: obligations due far beyond the run (they stay
    // pending — the queue-machinery cost at constant depth).
    let far_due = Tick(duration_ticks + 10_000);
    for _ in 0..depth {
        let spec = EventSpec::new(
            EventClass(0xE_00D),
            Schedule::AtLogicalTime { due: far_due },
            rng() % 1024,
        )
        .in_field(Field::Far);
        spine.schedule(spec).expect("the synthetic depth fill");
    }
    // The fixed executing chain: one obligation per tick, forever (each
    // execution follows itself up at the next tick).
    let boot = EventSpec::new(
        EventClass(0xE_00E),
        Schedule::AtLogicalTime { due: Tick(1) },
        0xE_CADE,
    )
    .in_field(Field::Far);
    spine.schedule(boot).expect("the boot obligation");
    let mut run_nanos = Vec::with_capacity(duration_ticks as usize);
    for _ in 0..duration_ticks {
        let started = Instant::now();
        spine.run_tick();
        run_nanos.push(started.elapsed().as_nanos() as u64);
    }
    let (rmin, rp50, rp90, rp99, rmax, rmean) = series_summary(&run_nanos);
    println!(
        "run_tick wall (ns): min={rmin} p50={rp50} p90={rp90} p99={rp99} max={rmax} mean={rmean:.1}"
    );
    let warm: Vec<u64> = run_nanos.iter().skip(60).copied().collect();
    let (_, wp50, wp90, wp99, wmax, wmean) = series_summary(&warm);
    println!(
        "run_tick wall (ns, ticks 61+ post-warmup): p50={wp50} p90={wp90} p99={wp99} max={wmax} mean={wmean:.1}"
    );
    let stats = spine.stats();
    println!(
        "counters: scheduled={} executed={} pending_final={} budget_exhausted_ticks={}",
        stats.scheduled,
        stats.executed,
        spine.pending_count(),
        stats.budget_exhausted_ticks
    );
    let rss_end = rss_kb();
    println!(
        "rss: start={rss_start}kB end={rss_end}kB delta={}kB",
        rss_end.saturating_sub(rss_start)
    );
    println!();
}

/// The queued cell's no-op world: chain executions schedule their own
/// successor one tick later, keeping the due-rate constant.
#[derive(Default)]
struct NoopWorld {
    executions: u64,
}

impl engine_core::spine::World for NoopWorld {
    fn execute(
        &mut self,
        event: &engine_core::event::Event,
        ctx: &mut engine_core::spine::ExecCtx,
    ) {
        self.executions += 1;
        if event.payload == 0xE_CADE {
            ctx.follow_up(1, EventClass(0xE_00E), event.payload, Vec::new());
        }
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        fp.write_u64(self.executions);
    }
}

/// Runs a named Phase-0 cell (the CLI entry: `--benchmark-perf-sweep
/// <name>`); one cell per process keeps RSS attribution clean.
pub fn run_named(name: &str) -> bool {
    for spec in PerfSweepSpec::phase0_cells() {
        if spec.name == name {
            let outcome = run_population_cell(&spec);
            print_cell_outcome(&outcome);
            return true;
        }
    }
    match name {
        "baseline-1" => {
            run_baseline_cell(30);
            true
        }
        "collide-1" => {
            run_collide_cell(1, 100_000, 0xC011);
            true
        }
        "collide-8" => {
            run_collide_cell(8, 100_000, 0xC011);
            true
        }
        "collide-64" => {
            run_collide_cell(64, 100_000, 0xC011);
            true
        }
        "collide-512" => {
            run_collide_cell(512, 100_000, 0xC011);
            true
        }
        "collide-4096" => {
            run_collide_cell(4096, 100_000, 0xC011);
            true
        }
        "queued-0" => {
            run_queued_cell(0, 600, 0x5EED);
            true
        }
        "queued-100" => {
            run_queued_cell(100, 600, 0x5EED);
            true
        }
        "queued-1000" => {
            run_queued_cell(1_000, 600, 0x5EED);
            true
        }
        "queued-10000" => {
            run_queued_cell(10_000, 600, 0x5EED);
            true
        }
        "queued-100000" => {
            run_queued_cell(100_000, 600, 0x5EED);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Determinism: the same spec produces identical logical counters
    /// and world fingerprint (the measurement evidence is reproducible;
    /// only wall-clock may vary).
    #[test]
    fn population_cells_are_deterministic() {
        let spec = PerfSweepSpec {
            name: "tiny".into(),
            population: 4,
            activity: Activity::OneActive { activation_tick: 5 },
            player_orbit: COMBAT_ORBIT,
            duration_ticks: 60,
            seed: 0xE_CEEE,
        };
        let first = run_population_cell(&spec);
        let second = run_population_cell(&spec);
        assert_eq!(first.executed, second.executed);
        assert_eq!(first.scheduled, second.scheduled);
        assert_eq!(first.world_fingerprint, second.world_fingerprint);
        assert_eq!(first.samples.len(), 60);
    }

    /// The dormant floor executes NOTHING (the C3.1 composition held
    /// forever): the executed-work floor is exactly zero at any
    /// population.
    #[test]
    fn the_dormant_floor_executes_nothing() {
        for population in [1usize, 8] {
            let spec = PerfSweepSpec {
                name: format!("floor-{population}"),
                population,
                activity: Activity::DormantFloor,
                player_orbit: PATROL_ORBIT,
                duration_ticks: 50,
                seed: 0xE_CEEE,
            };
            let outcome = run_population_cell(&spec);
            assert_eq!(outcome.executed, 0, "population {population}");
            assert_eq!(outcome.scheduled, 0);
            assert_eq!(outcome.active_adversaries, 0);
            assert_eq!(outcome.samples.last().map(|s| s.pending), Some(0));
        }
    }

    /// The single blessed activation runs the cadence chain: executed
    /// work becomes positive and the adversary turns active (AGGRO or
    /// beyond) within the combat regime.
    #[test]
    fn the_single_activation_runs_the_chain() {
        let spec = PerfSweepSpec {
            name: "one-tiny".into(),
            population: 4,
            activity: Activity::OneActive { activation_tick: 5 },
            player_orbit: COMBAT_ORBIT,
            duration_ticks: 120,
            seed: 0xE_CEEE,
        };
        let outcome = run_population_cell(&spec);
        assert!(outcome.executed > 0, "the chain must execute");
        assert!(
            outcome.active_adversaries >= 1,
            "the activated adversary must be threatening (active={})",
            outcome.active_adversaries
        );
        // The dormant majority stays dormant: only one adversary is
        // active of the four.
        assert_eq!(outcome.active_adversaries, 1);
    }

    /// The queued cell's chain executes exactly once per tick and the
    /// depth fill stays pending.
    #[test]
    fn the_queued_chain_executes_once_per_tick() {
        let spec = PerfSweepSpec {
            name: "queued-tiny".into(),
            population: 0,
            activity: Activity::DormantFloor,
            player_orbit: PATROL_ORBIT,
            duration_ticks: 40,
            seed: 0x5EED,
        };
        let _ = &spec;
        let mut spine = Spine::with_reservation(NoopWorld::default(), BUDGET, NEAR_RESERVE);
        let far_due = Tick(10_040);
        let mut rng = spec_rng(0x5EED);
        for _ in 0..50 {
            let fill = EventSpec::new(
                EventClass(0xE_00D),
                Schedule::AtLogicalTime { due: far_due },
                rng() % 1024,
            )
            .in_field(Field::Far);
            spine.schedule(fill).expect("the depth fill");
        }
        let boot = EventSpec::new(
            EventClass(0xE_00E),
            Schedule::AtLogicalTime { due: Tick(1) },
            0xE_CADE,
        )
        .in_field(Field::Far);
        spine.schedule(boot).expect("the boot obligation");
        for tick_index in 1..=40u64 {
            spine.run_tick();
            debug_assert_eq!(spine.clock().0, tick_index);
        }
        assert_eq!(spine.stats().executed, 40, "one execution per tick");
        // 50 depth fill + the chain's one in-flight successor (each
        // execution has already scheduled the next tick's event).
        assert_eq!(spine.pending_count(), 51);
    }
}
