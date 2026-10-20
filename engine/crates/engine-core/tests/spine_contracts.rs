//! Contract tests for the spine semantics the 2026-10-19 review required:
//! safe watermark vs max-executed, cross-class causal dependencies, budget
//! behavior, schedule-semantics distinction, and no silent drops.

use std::collections::BTreeMap;

use engine_core::{
    Event, EventClass, EventId, EventSpec, ExecCtx, Fingerprinter, Schedule, ScheduleError,
    ScheduleKind, Spine, Tick, World,
};

/// Minimal counting world: no follow-ups (unit-focused).
struct CountingWorld {
    counts: BTreeMap<u64, u64>,
}

impl World for CountingWorld {
    fn execute(&mut self, event: &Event, _ctx: &mut ExecCtx) {
        *self.counts.entry(event.payload).or_insert(0) += 1;
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        fp.write_str("engine-core-test/counting-world/2");
        fp.write_usize(self.counts.len());
        for (payload, count) in &self.counts {
            fp.write_u64(*payload);
            fp.write_u64(*count);
        }
    }
}

fn spine(budget: u32) -> Spine<CountingWorld> {
    Spine::new(
        CountingWorld {
            counts: BTreeMap::new(),
        },
        budget,
    )
}

fn at(due: u64, payload: u64) -> EventSpec {
    EventSpec::new(
        EventClass::new(0),
        Schedule::AtLogicalTime { due: Tick(due) },
        payload,
    )
}

#[test]
fn clock_advances_one_tick_per_run() {
    let mut spine = spine(4);
    assert_eq!(spine.clock(), Tick(0));
    spine.run_tick();
    assert_eq!(spine.clock(), Tick(1));
    spine.run_tick();
    assert_eq!(spine.clock(), Tick(2));
}

#[test]
fn in_class_same_due_executes_in_schedule_order() {
    let mut spine = spine(10);
    let first = spine.schedule(at(3, 10)).unwrap();
    let second = spine.schedule(at(3, 11)).unwrap();
    spine.run_ticks(3);
    let ledger = spine.ledger();
    assert_eq!(ledger.len(), 2);
    assert_eq!(ledger[0].id, first);
    assert_eq!(ledger[1].id, second);
    assert_eq!(ledger[0].executed_tick, Tick(3));
    assert_eq!(ledger[1].executed_tick, Tick(3));
}

#[test]
fn earlier_due_executes_first_even_if_scheduled_later() {
    let mut spine = spine(10);
    let later_scheduled = spine.schedule(at(5, 10)).unwrap();
    let earlier_due = spine.schedule(at(4, 11)).unwrap();
    spine.run_ticks(5);
    let ledger = spine.ledger();
    assert_eq!(ledger[0].id, earlier_due);
    assert_eq!(ledger[1].id, later_scheduled);
}

#[test]
fn watermark_is_max_executed_when_idle_and_never_reaches_clock() {
    let mut spine = spine(10);
    spine.schedule(at(3, 1)).unwrap();
    spine.run_ticks(10);
    assert_eq!(spine.clock(), Tick(10));
    assert_eq!(spine.max_executed_due(), Some(Tick(3)));
    assert_eq!(
        spine.processed_through(),
        Some(Tick(3)),
        "completeness may not be claimed beyond executed work even while idle — \
         a future late arrival may still carry an earlier due"
    );
}

