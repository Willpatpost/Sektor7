//! Stage 3 near/far lane contracts (reviewer clarification 2026-10-20):
//! the §2.4 three-phase budget policy — the near reservation as a
//! guaranteed allocation, not a ceiling — plus lane determinism and the
//! lane-aware fingerprint domain. Engine-mechanism tests only; NPC
//! workload policy tests live in `engine-slice`.

use engine_core::{
    Event, EventClass, EventSpec, ExecCtx, Field, Fingerprinter, Schedule, Spine, Tick, World,
};

/// Minimal counting world: records payload sequence in execution order.
/// No follow-ups — these tests exercise the lane mechanisms only.
struct CountingWorld {
    executed: Vec<u64>,
}

impl CountingWorld {
    fn new() -> Self {
        CountingWorld {
            executed: Vec::new(),
        }
    }
}

impl World for CountingWorld {
    fn execute(&mut self, event: &Event, _ctx: &mut ExecCtx) {
        self.executed.push(event.payload);
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        fp.write_str("engine-core-lanes-test/world/1");
        fp.write_usize(self.executed.len());
        for payload in &self.executed {
            fp.write_u64(*payload);
        }
    }
}

fn at(field: Field, due: u64, payload: u64) -> EventSpec {
    EventSpec::new(
        EventClass::new(0),
        Schedule::AtLogicalTime { due: Tick(due) },
        payload,
    )
    .in_field(field)
}

/// The reviewer's example: budget 100, reservation 30, 50 due near + 10
/// due far — all 60 execute. Near events must not stay backlogged merely
/// because they exceeded the reserve while unused budget remains.
#[test]
fn three_phase_redistribution_executes_all_available_work() {
    let mut spine = Spine::with_reservation(CountingWorld::new(), 100, 30);
    for payload in 0..50 {
        spine
            .schedule(at(Field::Near, 1, payload))
            .expect("schedule");
    }
    for payload in 50..60 {
        spine
            .schedule(at(Field::Far, 1, payload))
            .expect("schedule");
    }
    spine.run_tick();
    assert_eq!(spine.stats().executed, 60, "all available work runs");
    assert_eq!(spine.pending_count(), 0);
    let report = spine.lane_report();
    assert_eq!(report.near_executed, 50);
    assert_eq!(report.far_executed, 10);
    assert!(!report.near_backlogged);
    assert!(!report.far_backlogged);
}

/// Near saturation: near demand beyond the total budget backlogs — but
/// only at total exhaustion, and nothing is ever dropped.
#[test]
fn near_saturation_backlogs_only_at_total_exhaustion() {
    let mut spine = Spine::with_reservation(CountingWorld::new(), 20, 5);
    for payload in 0..100 {
        spine
            .schedule(at(Field::Near, 1, payload))
            .expect("schedule");
    }
    spine.run_tick();
    let report = spine.lane_report();
    assert_eq!(report.near_executed, 20, "guarantee 5 + 15 redistributed");
    assert_eq!(spine.stats().executed, 20);
    assert_eq!(spine.near_pending_count(), 80);
    assert!(report.near_backlogged);
    assert_eq!(spine.stats().budget_exhausted_ticks, 1);
    spine.run_to_completion(1000);
    assert_eq!(spine.stats().executed, 100, "every event eventually runs");
    assert_eq!(spine.stats().scheduled, 100, "nothing silently dropped");
}

/// Far saturation with idle near capacity: far work uses the genuinely
/// unused near capacity (phase 3 after a drained near lane).
#[test]
fn far_work_uses_genuinely_unused_near_capacity() {
    let mut spine = Spine::with_reservation(CountingWorld::new(), 20, 5);
    for payload in 0..100 {
        spine
            .schedule(at(Field::Far, 1, payload))
            .expect("schedule");
    }
    spine.run_tick();
    let report = spine.lane_report();
    assert_eq!(report.far_executed, 20, "nonreserved 15 + leftover 5");
    assert_eq!(spine.stats().executed, 20);
    assert!(report.far_backlogged);
    spine.run_to_completion(1000);
    assert_eq!(spine.stats().executed, 100);
}

/// Mutual saturation: both lanes beyond the budget — the guarantee
/// splits exactly (near 5, far 15 of budget 20), the reservation holding
/// under far pressure (E-10 anti-starvation) and the nonreserved share
/// holding under near pressure.
#[test]
fn mutual_saturation_splits_by_guarantee() {
    let mut spine = Spine::with_reservation(CountingWorld::new(), 20, 5);
    for payload in 0..100 {
        spine
            .schedule(at(Field::Near, 1, payload))
            .expect("schedule");
    }
    for payload in 100..200 {
        spine
            .schedule(at(Field::Far, 1, payload))
            .expect("schedule");
    }
    spine.run_tick();
    let report = spine.lane_report();
    assert_eq!(report.near_executed, 5, "near guarantee holds under load");
    assert_eq!(
        report.far_executed, 15,
        "nonreserved share holds under load"
    );
    assert!(report.near_backlogged);
    assert!(report.far_backlogged);
    spine.run_to_completion(2000);
    assert_eq!(spine.stats().executed, 200);
}

