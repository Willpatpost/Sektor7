//! The event spine: canonical campaign clock, scheduling with in-class
//! `(due, seq)` ordering, cross-class causal dependencies, per-tick budget,
//! the execution ledger, and the safe processed-through watermark.
//!
//! Stage 0 scope (`docs/TECH-ARCHITECTURE.md` §6): single queue, no near/far
//! lanes, no regions, no persistence — only the contract semantics the
//! 2026-10-19 review ratified. Lanes and regions attach above this core when
//! their stages arrive; nothing here assumes them.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use crate::clock::Tick;
use crate::diag::{Diagnostics, Level};
use crate::event::{
    Event, EventClass, EventId, EventSpec, ExecutionRecord, Schedule, ScheduleKind,
};
use crate::fingerprint::Fingerprinter;

/// Game-side event handler. Implemented by the application, never by the
/// engine — game policy stays outside reusable engine mechanisms by
/// *placement*, whatever form it takes (code, data, configuration, or a
/// future scripting layer) (`docs/TECH-ARCHITECTURE.md` §1 P5, review
/// correction 4).
pub trait World {
    /// Executes one event. Follow-ups are scheduled through `ctx`; the
    /// handler never reaches into the spine directly.
    fn execute(&mut self, event: &Event, ctx: &mut ExecCtx);

    /// Feeds canonical logical state into `fp`. Order-stable by contract:
    /// nothing derived from unordered iteration may reach a fingerprint.
    fn fingerprint(&self, fp: &mut Fingerprinter);
}

/// Handler context: what an executing event may observe and enqueue.
pub struct ExecCtx<'a> {
    executed_tick: Tick,
    executed: &'a BTreeSet<u64>,
    follow_ups: Vec<EventSpec>,
}

impl<'a> ExecCtx<'a> {
    /// The tick at which the current event is actually executing.
    pub fn executed_tick(&self) -> Tick {
        self.executed_tick
    }

    pub fn is_executed(&self, id: EventId) -> bool {
        self.executed.contains(&id.seq)
    }

    /// Schedules an execution-relative follow-up: due = executed_tick +
    /// `delay`. Anchors to *actual* execution, not to the event's intended
    /// time — an overdue cause shifts its effects honestly instead of
    /// stacking impossible deadlines (`docs/TECH-ARCHITECTURE.md` §3.2).
    pub fn follow_up(
        &mut self,
        delay: u64,
        class: EventClass,
        payload: u64,
        depends_on: Vec<EventId>,
    ) {
        self.follow_ups.push(EventSpec {
            class,
            schedule: Schedule::ExecutionRelative {
                origin: self.executed_tick,
                delay,
            },
            payload,
            depends_on,
        });
    }

    /// Schedules at a fixed logical time. `tick` may lie in the past — a
    /// late arrival; the spine records it and the safe watermark can
    /// retreat.
    pub fn schedule_at(
        &mut self,
        tick: Tick,
        class: EventClass,
        payload: u64,
        depends_on: Vec<EventId>,
    ) {
        self.follow_ups.push(EventSpec {
            class,
            schedule: Schedule::AtLogicalTime { due: tick },
            payload,
            depends_on,
        });
    }

    fn drain(&mut self) -> Vec<EventSpec> {
        std::mem::take(&mut self.follow_ups)
    }
}

/// Rejection reasons for a scheduling request. Rejections are returned to
/// the caller (or, for handler follow-ups, reported through diagnostics and
/// stats) — never silently dropped (E-10 discipline).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleError {
    /// A dependency references an event that was never scheduled. Because
    /// dependencies must reference already-scheduled or already-executed
    /// events, this rule also makes dependency cycles structurally
    /// impossible (review correction 2).
    UnknownDependency(EventId),
}

/// Spine counters. All fields are logical state and fingerprinted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub scheduled: u64,
    pub executed: u64,
    /// Events scheduled with a due already behind the clock.
    pub late_arrivals: u64,
    /// Executions with `executed_tick > intended_due`.
    pub late_executions: u64,
    /// Due events moved to the blocked set (unmet dependencies).
    pub blocked_transitions: u64,
    /// Handler follow-ups rejected at schedule time — reported, never silent.
    pub invalid_dropped: u64,
    /// Ticks that ended with the budget fully consumed and due work
    /// remaining in the queue.
    pub budget_exhausted_ticks: u64,
}

/// Queue entry, ordered by `(due, seq)`: in-class FIFO, and a global
/// `(due, seq)` order that incidentally interleaves classes without
/// promising anything across them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    due: Tick,
    seq: u64,
    event: Event,
}

impl Pending {
    fn new(event: Event) -> Self {
        let due = event.schedule.due();
        Pending {
            due,
            seq: event.id.seq,
            event,
        }
    }
}