#[test]
fn blocked_event_caps_watermark_until_dependency_executes() {
    let mut spine = spine(10);
    let dep = spine.schedule(at(8, 1)).unwrap();
    let dependent = spine
        .schedule(
            EventSpec::new(
                EventClass::new(1),
                Schedule::AtLogicalTime { due: Tick(6) },
                2,
            )
            .with_dependencies(vec![dep]),
        )
        .unwrap();
    spine.schedule(at(6, 3)).unwrap();

    spine.run_ticks(1);
    assert_eq!(spine.processed_through(), None, "nothing has executed yet");

    spine.run_ticks(5); // clock 6: the independent due-6 event runs; dependent parks
    assert_eq!(spine.blocked_count(), 1);
    assert_eq!(spine.max_executed_due(), Some(Tick(6)));
    assert_eq!(
        spine.processed_through(),
        Some(Tick(5)),
        "a causality-blocked event with due 6 must cap the watermark below its due"
    );

    spine.run_ticks(2); // clock 8: dep executes, dependent unblocks and runs late
    assert_eq!(spine.blocked_count(), 0);
    let ledger = spine.ledger();
    assert_eq!(ledger.len(), 3);
    let position = |id: EventId| ledger.iter().position(|r| r.id == id).unwrap();
    assert!(
        position(dep) < position(dependent),
        "dependency must execute before its dependent"
    );
    let dependent_record = ledger[position(dependent)];
    assert!(dependent_record.late);
    assert_eq!(dependent_record.executed_tick, Tick(8));
    assert_eq!(spine.processed_through(), Some(Tick(8)));
}

#[test]
fn late_arrival_retreats_watermark() {
    let mut spine = spine(10);
    spine.schedule(at(5, 1)).unwrap();
    spine.run_ticks(6); // executes at 5; clock 6
    assert_eq!(spine.processed_through(), Some(Tick(5)));

    spine.schedule(at(4, 2)).unwrap(); // late arrival
    assert_eq!(spine.stats().late_arrivals, 1);
    assert_eq!(
        spine.processed_through(),
        Some(Tick(3)),
        "a late arrival must retreat the safe watermark — overdue state never \
         silently appears more up-to-date than it actually is"
    );

    spine.run_to_completion(10);
    let record = spine
        .ledger()
        .iter()
        .find(|r| r.intended_due == Tick(4))
        .unwrap();
    assert_eq!(record.executed_tick, Tick(7));
    assert!(record.late);
    assert_eq!(spine.processed_through(), Some(Tick(5)));
}

#[test]
fn budget_leaves_due_work_queued_and_reported() {
    let mut spine = spine(2);
    for payload in 0..5 {
        spine.schedule(at(2, payload)).unwrap();
    }
    spine.run_ticks(2); // clock 2: two execute, three remain due
    assert_eq!(spine.stats().executed, 2);
    assert_eq!(spine.pending_count(), 3);
    assert_eq!(spine.stats().budget_exhausted_ticks, 1);
    assert_eq!(
        spine.processed_through(),
        Some(Tick(1)),
        "queued due events must cap the watermark"
    );

    spine.run_ticks(3); // clock 5: the rest execute late
    assert_eq!(spine.stats().executed, 5);
    assert_eq!(spine.stats().late_executions, 3);
    assert_eq!(spine.processed_through(), Some(Tick(2)));
}

#[test]
fn unknown_dependency_is_rejected_at_schedule_time() {
    let mut spine = spine(4);
    let ghost = EventId { seq: 999 };
    let error = spine
        .schedule(at(2, 1).with_dependencies(vec![ghost]))
        .unwrap_err();
    assert_eq!(error, ScheduleError::UnknownDependency(ghost));
    assert_eq!(
        spine.pending_count(),
        0,
        "rejected schedules leave no state"
    );
    assert_eq!(spine.stats().scheduled, 0);
}

#[test]
fn dependency_on_already_executed_event_does_not_block() {
    let mut spine = spine(4);
    let first = spine.schedule(at(2, 1)).unwrap();
    spine.run_ticks(2);
    let second = spine
        .schedule(
            EventSpec::new(
                EventClass::new(1),
                Schedule::AtLogicalTime { due: Tick(4) },
                2,
            )
            .with_dependencies(vec![first]),
        )
        .unwrap();
    spine.run_ticks(2);
    assert_eq!(spine.blocked_count(), 0);
    assert!(spine.ledger().iter().any(|r| r.id == second));
}

