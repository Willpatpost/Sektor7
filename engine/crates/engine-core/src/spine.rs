//! The event spine: canonical campaign clock, scheduling with deterministic
//! `(due, seq)` queue priority, cross-class causal dependencies, per-tick
//! budget with the near/far lane policy, the execution ledger, and the safe
//! processed-through watermark.
//!
//! Stage 3 (`docs/TECH-ARCHITECTURE.md` §2.4, §6): near and far lanes with
//! the three-phase budget policy — the near reservation is a **guaranteed
//! allocation, not a ceiling**: (1) near-field events execute up to the
//! reservation, (2) far-field events up to the nonreserved capacity, and
//! (3) remaining budget redistributes near-first (reviewer clarification
//! 2026-10-20). The legacy constructors keep `near_reserve == budget`,
//! reproducing the Stage 0 single-lane behavior exactly; the Stage 0
//! fingerprint domain ("engine-core/spine/2") is untouched and stays
//! byte-identical, while the new lane-aware domain ("spine-lanes/1")
//! represents the lane classification and reservation as authoritative
//! state. Regions, LOD levels, and persistence still attach above this
//! core in their stages.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use crate::clock::Tick;
use crate::diag::{Diagnostics, Level};
use crate::event::{
    Event, EventClass, EventId, EventSpec, ExecutionRecord, Field, Schedule, ScheduleKind,
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
    /// Lane of the currently executing event: handler follow-ups inherit
    /// their cause's lane (Stage 3 decision — chained work stays in its
    /// lane; an explicit override API is deferred until a use case
    /// exists).
    field: Field,
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
            field: self.field,
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
            field: self.field,
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

/// Observational per-tick near/far lane report (Stage 3). Not part of the
/// Stage 0 fingerprint schema and not authoritative state — the ledger
/// and queues already represent executions deterministically; this is
/// instrumentation for diagnostics and the overlay, like `Diagnostics`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LaneReport {
    pub near_executed: u32,
    pub far_executed: u32,
    pub near_reserve: u32,
    pub budget: u32,
    /// Due near work remained when the total budget was exhausted.
    pub near_backlogged: bool,
    /// Due far work remained when the total budget was exhausted.
    pub far_backlogged: bool,
}

/// Cumulative lane execution totals (observational, Stage 3): derivable
/// from the ledger, kept separately so diagnostics never touch
/// fingerprinted state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LaneTotals {
    pub near_executed: u64,
    pub far_executed: u64,
    pub near_backlogged_ticks: u64,
    pub far_backlogged_ticks: u64,
}

/// Observational suspension counters (Stage 4 diagnostics — like
/// `LaneTotals`, never fingerprinted; the held set itself is the
/// authoritative suspended state).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SuspensionStats {
    /// Total obligations moved into the held set.
    pub suspended_total: u64,
    /// Total obligations returned from the held set.
    pub resumed_total: u64,
}

/// Where an obligation currently lives (Stage 4 suspension).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObligationLocation {
    Queued,
    Blocked,
    Held,
}

/// Read-only view of one pending or held obligation, for suspension and
/// resumption matching. The spine treats `class` and `payload` as
/// opaque — the mapping between event classes and spatial regions is
/// application policy (Stage 4 reviewer clarification B).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObligationView<'a> {
    pub id: EventId,
    pub due: Tick,
    pub class: EventClass,
    pub payload: u64,
    pub field: Field,
    pub depends_on: &'a [EventId],
    pub location: ObligationLocation,
}