impl Ord for Pending {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.due, self.seq).cmp(&(other.due, other.seq))
    }
}

impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The event spine.
pub struct Spine<W: World> {
    clock: Tick,
    next_seq: u64,
    queue: BinaryHeap<Reverse<Pending>>,
    /// Due events waiting on unmet dependencies, keyed by seq.
    blocked: BTreeMap<u64, Event>,
    /// dependency seq -> dependent seqs currently registered on it.
    waiting_on: BTreeMap<u64, Vec<u64>>,
    /// Every seq ever scheduled (pending or executed).
    known: BTreeSet<u64>,
    /// Every seq already executed.
    executed: BTreeSet<u64>,
    ledger: Vec<ExecutionRecord>,
    world: W,
    budget: u32,
    stats: Stats,
    diag: Diagnostics,
}

impl<W: World> Spine<W> {
    pub fn new(world: W, budget: u32) -> Self {
        Self::with_diagnostics(world, budget, Diagnostics::new(Level::Off))
    }

    pub fn with_diagnostics(world: W, budget: u32, diag: Diagnostics) -> Self {
        Spine {
            clock: Tick::ZERO,
            next_seq: 0,
            queue: BinaryHeap::new(),
            blocked: BTreeMap::new(),
            waiting_on: BTreeMap::new(),
            known: BTreeSet::new(),
            executed: BTreeSet::new(),
            ledger: Vec::new(),
            world,
            budget: budget.max(1),
            stats: Stats::default(),
            diag,
        }
    }

    // ------------------------------------------------------------- queries

    pub fn clock(&self) -> Tick {
        self.clock
    }

    pub fn budget(&self) -> u32 {
        self.budget
    }

    pub fn world(&self) -> &W {
        &self.world
    }

