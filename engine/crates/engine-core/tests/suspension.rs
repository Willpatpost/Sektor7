//! Stage 4 suspension/resumption contracts (reviewer clarifications A–C):
//! identity preservation across suspension, dependency coherence
//! (cross-region blocking through dormancy, blocked-event
//! re-validation, held ids as dependency targets), historical
//! scheduled accounting with explicit held state, safe watermark
//! behavior under suspension, the held fingerprint domain, the
//! runnable-vs-unfinished distinction, and the O(log n) dormant-query
//! property.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use engine_core::{
    Event, EventClass, EventId, EventSpec, ExecCtx, Field, Fingerprinter, Schedule, Spine, Tick,
    World,
};

/// Minimal recording world: records payloads in execution order. No
/// follow-ups — these tests exercise the suspension mechanisms only.
struct RecordingWorld {
    executed: Vec<u64>,
}

impl RecordingWorld {
    fn new() -> Self {
        RecordingWorld {
            executed: Vec::new(),
        }
    }
}

impl World for RecordingWorld {
    fn execute(&mut self, event: &Event, _ctx: &mut ExecCtx) {
        self.executed.push(event.payload);
    }

    fn fingerprint(&self, fp: &mut Fingerprinter) {
        fp.write_str("engine-core-suspension-test/world/1");
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

/// Clarification B conservation identity: historical scheduled count ==
/// executed + queued + blocked + held (the Stage 0/3 identity when the
/// held set is empty).
fn conservation(spine: &Spine<RecordingWorld>) -> bool {
    let stats = spine.stats();
    stats.scheduled
        == stats.executed
            + spine.pending_count() as u64
            + spine.blocked_count() as u64
            + spine.held_count() as u64
}

#[test]
fn suspension_preserves_identity_priority_and_lane() {
    let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 50);
    let mut near_ids = Vec::new();
    for due in 1..=5u64 {
        near_ids.push(spine.schedule(at(Field::Near, due, due)).expect("schedule"));
    }
    for due in 1..=3u64 {
        spine
            .schedule(at(Field::Far, due, 100 + due))
            .expect("schedule");
    }
    // Suspend the near events with even payloads.
    assert_eq!(
        spine.suspend_matching(|view| view.field == Field::Near && view.payload % 2 == 0),
        2
    );
    assert_eq!(spine.held_count(), 2);
    assert!(conservation(&spine));
    // Schedule work that interleaves with the held dues.
    spine.schedule(at(Field::Near, 3, 200)).expect("schedule");
    // Resume with the original identities — no new seqs exist.
    assert_eq!(spine.resume_matching(|view| view.payload % 2 == 0), 2);
    assert_eq!(spine.held_count(), 0);
    spine.run_to_completion(50);
    let executed_seqs: BTreeSet<u64> = spine.ledger().iter().map(|record| record.id.seq).collect();
    for id in &near_ids {
        assert!(
            executed_seqs.contains(&id.seq),
            "original identity {id} executes"
        );
    }
    // Equal-due near executions must be ordered by the original seqs.
    let near_due_3: Vec<u64> = spine
        .ledger()
        .iter()
        .filter(|record| record.field == Field::Near && record.intended_due == Tick(3))
        .map(|record| record.id.seq)
        .collect();
    assert_eq!(near_due_3.len(), 2);
    assert!(
        near_due_3[0] < near_due_3[1],
        "equal-due executions order by original seq"
    );
    assert_eq!(spine.stats().executed, 9);
    assert!(conservation(&spine));
    assert!(!spine.has_unfinished_work());
}

#[test]
fn cross_region_dependency_survives_suspension() {
    let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 50);
    // Region A (class 3): the dependency, due 5.
    let a = spine
        .schedule(EventSpec::new(
            EventClass::new(3),
            Schedule::AtLogicalTime { due: Tick(5) },
            1,
        ))
        .expect("a");
    // Region B (class 5): depends on A, due earlier — it must block.
    let b = spine
        .schedule(
            EventSpec::new(
                EventClass::new(5),
                Schedule::AtLogicalTime { due: Tick(2) },
                2,
            )
            .with_dependencies(vec![a]),
        )
        .expect("b");
    // Suspend region A's obligations — B must remain blocked on the
    // ORIGINAL id (the reviewer's A#41 example).
    assert_eq!(
        spine.suspend_matching(|view| view.class == EventClass::new(3)),
        1
    );
    spine.run_ticks(3);
    assert_eq!(spine.blocked_count(), 1, "B waits on the held dependency");
    assert_eq!(spine.held_count(), 1);
    assert_eq!(
        spine.processed_through(),
        None,
        "nothing is provably complete"
    );
    assert!(conservation(&spine));
    // Resume: A re-enters with its original due, executes, releases B.
    assert_eq!(
        spine.resume_matching(|view| view.class == EventClass::new(3)),
        1
    );
    spine.run_to_completion(50);
    let b_record = spine
        .ledger()
        .iter()
        .find(|record| record.id.seq == b.seq)
        .expect("b executes after the release");
    assert!(
        b_record.late,
        "B executes late after the dormancy-imposed wait"
    );
    assert!(spine.ledger().iter().any(|record| record.id.seq == a.seq));
    assert_eq!(spine.blocked_count(), 0);
    assert_eq!(spine.held_count(), 0);
    assert!(conservation(&spine));
    assert!(!spine.has_unfinished_work());
}

