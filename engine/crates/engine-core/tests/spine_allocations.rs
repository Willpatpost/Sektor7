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
//! MEASUREMENT ATTRIBUTION (the review's second correction): every
//! allocation counted inside a window is ENGINE-side — the harness
//! constructs no `Vec` and no `String` inside a counted window (the
//! two dependency events are scheduled and the blocked event's
//! dependency list is built BEFORE the window opens, because the
//! list needs their ids; the world's bookkeeping is two plain
//! integers). The window covers the root/fanout/blocked scheduling
//! and all forty ticks of execution. Both allocation EVENTS and
//! allocation BYTES are pinned: the event count alone could hide
//! larger allocations behind an unchanged count.
//!
//! WAVE EQUIVALENCE (the review's first correction): every wave is
//! scheduled CLOCK-RELATIVE (offsets +1 and +5 from
//! `spine.clock()`), so the warm wave — an identical second wave on
//! the same spine, every scratch capacity retained — is a true
//! steady-state measurement, not a wave of late arrivals collapsing
//! into one tick. Each wave asserts its own semantics after the
//! window: zero late arrivals, exactly the per-wave execution count,
//! and the blocked event executing exactly at clock+5 (its late
//! dependency's tick — the partial release happened at clock+1 and
//! the full release four ticks later, never both in one tick).
//!
//! The counting global allocator is process-wide, so this file is its
//! own integration-test binary carrying EXACTLY ONE test — nothing
//! else competes for the counters. The pinned values are measured
//! actuals (Windows-measured; Linux and macOS ARM64 confirmed by the
//! CI run at this SHA — this branch carries its own workflow trigger
//! for that purpose). A pin move is an investigation, never a silent
//! repin — the same rule as the fingerprint pins. The double-run
//! equality inside the test mirrors the repo-wide determinism
//! discipline: the allocation sequence itself must be deterministic,
//! or the pins are meaningless.

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
/// (every counted allocation is engine-side — see the module docs'
/// attribution paragraph). Windows-measured; Linux/macOS confirmed by
/// the CI run at this SHA.
const PINNED_COLD: (usize, usize) = (18, 6880);

/// The measured allocation events and bytes for the WARM window: an
/// identical second wave on the same spine, scheduled clock-relative
/// (on time, dependencies four ticks apart), every scratch capacity
/// retained — the steady-state cost. Structural per-event storage
/// (index nodes, ledger growth) still allocates by design; anything
/// beyond the pinned value is overhead.
const PINNED_WARM: (usize, usize) = (9, 3024);

/// The SAME scenario and harness measured on the PRE-BATCH
/// implementation (main @ 30f8c13 with only this test file added):
/// the equivalent-workload baseline for the reduction claim — the
/// cold window dropped 87 -> 18 events and 12990 -> 6880 bytes. The
/// earlier 52-event figure was a SMALLER scenario (no fanout,
/// single-dependency blocked event) and is history, not a comparison.
const PRE_BATCH_COLD: (usize, usize) = (87, 12990);

/// Executions per wave: 4 four-deep chains (16) + 2 fanout events +
/// 6 fanout children + 2 dependency events + 1 blocked event.
const EXECUTIONS_PER_WAVE: u64 = 27;

const CHAIN_CLASS: EventClass = EventClass::new(1);
const FANOUT_CLASS: EventClass = EventClass::new(3);
const BLOCKED_CLASS: EventClass = EventClass::new(2);

/// A minimal deterministic `World` with DIFFERING follow-up counts:
/// fanout events emit three follow-ups, chain events emit one until
/// the payload-carried depth is exhausted, terminal events emit none.
/// Bookkeeping is two plain integers — the world allocates nothing.
struct ScenarioWorld {
    executions: u64,
    /// The logical tick at which the most recent blocked-class event
    /// executed (the partial-release timing assertion reads it).
    last_blocked_exec_tick: u64,
}

impl World for ScenarioWorld {
    fn execute(&mut self, event: &Event, ctx: &mut ExecCtx) {
        self.executions += 1;
        if event.class == BLOCKED_CLASS {
            self.last_blocked_exec_tick = ctx.executed_tick().as_u64();
        }
        if event.class == FANOUT_CLASS {
            for _ in 0..3 {
                ctx.follow_up(2, CHAIN_CLASS, 0, Vec::new());
            }
        } else if event.payload > 0 {
            ctx.follow_up(2, event.class, event.payload - 1, Vec::new());
        }
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        fp.write_str("spine-allocations/scenario-world/3");
        fp.write_u64(self.executions);
        fp.write_u64(self.last_blocked_exec_tick);
    }
}