/// Causality spans lanes: a near event depending on a far prerequisite
/// parks without consuming budget, and the far execution releases it
/// within the same tick's later phases.
#[test]
fn causal_blocking_spans_lanes_without_budget_consumption() {
    let mut spine = Spine::with_reservation(CountingWorld::new(), 10, 5);
    let prerequisite = spine.schedule(at(Field::Far, 1, 1)).expect("schedule");
    spine
        .schedule(
            EventSpec::new(
                EventClass::new(0),
                Schedule::AtLogicalTime { due: Tick(1) },
                2,
            )
            .in_field(Field::Near)
            .with_dependencies(vec![prerequisite]),
        )
        .expect("schedule");
    spine.run_tick();
    assert_eq!(spine.stats().executed, 2, "both events run this tick");
    assert_eq!(spine.stats().blocked_transitions, 1);
    assert_eq!(spine.blocked_count(), 0, "the waiter was released");
    assert_eq!(spine.stats().invalid_dropped, 0);
}

/// Lane behavior is deterministic: identical configuration and schedule
/// produce identical lane-domain fingerprints across fresh instances.
#[test]
fn lane_determinism_double_run() {
    fn build() -> Spine<CountingWorld> {
        let mut spine = Spine::with_reservation(CountingWorld::new(), 10, 3);
        for payload in 0..40 {
            let field = if payload % 4 == 0 {
                Field::Near
            } else {
                Field::Far
            };
            spine
                .schedule(at(field, 1 + payload % 5, payload))
                .expect("schedule");
        }
        spine.run_ticks(30);
        spine
    }
    let first = build();
    let second = build();
    assert_eq!(
        first.lane_state_fingerprint(),
        second.lane_state_fingerprint()
    );
    assert_eq!(
        first.lane_state_fingerprint_hex(),
        second.lane_state_fingerprint_hex()
    );
}

/// The 200-event synchrony cluster (acceptance requirement 7): all due on
/// one tick, processed across ticks with lateness recorded, drains
/// completely with zero drops under an ongoing budget.
#[test]
fn synchrony_burst_of_200_drains_with_lateness_zero_drops() {
    let mut spine = Spine::with_reservation(CountingWorld::new(), 50, 20);
    for payload in 0..200 {
        spine
            .schedule(at(Field::Near, 1, payload))
            .expect("schedule");
    }
    spine.run_tick();
    assert_eq!(spine.stats().executed, 50);
    assert_eq!(
        spine.stats().late_executions,
        0,
        "tick-1 executions are on time"
    );
    spine.run_to_completion(1000);
    assert_eq!(spine.stats().executed, 200, "the burst drains completely");
    assert_eq!(spine.stats().scheduled, 200, "zero dropped");
    assert_eq!(
        spine.stats().late_executions,
        150,
        "deferral recorded as lateness"
    );
    assert_eq!(spine.ledger().len(), 200);
    assert!(
        spine
            .ledger()
            .iter()
            .filter(|record| record.late)
            .all(|record| record.executed_tick.as_u64() > record.intended_due.as_u64())
    );
}

/// The lane-domain fingerprint meaningfully represents the new
/// authoritative state: different reservations and different lane
/// classifications produce different fingerprints (reviewer
/// clarification 3B), while identical configurations agree.
#[test]
fn lane_fingerprint_distinguishes_meaningful_configuration() {
    fn built(reserve: u32, near_first: bool) -> Spine<CountingWorld> {
        let mut spine = Spine::with_reservation(CountingWorld::new(), 10, reserve);
        for payload in 0..8u64 {
            let field = if (payload % 2 == 0) == near_first {
                Field::Near
            } else {
                Field::Far
            };
            spine
                .schedule(at(field, 1 + payload % 3, payload))
                .expect("schedule");
        }
        spine.run_ticks(10);
        spine
    }
    let base = built(3, true);
    let repeat = built(3, true);
    assert_eq!(base.lane_fingerprint(), repeat.lane_fingerprint());
    let other_reserve = built(4, true);
    assert_ne!(
        base.lane_fingerprint(),
        other_reserve.lane_fingerprint(),
        "reservation is authoritative state"
    );
    let other_classification = built(3, false);
    assert_ne!(
        base.lane_fingerprint(),
        other_classification.lane_fingerprint(),
        "lane classification is authoritative state"
    );
}

/// Legacy single-lane construction (reservation == budget, all events
/// Near by default) keeps the Stage 0 domain byte-stable across runs;
/// the historical hash equality itself is proven by engine-slice's
/// selftest, which pins the exact values.
#[test]
fn legacy_single_lane_is_deterministic_in_the_stage0_domain() {
    fn build() -> Spine<CountingWorld> {
        let mut spine = Spine::new(CountingWorld::new(), 8);
        for (due, payload) in [(3u64, 10u64), (3, 21), (5, 15), (9, 30), (12, 7)] {
            spine
                .schedule(at(Field::Near, due, payload))
                .expect("schedule");
        }
        spine.run_to_completion(500);
        spine
    }
    let first = build();
    let second = build();
    assert_eq!(first.engine_fingerprint(), second.engine_fingerprint());
    assert_eq!(first.state_fingerprint(), second.state_fingerprint());
    assert_eq!(first.stats().executed, first.stats().scheduled);
}