#[test]
fn blocked_event_suspension_revalidates_on_resume() {
    let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 50);
    let a = spine.schedule(at(Field::Near, 6, 1)).expect("a");
    let b = spine
        .schedule(at(Field::Near, 2, 2).with_dependencies(vec![a]))
        .expect("b");
    spine.run_ticks(2);
    assert_eq!(spine.blocked_count(), 1, "B is due and blocked on A");
    // Suspend B while blocked.
    assert_eq!(spine.suspend_matching(|view| view.payload == 2), 1);
    assert_eq!(spine.blocked_count(), 0);
    assert_eq!(spine.held_count(), 1);
    assert!(conservation(&spine));
    // A executes; the release path must not touch the held B.
    spine.run_to_completion(50);
    assert_eq!(spine.stats().executed, 1, "only A runs — B is held");
    assert_eq!(spine.held_count(), 1);
    assert!(spine.has_unfinished_work(), "held B is unfinished work");
    // Resume B: its dependency has executed → straight to the queue,
    // overdue, executing late with its original due intact.
    assert_eq!(spine.resume_matching(|view| view.payload == 2), 1);
    spine.run_to_completion(50);
    let b_record = spine
        .ledger()
        .iter()
        .find(|record| record.id.seq == b.seq)
        .expect("b executes after resume");
    assert!(
        b_record.late,
        "resumed obligations keep their original due and record lateness"
    );
    assert!(!spine.has_unfinished_work());
    assert!(conservation(&spine));
}

#[test]
fn new_dependencies_on_held_events_are_valid_and_wait() {
    let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 50);
    let a = spine.schedule(at(Field::Near, 5, 1)).expect("a");
    spine.suspend_matching(|view| view.payload == 1);
    // Held ids remain valid dependency targets (known, not executed).
    let c = spine.schedule(at(Field::Near, 3, 2).with_dependencies(vec![a]));
    assert!(c.is_ok(), "held ids remain valid dependency targets");
    spine.run_ticks(3);
    assert_eq!(spine.blocked_count(), 1, "C waits on the held dependency");
    assert!(conservation(&spine));
    spine.resume_matching(|view| view.payload == 1);
    spine.run_to_completion(50);
    let payloads = spine.world().executed.clone();
    assert_eq!(
        payloads,
        vec![1, 2],
        "cause before effect through suspension"
    );
    assert!(conservation(&spine));
}

