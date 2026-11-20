//! Zero-allocation gate for the spine's deterministic main path.
//!
//! The spine's per-tick execution path (`schedule`, `drain_due`,
//! `execute_now`, follow-up scheduling, waiter release) must perform
//! no heap allocation beyond the PINNED structural costs of its
//! canonical containers (queue pushes, ledger growth, index nodes).
//! Two disciplines make that true:
//!
//! 1. Diagnostics must never allocate when their record is
//!    suppressed: the eager `format!` construction at every hot-path
//!    `diag.log` site is guarded by `Diagnostics::would_log`, so a
//!    `Level::Off` spine pays nothing (the message construction, not
//!    just the record store, is threshold-gated).
//! 2. The handler follow-up buffer is a reused scratch, not a fresh
//!    `Vec` per executed event (`Spine`'s `scratch_follow_ups`, the
//!    F-02 caller-held-scratch discipline applied to the spine).
//!
//! The counting global allocator is process-wide, so this file is its
//! own integration-test binary carrying EXACTLY ONE test — nothing
//! else competes for the counter. The pinned count is a measured
//! actual, recorded when the gate was introduced (fail-first: before
//! the allocation-elimination change it was higher — one diagnostic
//! `String` per scheduled event, one per executed event, and one
//! follow-up `Vec` per execution reached the allocator). A count move
//! is an investigation, never a silent repin — the same rule as the
//! fingerprint pins.
//!
//! The double-run equality inside the test mirrors the repo-wide
//! determinism discipline: the allocation sequence itself must be
//! deterministic, or the pin is meaningless.

// SAFETY EXCEPTION (the unsafe-with-proofs policy): this file is the
// repo's only `unsafe` outside vendored dependencies, and it exists
// only in a test target. The `GlobalAlloc` implementation is a pure
// forwarder — every method passes its arguments unchanged to the
// system allocator and adds only a relaxed atomic increment, which
// is lock-free, allocation-free, and cannot reenter the allocator.
// No production code path can reach this implementation.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use engine_core::{
    Diagnostics, Event, EventClass, EventSpec, ExecCtx, Field, Fingerprinter, Level, Schedule,
    Spine, Tick, World,
};

/// Process-wide allocation-event counter (alloc, alloc_zeroed, realloc).
static ALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Forwarding allocator that only counts allocation events.
///
/// The counter increment is atomic and lock-free; the allocator itself
/// performs no allocation, so forwarding to the system allocator
/// preserves every guarantee of the global allocator contract.
struct CountingAllocator;

// SAFETY: every method forwards its arguments unchanged to the system
// allocator (which satisfies the global allocator contract), adding
// only a relaxed atomic increment that cannot reenter allocation.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
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

/// The measured allocation-event count for the scripted scenario below
/// at the gate's introduction (Windows and Linux agree — the scenario
/// touches only std containers with platform-uniform growth).
///
/// MEASUREMENT RECORD (fail-first discipline): before the
/// allocation-elimination change this same scenario measured **52** —
/// one diagnostic `String` per scheduled event, one per executed
/// event, and one fresh follow-up `Vec` per execution reached the
/// allocator with `Level::Off` diagnostics; the change (the
/// `would_log` construction guards, the reused follow-up scratch, and
/// the allocation-free unmet-dependency paths) brought it to the
/// eight structural costs below, with every pinned fingerprint and
/// scenario trace byte-identical.
///
/// Included structural costs (per the scenario: 4 four-deep execution
/// chains with follow-up delay 2, plus one causality-blocked event
/// released through the waiter path, run over 40 ticks):
/// near/far-queue heap pushes, `unfinished` index nodes, ledger
/// growth, `waiting_on` map nodes, and the first scratch capacity
/// acquisition for the follow-up buffer (retained and reused
/// thereafter).
const PINNED_MAIN_PATH_ALLOCATIONS: usize = 8;

/// A minimal deterministic `World`: counts executions, chains each
/// execution into one follow-up (the adversary-wake shape — exactly
/// one follow-up per executed event) until the payload-carried chain
/// depth is exhausted.
struct ChainWorld {
    executions: u64,
}

impl World for ChainWorld {
    fn execute(&mut self, event: &Event, ctx: &mut ExecCtx) {
        self.executions += 1;
        if event.payload > 0 {
            ctx.follow_up(2, event.class, event.payload - 1, Vec::new());
        }
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        fp.write_str("spine-allocations/chain-world/1");
        fp.write_u64(self.executions);
    }
}

/// Runs the scripted scenario and returns the number of allocation
/// events the execution main path took: scheduling the blocked event
/// (the last setup allocation is deliberately outside the counted
/// window) through the final tick, covering queue drains, executions,
/// follow-up scheduling, the blocked park, and the waiter release.
fn run_scenario() -> usize {
    let world = ChainWorld { executions: 0 };
    let mut spine = Spine::with_diagnostics(world, 200, Diagnostics::new(Level::Off));
    // Four chain roots, each four executions deep (payload counts down
    // to zero), due at the first tick; the fourth rides the far lane.
    let roots: Vec<EventSpec> = (0..4u64)
        .map(|index| {
            EventSpec::new(
                EventClass::new(1),
                Schedule::AtLogicalTime {
                    due: Tick::ZERO.saturating_add(1),
                },
                3,
            )
            .in_field(if index == 3 { Field::Far } else { Field::Near })
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
    // One causality-blocked event: near-lane, due at the first tick,
    // depending on the far-lane root — the near lane drains first, so
    // it parks in the blocked set and is released through the waiter
    // path when the far-lane root executes later in the same tick.
    let blocked_spec = EventSpec::new(
        EventClass::new(2),
        Schedule::AtLogicalTime {
            due: Tick::ZERO.saturating_add(1),
        },
        0,
    )
    .with_dependencies(vec![ids[3]]);

    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    spine
        .schedule(blocked_spec)
        .expect("blocked scheduling is valid");
    spine.run_ticks(40);
    ALLOCATION_COUNT.load(Ordering::Relaxed)
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
        first, PINNED_MAIN_PATH_ALLOCATIONS,
        "the spine main-path allocation count moved: an investigation, never a \
         silent repin (a new per-tick allocation is the usual cause — check \
         diagnostics guards and scratch reuse before re-measuring)"
    );
}
