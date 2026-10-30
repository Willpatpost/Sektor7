//! The S6-C4 crossing-latency profiling instrumentation — STRICTLY
//! observational, OPT-IN, and INACTIVE BY DEFAULT (the profiling
//! GO's boundary). Nothing here touches logical state, persistence
//! formats, transaction semantics, the scenario schedule, or
//! recovery behavior: the marks read `Instant`s around existing
//! operations and record wall-time durations into a bounded global
//! collector, enabled once through the CLI flag. Instrumented and
//! non-instrumented runs produce IDENTICAL logical state (the
//! equivalence regression proves it against the closing stage-4
//! fingerprint); every recorded duration is fingerprint-excluded
//! observational evidence in the S6-C1 discipline.
//!
//! **The step classes** (so wall time is never misread as active
//! CPU time): `Compute` marks are CPU-bound serialization/decode
//! work; `IoWait` marks include provider waits (fsyncs, queue
//! waits, lock waits) where the thread is not necessarily busy —
//! the records carry wall milliseconds only, and the class label
//! keeps the report from conflating them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// One measured step inside (or immediately after) a consequential
/// crossing.
#[derive(Debug, Clone)]
struct StepRecord {
    step: &'static str,
    class: StepClass,
    region: usize,
    tick: u64,
    wall_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepClass {
    Compute,
    IoWait,
}

/// One logical tick's crossing aggregate: how many region
/// transitions the orchestrated tick performed and the summed
/// crossing wall time.
#[derive(Debug, Clone)]
struct TickRecord {
    tick: u64,
    transitions: usize,
    crossing_ms: f64,
}

/// One worker-side canonical operation: the QUEUE wait (submit to
/// dequeue) distinguished from the PROVIDER operation time
/// (the worker's execution).
#[derive(Debug, Clone)]
struct WorkerRecord {
    op: &'static str,
    queue_ms: f64,
    op_ms: f64,
}

const STEP_CAP: usize = 100_000;
const TICK_CAP: usize = 10_000;
const WORKER_CAP: usize = 100_000;

fn steps() -> &'static Mutex<Vec<StepRecord>> {
    static STEPS: OnceLock<Mutex<Vec<StepRecord>>> = OnceLock::new();
    STEPS.get_or_init(|| Mutex::new(Vec::new()))
}

fn ticks() -> &'static Mutex<Vec<TickRecord>> {
    static TICKS: OnceLock<Mutex<Vec<TickRecord>>> = OnceLock::new();
    TICKS.get_or_init(|| Mutex::new(Vec::new()))
}

fn worker_ops() -> &'static Mutex<Vec<WorkerRecord>> {
    static WORKER: OnceLock<Mutex<Vec<WorkerRecord>>> = OnceLock::new();
    WORKER.get_or_init(|| Mutex::new(Vec::new()))
}

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Enables the collector (the CLI flag; once enabled it stays on
/// for the process — a diagnostic run is a whole run).
pub fn enable() {
    ENABLED.store(true, Ordering::SeqCst);
}

/// Whether the collector is on.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// Records one step's wall duration (a no-op when disabled).
fn record_step(step: &'static str, class: StepClass, region: usize, tick: u64, start: Instant) {
    if !enabled() {
        return;
    }
    let record = StepRecord {
        step,
        class,
        region,
        tick,
        wall_ms: start.elapsed().as_secs_f64() * 1000.0,
    };
    if let Ok(mut steps) = steps().lock()
        && steps.len() < STEP_CAP
    {
        steps.push(record);
    }
}

/// Records one logical tick's crossing aggregate.
pub fn record_tick(tick: u64, transitions: usize, crossing_ms: f64) {
    if !enabled() {
        return;
    }
    if let Ok(mut ticks) = ticks().lock()
        && ticks.len() < TICK_CAP
    {
        ticks.push(TickRecord {
            tick,
            transitions,
            crossing_ms,
        });
    }
}

/// Records one worker-side canonical operation's split.
pub fn record_worker_op(op: &'static str, queue_ms: f64, op_ms: f64) {
    if !enabled() {
        return;
    }
    if let Ok(mut ops) = worker_ops().lock()
        && ops.len() < WORKER_CAP
    {
        ops.push(WorkerRecord {
            op,
            queue_ms,
            op_ms,
        });
    }
}

/// The compute-class mark helper (CPU-bound work).
pub fn compute(step: &'static str, region: usize, tick: u64, start: Instant) {
    record_step(step, StepClass::Compute, region, tick, start);
}

/// The I/O-wait-class mark helper (provider waits included).
pub fn io_wait(step: &'static str, region: usize, tick: u64, start: Instant) {
    record_step(step, StepClass::IoWait, region, tick, start);
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() as f64 - 1.0) * fraction).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