#[test]
fn repeated_cycles_and_duplicate_attempts_are_stable() {
    let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 50);
    let ids: Vec<EventId> = (1..=10u64)
        .map(|due| spine.schedule(at(Field::Near, due, due)).expect("schedule"))
        .collect();
    let before = spine.engine_fingerprint();
    for _ in 0..3 {
        assert_eq!(spine.suspend_matching(|_| true), 10);
        assert_eq!(
            spine.suspend_matching(|_| true),
            0,
            "double suspend is a no-op"
        );
        assert_eq!(spine.held_count(), 10);
        let held_ids: BTreeSet<u64> = spine.held_view().iter().map(|view| view.id.seq).collect();
        assert_eq!(
            held_ids,
            ids.iter().map(|id| id.seq).collect::<BTreeSet<u64>>(),
            "identity stable across cycles"
        );
        assert_eq!(spine.resume_matching(|_| true), 10);
        assert_eq!(
            spine.resume_matching(|_| true),
            0,
            "double resume is a no-op"
        );
        assert_eq!(spine.held_count(), 0);
        assert!(conservation(&spine));
    }
    assert_eq!(
        spine.engine_fingerprint(),
        before,
        "a full suspension cycle restores identical engine state"
    );
}

#[test]
fn scheduled_is_historical_and_conservation_extends_to_held() {
    let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 50);
    for due in 1..=8u64 {
        spine.schedule(at(Field::Near, due, due)).expect("schedule");
    }
    assert_eq!(spine.stats().scheduled, 8);
    spine.suspend_matching(|view| view.payload > 4);
    assert_eq!(
        spine.stats().scheduled,
        8,
        "suspension never decrements the historical count"
    );
    spine.run_to_completion(50);
    assert_eq!(spine.stats().scheduled, 8);
    assert_eq!(spine.stats().executed, 4);
    assert_eq!(spine.held_count(), 4);
    assert!(conservation(&spine));
    spine.resume_matching(|_| true);
    spine.run_to_completion(50);
    assert_eq!(spine.stats().executed, 8);
    assert_eq!(spine.stats().scheduled, 8);
    assert!(conservation(&spine));
}

#[test]
fn watermark_cannot_advance_past_held_overdue_work() {
    let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 50);
    for due in 1..=4u64 {
        spine.schedule(at(Field::Near, due, due)).expect("schedule");
    }
    spine.run_ticks(4);
    assert_eq!(spine.processed_through(), Some(Tick(4)));
    // A new overdue obligation — suspended before it can run.
    let overdue = spine.schedule(at(Field::Near, 2, 99)).expect("schedule");
    spine.suspend_matching(|view| view.payload == 99);
    // Suspension is not a completeness loophole: the watermark stays
    // capped by the held obligation's due.
    assert_eq!(
        spine.processed_through(),
        Some(Tick(1)),
        "held overdue work still caps the watermark"
    );
    assert!(conservation(&spine));
    // Resume: the obligation executes late and the watermark recovers.
    spine.resume_matching(|view| view.payload == 99);
    spine.run_to_completion(10);
    assert_eq!(spine.processed_through(), Some(Tick(4)));
    assert!(
        spine
            .ledger()
            .iter()
            .any(|record| record.id.seq == overdue.seq && record.late)
    );
}

#[test]
fn held_fingerprint_is_deterministic_and_sensitive() {
    let build = |suspend_even: bool| {
        let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 50);
        for due in 1..=6u64 {
            spine.schedule(at(Field::Near, due, due)).expect("schedule");
        }
        spine.suspend_matching(|view| {
            if suspend_even {
                view.payload % 2 == 0
            } else {
                view.payload % 2 == 1
            }
        });
        spine.held_fingerprint()
    };
    assert_eq!(build(true), build(true), "double-run determinism");
    assert_ne!(build(true), build(false), "different held sets differ");
    let empty_a = Spine::with_reservation(RecordingWorld::new(), 100, 50).held_fingerprint();
    let empty_b = Spine::with_reservation(RecordingWorld::new(), 100, 50).held_fingerprint();
    assert_eq!(empty_a, empty_b, "the empty held set is a stable baseline");
}