fn due_offset(clock_before: u64, offset: u64) -> Schedule {
    Schedule::AtLogicalTime {
        due: Tick(clock_before).saturating_add(offset),
    }
}

/// Runs one wave and returns `(allocation events, allocation bytes)`
/// for its counted window. Wave shape: four four-deep execution chains
/// (one per lane mix), two three-way fanouts, and one
/// causality-blocked near-lane event with TWO dependencies resolving
/// four ticks apart — it parks, is PARTIALLY released (in-place
/// re-registration, the review's P1 path), and is fully released four
/// ticks after the partial release.
///
/// The two dependency events are scheduled and the blocked event's
/// dependency list is built BEFORE the window opens (the list needs
/// their ids) — no harness allocation occurs inside the window.
fn run_wave(spine: &mut Spine<ScenarioWorld>) -> (usize, usize) {
    let clock_before = spine.clock().as_u64();
    let late_before = spine.stats().late_arrivals;
    let executions_before = spine.world().executions;
    let dep_early = spine
        .schedule(EventSpec::new(CHAIN_CLASS, due_offset(clock_before, 1), 0).in_field(Field::Far))
        .expect("dependency scheduling is valid");
    let dep_late = spine
        .schedule(EventSpec::new(CHAIN_CLASS, due_offset(clock_before, 5), 0).in_field(Field::Far))
        .expect("dependency scheduling is valid");
    let blocked_spec = EventSpec::new(BLOCKED_CLASS, due_offset(clock_before, 1), 0)
        .with_dependencies(vec![dep_early, dep_late]);

    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    ALLOCATION_BYTES.store(0, Ordering::Relaxed);
    for index in 0..4u64 {
        spine
            .schedule(
                EventSpec::new(CHAIN_CLASS, due_offset(clock_before, 1), 3)
                    .in_field(if index == 3 { Field::Far } else { Field::Near }),
            )
            .expect("root scheduling is valid");
    }
    for _ in 0..2 {
        spine
            .schedule(EventSpec::new(FANOUT_CLASS, due_offset(clock_before, 1), 0))
            .expect("fanout scheduling is valid");
    }
    spine
        .schedule(blocked_spec)
        .expect("blocked scheduling is valid");
    spine.run_ticks(40);
    let measured = (
        ALLOCATION_COUNT.load(Ordering::Relaxed),
        ALLOCATION_BYTES.load(Ordering::Relaxed),
    );

    // Wave-semantics assertions (integer comparisons — no allocation
    // on the success path, and the window is already closed).
    assert_eq!(
        spine.stats().late_arrivals,
        late_before,
        "a clock-relative wave must produce zero late arrivals (the \
         warm wave is an equivalent second wave, not a past-due one)"
    );
    assert_eq!(
        spine.world().executions - executions_before,
        EXECUTIONS_PER_WAVE,
        "every wave must execute exactly the same work"
    );
    assert_eq!(
        spine.world().last_blocked_exec_tick,
        clock_before + 5,
        "partial-release timing: partial release at clock+1 (the early \
         dependency), full release at clock+5 (the late dependency, four \
         ticks apart), and the blocked event executes in that same tick's \
         near redistribution phase — never with both dependencies \
         resolving in one tick"
    );
    measured
}

fn run_scenario() -> ((usize, usize), (usize, usize)) {
    // The representative lane reservation (budget 200 / near reserve
    // 60, the sustained-load benchmark's configuration): the far lane
    // drains in its guaranteed phase BEFORE near redistribution, so a
    // blocked event released by a far-lane dependency executes in the
    // same tick's redistribution phase — the timing the assertion
    // below pins. (Under the legacy single-reserve constructor the
    // far lane runs only in the redistribution phase, and a release
    // lands one tick later — a lane-phasing property, identical on
    // every implementation.)
    let mut spine = Spine::with_reservation_and_diagnostics(
        ScenarioWorld {
            executions: 0,
            last_blocked_exec_tick: 0,
        },
        200,
        60,
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
    assert!(
        first.0.0 < PRE_BATCH_COLD.0 && first.0.1 < PRE_BATCH_COLD.1,
        "the cold window {:?} must measure below the pre-batch baseline \
         {:?} for this same scenario",
        first.0,
        PRE_BATCH_COLD
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