/// The profiling summary (empty when nothing was recorded): the
/// per-step aggregates (count · sum · mean · p50 · p95 · max, the
/// class label separating wall-time I/O waits from compute), the
/// worst logical ticks by summed crossing time with their
/// transition counts, and the worker-side queue-vs-provider split.
pub fn summary() -> String {
    let mut out = String::new();
    let steps = steps()
        .lock()
        .map(|steps| steps.clone())
        .unwrap_or_default();
    if steps.is_empty() {
        out.push_str("crossing-profile: no steps recorded\n");
        return out;
    }
    out.push_str("--- crossing profile (wall ms; class separates I/O waits from compute) ---\n");
    let mut step_names: Vec<&'static str> = steps.iter().map(|record| record.step).collect();
    step_names.sort_unstable();
    step_names.dedup();
    for name in step_names {
        let matching: Vec<&StepRecord> =
            steps.iter().filter(|record| record.step == name).collect();
        let mut durations: Vec<f64> = matching.iter().map(|record| record.wall_ms).collect();
        durations.sort_by(|a, b| a.total_cmp(b));
        let sum: f64 = durations.iter().sum();
        let class = match matching[0].class {
            StepClass::Compute => "compute",
            StepClass::IoWait => "io-wait",
        };
        let worst = matching
            .iter()
            .max_by(|a, b| a.wall_ms.total_cmp(&b.wall_ms))
            .expect("nonempty");
        out.push_str(&format!(
            "  {name} [{class}]: n {} · sum {:.3} · mean {:.3} · p50 {:.3} · p95 {:.3} · max {:.3} ms (worst at tick {} region {})\n",
            durations.len(),
            sum,
            sum / durations.len() as f64,
            percentile(&durations, 0.50),
            percentile(&durations, 0.95),
            durations[durations.len() - 1],
            worst.tick,
            worst.region,
        ));
    }
    let ticks = ticks()
        .lock()
        .map(|ticks| ticks.clone())
        .unwrap_or_default();
    if !ticks.is_empty() {
        let mut worst = ticks.clone();
        worst.sort_by(|a, b| b.crossing_ms.total_cmp(&a.crossing_ms));
        worst.truncate(8);
        out.push_str("--- worst logical ticks by summed crossing wall time ---\n");
        for record in &worst {
            out.push_str(&format!(
                "  tick {}: {} transition(s) · {:.3} ms summed crossings\n",
                record.tick, record.transitions, record.crossing_ms
            ));
        }
    }
    let ops = worker_ops()
        .lock()
        .map(|ops| ops.clone())
        .unwrap_or_default();
    if !ops.is_empty() {
        let mut names: Vec<&'static str> = ops.iter().map(|record| record.op).collect();
        names.sort_unstable();
        names.dedup();
        out.push_str("--- canonical worker split (queue wait vs provider op, wall ms) ---\n");
        for name in names {
            let matching: Vec<&WorkerRecord> =
                ops.iter().filter(|record| record.op == name).collect();
            let mut queues: Vec<f64> = matching.iter().map(|record| record.queue_ms).collect();
            let mut ops_durations: Vec<f64> = matching.iter().map(|record| record.op_ms).collect();
            queues.sort_by(|a, b| a.total_cmp(b));
            ops_durations.sort_by(|a, b| a.total_cmp(b));
            out.push_str(&format!(
                "  {name}: n {} · queue p50 {:.3} / p95 {:.3} / max {:.3} · op p50 {:.3} / p95 {:.3} / max {:.3} ms\n",
                matching.len(),
                percentile(&queues, 0.50),
                percentile(&queues, 0.95),
                queues[queues.len() - 1],
                percentile(&ops_durations, 0.50),
                percentile(&ops_durations, 0.95),
                ops_durations[ops_durations.len() - 1],
            ));
        }
    }
    out
}