#[test]
fn follow_up_anchors_to_execution_not_intended_time() {
    /// Spawns one execution-relative follow-up (delay 3) for payload 0.
    struct ChainWorld;
    impl World for ChainWorld {
        fn execute(&mut self, event: &Event, ctx: &mut ExecCtx) {
            if event.payload == 0 {
                ctx.follow_up(3, event.class, 1, Vec::new());
            }
        }
        fn fingerprint(&self, _fp: &mut Fingerprinter) {}
    }

    let mut spine = Spine::new(ChainWorld, 1); // budget 1 forces one tick of lateness
    spine.schedule(at(2, 5)).unwrap(); // occupies tick 2's budget (seq 0)
    spine.schedule(at(2, 0)).unwrap(); // spawns the follow-up (seq 1)
    spine.run_ticks(2); // clock 2: filler executes on time
    spine.run_ticks(1); // clock 3: spawner executes LATE (intended 2, executed 3)
    spine.run_ticks(3); // clock 6: follow-up due 3+3=6 executes on time

    let follow_up = spine
        .ledger()
        .iter()
        .find(|r| r.schedule_kind == ScheduleKind::ExecutionRelative)
        .expect("follow-up must have executed");
    assert_eq!(
        follow_up.intended_due,
        Tick(6),
        "follow-up must anchor to the actual execution tick (3 + 3), not the \
         intended time (2 + 3 = 5)"
    );
    assert_eq!(follow_up.executed_tick, Tick(6));
    assert!(!follow_up.late);

    let cause = spine
        .ledger()
        .iter()
        .find(|r| r.schedule_kind == ScheduleKind::AtLogicalTime && r.late)
        .expect("the spawner executed late");
    assert_eq!(cause.executed_tick, Tick(3));
    assert_eq!(cause.intended_due, Tick(2));
}

#[test]
fn every_scheduled_event_eventually_executes() {
    let mut spine = spine(3);
    for i in 0..40u64 {
        spine
            .schedule(EventSpec::new(
                EventClass::new((i % 3) as u16),
                Schedule::AtLogicalTime {
                    due: Tick(1 + i % 7),
                },
                i,
            ))
            .unwrap();
    }
    // A descending-due chain: each event depends on the previously scheduled
    // one, whose due is later — every link parks in the blocked set first.
    let mut previous: Option<EventId> = None;
    for j in 0..10u64 {
        let spec = EventSpec::new(
            EventClass::new(7),
            Schedule::AtLogicalTime {
                due: Tick(20 - j * 2),
            },
            100 + j,
        );
        let spec = match previous {
            Some(dep) => spec.with_dependencies(vec![dep]),
            None => spec,
        };
        previous = Some(spine.schedule(spec).unwrap());
    }

    spine.run_to_completion(1000);

    let stats = spine.stats();
    assert_eq!(stats.scheduled, 50);
    assert_eq!(
        stats.executed, stats.scheduled,
        "no silent dropping: every scheduled event executes ({stats:?})"
    );
    assert_eq!(spine.pending_count() + spine.blocked_count(), 0);
    assert_eq!(stats.invalid_dropped, 0);
    assert!(
        stats.blocked_transitions > 0,
        "the chain must exercise blocking"
    );
}

#[test]
fn watermark_is_none_while_a_pending_event_sits_at_tick_zero() {
    // Regression (independent audit 2026-10-19, item 1): a pending event at
    // tick 0 leaves no non-negative tick whose completeness is provable, so
    // the safe watermark must be None. The previous saturating subtraction
    // wrongly returned Some(Tick(0)).
    let mut spine = spine(4);
    spine.schedule(at(0, 1)).unwrap();
    spine.run_ticks(1); // the tick-0 event executes at clock 1
    assert_eq!(spine.max_executed_due(), Some(Tick(0)));
    assert_eq!(spine.processed_through(), Some(Tick(0)));

    spine.schedule(at(0, 2)).unwrap(); // late arrival, pending at tick 0
    assert_eq!(
        spine.processed_through(),
        None,
        "no non-negative tick is provably complete while a tick-0 event is pending"
    );

    spine.run_to_completion(10);
    assert_eq!(spine.processed_through(), Some(Tick(0)));
}