#[test]
fn watermark_queries_do_not_scale_with_held_obligations() {
    // Reviewer constraint: dormant simulation cost must not scale with
    // the number of dormant obligations merely because the spine exposes
    // watermarks. Validity requirements (preliminary-review item 2):
    // processed_through() consults max_executed_due() first and returns
    // early on an empty ledger — so the benchmark spine carries a real
    // executed history (100 events, dues 1..=100), and the held
    // obligations sit due BELOW the executed horizon, making the
    // watermark value itself proof that each query read the held minimum
    // through the index (min(100 executed, 50 held − 1) = 49). With
    // 60,000 held obligations, 2,000 watermark queries must cost roughly
    // the same as the executed-history control — the held minimum comes
    // from the incrementally maintained index, never a scan (a scan
    // would be orders of magnitude off; the bound is deliberately
    // generous).
    let run = |held: u64| {
        let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 50);
        for due in 1..=100u64 {
            spine.schedule(at(Field::Near, due, due)).expect("schedule");
        }
        spine.run_ticks(100);
        for payload in 0..held {
            spine
                .schedule(at(Field::Far, 50, 1_000 + payload))
                .expect("schedule");
        }
        if held > 0 {
            spine.suspend_matching(|_| true);
        }
        let expected = if held > 0 {
            Some(Tick(49))
        } else {
            Some(Tick(100))
        };
        assert_eq!(
            spine.processed_through(),
            expected,
            "the watermark reads the held minimum through the guard"
        );
        let start = Instant::now();
        for _ in 0..2_000 {
            spine.run_ticks(1);
            assert!(spine.processed_through().is_some());
        }
        start.elapsed()
    };
    let unheld = run(0);
    let dormant = run(60_000);
    println!("measured: unheld {unheld:?}, 60k held {dormant:?}");
    assert!(
        dormant <= unheld * 20 + Duration::from_millis(250),
        "dormant per-tick cost must not scale with held obligations \
         (unheld {unheld:?}, dormant {dormant:?})"
    );
}

#[test]
fn held_min_due_index_stays_consistent_with_the_held_set() {
    // Structural invariant behind the O(log n) watermark contribution:
    // after arbitrary suspend/resume interleavings, the index-derived
    // minimum equals the held set's actual minimum due.
    let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 50);
    for payload in 1..=50u64 {
        spine
            .schedule(at(Field::Near, payload * 7, payload))
            .expect("schedule");
    }
    assert_eq!(spine.suspend_matching(|view| view.payload % 3 == 0), 16);
    assert_eq!(spine.resume_matching(|view| view.payload % 9 == 0), 5);
    assert_eq!(spine.held_count(), 11);
    // Multiples of 4 among the queued payloads: twelve total, but 12/24/48
    // are already held and 36 was resumed above (a multiple of 9) — so
    // nine newly suspended.
    assert_eq!(spine.suspend_matching(|view| view.payload % 4 == 0), 9);
    assert_eq!(spine.held_count(), 20);
    // Everything held: queues and blocked are empty, so the spine's
    // minimum IS the held minimum.
    assert_eq!(spine.suspend_matching(|_| true), 30);
    assert_eq!(spine.held_count(), 50);
    let min_from_views = spine.held_view().iter().map(|view| view.due).min();
    assert_eq!(spine.min_pending_due(), min_from_views);
    assert!(conservation(&spine));
}

#[test]
fn run_to_completion_distinguishes_runnable_from_unfinished() {
    let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 50);
    spine.schedule(at(Field::Near, 1, 1)).expect("schedule");
    spine.suspend_matching(|_| true);
    // No runnable work: run_to_completion stops immediately — but the
    // result must not imply the obligations completed.
    assert_eq!(spine.run_to_completion(100), 0);
    assert!(spine.has_unfinished_work());
    assert_eq!(spine.held_count(), 1);
    spine.resume_matching(|_| true);
    assert_eq!(spine.run_to_completion(100), 1);
    assert!(!spine.has_unfinished_work());
}