    pub fn world_mut(&mut self) -> &mut W {
        &mut self.world
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn diagnostics(&self) -> &Diagnostics {
        &self.diag
    }

    pub fn diagnostics_mut(&mut self) -> &mut Diagnostics {
        &mut self.diag
    }

    pub fn ledger(&self) -> &[ExecutionRecord] {
        &self.ledger
    }

    /// Events waiting in the queue (due or not yet due).
    pub fn pending_count(&self) -> usize {
        self.queue.len()
    }

    /// Due events parked on unmet dependencies.
    pub fn blocked_count(&self) -> usize {
        self.blocked.len()
    }

    pub fn is_executed(&self, id: EventId) -> bool {
        self.executed.contains(&id.seq)
    }

    /// Maximum intended-due tick among executed events — a raw progress
    /// measure, **not** a completeness claim (review correction 1).
    pub fn max_executed_due(&self) -> Option<Tick> {
        self.ledger.iter().map(|r| r.intended_due).max()
    }

    /// Smallest intended due among pending events (queued or blocked).
    pub fn min_pending_due(&self) -> Option<Tick> {
        let mut min = self.queue.peek().map(|item| item.0.due);
        for event in self.blocked.values() {
            let due = event.schedule.due();
            min = Some(min.map_or(due, |m| m.min(due)));
        }
        min
    }

    /// Safe processed-through watermark (review correction 1): the largest
    /// tick `T` such that
    ///
    /// 1. `T <= max_executed_due` — completeness is never claimed beyond
    ///    work that has actually run, because a future late arrival may
    ///    still carry an earlier due; and
    /// 2. no pending event — queued, executing, or blocked by unresolved
    ///    causal dependencies — has an intended due `<= T`.
    ///
    /// Computed live from queue state, so it may **retreat** when late
    /// events are scheduled; consumers must re-read, never cache. Stage 0
    /// executes synchronously, so the "executing" state is observable only
    /// between ticks — where this query is meaningful.
    pub fn processed_through(&self) -> Option<Tick> {
        let max_exec = self.max_executed_due()?;
        match self.min_pending_due() {
            Some(min_pending) => Some(Tick(
                max_exec
                    .as_u64()
                    .min(min_pending.as_u64().saturating_sub(1)),
            )),
            None => Some(max_exec),
        }
    }

    // ---------------------------------------------------------- scheduling

    /// Schedules an event. Dependency ids must reference already-scheduled
    /// or already-executed events; unknown ids are rejected immediately
    /// (which also makes dependency cycles structurally impossible).
    pub fn schedule(&mut self, spec: EventSpec) -> Result<EventId, ScheduleError> {
        if let Some(unknown) = spec
            .depends_on
            .iter()
            .find(|id| !self.known.contains(&id.seq))
        {
            return Err(ScheduleError::UnknownDependency(*unknown));
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        let id = EventId { seq };
        let event = Event {
            id,
            class: spec.class,
            schedule: spec.schedule,
            payload: spec.payload,
            depends_on: spec.depends_on,
        };
        let due = event.schedule.due();
        if due < self.clock {
            self.stats.late_arrivals += 1;
            self.diag.log(
                Level::Warn,
                format!(
                    "late arrival {id}: due {due} behind clock {} — completeness claims above {due} are invalidated",
                    self.clock
                ),
            );
        }
        self.diag.log(
            Level::Trace,
            format!(
                "scheduled {id} class {} payload {} due {due} deps {}",
                event.class,
                event.payload,
                event.depends_on.len()
            ),
        );
        self.known.insert(seq);
        self.queue.push(Reverse(Pending::new(event)));
        self.stats.scheduled += 1;
        Ok(id)
    }

    // -------------------------------------------------------------- ticking

    /// Advances one tick and processes due events up to the per-tick budget.
    /// The budget is a *mechanism*; the budget *value* is game policy (P5).
    pub fn run_tick(&mut self) -> Tick {
        self.clock = self.clock.saturating_add(1);
        let mut remaining = self.budget;
        while remaining > 0 {
            let Some(top) = self.queue.peek() else { break };
            if top.0.due > self.clock {
                break;
            }
            let Reverse(pending) = self.queue.pop().expect("queue head was just peeked");
            let event = pending.event;
            let unmet = self.unmet_dependencies(&event);
            if unmet.is_empty() {
                self.execute_now(event);
                remaining -= 1;
            } else {
                // Due, but causality-blocked: parked, not dropped. Blocked
                // transitions consume no execution budget (no work was
                // done), and the parked event keeps capping the safe
                // watermark until its dependencies resolve.
                self.stats.blocked_transitions += 1;
                self.diag.log(
                    Level::Trace,
                    format!(
                        "blocked {} at {} ({} unmet dependencies)",
                        event.id,
                        self.clock,
                        unmet.len()
                    ),
                );
                self.register_waiters(event.id.seq, &unmet);
                self.blocked.insert(event.id.seq, event);
            }
        }
        if remaining == 0 && self.has_due_work() {
            self.stats.budget_exhausted_ticks += 1;
            self.diag.log(
                Level::Info,
                format!("budget exhausted at {} with due work remaining", self.clock),
            );
        }
        self.clock
    }

    pub fn run_ticks(&mut self, ticks: u64) -> Tick {
        for _ in 0..ticks {
            self.run_tick();
        }
        self.clock
    }

    /// Runs until no pending work remains (queue and blocked set both
    /// empty) or `max_ticks` elapse; returns the ticks consumed. Dependency
    /// chains are finite and acyclic by construction and every pending due
    /// is eventually reached by the advancing clock, so a quiet spine is
    /// reached for sufficiently large `max_ticks`.
    pub fn run_to_completion(&mut self, max_ticks: u64) -> u64 {
        let mut used = 0;
        while used < max_ticks && self.has_any_work() {
            self.run_tick();
            used += 1;
        }
        used
    }

    // ------------------------------------------------------- fingerprints

    /// Fingerprint of engine-logical state (clock, budget, stats, pending
    /// queue, blocked set, ledger). Excludes diagnostics (observational)
    /// and the world — see [`Self::state_fingerprint`].
    pub fn engine_fingerprint(&self) -> u64 {
        let mut fp = Fingerprinter::new();
        fp.write_str("engine-core/spine/1");
        fp.write_tick(self.clock);
        fp.write_u64(self.next_seq);
        fp.write_u32(self.budget);
        fp.write_u64(self.stats.scheduled);
        fp.write_u64(self.stats.executed);
        fp.write_u64(self.stats.late_arrivals);
        fp.write_u64(self.stats.late_executions);
        fp.write_u64(self.stats.blocked_transitions);
        fp.write_u64(self.stats.invalid_dropped);
        fp.write_u64(self.stats.budget_exhausted_ticks);

        // Heap iteration order is unspecified — sort canonically first.
        let mut queued: Vec<&Pending> = self.queue.iter().map(|item| &item.0).collect();
        queued.sort_by_key(|pending| (pending.due, pending.seq));
        fp.write_usize(queued.len());
        for pending in queued {
            fp.write_u64(pending.seq);
            fp.write_u32(u32::from(pending.event.class.0));
            fp.write_u64(pending.event.payload);
            fingerprint_schedule(&mut fp, &pending.event.schedule);
            fingerprint_deps(&mut fp, &pending.event);
        }

        fp.write_usize(self.blocked.len());
        for (seq, event) in &self.blocked {
            fp.write_u64(*seq);
            fp.write_u32(u32::from(event.class.0));
            fp.write_u64(event.payload);
            fingerprint_schedule(&mut fp, &event.schedule);
            fingerprint_deps(&mut fp, event);
        }

        fp.write_usize(self.ledger.len());
        for record in &self.ledger {
            fp.write_u64(record.id.seq);
            fp.write_u32(u32::from(record.class.0));
            fp.write_tick(record.intended_due);
            fp.write_tick(record.executed_tick);
            fp.write_u8(match record.schedule_kind {
                ScheduleKind::AtLogicalTime => 0,
                ScheduleKind::ExecutionRelative => 1,
            });
            fp.write_u8(u8::from(record.late));
        }
        fp.finish()
    }

    /// Fingerprint of engine state plus world state — the full logical
    /// state, in the E-07…E-10 double-run tradition.
    pub fn state_fingerprint(&self) -> u64 {
        let mut fp = Fingerprinter::new();
        fp.write_str("engine-core/state/1");
        fp.write_u64(self.engine_fingerprint());
        self.world.fingerprint(&mut fp);
        fp.finish()
    }

    pub fn engine_fingerprint_hex(&self) -> String {
        format!("{:016x}", self.engine_fingerprint())
    }

    pub fn state_fingerprint_hex(&self) -> String {
        format!("{:016x}", self.state_fingerprint())
    }

    // ------------------------------------------------------------ internals

    fn execute_now(&mut self, event: Event) {
        let id = event.id;
        let executed_tick = self.clock;
        let due = event.schedule.due();
        let record = ExecutionRecord {
            id: event.id,
            class: event.class,
            intended_due: due,
            executed_tick,
            schedule_kind: event.schedule.kind(),
            late: executed_tick > due,
        };
        // Record execution *before* the handler runs, so follow-ups may
        // declare dependencies on the event currently executing.
        self.ledger.push(record);
        self.executed.insert(event.id.seq);
        self.stats.executed += 1;
        if executed_tick > due {
            self.stats.late_executions += 1;
            self.diag.log(
                Level::Debug,
                format!(
                    "late execution {id}: due {due} executed {executed_tick} ({} ticks late)",
                    executed_tick.as_u64() - due.as_u64()
                ),
            );
        } else {
            self.diag
                .log(Level::Trace, format!("executed {id} at {executed_tick}"));
        }

        let follow_ups = {
            let executed = &self.executed;
            let mut ctx = ExecCtx {
                executed_tick,
                executed,
                follow_ups: Vec::new(),
            };
            self.world.execute(&event, &mut ctx);
            ctx.drain()
        };
        for spec in follow_ups {
            if let Err(error) = self.schedule(spec) {
                // Handler follow-ups cannot return errors to their
                // producer; rejection is reported through diagnostics and
                // stats — never silently dropped (E-10 discipline).
                self.stats.invalid_dropped += 1;
                self.diag.log(
                    Level::Error,
                    format!("dropped invalid follow-up from {id}: {error:?}"),
                );
            }
        }

        // Release dependents that were waiting on this event.
        if let Some(waiters) = self.waiting_on.remove(&event.id.seq) {
            for waiter in waiters {
                self.try_unblock(waiter);
            }
        }
    }

    fn unmet_dependencies(&self, event: &Event) -> Vec<EventId> {
        event
            .depends_on
            .iter()
            .copied()
            .filter(|id| !self.executed.contains(&id.seq))
            .collect()
    }

    fn register_waiters(&mut self, dependent: u64, deps: &[EventId]) {
        for dep in deps {
            let waiters = self.waiting_on.entry(dep.seq).or_default();
            if !waiters.contains(&dependent) {
                waiters.push(dependent);
            }
        }
    }

    fn try_unblock(&mut self, seq: u64) {
        let Some(event) = self.blocked.get(&seq) else {
            return;
        };
        let unmet = self.unmet_dependencies(event);
        if !unmet.is_empty() {
            // Other dependencies remain; re-register (deduplicated).
            self.register_waiters(seq, &unmet);
            return;
        }
        let Some(event) = self.blocked.remove(&seq) else {
            return;
        };
        let due = event.schedule.due();
        self.diag
            .log(Level::Trace, format!("unblocked {} (due {due})", event.id));
        self.queue.push(Reverse(Pending::new(event)));
    }

    fn has_due_work(&self) -> bool {
        self.queue
            .peek()
            .is_some_and(|item| item.0.due <= self.clock)
    }

    fn has_any_work(&self) -> bool {
        !self.queue.is_empty() || !self.blocked.is_empty()
    }
}

fn fingerprint_schedule(fp: &mut Fingerprinter, schedule: &Schedule) {
    match schedule {
        Schedule::AtLogicalTime { due } => {
            fp.write_u8(0);
            fp.write_tick(*due);
        }
        Schedule::ExecutionRelative { origin, delay } => {
            fp.write_u8(1);
            fp.write_tick(*origin);
            fp.write_u64(*delay);
        }
    }
}

fn fingerprint_deps(fp: &mut Fingerprinter, event: &Event) {
    fp.write_usize(event.depends_on.len());
    for dep in &event.depends_on {
        fp.write_u64(dep.seq);
    }
}