#[test]
fn audit_counterexample_blocking_reorders_in_class_execution() {
    // Documents the clarified in-class contract (independent audit
    // 2026-10-19, item 2): (due, seq) is a queue-priority guarantee. A
    // causality-blocked event can execute after a later same-class sibling.
    let mut spine = spine(8);
    let prerequisite = spine.schedule(at(10, 1)).unwrap();
    let blocked = spine
        .schedule(at(5, 2).with_dependencies(vec![prerequisite]))
        .unwrap();
    let sibling = spine.schedule(at(6, 3)).unwrap();

    spine.run_to_completion(50);

    let ledger = spine.ledger();
    let position = |id: EventId| ledger.iter().position(|r| r.id == id).unwrap();
    assert!(
        position(sibling) < position(blocked),
        "the due-6 sibling executes before the blocked due-5 event: \
         (due, seq) dequeue priority does not survive causal blocking"
    );
    let blocked_record = ledger[position(blocked)];
    assert!(blocked_record.late);
    assert_eq!(blocked_record.executed_tick, Tick(10));
}

#[test]
fn audit_counterexample_late_arrival_reorders_in_class_execution() {
    // Documents the clarified in-class contract (independent audit
    // 2026-10-19, item 2): a late arrival executes after earlier-due
    // same-class events that already ran — by construction, not by fault.
    let mut spine = spine(8);
    let on_time = spine.schedule(at(5, 1)).unwrap();
    spine.run_ticks(5); // executes on time at tick 5

    let late_arrival = spine.schedule(at(3, 2)).unwrap(); // due 3, scheduled late
    spine.run_ticks(1); // clock 6: executes late

    let ledger = spine.ledger();
    let position = |id: EventId| ledger.iter().position(|r| r.id == id).unwrap();
    assert!(
        position(on_time) < position(late_arrival),
        "the earlier-due (3) late arrival executes after the due-5 event"
    );
    let late_record = ledger[position(late_arrival)];
    assert!(late_record.late);
    assert_eq!(late_record.intended_due, Tick(3));
    assert_eq!(late_record.executed_tick, Tick(6));
}

#[test]
fn max_executed_due_is_incremental_exact_and_query_bounded() {
    // Stage 4 final-verification finding: the graphical overlay queries
    // `max_executed_due()` — and `processed_through()`, which opens with
    // it — every frame, so a ledger scan would scale per-frame cost with
    // the total execution history. The O(1) incremental index must be
    // value-identical to the canonical scan, and the query cost must not
    // grow with the ledger (measured against a near-empty control — the
    // Batch-1 watermark-benchmark pattern).
    // The near-empty control for the relative timing bound — built
    // before the `spine` name is shadowed by the main spine below.
    let mut control = spine(1);
    control.run_tick();
    let mut spine = spine(150_000);
    for payload in 0..150_000u64 {
        spine
            .schedule(at(1 + payload % 4, payload))
            .expect("dependency-free burst");
    }
    spine.run_ticks(8);
    assert_eq!(
        spine.stats().executed,
        150_000,
        "the burst drained completely — real executions past any query guard"
    );
    // Value-identical to the canonical scan over the public ledger.
    let scan = spine
        .ledger()
        .iter()
        .map(|record| record.intended_due)
        .max();
    assert_eq!(spine.max_executed_due(), scan);
    assert_eq!(spine.max_executed_due(), Some(Tick(4)));
    // No pending work: the watermark is exactly the max executed due.
    assert_eq!(spine.processed_through(), spine.max_executed_due());
    // Query cost does not scale with the ledger: the same query loop on
    // the 150k-record spine stays within a small multiple of a
    // near-empty control (a scan would be hundreds of times slower).
    let query_loop = |spine: &Spine<CountingWorld>| {
        let start = std::time::Instant::now();
        for _ in 0..6_000 {
            let _ = spine.max_executed_due();
            let _ = spine.processed_through();
        }
        start.elapsed()
    };
    let control_ms = query_loop(&control).as_secs_f64() * 1000.0;
    let measured_ms = query_loop(&spine).as_secs_f64() * 1000.0;
    assert!(
        measured_ms <= control_ms * 20.0 + 250.0,
        "per-frame watermark queries do not scale with the execution history \
         (measured {measured_ms:.2} ms vs control {control_ms:.2} ms over 6000 queries)"
    );
}