/// Queue entry, ordered by `(due, seq)`. Each dequeue selects the minimum
/// key among currently queued, due events; a causality-blocked event leaves
/// the queue and reenters with its original priority once its dependencies
/// are satisfied, and late arrivals can introduce earlier-due events after
/// later ones have executed — so neither dequeue nor execution order is
/// globally monotonic (see [`EventClass`](crate::EventClass)); the ledger
/// records actual execution.
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

    fn view(&self, location: ObligationLocation) -> ObligationView<'_> {
        ObligationView {
            id: self.event.id,
            due: self.due,
            class: self.event.class,
            payload: self.event.payload,
            field: self.event.field,
            depends_on: &self.event.depends_on,
            location,
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
    near_queue: BinaryHeap<Reverse<Pending>>,
    far_queue: BinaryHeap<Reverse<Pending>>,
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
    /// Guaranteed near-lane capacity per tick — never a ceiling; leftover
    /// budget redistributes near-first (§2.4 three-phase policy).
    near_reserve: u32,
    stats: Stats,
    diag: Diagnostics,
    last_report: LaneReport,
    lane_totals: LaneTotals,
    /// Stage 4 suspended obligations, keyed by seq (canonical order —
    /// the authoritative held state). Identity-preserving: records keep
    /// their original `(due, seq)`, lane, schedule, payload, and
    /// dependencies; `stats.scheduled` is never decremented (suspension
    /// changes an obligation's location, not the fact it was scheduled).
    held: BTreeMap<u64, Pending>,
    /// Incrementally maintained `(due, seq)` index over the held set —
    /// updated only at suspension/resumption, so the watermark's held
    /// contribution is O(log n) and dormant cost never scales with the
    /// number of held obligations per tick (Stage 4 reviewer
    /// constraint). Derived state; the held map is canonical.
    held_index: BTreeSet<(Tick, u64)>,
    suspension: SuspensionStats,
}

impl<W: World> Spine<W> {
    pub fn new(world: W, budget: u32) -> Self {
        Self::with_diagnostics(world, budget, Diagnostics::new(Level::Off))
    }

    pub fn with_diagnostics(world: W, budget: u32, diag: Diagnostics) -> Self {
        Self::with_reservation_and_diagnostics(world, budget, budget, diag)
    }

    /// Stage 3 near/far lanes: `near_reserve` is the guaranteed near-lane
    /// capacity per tick, clamped to the total budget. The legacy
    /// constructors keep `near_reserve == budget`, which reproduces the
    /// Stage 0 single-lane behavior exactly.
    pub fn with_reservation(world: W, budget: u32, near_reserve: u32) -> Self {
        Self::with_reservation_and_diagnostics(
            world,
            budget,
            near_reserve,
            Diagnostics::new(Level::Off),
        )
    }

    pub fn with_reservation_and_diagnostics(
        world: W,
        budget: u32,
        near_reserve: u32,
        diag: Diagnostics,
    ) -> Self {
        let budget = budget.max(1);
        Spine {
            clock: Tick::ZERO,
            next_seq: 0,
            near_queue: BinaryHeap::new(),
            far_queue: BinaryHeap::new(),
            blocked: BTreeMap::new(),
            waiting_on: BTreeMap::new(),
            known: BTreeSet::new(),
            executed: BTreeSet::new(),
            ledger: Vec::new(),
            world,
            budget,
            near_reserve: near_reserve.min(budget),
            stats: Stats::default(),
            diag,
            last_report: LaneReport::default(),
            lane_totals: LaneTotals::default(),
            held: BTreeMap::new(),
            held_index: BTreeSet::new(),
            suspension: SuspensionStats::default(),
        }
    }

    // ------------------------------------------------------------- queries

    pub fn clock(&self) -> Tick {
        self.clock
    }

    pub fn budget(&self) -> u32 {
        self.budget
    }

    /// Guaranteed near-lane capacity per tick (Stage 3).
    pub fn near_reserve(&self) -> u32 {
        self.near_reserve
    }

    /// Events waiting in the near lane (due or not yet due).
    pub fn near_pending_count(&self) -> usize {
        self.near_queue.len()
    }

    /// Events waiting in the far lane (due or not yet due).
    pub fn far_pending_count(&self) -> usize {
        self.far_queue.len()
    }

    /// Observational per-tick lane report (Stage 3 diagnostics).
    pub fn lane_report(&self) -> LaneReport {
        self.last_report
    }