#[test]
fn suspension_cycle_is_invisible_to_historical_fingerprint_domains() {
    // The historical domains exclude the held set entirely; a workload
    // that never suspends — or suspends and fully resumes before running
    // — produces byte-identical fingerprints. The Stage 0/3 baselines
    // rest on this mechanically.
    let run = |suspend: bool| {
        let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 50);
        for due in 1..=10u64 {
            spine.schedule(at(Field::Near, due, due)).expect("schedule");
        }
        if suspend {
            spine.suspend_matching(|_| true);
            spine.resume_matching(|_| true);
        }
        spine.run_to_completion(50);
        (spine.engine_fingerprint(), spine.lane_fingerprint())
    };
    assert_eq!(
        run(false),
        run(true),
        "a full suspend+resume cycle is invisible to the historical domains"
    );
}

#[test]
fn run_to_completion_does_not_advance_time_on_held_blocked_graphs() {
    // Batch 1 review item C: with A held and B blocked on A, no queued
    // event can make progress — a completion run must consume zero ticks
    // instead of burning its budget advancing motionless campaign time.
    // Resumption then completes the graph.
    let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 50);
    let a = spine.schedule(at(Field::Near, 5, 1)).expect("a");
    let b = spine
        .schedule(at(Field::Near, 2, 2).with_dependencies(vec![a]))
        .expect("b");
    spine.run_ticks(2); // tick 2: B due, blocked on unexecuted A
    assert_eq!(spine.blocked_count(), 1);
    spine.suspend_matching(|view| view.payload == 1); // A held; B blocked
    let clock_before = spine.clock();
    assert_eq!(
        spine.run_to_completion(1_000),
        0,
        "no progressible work: zero ticks consumed"
    );
    assert_eq!(
        spine.clock(),
        clock_before,
        "campaign time does not advance on a motionless graph"
    );
    assert_eq!(spine.blocked_count(), 1, "B still waits on the held A");
    assert!(spine.has_unfinished_work());
    assert!(conservation(&spine));
    // Resumption completes: A re-enters with its original due, executes,
    // releases B; B executes late with its original due intact.
    spine.resume_matching(|view| view.payload == 1);
    let used = spine.run_to_completion(1_000);
    assert!(used > 0, "resumed work progresses again");
    assert!(!spine.has_unfinished_work());
    let b_record = spine
        .ledger()
        .iter()
        .find(|record| record.id.seq == b.seq)
        .expect("b executes after resumption");
    assert!(b_record.late);
    assert!(conservation(&spine));
}

#[test]
fn schedule_held_targets_dormant_regions_without_execution() {
    // Batch 2 priority B: new obligations targeting dormant regions are
    // created directly in the held state — they cannot execute
    // unnoticed, are accounted at creation, validate dependencies like
    // schedule(), and resume with full identity (original due, lateness
    // honestly recorded when the hold made them overdue).
    let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 50);
    let dep = spine.schedule(at(Field::Near, 1, 1)).expect("dep");
    spine.run_ticks(1);
    let id = spine
        .schedule_held(at(Field::Far, 5, 2).with_dependencies(vec![dep]))
        .expect("held schedule validates dependencies");
    assert_eq!(spine.held_count(), 1);
    assert_eq!(spine.pending_count(), 0, "never queued");
    assert_eq!(spine.suspension_stats().scheduled_held_total, 1);
    assert_eq!(spine.stats().scheduled, 2, "accounted at creation");
    assert!(conservation(&spine));
    // Unknown dependencies are rejected exactly as with schedule().
    let unknown = EventId { seq: 999 };
    assert!(
        spine
            .schedule_held(at(Field::Far, 5, 3).with_dependencies(vec![unknown]))
            .is_err()
    );
    // It cannot execute while held, however far time advances.
    spine.run_ticks(100);
    assert_eq!(spine.stats().executed, 1, "the held obligation never runs");
    assert!(spine.has_unfinished_work());
    // Resume: full identity, original due, lateness from the hold.
    assert_eq!(spine.resume_matching(|view| view.id.seq == id.seq), 1);
    spine.run_to_completion(10);
    let record = spine
        .ledger()
        .iter()
        .find(|record| record.id.seq == id.seq)
        .expect("executed after resumption");
    assert_eq!(record.intended_due, Tick(5));
    assert!(
        record.late,
        "overdue through the dormant hold — lateness recorded"
    );
    assert!(conservation(&spine));
    assert!(!spine.has_unfinished_work());
}

