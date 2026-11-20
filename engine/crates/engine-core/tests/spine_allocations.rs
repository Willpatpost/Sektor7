//! Zero-allocation gate for the spine's deterministic main path.
//!
//! The spine's per-tick execution path (`schedule`, `drain_due`,
//! `execute_now`, follow-up scheduling, waiter release) must perform
//! no heap allocation beyond the PINNED structural costs of its
//! canonical containers (queue pushes, ledger growth, index nodes).
//! The disciplines that make that true:
//!
//! 1. Diagnostics must never allocate when their record is
//!    suppressed: the eager `format!` construction at every hot-path
//!    `diag.log` site is guarded by `Diagnostics::would_log`, so a
//!    `Level::Off` spine pays nothing (the message construction, not
//!    just the record store, is threshold-gated).
//! 2. The handler follow-up buffer is a reused scratch, not a fresh
//!    `Vec` per executed event (`Spine`'s `scratch_follow_ups`, the
//!    F-02 caller-held-scratch discipline applied to the spine).
//! 3. A causality-blocked event that is only PARTIALLY released (some
//!    dependencies executed, others not) is re-registered in place
//!    and never leaves the blocked map — a B-tree remove+reinsert
//!    could deallocate and reallocate nodes per partial release (the
//!    independent review's P1 correction; the scenario below
//!    exercises exactly that path through a two-dependency event
//!    whose dependencies resolve four ticks apart).
//!
//! Both allocation EVENTS and allocation BYTES are pinned: the event
//! count alone could hide larger allocations behind an unchanged
//! count.
//!
//! The counting global allocator is process-wide, so this file is its
//! own integration-test binary carrying EXACTLY ONE test — nothing
//! else competes for the counters. The pinned values are measured
//! actuals (fail-first: before the allocation-elimination change the
//! same cold window measured 52 events — one diagnostic `String` per
//! scheduled event, one per executed event, one follow-up `Vec` per
//! execution); the values are measured on Windows and confirmed on
//! Linux/macOS by the CI run at this SHA (this branch carries its own
//! workflow trigger for that purpose). A pin move is an
//! investigation, never a silent repin — the same rule as the
//! fingerprint pins.
//!
//! The cold window measures a fresh spine (scratch capacities
//! acquired); the warm window measures an identical second wave on
//! the SAME spine — steady state, with every scratch capacity
//! retained. Structural per-event storage (unfinished-index nodes,
//! queue growth, ledger growth) still allocates by design; the warm
//! pin proves no overhead BEYOND that structural cost.
//!
//! The double-run equality inside the test mirrors the repo-wide
//! determinism discipline: the allocation sequence itself must be
//! deterministic, or the pins are meaningless.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use engine_core::{
    Diagnostics, Event, EventClass, EventSpec, ExecCtx, Field, Fingerprinter, Level, Schedule,
    Spine, Tick, World,
};

/// Process-wide allocation-event and allocation-byte counters.
static ALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);
static ALLOCATION_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Forwarding allocator that only counts allocation events and the
/// requested byte sizes. The counter increments are atomic and
/// lock-free; the allocator itself performs no allocation, so
/// forwarding to the system allocator preserves every guarantee of
/// the global allocator contract.
///
/// SAFETY EXCEPTION (the unsafe-with-proofs policy): engine-core's
/// first `unsafe` exception — the Vulkan backend in engine-renderer
/// already carries intentionally isolated unsafe; this one is confined
/// to a test target, and the allowance is scoped to the allocator
/// implementation below so the rest of the file keeps the workspace's
/// ordinary `unsafe_code = "deny"` restriction.
struct CountingAllocator;

#[allow(unsafe_code)]
// SAFETY: every method forwards its arguments unchanged to the system
// allocator (which satisfies the global allocator contract), adding
// only relaxed atomic increments that cannot reenter allocation.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOCATION_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOCATION_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOCATION_BYTES.fetch_add(new_size, Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// The measured allocation events (count) and requested bytes for the
/// COLD window: scheduling and running one full wave on a fresh spine
/// (see `run_scenario` for the wave's shape). MEASUREMENT RECORD
/// (fail-first discipline): before the allocation-elimination change
/// this window measured 52 events — one diagnostic `String` per
/// scheduled event, one per executed event, and one fresh follow-up
/// `Vec` per execution reached the allocator with `Level::Off`
/// diagnostics. Windows-measured; Linux/macOS confirmed by the CI run
/// at this SHA.
const PINNED_COLD: (usize, usize) = (23, 7640);