    /// Cumulative lane execution totals (observational, Stage 3).
    pub fn lane_totals(&self) -> LaneTotals {
        self.lane_totals
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

    /// Events waiting in either lane (due or not yet due).
    pub fn pending_count(&self) -> usize {
        self.near_queue.len() + self.far_queue.len()
    }

    /// Due events parked on unmet dependencies.
    pub fn blocked_count(&self) -> usize {
        self.blocked.len()
    }

    /// Suspended (held) obligations — authoritative unfinished work that
    /// cannot execute while suspended (Stage 4).
    pub fn held_count(&self) -> usize {
        self.held.len()
    }

    /// Read-only held-obligation views, in canonical seq order.
    pub fn held_view(&self) -> Vec<ObligationView<'_>> {
        self.held
            .values()
            .map(|pending| pending.view(ObligationLocation::Held))
            .collect()
    }

    /// Observational suspension counters (never fingerprinted).
    pub fn suspension_stats(&self) -> SuspensionStats {
        self.suspension
    }

    /// True while any obligation remains unfinished — runnable (queued
    /// or blocked) **or held**. [`Self::run_to_completion`] stops when
    /// no *runnable* work remains; held obligations are unfinished work
    /// that cannot execute while suspended — no runnable work is not no
    /// unfinished work (Stage 4 reviewer clarification C).
    pub fn has_unfinished_work(&self) -> bool {
        self.has_any_work() || !self.held.is_empty()
    }

    pub fn is_executed(&self, id: EventId) -> bool {
        self.executed.contains(&id.seq)
    }

    /// Maximum intended-due tick among executed events — a raw progress
    /// measure, **not** a completeness claim (review correction 1).
    pub fn max_executed_due(&self) -> Option<Tick> {
        self.ledger.iter().map(|r| r.intended_due).max()
    }

    /// Smallest intended due among pending events — queued, blocked, or
    /// held (suspended obligations are authoritative, so suspension can
    /// never let the safe watermark advance past them; Stage 4 reviewer
    /// clarification B). The held contribution reads the incrementally
    /// maintained `(due, seq)` index — O(log n), never a scan.
    pub fn min_pending_due(&self) -> Option<Tick> {
        let mut min = self
            .near_queue
            .peek()
            .map(|item| item.0.due)
            .into_iter()
            .chain(self.far_queue.peek().map(|item| item.0.due))
            .min();
        for event in self.blocked.values() {
            let due = event.schedule.due();
            min = Some(min.map_or(due, |m| m.min(due)));
        }
        if let Some((due, _)) = self.held_index.first() {
            min = Some(min.map_or(*due, |m| m.min(*due)));
        }
        min
    }