#[test]
fn reclassify_protects_promoted_regions_under_far_overload() {
    // Batch 2 priority C: dynamic near/far classification under actual
    // far-lane overload. A region reclassified to near on promotion
    // executes immediately inside the reservation — zero
    // priority-transition latency — while the same region without
    // reclassification starves behind the far backlog for a measured
    // multi-tick window.
    let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 30);
    // Far-lane bulk: 300 due events (far drains ~100/tick here: 70
    // guaranteed + near's unused leftover), so a durable backlog forms.
    for payload in 0..300u64 {
        spine
            .schedule(at(Field::Far, 1, 1000 + payload))
            .expect("schedule");
    }
    // The promoted region (class 7): classified Far before promotion,
    // scheduled after the bulk so its (due, seq) keys order last.
    let mut promoted = Vec::new();
    for payload in 0..10u64 {
        promoted.push(
            spine
                .schedule(
                    EventSpec::new(
                        EventClass::new(7),
                        Schedule::AtLogicalTime { due: Tick(1) },
                        payload,
                    )
                    .in_field(Field::Far),
                )
                .expect("schedule"),
        );
    }
    // Promotion: the region's obligations move to the near lane with
    // identity intact — (due, seq), schedule, payload, dependencies.
    assert_eq!(
        spine.reclassify_matching(|view| view.class == EventClass::new(7), Field::Near),
        10
    );
    assert_eq!(spine.near_pending_count(), 10);
    assert_eq!(spine.far_pending_count(), 300);
    assert_eq!(spine.suspension_stats().reclassified_total, 10);
    assert!(conservation(&spine));
    // Tick 1: the promoted region executes fully inside the reservation
    // while the far bulk still backlogs.
    spine.run_ticks(1);
    let promoted_executed = spine
        .ledger()
        .iter()
        .filter(|record| promoted.iter().any(|id| id.seq == record.id.seq))
        .count();
    assert_eq!(
        promoted_executed, 10,
        "the promoted region executes fully on the first tick — zero latency"
    );
    for record in spine
        .ledger()
        .iter()
        .filter(|record| promoted.iter().any(|id| id.seq == record.id.seq))
    {
        assert_eq!(
            record.field,
            Field::Near,
            "ledger records the lane at execution"
        );
        assert_eq!(record.intended_due, Tick(1), "identity preserved");
    }
    assert_eq!(spine.far_pending_count(), 210, "far bulk still backlogged");
    // Control: the same setup WITHOUT reclassification starves the
    // promoted events behind the bulk for a measured multi-tick window.
    let mut control = Spine::with_reservation(RecordingWorld::new(), 100, 30);
    for payload in 0..300u64 {
        control
            .schedule(at(Field::Far, 1, 1000 + payload))
            .expect("schedule");
    }
    let mut control_ids = Vec::new();
    for payload in 0..10u64 {
        control_ids.push(
            control
                .schedule(
                    EventSpec::new(
                        EventClass::new(7),
                        Schedule::AtLogicalTime { due: Tick(1) },
                        payload,
                    )
                    .in_field(Field::Far),
                )
                .expect("schedule"),
        );
    }
    control.run_ticks(1);
    let starved = control
        .ledger()
        .iter()
        .filter(|record| control_ids.iter().any(|id| id.seq == record.id.seq))
        .count();
    assert_eq!(
        starved, 0,
        "without reclassification the region starves on tick 1"
    );
    let mut control_ticks = 0;
    while control_ticks < 50 {
        control.run_ticks(1);
        control_ticks += 1;
        let done = control
            .ledger()
            .iter()
            .filter(|record| control_ids.iter().any(|id| id.seq == record.id.seq))
            .count();
        if done == 10 {
            break;
        }
    }
    assert!(
        control_ticks >= 3,
        "the control starvation window is real and multi-tick (measured {control_ticks})"
    );
}

