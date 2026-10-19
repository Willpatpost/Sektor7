//! Events, schedules, and execution records.

use std::fmt;

use crate::clock::Tick;

/// Opaque event classification tag (game-defined).
///
/// Execution order is guaranteed only **within** a class — `(due, seq)`,
/// monotone and deterministic. Across classes no ordering is guaranteed;
/// causal ordering across classes requires explicit
/// [`EventSpec::depends_on`] links (`docs/TECH-ARCHITECTURE.md` §3.2,
/// review correction 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventClass(pub u16);

impl EventClass {
    pub const fn new(id: u16) -> Self {
        EventClass(id)
    }
}

impl fmt::Display for EventClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "c{}", self.0)
    }
}

/// Globally unique event identifier (monotonic sequence number).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventId {
    pub seq: u64,
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.seq)
    }
}

/// How an event's due tick is defined (`docs/TECH-ARCHITECTURE.md` §3.2,
/// review correction 3).
///
/// The spine never silently coerces one semantics into the other; producers
/// choose explicitly. Logical-time scheduling goes through
/// [`Spine::schedule`](crate::Spine::schedule); execution-relative follow-ups
/// go through [`ExecCtx::follow_up`](crate::ExecCtx::follow_up).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
    /// Logical scheduled-time semantics: the due tick is fixed at schedule
    /// time. If the event executes after `due`, the lateness is recorded —
    /// never hidden and never shifted.
    AtLogicalTime { due: Tick },
    /// Execution-relative semantics: due = `origin` + `delay`, where `origin`
    /// is the tick at which the *scheduling* event actually executed. A
    /// follow-up spawned by an overdue event anchors to actual execution,
    /// not to the overdue event's intended time.
    ExecutionRelative { origin: Tick, delay: u64 },
}

impl Schedule {
    pub fn due(&self) -> Tick {
        match *self {
            Schedule::AtLogicalTime { due } => due,
            Schedule::ExecutionRelative { origin, delay } => origin.saturating_add(delay),
        }
    }

    pub fn kind(&self) -> ScheduleKind {
        match *self {
            Schedule::AtLogicalTime { .. } => ScheduleKind::AtLogicalTime,
            Schedule::ExecutionRelative { .. } => ScheduleKind::ExecutionRelative,
        }
    }
}

/// Which schedule semantics produced an execution (ledger metadata).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleKind {
    AtLogicalTime,
    ExecutionRelative,
}

impl fmt::Display for ScheduleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScheduleKind::AtLogicalTime => write!(f, "at-time"),
            ScheduleKind::ExecutionRelative => write!(f, "exec-relative"),
        }
    }
}

/// A request to schedule an event (before the spine assigns an id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventSpec {
    pub class: EventClass,
    pub schedule: Schedule,
    /// Opaque payload token; meaning is game-side — the engine never
    /// inspects it.
    pub payload: u64,
    /// Cross-class causal prerequisites. Every id must have executed before
    /// this event runs. Ids must reference already-scheduled or
    /// already-executed events — the spine rejects unknown ids at schedule
    /// time, which makes dependency cycles structurally impossible.
    pub depends_on: Vec<EventId>,
}

impl EventSpec {
    pub fn new(class: EventClass, schedule: Schedule, payload: u64) -> Self {
        EventSpec {
            class,
            schedule,
            payload,
            depends_on: Vec::new(),
        }
    }

    pub fn with_dependencies(mut self, depends_on: Vec<EventId>) -> Self {
        self.depends_on = depends_on;
        self
    }
}

/// A scheduled event (id assigned by the spine).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: EventId,
    pub class: EventClass,
    pub schedule: Schedule,
    pub payload: u64,
    pub depends_on: Vec<EventId>,
}

/// Ledger entry recorded for every execution. Lateness is first-class:
/// `late` is true exactly when `executed_tick > intended_due`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionRecord {
    pub id: EventId,
    pub class: EventClass,
    pub intended_due: Tick,
    pub executed_tick: Tick,
    pub schedule_kind: ScheduleKind,
    pub late: bool,
}

impl fmt::Display for ExecutionRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} due={} executed={} {}{}",
            self.id,
            self.class,
            self.intended_due,
            self.executed_tick,
            self.schedule_kind,
            if self.late { " LATE" } else { "" }
        )
    }
}