    /// Safe processed-through watermark (review correction 1): the largest
    /// tick `T` such that
    ///
    /// 1. `T <= max_executed_due` — completeness is never claimed beyond
    ///    work that has actually run, because a future late arrival may
    ///    still carry an earlier due; and
    /// 2. no pending event — queued, executing, blocked by unresolved
    ///    causal dependencies, **or held (suspended)** — has an intended
    ///    due `<= T`.
    ///
    /// Returns `None` when no such `T` exists: nothing has executed yet, or
    /// a pending event sits at tick 0 (independent audit 2026-10-19, item 1
    /// — the largest safe `T` would have to be negative, so no
    /// non-negative tick is provably complete).
    ///
    /// Computed live from queue state, so it may **retreat** when late
    /// events are scheduled; consumers must re-read, never cache. This is a
    /// **live progress measure, not irreversible finality** — it makes no
    /// permanence claim; durable finality belongs to the Commit Pipeline's
    /// confirmed lane, a separate subsystem. Stage 0 executes
    /// synchronously, so the "executing" state is observable only between
    /// ticks — where this query is meaningful.
    pub fn processed_through(&self) -> Option<Tick> {
        let max_exec = self.max_executed_due()?;
        match self.min_pending_due() {
            Some(min_pending) => {
                let due = min_pending.as_u64();
                if due == 0 {
                    None
                } else {
                    Some(Tick(max_exec.as_u64().min(due - 1)))
                }
            }
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
            field: spec.field,
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
        match event.field {
            Field::Near => self.near_queue.push(Reverse(Pending::new(event))),
            Field::Far => self.far_queue.push(Reverse(Pending::new(event))),
        }
        self.stats.scheduled += 1;
        Ok(id)
    }

    // ---------------------------------------------------------- suspension

    /// Suspends matching pending (queued or blocked) obligations into
    /// the held set — **identity-preserving** relocation (Stage 4
    /// reviewer clarification A): each record keeps its original
    /// `EventId`, due, class, schedule, payload, lane, and dependencies;
    /// nothing is re-scheduled, and `stats.scheduled` is never
    /// decremented (clarification B — suspension changes an obligation's
    /// location, not the fact it was scheduled).
    ///
    /// Suspending a *queued* event leaves `waiting_on` registrations
    /// intact, so dependents of the suspended event correctly remain
    /// blocked until it resumes and executes. Suspending a *blocked*
    /// event also removes its `waiting_on` registrations, keeping the
    /// dependency-release path hold-unaware; `resume` re-validates
    /// dependencies through the normal path. Held ids remain valid
    /// dependency targets (`known` is untouched). Already-held matches
    /// are no-ops. The caller's predicate decides what matches — the
    /// spine never assumes event classes mean regions (clarification B).
    /// Suspension happens between ticks; an executing event cannot be
    /// suspended.
    pub fn suspend_matching(&mut self, mut matches: impl FnMut(&ObligationView) -> bool) -> u32 {
        let mut suspended = 0u32;
        suspended += Self::suspend_from_queue(
            &mut self.near_queue,
            &mut self.held,
            &mut self.held_index,
            &mut matches,
        );
        suspended += Self::suspend_from_queue(
            &mut self.far_queue,
            &mut self.held,
            &mut self.held_index,
            &mut matches,
        );
        let blocked_matches: Vec<u64> = self
            .blocked
            .values()
            .filter(|event| matches(&event_view(event, ObligationLocation::Blocked)))
            .map(|event| event.id.seq)
            .collect();
        for seq in blocked_matches {
            if let Some(event) = self.blocked.remove(&seq) {
                // Leave the dependency-release path hold-unaware: drop
                // this event's registrations on its unexecuted deps.
                for dep in &event.depends_on {
                    if !self.executed.contains(&dep.seq)
                        && let Some(waiters) = self.waiting_on.get_mut(&dep.seq)
                    {
                        waiters.retain(|&waiter| waiter != seq);
                        if waiters.is_empty() {
                            self.waiting_on.remove(&dep.seq);
                        }
                    }
                }
                let pending = Pending::new(event);
                self.held_index.insert((pending.due, pending.seq));
                self.held.insert(pending.seq, pending);
                suspended += 1;
            }
        }
        self.suspension.suspended_total += u64::from(suspended);
        suspended
    }

    /// Returns matching held obligations to pending state with their
    /// original `(due, seq)`, lane, schedule, payload, and dependencies
    /// — identity preserved through the entire suspension. An event
    /// suspended while queued simply re-enters its lane with its
    /// original priority; an event suspended while blocked re-validates
    /// its dependencies — queueing if all have executed, re-entering the
    /// blocked set with fresh `waiting_on` registrations otherwise
    /// (execution statistics are untouched either way). Non-held
    /// matches are no-ops.
    ///
    /// Resumed obligations that became overdue during suspension keep
    /// their original due and record lateness upon actual execution —
    /// clock coherence is a property of the reconstructed state, never
    /// an erasure of historical due times (reviewer clarification C).
    pub fn resume_matching(&mut self, mut matches: impl FnMut(&ObligationView) -> bool) -> u32 {
        let to_resume: Vec<u64> = self
            .held
            .values()
            .filter(|pending| matches(&pending.view(ObligationLocation::Held)))
            .map(|pending| pending.seq)
            .collect();
        let mut resumed = 0u32;
        for seq in to_resume {
            if let Some(pending) = self.held.remove(&seq) {
                self.held_index.remove(&(pending.due, pending.seq));
                let unmet = self.unmet_dependencies(&pending.event);
                if unmet.is_empty() {
                    match pending.event.field {
                        Field::Near => self.near_queue.push(Reverse(pending)),
                        Field::Far => self.far_queue.push(Reverse(pending)),
                    }
                } else {
                    self.register_waiters(seq, &unmet);
                    self.blocked.insert(seq, pending.event);
                }
                resumed += 1;
            }
        }
        self.suspension.resumed_total += u64::from(resumed);
        resumed
    }

    /// Drains one lane, holding matching entries and re-pushing the
    /// rest. Heap layout is rebuild-order-dependent but every observable
    /// behavior (pop order, fingerprints) derives from the canonical
    /// `(due, seq)` keys, so a rebuild is invisible.
    fn suspend_from_queue(
        queue: &mut BinaryHeap<Reverse<Pending>>,
        held: &mut BTreeMap<u64, Pending>,
        held_index: &mut BTreeSet<(Tick, u64)>,
        matches: &mut impl FnMut(&ObligationView) -> bool,
    ) -> u32 {
        let mut suspended = 0u32;
        let mut retained: Vec<Reverse<Pending>> = Vec::with_capacity(queue.len());
        while let Some(Reverse(pending)) = queue.pop() {
            if matches(&pending.view(ObligationLocation::Queued)) {
                held_index.insert((pending.due, pending.seq));
                held.insert(pending.seq, pending);
                suspended += 1;
            } else {
                retained.push(Reverse(pending));
            }
        }
        for item in retained {
            queue.push(item);
        }
        suspended
    }

    // -------------------------------------------------------------- ticking

    /// Advances one tick and processes due events under the per-tick
    /// budget. The budget is a *mechanism*; the budget *value* is game
    /// policy (P5).
    ///
    /// §2.4 three-phase policy (reviewer clarification 2026-10-20): the
    /// near reservation is a **guaranteed allocation, not a ceiling** —
    /// (1) near-field events execute up to the reservation, (2) far-field
    /// events up to the nonreserved capacity, and (3) remaining budget
    /// redistributes near-first. With the legacy constructors
    /// (`near_reserve == budget`, all events Near by default) the three
    /// phases collapse to the Stage 0 single-queue behavior exactly.
    pub fn run_tick(&mut self) -> Tick {
        self.clock = self.clock.saturating_add(1);
        let near_guaranteed = self.drain_due(Field::Near, self.near_reserve);
        let far_guaranteed = self.drain_due(Field::Far, self.budget - self.near_reserve);
        let near_extra =
            self.drain_due(Field::Near, self.budget - near_guaranteed - far_guaranteed);
        let far_extra = self.drain_due(
            Field::Far,
            self.budget - near_guaranteed - far_guaranteed - near_extra,
        );
        let near_executed = near_guaranteed + near_extra;
        let far_executed = far_guaranteed + far_extra;
        let executed = near_executed + far_executed;
        let near_backlogged = self.lane_has_due_work(Field::Near);
        let far_backlogged = self.lane_has_due_work(Field::Far);
        if executed >= self.budget && (near_backlogged || far_backlogged) {
            self.stats.budget_exhausted_ticks += 1;
            self.diag.log(
                Level::Info,
                format!("budget exhausted at {} with due work remaining", self.clock),
            );
        }
        self.lane_totals.near_executed += u64::from(near_executed);
        self.lane_totals.far_executed += u64::from(far_executed);
        if near_backlogged {
            self.lane_totals.near_backlogged_ticks += 1;
        }
        if far_backlogged {
            self.lane_totals.far_backlogged_ticks += 1;
        }
        self.last_report = LaneReport {
            near_executed,
            far_executed,
            near_reserve: self.near_reserve,
            budget: self.budget,
            near_backlogged,
            far_backlogged,
        };
        self.clock
    }

    pub fn run_ticks(&mut self, ticks: u64) -> Tick {
        for _ in 0..ticks {
            self.run_tick();
        }
        self.clock
    }

    /// Runs until no *runnable* work remains (queue and blocked set both
    /// empty) or `max_ticks` elapse; returns the ticks consumed. Held
    /// (suspended) obligations are unfinished work that cannot execute
    /// while suspended — stopping does **not** imply all obligations
    /// have completed; see [`Self::has_unfinished_work`] (Stage 4
    /// reviewer clarification C: no runnable work ≠ no unfinished work).
    ///
    /// Termination is guaranteed only for **finite** event graphs: within a
    /// fixed scheduled set, dependency chains are acyclic by construction
    /// and the advancing clock eventually reaches every pending due. A
    /// `World` whose handlers unconditionally spawn follow-ups can keep the
    /// spine busy indefinitely — `max_ticks` is the bound that protects
    /// against that, and remaining work stays observable through
    /// `pending_count`/`blocked_count` rather than being assumed absent.
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
        // Schema v2: length-prefixed strings in Fingerprinter::write_str
        // (independent audit 2026-10-19, item 4).
        fp.write_str("engine-core/spine/2");
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
        // The two lanes merge into one (due, seq)-sorted view so the
        // Stage 0 schema hashes identical bytes for single-lane states.
        let mut queued: Vec<&Pending> = self
            .near_queue
            .iter()
            .chain(self.far_queue.iter())
            .map(|item| &item.0)
            .collect();
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
        fp.write_str("engine-core/state/2");
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

    /// Stage 3 lane-aware engine fingerprint (domain
    /// "engine-core/spine-lanes/1"): the authoritative engine state
    /// **including** the near/far lane classification and the reservation.
    /// The Stage 0 domain ("engine-core/spine/2") is deliberately
    /// untouched so its historical fingerprints stay byte-identical;
    /// this domain meaningfully represents the new authoritative state —
    /// different lane configurations or classifications produce
    /// different fingerprints (reviewer clarification 2026-10-20). Like
    /// every fingerprint here, it derives only from logical state:
    /// wall-clock time, rendering cadence, and unordered iteration never
    /// reach it.
    pub fn lane_fingerprint(&self) -> u64 {
        let mut fp = Fingerprinter::new();
        fp.write_str("engine-core/spine-lanes/1");
        fp.write_tick(self.clock);
        fp.write_u64(self.next_seq);
        fp.write_u32(self.budget);
        fp.write_u32(self.near_reserve);
        fp.write_u64(self.stats.scheduled);
        fp.write_u64(self.stats.executed);
        fp.write_u64(self.stats.late_arrivals);
        fp.write_u64(self.stats.late_executions);
        fp.write_u64(self.stats.blocked_transitions);
        fp.write_u64(self.stats.invalid_dropped);
        fp.write_u64(self.stats.budget_exhausted_ticks);

        // Each lane sorts canonically by (due, seq); the section itself
        // encodes the lane, so the classification is authoritative here.
        for (queue, field) in [
            (&self.near_queue, Field::Near),
            (&self.far_queue, Field::Far),
        ] {
            let mut lane: Vec<&Pending> = queue.iter().map(|item| &item.0).collect();
            lane.sort_by_key(|pending| (pending.due, pending.seq));
            fp.write_usize(lane.len());
            for pending in lane {
                fp.write_u64(pending.seq);
                fp.write_u8(field_byte(field));
                fp.write_u32(u32::from(pending.event.class.0));
                fp.write_u64(pending.event.payload);
                fingerprint_schedule(&mut fp, &pending.event.schedule);
                fingerprint_deps(&mut fp, &pending.event);
            }
        }

        fp.write_usize(self.blocked.len());
        for (seq, event) in &self.blocked {
            fp.write_u64(*seq);
            fp.write_u8(field_byte(event.field));
            fp.write_u32(u32::from(event.class.0));
            fp.write_u64(event.payload);
            fingerprint_schedule(&mut fp, &event.schedule);
            fingerprint_deps(&mut fp, event);
        }

        fp.write_usize(self.ledger.len());
        for record in &self.ledger {
            fp.write_u64(record.id.seq);
            fp.write_u8(field_byte(record.field));
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

    /// Stage 3 lane-aware state fingerprint (domain
    /// "engine-core/state-lanes/1"): the lane fingerprint plus world
    /// state.
    pub fn lane_state_fingerprint(&self) -> u64 {
        let mut fp = Fingerprinter::new();
        fp.write_str("engine-core/state-lanes/1");
        fp.write_u64(self.lane_fingerprint());
        self.world.fingerprint(&mut fp);
        fp.finish()
    }

    pub fn lane_fingerprint_hex(&self) -> String {
        format!("{:016x}", self.lane_fingerprint())
    }

    pub fn lane_state_fingerprint_hex(&self) -> String {
        format!("{:016x}", self.lane_state_fingerprint())
    }

    /// Stage 4 held-state fingerprint (domain "engine-core/spine-held/1"):
    /// the authoritative suspension state, canonically ordered by seq.
    /// The existing domains ("engine-core/spine/2", "spine-lanes/1")
    /// deliberately exclude the held set, so every historical baseline
    /// stays byte-identical when suspension is unused; authoritative
    /// Stage 4 comparisons compose this domain with the lane and world
    /// domains at the application level (the combined fingerprint lives
    /// in `engine-slice`, reviewer clarification C).
    pub fn held_fingerprint(&self) -> u64 {
        let mut fp = Fingerprinter::new();
        fp.write_str("engine-core/spine-held/1");
        fp.write_usize(self.held.len());
        for pending in self.held.values() {
            fp.write_u64(pending.seq);
            fp.write_u8(field_byte(pending.event.field));
            fp.write_u32(u32::from(pending.event.class.0));
            fp.write_u64(pending.event.payload);
            fingerprint_schedule(&mut fp, &pending.event.schedule);
            fingerprint_deps(&mut fp, &pending.event);
        }
        fp.finish()
    }

    pub fn held_fingerprint_hex(&self) -> String {
        format!("{:016x}", self.held_fingerprint())
    }

    // ------------------------------------------------------------ internals

    /// Drains due events from one lane, executing up to `cap`.
    /// Causality-blocked events park without consuming budget (no work
    /// was done) and keep their original `(due, seq)` priority on
    /// reentry.
    fn drain_due(&mut self, field: Field, cap: u32) -> u32 {
        let mut executed = 0;
        while executed < cap {
            let head = match field {
                Field::Near => self.near_queue.peek(),
                Field::Far => self.far_queue.peek(),
            };
            let Some(top) = head else { break };
            if top.0.due > self.clock {
                break;
            }
            let Reverse(pending) = match field {
                Field::Near => self.near_queue.pop(),
                Field::Far => self.far_queue.pop(),
            }
            .expect("queue head was just peeked");
            let event = pending.event;
            let unmet = self.unmet_dependencies(&event);
            if unmet.is_empty() {
                self.execute_now(event);
                executed += 1;
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
        executed
    }

    fn lane_has_due_work(&self, field: Field) -> bool {
        match field {
            Field::Near => self
                .near_queue
                .peek()
                .is_some_and(|item| item.0.due <= self.clock),
            Field::Far => self
                .far_queue
                .peek()
                .is_some_and(|item| item.0.due <= self.clock),
        }
    }

    fn execute_now(&mut self, event: Event) {
        let id = event.id;
        let executed_tick = self.clock;
        let due = event.schedule.due();
        let record = ExecutionRecord {
            id: event.id,
            class: event.class,
            field: event.field,
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
                field: event.field,
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
        match event.field {
            Field::Near => self.near_queue.push(Reverse(Pending::new(event))),
            Field::Far => self.far_queue.push(Reverse(Pending::new(event))),
        }
    }

    fn has_any_work(&self) -> bool {
        !self.near_queue.is_empty() || !self.far_queue.is_empty() || !self.blocked.is_empty()
    }
}

fn event_view(event: &Event, location: ObligationLocation) -> ObligationView<'_> {
    ObligationView {
        id: event.id,
        due: event.schedule.due(),
        class: event.class,
        payload: event.payload,
        field: event.field,
        depends_on: &event.depends_on,
        location,
    }
}

fn field_byte(field: Field) -> u8 {
    match field {
        Field::Near => 0,
        Field::Far => 1,
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