/// The measured allocation events and bytes for the WARM window: an
/// identical second wave on the same spine, every scratch capacity
/// retained — the steady-state cost. Structural per-event storage
/// (index nodes, ledger growth) still allocates by design; anything
/// beyond the pinned value is overhead.
const PINNED_WARM: (usize, usize) = (8, 2720);

const CHAIN_CLASS: EventClass = EventClass::new(1);
const FANOUT_CLASS: EventClass = EventClass::new(3);

/// A minimal deterministic `World` with DIFFERING follow-up counts:
/// fanout events emit three follow-ups, chain events emit one until
/// the payload-carried depth is exhausted, terminal events emit none.
struct ScenarioWorld {
    executions: u64,
}

impl World for ScenarioWorld {
    fn execute(&mut self, event: &Event, ctx: &mut ExecCtx) {
        self.executions += 1;
        if event.class == FANOUT_CLASS {
            for _ in 0..3 {
                ctx.follow_up(2, CHAIN_CLASS, 0, Vec::new());
            }
        } else if event.payload > 0 {
            ctx.follow_up(2, event.class, event.payload - 1, Vec::new());
        }
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        fp.write_str("spine-allocations/scenario-world/2");
        fp.write_u64(self.executions);
    }
}

fn due_at(tick: u64) -> Schedule {
    Schedule::AtLogicalTime {
        due: Tick::ZERO.saturating_add(tick),
    }
}

/// Schedules one full wave on `spine`: four four-deep execution chains
/// (one per lane mix), two three-way fanouts, and one
/// causality-blocked near-lane event with TWO dependencies resolving
/// four ticks apart — so it parks, is PARTIALLY released (in-place
/// re-registration, the review's P1 path), and is fully released
/// later.
fn schedule_wave(spine: &mut Spine<ScenarioWorld>) {
    let roots: Vec<EventSpec> = (0..4u64)
        .map(|index| {
            EventSpec::new(CHAIN_CLASS, due_at(1), 3).in_field(if index == 3 {
                Field::Far
            } else {
                Field::Near
            })
        })
        .collect();
    let ids: Vec<engine_core::EventId> = roots
        .iter()
        .map(|root| {
            spine
                .schedule(root.clone())
                .expect("root scheduling is valid")
        })
        .collect();
    // The blocked event's two dependencies: far-lane, resolving at
    // ticks 1 and 5 respectively — partial release at tick 1, full
    // release at tick 5.
    let dep_early = spine
        .schedule(EventSpec::new(CHAIN_CLASS, due_at(1), 0).in_field(Field::Far))
        .expect("dependency scheduling is valid");
    let dep_late = spine
        .schedule(EventSpec::new(CHAIN_CLASS, due_at(5), 0).in_field(Field::Far))
        .expect("dependency scheduling is valid");
    for _ in 0..2 {
        spine
            .schedule(EventSpec::new(FANOUT_CLASS, due_at(1), 0))
            .expect("fanout scheduling is valid");
    }
    spine
        .schedule(
            EventSpec::new(EventClass::new(2), due_at(1), 0)
                .with_dependencies(vec![dep_early, dep_late]),
        )
        .expect("blocked scheduling is valid");
    // Touch the root ids so the wave shape is self-documenting.
    let _ = ids;
}

/// Runs one wave inside a counted window and returns
/// `(allocation events, allocation bytes)` for it.
fn run_wave(spine: &mut Spine<ScenarioWorld>) -> (usize, usize) {
    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    ALLOCATION_BYTES.store(0, Ordering::Relaxed);
    schedule_wave(spine);
    spine.run_ticks(40);
    (
        ALLOCATION_COUNT.load(Ordering::Relaxed),
        ALLOCATION_BYTES.load(Ordering::Relaxed),
    )
}

fn run_scenario() -> ((usize, usize), (usize, usize)) {
    let mut spine = Spine::with_diagnostics(
        ScenarioWorld { executions: 0 },
        200,
        Diagnostics::new(Level::Off),
    );
    let cold = run_wave(&mut spine);
    let warm = run_wave(&mut spine);
    (cold, warm)
}

#[test]
fn spine_main_path_allocations_are_pinned() {
    let first = run_scenario();
    let second = run_scenario();
    assert_eq!(
        first, second,
        "the allocation sequence must be deterministic across identical runs"
    );
    assert_eq!(
        first.0, PINNED_COLD,
        "the COLD-window allocation pin moved: an investigation, never a \
         silent repin (a new per-tick allocation is the usual cause — check \
         diagnostics guards, scratch reuse, and partial-release \
         re-registration before re-measuring)"
    );
    assert_eq!(
        first.1, PINNED_WARM,
        "the WARM-window allocation pin moved: steady-state cost beyond the \
         pinned structural storage is overhead — an investigation, never a \
         silent repin"
    );
}