#[test]
fn follow_up_in_field_classifies_new_obligations() {
    // The Stage 3 "deferred until a use case exists" explicit-field
    // follow-up (Batch 2 priority C): a new follow-up classifies under
    // the region's CURRENT policy while its cause keeps its own lane;
    // the ledger records each execution's actual lane.
    struct LaneWorld {
        follow_up_field: Field,
        executed: Vec<u64>,
    }
    impl World for LaneWorld {
        fn execute(&mut self, event: &Event, ctx: &mut ExecCtx) {
            self.executed.push(event.payload);
            ctx.follow_up_in_field(
                self.follow_up_field,
                1,
                event.class,
                event.payload + 1,
                Vec::new(),
            );
        }
        fn fingerprint(&self, fp: &mut Fingerprinter) {
            fp.write_str("engine-core-suspension-test/lane-world/1");
            fp.write_usize(self.executed.len());
            for payload in &self.executed {
                fp.write_u64(*payload);
            }
        }
    }
    let mut spine = Spine::with_reservation(
        LaneWorld {
            follow_up_field: Field::Far,
            executed: Vec::new(),
        },
        100,
        50,
    );
    // Cause in Near; its follow-up classifies Far per current policy.
    spine.schedule(at(Field::Near, 1, 1)).expect("schedule");
    spine.run_ticks(1);
    assert_eq!(spine.near_pending_count(), 0, "the cause executed");
    assert_eq!(
        spine.far_pending_count(),
        1,
        "the follow-up lands in the explicit lane"
    );
    spine.run_ticks(1);
    assert_eq!(spine.stats().executed, 2);
    let lanes: Vec<Field> = spine.ledger().iter().map(|record| record.field).collect();
    assert_eq!(
        lanes,
        vec![Field::Near, Field::Far],
        "the ledger records each execution's actual lane"
    );
    let stats = spine.stats();
    assert_eq!(
        stats.scheduled,
        stats.executed
            + spine.pending_count() as u64
            + spine.blocked_count() as u64
            + spine.held_count() as u64
    );
}

// ------------------------------------------- B4: collect/suspend_ids

/// The shared fixture for the collect_matching/suspend_ids contracts
/// (the six-phase protocol's read-only preparation and id-exact
/// install): queued near and far work, one blocked near entry, and —
/// after three ticks — a settled mixed state identical across builds.
fn collect_fixture() -> Spine<RecordingWorld> {
    let mut spine = Spine::with_reservation(RecordingWorld::new(), 100, 50);
    for due in 1..=6u64 {
        spine.schedule(at(Field::Near, due, due)).expect("schedule");
    }
    for due in 1..=4u64 {
        spine
            .schedule(at(Field::Far, due, 100 + due))
            .expect("schedule");
    }
    let dep = spine.schedule(at(Field::Far, 50, 900)).expect("schedule");
    spine
        .schedule(
            EventSpec::new(
                EventClass::new(0),
                Schedule::AtLogicalTime { due: Tick(2) },
                901,
            )
            .in_field(Field::Near)
            .with_dependencies(vec![dep]),
        )
        .expect("schedule");
    spine.run_ticks(3);
    spine
}

#[test]
fn collect_matching_enumerates_the_suspension_set_read_only() {
    // The six-phase preparation query (§5.5.1): collect_matching
    // enumerates exactly what suspend_matching with the same predicate
    // would move — queued and blocked entries only, never held — with
    // NO mutation: no accounting, no suspension stats, no fingerprint
    // movement. Canonically ordered by (due, seq).
    let mut spine = collect_fixture();
    let stats_before = spine.stats();
    let held_before = spine.held_fingerprint();
    let lanes_before = spine.lane_state_fingerprint();
    // The views borrow the spine; the owned payloads and ids are
    // copied out inside the block so the read-only assertions and the
    // mutation below never overlap borrows.
    let (collected_payloads, collected_ids) = {
        let collected = spine.collect_matching(|view| view.field == Field::Near);
        assert_eq!(
            collected.len(),
            4,
            "three queued near + the blocked near entry"
        );
        (
            collected
                .iter()
                .map(|view| view.payload)
                .collect::<Vec<_>>(),
            collected
                .iter()
                .map(|view| view.id.seq)
                .collect::<BTreeSet<_>>(),
        )
    };
    assert_eq!(
        collected_payloads,
        vec![901, 4, 5, 6],
        "canonically ordered by (due, seq): the blocked due-2 entry first"
    );
    assert_eq!(spine.stats().executed, stats_before.executed);
    assert_eq!(spine.stats().scheduled, stats_before.scheduled);
    assert_eq!(spine.held_count(), 0, "nothing moved");
    assert_eq!(spine.held_fingerprint(), held_before);
    assert_eq!(spine.lane_state_fingerprint(), lanes_before);
    assert!(conservation(&spine));
    // The predicate form moves exactly the collected set.
    assert_eq!(spine.suspend_matching(|view| view.field == Field::Near), 4);
    assert_eq!(spine.held_count(), 4);
    let moved: BTreeSet<u64> = spine.held_view().iter().map(|view| view.id.seq).collect();
    assert_eq!(moved, collected_ids);
    assert!(conservation(&spine));
}

#[test]
fn suspend_ids_is_the_exact_batch_of_the_predicate_form() {
    // The staged-install step (§5.5.2): suspending the ids a plan
    // collected is behaviorally identical to the predicate form — the
    // same held state, the same lane state, the same resumption and
    // completion.
    let mut planned = collect_fixture();
    let mut direct = collect_fixture();
    let ids: Vec<EventId> = planned
        .collect_matching(|view| view.field == Field::Near)
        .iter()
        .map(|view| view.id)
        .collect();
    assert_eq!(planned.suspend_ids(&ids), 4);
    assert_eq!(direct.suspend_matching(|view| view.field == Field::Near), 4);
    assert_eq!(planned.held_fingerprint(), direct.held_fingerprint());
    assert_eq!(
        planned.lane_state_fingerprint(),
        direct.lane_state_fingerprint()
    );
    assert!(conservation(&planned));
    assert!(conservation(&direct));
    assert_eq!(planned.resume_matching(|_| true), 4);
    assert_eq!(direct.resume_matching(|_| true), 4);
    assert_eq!(
        planned.lane_state_fingerprint(),
        direct.lane_state_fingerprint()
    );
    planned.run_to_completion(60);
    direct.run_to_completion(60);
    assert_eq!(planned.stats().executed, direct.stats().executed);
    assert_eq!(planned.engine_fingerprint(), direct.engine_fingerprint());
}

#[test]
fn suspend_ids_ignores_absent_targets() {
    // Id-exact means exactly that: ids not currently queued or blocked
    // (already held, already executed, never allocated) are not moved
    // and not counted — the returned count is the staged-commit
    // discipline's defensive-failure signal, never a silent partial
    // install.
    let mut spine = collect_fixture();
    let held_id = spine
        .collect_matching(|view| view.payload == 4)
        .remove(0)
        .id;
    assert_eq!(spine.suspend_ids(&[held_id]), 1);
    spine.run_ticks(10);
    let Some(executed_id) = spine.ledger().iter().next().map(|record| record.id) else {
        panic!("the fixture executed work");
    };
    let live_id = spine
        .collect_matching(|view| view.payload == 900)
        .remove(0)
        .id;
    let moved = spine.suspend_ids(&[held_id, executed_id, EventId { seq: 999_999 }, live_id]);
    assert_eq!(moved, 1, "only the live queued target moves");
    assert_eq!(spine.held_count(), 2);
    let held_payloads: BTreeSet<u64> = spine.held_view().iter().map(|view| view.payload).collect();
    assert_eq!(held_payloads, BTreeSet::from([4, 900]));
    assert!(conservation(&spine));
}
