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
use crate::sha256::{Sha256, digest};
use crate::wire::{
    Decoder, Encoder, MAX_BYTE_LEN, MAX_COLLECTION_COUNT, MAX_TAG_LEN, SAVE_DOMAIN, SAVE_DOMAIN_V2,
    SAVE_FORMAT_VERSION, SAVE_FORMAT_VERSION_V2, SPINE_DOMAIN, SPINE_HISTORY_DOMAIN, SaveError,
    SaveWorld,
};

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
    /// The complement-of-unfinished executed-membership view: the
    /// unfinished-id index plus the allocation frontier. Identical
    /// semantics to the replaced per-identity executed set (the E3
    /// selection — see [`Spine::is_executed`]).
    unfinished: &'a BTreeSet<u64>,
    next_seq: u64,
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
        id.seq < self.next_seq && !self.unfinished.contains(&id.seq)
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

    /// Schedules an execution-relative follow-up in an **explicit lane**
    /// — the Stage 3 "deferred until a use case exists" override, whose
    /// use case is Stage 4's dynamic near/far classification: the
    /// workload classifies each *new* follow-up under the region's
    /// current policy, while live obligations keep their creation-time
    /// lane (reclassified only through the explicit transition
    /// operation, [`Spine::reclassify_matching`]). Identity,
    /// dependencies, and accounting are identical to [`Self::follow_up`].
    pub fn follow_up_in_field(
        &mut self,
        field: Field,
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
            field,
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

/// The spine's execution-history representation (PP-B2, the ratified
/// v1.8 §5.6.1 `HistoryKind`). `Full` retains every
/// [`ExecutionRecord`]; `Compacted` replaced the first
/// `summarized_records` records with the authenticated SHA-256 prefix
/// summary — a derivation witness that authenticates *what was
/// discarded*, never a reconstruction of it — and retains the tail
/// verbatim. The Spine's logical state (scheduling, conservation,
/// watermarks, every fingerprint on `Full` instances) is identical
/// under both representations; only per-record history retention
/// differs. Discarded records are never reconstructible: the
/// full-history APIs surface their unavailability loudly, and the
/// safe accessors ([`Spine::ledger_retained`],
/// [`Spine::try_ledger_full`], the `try_*` fingerprint variants)
/// report it without crashing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryKind {
    Full,
    Compacted {
        /// Records below the compaction cut, summarized into the
        /// prefix summary (cumulative across repeated compactions).
        summarized_records: u64,
        /// The retained tail's record count (always the live ledger
        /// length).
        retained_records: u64,
        /// The authenticated SHA-256 prefix-summary chain over the
        /// summarized records' canonical bytes, seeded from the
        /// spine-history domain tag and chained across repeated
        /// compactions.
        prefix_summary: [u8; 32],
    },
}

/// Typed refusals of the explicit compaction operation
/// ([`Spine::compact`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionError {
    /// The requested retained window covers the whole ledger —
    /// nothing would be summarized, and a compaction that summarizes
    /// nothing is not a compaction (the `spine-history/2` form
    /// requires `summarized_records >= 1`, keeping the full and
    /// compacted forms honestly disjoint).
    NothingToSummarize { ledger_len: u64, retain: u64 },
    /// The requested retained window exceeds the ledger length — the
    /// explicit operation takes an exact window, never a clamped one.
    RetainExceedsLedger { ledger_len: u64, retain: u64 },
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

/// Observational suspension and reclassification counters (Stage 4
/// diagnostics — like `LaneTotals`, never fingerprinted; the held set
/// itself is the authoritative suspended state).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SuspensionStats {
    /// Total obligations moved into the held set.
    pub suspended_total: u64,
    /// Total obligations returned from the held set.
    pub resumed_total: u64,
    /// Total obligations created directly in the held state
    /// ([`Spine::schedule_held`] — new work targeting dormant regions).
    pub scheduled_held_total: u64,
    /// Total queued/blocked obligations moved between lanes
    /// ([`Spine::reclassify_matching`]).
    pub reclassified_total: u64,
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
    /// The obligation's FULL schedule form (Batch 5's lifecycle
    /// corrective: the collected obligation's exact semantics —
    /// logical-time or execution-relative — must survive the plan's
    /// owned form, so a recovered chain resumes with its original
    /// anchoring, never a flattened due).
    pub schedule: Schedule,
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
            schedule: self.event.schedule,
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

/// The stored compacted-history state (see [`HistoryKind`]): the
/// cumulative summarized-record count and the authenticated prefix
/// summary. The retained-record count is always the live ledger
/// length, so it is derived, never stored.
struct CompactedHistory {
    summarized_records: u64,
    prefix_summary: [u8; 32],
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
    /// The unfinished-id index — every identity currently holding an
    /// obligation (queued, blocked, or held). Under the ratified
    /// dense-allocation invariant (`known == [0, next_seq)` exactly,
    /// allocation is cancellation-free) executed-membership is the
    /// COMPLEMENT: a seq is executed iff `seq < next_seq` and not in
    /// this index. The index is bounded by live work, never by
    /// lifetime executions — the v1.8 §5.6 erratum E3 selection,
    /// which replaces the per-identity `known`/`executed` membership
    /// sets (one entry per ever-allocated id — lifetime-growing)
    /// with a bounds check plus this index. Representation-internal
    /// and fingerprint-invisible: no Stage 0–4 domain hashed either
    /// set, so every historical pin is mechanically preserved.
    unfinished: BTreeSet<u64>,
    ledger: Vec<ExecutionRecord>,
    /// The compacted-history state; `None` is `HistoryKind::Full`.
    /// The retained-record count is always the live ledger length
    /// (execution appends to the tail), so only the cumulative
    /// summarized count and the authenticated prefix summary are
    /// stored.
    compacted: Option<CompactedHistory>,
    /// Incrementally maintained maximum intended-due among executed
    /// events — updated at the single ledger-append site, so
    /// `max_executed_due()` is O(1) instead of a scan over the
    /// ever-growing execution history (the graphical overlay queries it
    /// and `processed_through()` every frame — Stage 4
    /// final-verification finding). Derived state, value-identical to
    /// the ledger scan; the ledger stays canonical and neither is
    /// fingerprinted (the `held_index` precedent).
    max_executed_index: Option<Tick>,
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
            unfinished: BTreeSet::new(),
            ledger: Vec::new(),
            compacted: None,
            max_executed_index: None,
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

    /// The full execution history — **full-history API** (the v1.8
    /// §5.6.1 contract): keeps its exact historical meaning on every
    /// instance and never silently returns a retained tail. On a
    /// compacted instance the full history is not retained, so
    /// calling this is a documented contract violation surfaced
    /// LOUDLY — a panic naming the safe accessors — never a silently
    /// misleading value. Production code uses [`Self::history_kind`],
    /// [`Self::ledger_retained`], or [`Self::try_ledger_full`].
    pub fn ledger(&self) -> &[ExecutionRecord] {
        self.require_full_history("ledger()", "ledger_retained() / try_ledger_full()");
        &self.ledger
    }

    /// The spine's execution-history representation (the capability
    /// check every history-touching call site should consult first).
    pub fn history_kind(&self) -> HistoryKind {
        match &self.compacted {
            None => HistoryKind::Full,
            Some(compacted) => HistoryKind::Compacted {
                summarized_records: compacted.summarized_records,
                retained_records: self.ledger.len() as u64,
                prefix_summary: compacted.prefix_summary,
            },
        }
    }

    /// The retained execution records — the full ledger on a `Full`
    /// instance, the verbatim tail on a `Compacted` one (the
    /// explicitly named safe accessor; identical to [`Self::ledger`]
    /// on `Full`).
    pub fn ledger_retained(&self) -> &[ExecutionRecord] {
        &self.ledger
    }

    /// The safe full-history request: `Some` on `Full`, `None` on
    /// `Compacted` — reports unavailability, never crashes, never
    /// fabricates.
    pub fn try_ledger_full(&self) -> Option<&[ExecutionRecord]> {
        if self.compacted.is_some() {
            None
        } else {
            Some(&self.ledger)
        }
    }

    /// The loud full-history guard (the fail-visible contract — see
    /// [`Self::ledger`]).
    fn require_full_history(&self, api: &'static str, remedy: &'static str) {
        if self.compacted.is_some() {
            panic!(
                "{api} requires the full execution history, which this compacted spine does \
                 not retain (history_kind() is Compacted) — use {remedy}, or the try_* \
                 fingerprint variants for the fingerprint domains; calling a full-history \
                 API on a compacted instance is a documented contract violation surfaced \
                 loudly, never a silently misleading value"
            );
        }
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
        self.is_executed_seq(id.seq)
    }

    /// Executed-membership by seq — the complement of unfinished
    /// under the dense-allocation invariant: an allocated identity is
    /// exactly one of Executed/Queued/Blocked/Held at every stable
    /// boundary, so `seq < next_seq ∧ seq ∉ unfinished` is executed
    /// (the E3 representation; identical semantics to the replaced
    /// per-identity set).
    fn is_executed_seq(&self, seq: u64) -> bool {
        seq < self.next_seq && !self.unfinished.contains(&seq)
    }

    /// Maximum intended-due tick among executed events — a raw progress
    /// measure, **not** a completeness claim (review correction 1).
    /// O(1): read from the incrementally maintained index updated at the
    /// ledger-append site (Stage 4 final-verification finding — the
    /// graphical overlay queries this and `processed_through()` every
    /// frame, and a ledger scan would scale per-frame cost with the
    /// total execution history).
    pub fn max_executed_due(&self) -> Option<Tick> {
        self.max_executed_index
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
        // Dense allocation issues exactly `[0, next_seq)`, so `known`
        // is the frontier bounds check (the E3 representation).
        if let Some(unknown) = spec.depends_on.iter().find(|id| id.seq >= self.next_seq) {
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
        self.unfinished.insert(seq);
        match event.field {
            Field::Near => self.near_queue.push(Reverse(Pending::new(event))),
            Field::Far => self.far_queue.push(Reverse(Pending::new(event))),
        }
        self.stats.scheduled += 1;
        Ok(id)
    }

    /// Schedules an obligation directly into the held (suspended) state
    /// — for new work targeting dormant regions (Stage 4 Batch 2
    /// priority B): it cannot execute while held, so nothing resumes
    /// ordinary processing unnoticed; it resumes with full identity
    /// when its region reactivates, and it is accounted at creation
    /// (both `scheduled` and the held set count it — the conservation
    /// identity holds unchanged). Dependency validation and late-
    /// arrival accounting are identical to [`Self::schedule`].
    pub fn schedule_held(&mut self, spec: EventSpec) -> Result<EventId, ScheduleError> {
        // The dense-allocation bounds check (the E3 representation).
        if let Some(unknown) = spec.depends_on.iter().find(|id| id.seq >= self.next_seq) {
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
        let pending = Pending::new(event);
        if pending.due < self.clock {
            self.stats.late_arrivals += 1;
            self.diag.log(
                Level::Warn,
                format!(
                    "late arrival {id}: due {} behind clock {} — completeness claims above {} are invalidated",
                    pending.due, self.clock, pending.due
                ),
            );
        }
        self.unfinished.insert(seq);
        self.held_index.insert((pending.due, pending.seq));
        self.held.insert(pending.seq, pending);
        self.stats.scheduled += 1;
        self.suspension.scheduled_held_total += 1;
        Ok(id)
    }

    // ---------------------------------------------------------- suspension

    /// Read-only enumeration of the queued and blocked obligations a
    /// subsequent [`Self::suspend_matching`] with the same predicate
    /// would move — the six-phase protocol's non-destructive
    /// preparation query (the v1.8 proposal §5.5.1: a
    /// `DeactivationPlan` builds from read-only sources only, so
    /// phases 1–2 cost nothing and a pre-durable abort leaves the
    /// pre-transition state intact). No mutation, no accounting
    /// change, no suspension stats; held obligations are excluded —
    /// suspension moves only queued and blocked entries. The result
    /// is canonically ordered by `(due, seq)` — the spine's priority
    /// key — so binary-heap layout never reaches the caller (the same
    /// rebuild-invisibility discipline as the suspension mechanics
    /// themselves).
    pub fn collect_matching(
        &self,
        matches: impl Fn(&ObligationView) -> bool,
    ) -> Vec<ObligationView<'_>> {
        let mut collected = Vec::new();
        for Reverse(pending) in self.near_queue.iter() {
            let view = pending.view(ObligationLocation::Queued);
            if matches(&view) {
                collected.push(view);
            }
        }
        for Reverse(pending) in self.far_queue.iter() {
            let view = pending.view(ObligationLocation::Queued);
            if matches(&view) {
                collected.push(view);
            }
        }
        for event in self.blocked.values() {
            let view = event_view(event, ObligationLocation::Blocked);
            if matches(&view) {
                collected.push(view);
            }
        }
        collected.sort_by_key(|view| (view.due, view.id.seq));
        collected
    }

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
                    if !self.is_executed_seq(dep.seq)
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

    /// The id-exact batch variant of [`Self::suspend_matching`] (the
    /// v1.8 proposal §5.5.2's staged-install step): suspends exactly
    /// the obligations a previously collected plan names, with all the
    /// identity-preserving mechanics of the predicate form. Ids not
    /// currently queued or blocked (already executed, already held,
    /// or never allocated) are not moved and not counted — the
    /// caller's preparation validated presence against
    /// [`Self::collect_matching`], so a short count is the
    /// staged-commit discipline's defensive-failure signal, never a
    /// silent partial install.
    pub fn suspend_ids(&mut self, ids: &[EventId]) -> u32 {
        let targets: BTreeSet<u64> = ids.iter().map(|id| id.seq).collect();
        self.suspend_matching(|view| targets.contains(&view.id.seq))
    }

    /// The recovery's id-exact held reconstruction (the v1.8 §5.5.1
    /// case 6 load path, never a live-mutation scheduling API):
    /// REMOVE every queued, blocked, AND held obligation matching
    /// `matches` — the stale pre-crash chains, superseded by the
    /// journal's plan data — then INSERT `events` as held under
    /// their OWN identities (the dormant region's chains exactly as
    /// the deactivation captured them). The scheduled statistic
    /// adjusts by the replacement delta so the conservation identity
    /// holds. Returns the inserted count.
    pub fn reconstruct_held(
        &mut self,
        mut matches: impl FnMut(&ObligationView) -> bool,
        events: Vec<Event>,
    ) -> u32 {
        let mut removed = 0u64;
        removed += u64::from(Self::drop_from_queue(
            &mut self.near_queue,
            &mut self.unfinished,
            &mut matches,
        ));
        removed += u64::from(Self::drop_from_queue(
            &mut self.far_queue,
            &mut self.unfinished,
            &mut matches,
        ));
        let blocked_drop: Vec<u64> = self
            .blocked
            .values()
            .filter(|event| matches(&event_view(event, ObligationLocation::Blocked)))
            .map(|event| event.id.seq)
            .collect();
        for seq in blocked_drop {
            if let Some(event) = self.blocked.remove(&seq) {
                // The suspend path's discipline: leave the
                // dependency-release path unaware of the removal.
                for dep in &event.depends_on {
                    if !self.is_executed_seq(dep.seq)
                        && let Some(waiters) = self.waiting_on.get_mut(&dep.seq)
                    {
                        waiters.retain(|&waiter| waiter != seq);
                        if waiters.is_empty() {
                            self.waiting_on.remove(&dep.seq);
                        }
                    }
                }
                self.unfinished.remove(&seq);
                removed += 1;
            }
        }
        let held_drop: Vec<u64> = self
            .held
            .values()
            .filter(|pending| matches(&pending.view(ObligationLocation::Held)))
            .map(|pending| pending.seq)
            .collect();
        for seq in held_drop {
            if let Some(pending) = self.held.remove(&seq) {
                self.held_index.remove(&(pending.due, pending.seq));
                self.unfinished.remove(&seq);
                removed += 1;
            }
        }
        let mut inserted = 0u32;
        for event in events {
            let pending = Pending::new(event);
            self.held_index.insert((pending.due, pending.seq));
            self.unfinished.insert(pending.seq);
            self.held.insert(pending.seq, pending);
            inserted += 1;
        }
        // The replacement semantics: the conservation identity holds
        // with the scheduled statistic adjusted by the delta.
        self.stats.scheduled = self
            .stats
            .scheduled
            .saturating_sub(removed)
            .checked_add(u64::from(inserted))
            .expect("the scheduled count is bounded");
        inserted
    }

    /// Drops matching entries from one lane (the reconstruction's
    /// removal half — the same rebuild-invisibility argument as the
    /// suspension drain: observable order derives from the canonical
    /// `(due, seq)` keys). A dropped obligation leaves the unfinished
    /// index — it is neither queued nor held anymore, and the
    /// reconstruction's replacement insertions re-enter it.
    fn drop_from_queue(
        queue: &mut BinaryHeap<Reverse<Pending>>,
        unfinished: &mut BTreeSet<u64>,
        matches: &mut impl FnMut(&ObligationView) -> bool,
    ) -> u32 {
        let mut dropped = 0u32;
        let mut retained: Vec<Reverse<Pending>> = Vec::with_capacity(queue.len());
        while let Some(Reverse(pending)) = queue.pop() {
            if matches(&pending.view(ObligationLocation::Queued)) {
                unfinished.remove(&pending.seq);
                dropped += 1;
            } else {
                retained.push(Reverse(pending));
            }
        }
        for item in retained {
            queue.push(item);
        }
        dropped
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

    /// Moves matching queued and blocked obligations to the target lane,
    /// **preserving full identity** — `(due, seq)`, schedule, payload,
    /// and dependencies (Stage 4 Batch 2 priority C: dynamic near/far
    /// classification, so a newly player-proximate region is protected
    /// by the near reservation immediately instead of starving behind
    /// far-lane overload for the remainder of a long event-driven
    /// cadence). The execution ledger records the lane at execution
    /// time, as always. Conservation is untouched — obligations stay
    /// queued/blocked; only their lane changes. Matching events already
    /// in the target lane are left as they are (not counted). Held
    /// obligations are **not** reclassified here: `resume` restores
    /// their original lane, and the caller reclassifies after
    /// resumption if the region's policy changed.
    pub fn reclassify_matching(
        &mut self,
        mut matches: impl FnMut(&ObligationView) -> bool,
        field: Field,
    ) -> u32 {
        let mut moved: Vec<Pending> = Vec::new();
        Self::reclassify_queue(
            &mut self.near_queue,
            &mut matches,
            field,
            Field::Near,
            &mut moved,
        );
        Self::reclassify_queue(
            &mut self.far_queue,
            &mut matches,
            field,
            Field::Far,
            &mut moved,
        );
        let mut reclassified = moved.len() as u32;
        for mut pending in moved {
            pending.event.field = field;
            match field {
                Field::Near => self.near_queue.push(Reverse(pending)),
                Field::Far => self.far_queue.push(Reverse(pending)),
            }
        }
        for event in self.blocked.values_mut() {
            if event.field != field && matches(&event_view(event, ObligationLocation::Blocked)) {
                event.field = field;
                reclassified += 1;
            }
        }
        self.suspension.reclassified_total += u64::from(reclassified);
        reclassified
    }

    /// Drains one lane, collecting matching obligations into `moved`
    /// (the caller re-lanes and re-pushes them) and re-pushing the rest.
    /// Same rebuild-invisibility argument as [`Self::suspend_from_queue`]:
    /// observable order derives from the canonical `(due, seq)` keys.
    fn reclassify_queue(
        queue: &mut BinaryHeap<Reverse<Pending>>,
        matches: &mut impl FnMut(&ObligationView) -> bool,
        target: Field,
        source: Field,
        moved: &mut Vec<Pending>,
    ) {
        if target == source {
            return;
        }
        let mut retained: Vec<Reverse<Pending>> = Vec::with_capacity(queue.len());
        while let Some(Reverse(pending)) = queue.pop() {
            if matches(&pending.view(ObligationLocation::Queued)) {
                moved.push(pending);
            } else {
                retained.push(Reverse(pending));
            }
        }
        for item in retained {
            queue.push(item);
        }
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

    /// Runs until no *progressible* work remains (the queues are empty —
    /// a blocked obligation whose unmet dependencies all resolve to
    /// held, or to blocked chains beneath held roots, can never execute,
    /// so advancing campaign time would be pointless — Batch 1 review
    /// item C) or `max_ticks` elapse; returns the ticks consumed. Held
    /// obligations are unfinished but not executable, and stopping never
    /// implies all obligations completed — see
    /// [`Self::has_unfinished_work`] (no runnable work ≠ no unfinished
    /// work).
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
        while used < max_ticks && self.has_progressible_work() {
            self.run_tick();
            used += 1;
        }
        used
    }

    // ------------------------------------------------------------ compaction

    /// The explicit, versioned history compaction (PP-B2 — the
    /// ratified v1.8 §5.6 / D-PP-3): summarizes the first
    /// `ledger.len() - retain` execution records into the
    /// authenticated SHA-256 prefix summary and retains the last
    /// `retain` records verbatim. The logical state — scheduling,
    /// conservation, watermarks, every active structure — is
    /// untouched; only per-record history retention changes, and the
    /// spine's subsequent saves take the `spine-history/2` form.
    ///
    /// **THE AUTHENTICATED PREFIX SUMMARY**: a running SHA-256 chain
    /// over the discarded records' canonical wire bytes — the first
    /// compaction seeds from the spine-history domain tag, a repeated
    /// compaction chains from the prior summary, so successive
    /// compactions authenticate end-to-end and two independent
    /// compactions of the same history produce identical summaries.
    /// The summary is a derivation witness: it authenticates *what*
    /// was discarded; it never reconstructs it.
    ///
    /// **THE ELIGIBILITY DISCIPLINE**: this method is the spine-side
    /// mechanism — any prefix cut of a live ledger is structurally
    /// valid (the retained tail satisfies every load gate). The
    /// ratified eligibility conditions (which identities' consequence
    /// lane state may be summarized — the B4 extension) are the
    /// application compaction driver's to verify before calling this;
    /// compaction is an explicit operation, never a silent default.
    pub fn compact(&mut self, retain: u64) -> Result<HistoryKind, CompactionError> {
        let ledger_len = self.ledger.len() as u64;
        if retain > ledger_len {
            return Err(CompactionError::RetainExceedsLedger { ledger_len, retain });
        }
        let cut = ledger_len - retain;
        if cut == 0 {
            return Err(CompactionError::NothingToSummarize { ledger_len, retain });
        }
        let mut chain = match &self.compacted {
            Some(prior) => prior.prefix_summary,
            None => digest(SPINE_HISTORY_DOMAIN.as_bytes()),
        };
        for record in &self.ledger[..cut as usize] {
            let mut hasher = Sha256::new();
            hasher.update(&chain);
            hasher.update(&record_wire_bytes(record));
            chain = hasher.finish();
        }
        let tail = self.ledger.split_off(cut as usize);
        self.ledger = tail;
        let summarized_records = match &self.compacted {
            Some(prior) => prior.summarized_records + cut,
            None => cut,
        };
        self.compacted = Some(CompactedHistory {
            summarized_records,
            prefix_summary: chain,
        });
        Ok(self.history_kind())
    }

    // ------------------------------------------------------- fingerprints

    /// Fingerprint of engine-logical state (clock, budget, stats, pending
    /// queue, blocked set, ledger). Excludes diagnostics (observational)
    /// and the world — see [`Self::state_fingerprint`]. **Full-history
    /// API**: hashes the execution ledger, so a compacted instance —
    /// which does not retain it — surfaces the call loudly (see
    /// [`Self::ledger`]); the safe path is [`Self::try_engine_fingerprint`].
    pub fn engine_fingerprint(&self) -> u64 {
        self.require_full_history("engine_fingerprint()", "try_engine_fingerprint()");
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
    /// state, in the E-07…E-10 double-run tradition. **Full-history
    /// API** (composes [`Self::engine_fingerprint`]): the safe path on
    /// a compacted instance is [`Self::try_state_fingerprint`].
    pub fn state_fingerprint(&self) -> u64 {
        self.require_full_history("state_fingerprint()", "try_state_fingerprint()");
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
        self.require_full_history("lane_fingerprint()", "try_lane_fingerprint()");
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
    /// state. **Full-history API** (composes
    /// [`Self::lane_fingerprint`]): the safe path on a compacted
    /// instance is [`Self::try_lane_state_fingerprint`].
    pub fn lane_state_fingerprint(&self) -> u64 {
        self.require_full_history("lane_state_fingerprint()", "try_lane_state_fingerprint()");
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

    // --------------------------------------------- history-safe variants

    /// The safe engine-fingerprint path: `Some` on `Full` (identical
    /// to [`Self::engine_fingerprint`]), `None` on `Compacted` —
    /// reports unavailability, never crashes, never fabricates (the
    /// v1.8 §5.6.1 `try_*` contract).
    pub fn try_engine_fingerprint(&self) -> Option<u64> {
        if self.compacted.is_some() {
            None
        } else {
            Some(self.engine_fingerprint())
        }
    }

    /// The safe lane-fingerprint path (see
    /// [`Self::try_engine_fingerprint`]).
    pub fn try_lane_fingerprint(&self) -> Option<u64> {
        if self.compacted.is_some() {
            None
        } else {
            Some(self.lane_fingerprint())
        }
    }

    /// The safe state-fingerprint path (see
    /// [`Self::try_engine_fingerprint`]).
    pub fn try_state_fingerprint(&self) -> Option<u64> {
        if self.compacted.is_some() {
            None
        } else {
            Some(self.state_fingerprint())
        }
    }

    /// The safe lane-state-fingerprint path (see
    /// [`Self::try_engine_fingerprint`]).
    pub fn try_lane_state_fingerprint(&self) -> Option<u64> {
        if self.compacted.is_some() {
            None
        } else {
            Some(self.lane_state_fingerprint())
        }
    }

    // -------------------------------------------------- compacted domains

    /// The compacted-history fingerprint (domain
    /// "engine-core/spine-compacted/1" — the ratified v1.8 §5.6.1
    /// domain): the domain tag, clock, `next_seq`, budget,
    /// near-reserve, every Stats counter, both lane queues (canonical
    /// `(due, seq)` order, the lane-aware entry encoding), blocked,
    /// held, the prefix-summary bytes, `summarized_records`,
    /// `retained_records`, the retained tail records (the lane-aware
    /// record encoding), and a SHA-256 digest of the unfinished-id
    /// index. **Comparison semantics**: two compacted instances agree
    /// iff the hashed content agrees — the same logical state AND the
    /// same compaction cut; an uncompacted instance and its compacted
    /// image never compare through this domain (their equivalence is
    /// behavioral, established by the twin-continuation protocol).
    /// **Compacted-domain API**: requires the compacted representation
    /// — a `Full` instance surfaces the call loudly (the symmetric
    /// fail-visible discipline).
    pub fn compacted_fingerprint(&self) -> u64 {
        self.require_compacted("compacted_fingerprint()");
        let Some(compacted) = &self.compacted else {
            unreachable!("require_compacted enforced the variant")
        };
        let mut fp = Fingerprinter::new();
        fp.write_str("engine-core/spine-compacted/1");
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
        fp.write_usize(self.held.len());
        for pending in self.held.values() {
            fp.write_u64(pending.seq);
            fp.write_u8(field_byte(pending.event.field));
            fp.write_u32(u32::from(pending.event.class.0));
            fp.write_u64(pending.event.payload);
            fingerprint_schedule(&mut fp, &pending.event.schedule);
            fingerprint_deps(&mut fp, &pending.event);
        }
        fp.write_bytes(&compacted.prefix_summary);
        fp.write_u64(compacted.summarized_records);
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
        // The unfinished-id digest — SHA-256 over the ascending ids
        // (the complement representation's authoritative membership
        // index, hashed per the v1.8 §5.6.1 domain definition).
        let mut hasher = Sha256::new();
        for seq in &self.unfinished {
            hasher.update(&seq.to_le_bytes());
        }
        fp.write_bytes(&hasher.finish());
        fp.finish()
    }

    /// The compacted-history state fingerprint (domain
    /// "engine-core/state-compacted/1"): the compacted fingerprint
    /// plus world state.
    pub fn compacted_state_fingerprint(&self) -> u64 {
        self.require_compacted("compacted_state_fingerprint()");
        let mut fp = Fingerprinter::new();
        fp.write_str("engine-core/state-compacted/1");
        fp.write_u64(self.compacted_fingerprint());
        self.world.fingerprint(&mut fp);
        fp.finish()
    }

    pub fn compacted_fingerprint_hex(&self) -> String {
        format!("{:016x}", self.compacted_fingerprint())
    }

    pub fn compacted_state_fingerprint_hex(&self) -> String {
        format!("{:016x}", self.compacted_state_fingerprint())
    }

    /// The loud compacted-domain guard (the symmetric discipline — a
    /// `Full` instance has no compaction cut to compare through).
    fn require_compacted(&self, api: &'static str) {
        if self.compacted.is_none() {
            panic!(
                "{api} compares through the compacted-history domains, which require the \
                 spine-history/2 representation (history_kind() is Full) — compact first via \
                 Spine::compact, or compare Full instances through the existing fingerprint \
                 domains; this call is a documented contract violation surfaced loudly, \
                 never a silently misleading value"
            );
        }
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
        // The derived max-intended-due index (O(1) per execution — the
        // overlay queries it every frame; a ledger scan would scale
        // per-frame cost with total execution history).
        self.max_executed_index = Some(match self.max_executed_index {
            Some(max) if max >= due => max,
            _ => due,
        });
        // The event leaves the unfinished index BEFORE the handler
        // runs, so follow-ups may declare dependencies on the event
        // currently executing (the complement representation's form
        // of the old executed-set insert — same sequencing, same
        // semantics).
        self.unfinished.remove(&event.id.seq);
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
            let unfinished = &self.unfinished;
            let next_seq = self.next_seq;
            let mut ctx = ExecCtx {
                executed_tick,
                unfinished,
                next_seq,
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
            .filter(|id| !self.is_executed_seq(id.seq))
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

    /// True while work exists that can eventually execute: queued events
    /// — the advancing clock reaches their dues, and executing them
    /// releases blocked dependents. A blocked-only graph whose unmet
    /// dependencies resolve to held obligations can make no progress
    /// (no execution can ever occur), so `run_to_completion` stops
    /// rather than burning its tick budget on motionless time (Batch 1
    /// review item C).
    fn has_progressible_work(&self) -> bool {
        !self.near_queue.is_empty() || !self.far_queue.is_empty()
    }
}

fn event_view(event: &Event, location: ObligationLocation) -> ObligationView<'_> {
    ObligationView {
        id: event.id,
        due: event.schedule.due(),
        schedule: event.schedule,
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

// ------------------------------------------------------------ save/load
//
// Stage 5 Batch 1 (`docs/STAGE5-PERSISTENCE-ARCHITECTURE-PROPOSAL.md`
// §5.1/§5.2; the frozen layout lives in `docs/STAGE5-B1-WIRE-FORMAT.md`):
// the full-fidelity reference encoding of authoritative spine state,
// fail-closed load-time integrity gates, and derived-state
// reconstruction.
//
// Authoritative on the wire: clock, `next_seq`, budget and reservation,
// every Stats counter, both lane queues (canonical `(due, seq)` order —
// heap layout is rebuild-dependent and invisible), the blocked set (seq
// order), the held set (seq order), and the full execution ledger
// (execution order). Derived — `known` (dense allocation), `executed`
// (the complement of unfinished), `waiting_on`, `held_index`,
// `max_executed_index` — is rebuilt from those canonical sources through
// the same code paths the live spine uses. Observational — `LaneReport`,
// `LaneTotals`, `SuspensionStats`, `Diagnostics` — is not logical state:
// a restored spine resumes them at defaults, they never affect
// scheduling, accounting, or fingerprints, and the boundary is documented
// in the wire spec.

/// Minimum encoded size of one obligation entry (wire-format spec
/// §maxima): seq 8 + class 4 + field 1 + schedule kind 1 + due 8 +
/// payload 8 + dependency count 8.
const ENTRY_MIN_BYTES: u64 = 38;

/// Encoded size of one execution-record entry: seq 8 + class 4 + field
/// 1 + intended due 8 + executed tick 8 + schedule kind 1 + late flag
/// 1.
const RECORD_BYTES: u64 = 31;

/// Encoded size of one dependency identity (a bare seq).
const DEP_BYTES: u64 = 8;

impl<W: SaveWorld> Spine<W> {
    /// Encodes the complete save — header, spine section, world
    /// section — into `enc`, so a save can nest inside larger canonical
    /// streams (the revision objects of later batches). The outer
    /// domain and version select the representation: `sektor7/save/1`
    /// (the full-fidelity reference) for a `Full` history,
    /// `sektor7/save/2` (`spine-history/2`) for a compacted one — the
    /// forms reject one another at load, never silently reinterpreted.
    pub fn save(&self, enc: &mut Encoder) {
        if let Some(compacted) = &self.compacted {
            enc.write_str(SAVE_DOMAIN_V2);
            enc.write_u32(SAVE_FORMAT_VERSION_V2);
            self.save_spine_history_section(enc, compacted);
        } else {
            enc.write_str(SAVE_DOMAIN);
            enc.write_u32(SAVE_FORMAT_VERSION);
            self.save_spine_section(enc);
        }
        let mut world_enc = Encoder::new();
        self.world.save(&mut world_enc);
        enc.write_str(W::SAVE_DOMAIN);
        enc.write_bytes(&world_enc.into_bytes());
    }

    /// The complete save as standalone canonical bytes.
    pub fn save_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        self.save(&mut enc);
        enc.into_bytes()
    }

    /// Restores a spine from complete save bytes (fail-closed).
    ///
    /// The outer domain tag selects the representation —
    /// `sektor7/save/1` (full fidelity) or `sektor7/save/2`
    /// (`spine-history/2`) — and the forms reject one another by
    /// domain and version, never silently reinterpreted.
    ///
    /// Every §5.2 integrity gate is enforced with a typed
    /// [`SaveError`]: domain/schema match; the dense-allocation identity
    /// (`scheduled == next_seq`); conservation (`scheduled == executed +
    /// queued + blocked + held`, and its structural form `executed ==
    /// next_seq - unfinished`); the ledger-count identity; identity
    /// uniqueness and frontier bounds; clock coherence; dependency
    /// resolvability against `[0, next_seq)`; coherent blocked state.
    /// The `spine-history/2` form adds the ratified compacted gates:
    /// the count identity (`summarized + retained == executed`); a
    /// non-empty summary; the retained tail's disjointness, clock
    /// coherence, and order; the late-count bound; and the carried
    /// `max_executed_index`'s coherence (present, at-or-above the
    /// tail's maximum, at-or-below the clock). Corrupted input is
    /// rejected, never repaired — no best-effort reconstruction of
    /// unverified history.
    ///
    /// The clock is initialized from the verified save — the one
    /// sanctioned deviation from tick-zero construction, meaningful only
    /// after every gate above has passed (an incoherent save is rejected
    /// rather than time-traveled).
    pub fn restore(bytes: &[u8], diag: Diagnostics) -> Result<Spine<W>, SaveError> {
        let mut dec = Decoder::new(bytes);
        // The representation dispatch: the outer domain tag selects
        // full fidelity or the compacted form; each form's version and
        // spine-section domain are then checked exactly.
        let compacted_form = {
            let observed = dec.read_str(MAX_TAG_LEN, "domain-tag")?;
            match observed {
                SAVE_DOMAIN => false,
                SAVE_DOMAIN_V2 => true,
                other => {
                    return Err(SaveError::Domain {
                        expected: SAVE_DOMAIN,
                        observed: other.to_owned(),
                    });
                }
            }
        };
        let supported = if compacted_form {
            SAVE_FORMAT_VERSION_V2
        } else {
            SAVE_FORMAT_VERSION
        };
        let version = dec.read_u32()?;
        if version != supported {
            return Err(SaveError::Version {
                observed: version,
                supported,
            });
        }
        if compacted_form {
            dec.read_domain(SPINE_HISTORY_DOMAIN)?;
        } else {
            dec.read_domain(SPINE_DOMAIN)?;
        }
        let clock = dec.read_tick()?;
        let next_seq = dec.read_u64()?;
        let budget = dec.read_u32()?;
        let near_reserve = dec.read_u32()?;
        let stats = Stats {
            scheduled: dec.read_u64()?,
            executed: dec.read_u64()?,
            late_arrivals: dec.read_u64()?,
            late_executions: dec.read_u64()?,
            blocked_transitions: dec.read_u64()?,
            invalid_dropped: dec.read_u64()?,
            budget_exhausted_ticks: dec.read_u64()?,
        };

        // Configuration bounds. The live constructors normalize
        // (`budget >= 1`, `near_reserve <= budget`); a save holding
        // unnormalized values is corrupt, not quietly re-normalized.
        if budget == 0 {
            return Err(SaveError::Integrity("spine-save/budget-zero"));
        }
        if near_reserve > budget {
            return Err(SaveError::Integrity("spine-save/reserve-exceeds-budget"));
        }

        let near = read_lane(&mut dec, "spine-save/near", Field::Near)?;
        let far = read_lane(&mut dec, "spine-save/far", Field::Far)?;
        let blocked = read_seq_section(&mut dec, "spine-save/blocked")?;
        let held = read_seq_section(&mut dec, "spine-save/held")?;
        // The history payload: the full ledger, or the compacted
        // form's fields — the carried derived max, the authenticated
        // prefix summary, the cumulative summarized count, and the
        // retained tail in the full form's per-record encoding.
        let payload = if compacted_form {
            let max_executed_index = match dec.read_u8()? {
                0 => None,
                1 => Some(dec.read_tick()?),
                discriminant => {
                    return Err(SaveError::Enum {
                        section: "spine-history/max-present",
                        discriminant: u64::from(discriminant),
                    });
                }
            };
            let summary_bytes = dec.read_bytes(32, "spine-history/summary")?;
            if summary_bytes.len() != 32 {
                return Err(SaveError::Integrity("spine-history/summary-length"));
            }
            let mut prefix_summary = [0u8; 32];
            prefix_summary.copy_from_slice(summary_bytes);
            let summarized_records = dec.read_u64()?;
            let tail = read_ledger_section(&mut dec, "spine-history/tail")?;
            HistoryPayload::Compacted {
                max_executed_index,
                prefix_summary,
                summarized_records,
                tail,
            }
        } else {
            HistoryPayload::Full {
                ledger: read_ledger_section(&mut dec, "spine-save/ledger")?,
            }
        };

        // ---- integrity gates -----------------------------------------

        // Identity uniqueness across every unfinished structure, frontier
        // bounds, and dependency resolvability: dense allocation issues
        // exactly `[0, next_seq)`, so every identity lives below the
        // frontier, appears at most once anywhere, and every dependency
        // references the same range (the live `known` gate).
        let mut unfinished = BTreeSet::new();
        for event in near.iter().chain(far.iter()).chain(&blocked).chain(&held) {
            let seq = event.id.seq;
            if seq >= next_seq {
                return Err(SaveError::Integrity("spine-save/frontier"));
            }
            if !unfinished.insert(seq) {
                return Err(SaveError::Duplicate {
                    section: "spine-save/identities",
                    seq,
                });
            }
            for dep in &event.depends_on {
                if dep.seq >= next_seq {
                    return Err(SaveError::Integrity("spine-save/dependency-frontier"));
                }
            }
        }

        // Dense allocation: `scheduled` counts every issued identity.
        if stats.scheduled != next_seq {
            return Err(SaveError::Integrity("spine-save/dense-allocation"));
        }

        // Structural conservation: allocation is dense and
        // cancellation-free, so `executed == next_seq - unfinished`
        // exactly — the derivation the complement rebuild below rests
        // on, and the no-cancellation invariant in one check.
        let unfinished_count =
            u64::try_from(unfinished.len()).expect("counts are bounded by the input length");
        let executed_derived =
            next_seq
                .checked_sub(unfinished_count)
                .ok_or(SaveError::Overflow {
                    section: "spine-save/conservation",
                })?;
        if stats.executed != executed_derived {
            return Err(SaveError::Integrity("spine-save/structural-conservation"));
        }

        // The §5.2 conservation identity in its stated form, each
        // section counted once: `scheduled == executed + queued +
        // blocked + held`. (Algebraically implied by dense allocation
        // plus the structural gate above — kept as the named gate the
        // spec states, with checked arithmetic guarding the sums.)
        let queued =
            u64::try_from(near.len() + far.len()).expect("counts are bounded by the input length");
        let blocked_count =
            u64::try_from(blocked.len()).expect("counts are bounded by the input length");
        let held_count = u64::try_from(held.len()).expect("counts are bounded by the input length");
        let conserved = stats
            .executed
            .checked_add(queued)
            .and_then(|sum| sum.checked_add(blocked_count))
            .and_then(|sum| sum.checked_add(held_count))
            .ok_or(SaveError::Overflow {
                section: "spine-save/conservation",
            })?;
        if stats.scheduled != conserved {
            return Err(SaveError::Integrity("spine-save/conservation"));
        }

        // The history payload's own gates. The per-record checks are
        // shared between the forms — frontier bounds, disjointness
        // from unfinished work, per-record clock coherence with the
        // encoded lateness flag, and chronological order — applied to
        // whichever records the form retains.
        let (ledger, compacted, max_carried) = match payload {
            HistoryPayload::Full { ledger } => {
                if stats.executed != ledger.len() as u64 {
                    return Err(SaveError::Integrity("spine-save/ledger-count"));
                }
                check_record_section(&ledger, &unfinished, next_seq, clock)?;
                // The authoritative ledger reconciles the statistic
                // exactly: the live spine increments
                // `late_executions` precisely when it appends a
                // record carrying the lateness flag, so the two
                // counts can never legitimately disagree.
                if stats.late_executions != count_late(&ledger) {
                    return Err(SaveError::Integrity("spine-save/late-execution-count"));
                }
                (ledger, None, None)
            }
            HistoryPayload::Compacted {
                max_executed_index,
                prefix_summary,
                summarized_records,
                tail,
            } => {
                // An explicit compaction summarizes at least one
                // record — the full and compacted forms stay honestly
                // disjoint.
                if summarized_records == 0 {
                    return Err(SaveError::Integrity("spine-history/empty-summary"));
                }
                // The compacted count identity.
                let retained =
                    u64::try_from(tail.len()).expect("counts are bounded by the input length");
                let counted =
                    summarized_records
                        .checked_add(retained)
                        .ok_or(SaveError::Overflow {
                            section: "spine-history/count-identity",
                        })?;
                if counted != stats.executed {
                    return Err(SaveError::Integrity("spine-history/count-identity"));
                }
                check_record_section(&tail, &unfinished, next_seq, clock)?;
                // The late-count bound (equality is the full form's
                // gate — the summarized records may include late
                // ones, so the retained tail's late count is a lower
                // bound on the statistic).
                if stats.late_executions < count_late(&tail) {
                    return Err(SaveError::Integrity("spine-history/late-count-bound"));
                }
                // The carried max: required (a compacted history
                // executed at least one record), at-or-above the
                // retained tail's maximum intended due, at-or-below
                // the clock.
                let Some(max) = max_executed_index else {
                    return Err(SaveError::Integrity("spine-history/max-required"));
                };
                if let Some(tail_max) = tail.iter().map(|record| record.intended_due).max()
                    && tail_max > max
                {
                    return Err(SaveError::Integrity("spine-history/max-below-tail"));
                }
                if max > clock {
                    return Err(SaveError::Integrity("spine-history/max-clock-coherence"));
                }
                (
                    tail,
                    Some(CompactedHistory {
                        summarized_records,
                        prefix_summary,
                    }),
                    max_executed_index,
                )
            }
        };

        // Counter bounds, each structurally implied by live behavior:
        // late executions are counted per executed event, late arrivals
        // per scheduled event, blocked transitions at most once per
        // obligation, and a tick is budget-exhausted at most once.
        if stats.late_executions > stats.executed
            || stats.late_arrivals > stats.scheduled
            || stats.blocked_transitions > stats.scheduled
            || stats.budget_exhausted_ticks > clock.as_u64()
        {
            return Err(SaveError::Integrity("spine-save/counter-bounds"));
        }

        // ---- world section -------------------------------------------

        dec.read_domain(W::SAVE_DOMAIN)?;
        let world_bytes = dec.read_bytes(MAX_BYTE_LEN, "spine-save/world")?;
        let mut world_dec = Decoder::new(world_bytes);
        let world = W::restore(&mut world_dec)?;
        world_dec.finish()?;
        dec.finish()?;

        // ---- derived-state reconstruction -----------------------------

        // `known` is the dense allocation range (a bounds check);
        // `executed` is the complement of unfinished — both exact by
        // the gates above, so the unfinished index the gates built IS
        // the live membership structure (the E3 representation: no
        // per-identity sets are materialized, so a restored spine's
        // memory scales with live work, never with lifetime
        // executions).

        let mut spine = Spine {
            clock,
            next_seq,
            near_queue: BinaryHeap::new(),
            far_queue: BinaryHeap::new(),
            blocked: BTreeMap::new(),
            waiting_on: BTreeMap::new(),
            unfinished,
            ledger,
            compacted,
            max_executed_index: None,
            world,
            budget,
            near_reserve,
            stats,
            diag,
            last_report: LaneReport::default(),
            lane_totals: LaneTotals::default(),
            held: BTreeMap::new(),
            held_index: BTreeSet::new(),
            suspension: SuspensionStats::default(),
        };

        // Queues: heap layout is rebuild-order-dependent and invisible —
        // every observable behavior (pop order, fingerprints) derives
        // from the canonical `(due, seq)` keys (the
        // `suspend_from_queue` argument).
        for event in near {
            spine.near_queue.push(Reverse(Pending::new(event)));
        }
        for event in far {
            spine.far_queue.push(Reverse(Pending::new(event)));
        }

        // Blocked obligations and their waiter registrations, through
        // the live code paths (`unmet_dependencies` against the rebuilt
        // executed set, `register_waiters` with its dedup) — identical
        // structures by construction. A blocked obligation without
        // unmet dependencies is not a live state (the release path
        // would have returned it to its queue): rejected, not repaired.
        // Waiter-vec order may differ from the original registration
        // history; it is behaviorally invisible (queues are
        // `(due, seq)`-keyed and no fingerprint hashes `waiting_on`) —
        // proven so by the continuation-invisibility tests.
        for event in blocked {
            let seq = event.id.seq;
            let unmet = spine.unmet_dependencies(&event);
            if unmet.is_empty() {
                return Err(SaveError::Integrity(
                    "spine-save/blocked-without-unmet-deps",
                ));
            }
            spine.register_waiters(seq, &unmet);
            spine.blocked.insert(seq, event);
        }

        // Held obligations and the incrementally maintained
        // `(due, seq)` index.
        for event in held {
            let pending = Pending::new(event);
            spine.held_index.insert((pending.due, pending.seq));
            spine.held.insert(pending.seq, pending);
        }

        // The derived max-intended-due index: recomputed from the
        // canonical ledger on the full path (value-identical to the
        // incrementally maintained index by construction); carried
        // from the compacted save on the spine-history/2 path (the
        // pre-cut records are summarized — the writer-computed value,
        // gated above against the retained tail and the clock).
        spine.max_executed_index = match max_carried {
            Some(carried) => Some(carried),
            None => spine.ledger.iter().map(|record| record.intended_due).max(),
        };

        Ok(spine)
    }

    fn save_spine_section(&self, enc: &mut Encoder) {
        enc.write_str(SPINE_DOMAIN);
        enc.write_tick(self.clock);
        enc.write_u64(self.next_seq);
        enc.write_u32(self.budget);
        enc.write_u32(self.near_reserve);
        enc.write_u64(self.stats.scheduled);
        enc.write_u64(self.stats.executed);
        enc.write_u64(self.stats.late_arrivals);
        enc.write_u64(self.stats.late_executions);
        enc.write_u64(self.stats.blocked_transitions);
        enc.write_u64(self.stats.invalid_dropped);
        enc.write_u64(self.stats.budget_exhausted_ticks);
        for queue in [&self.near_queue, &self.far_queue] {
            // Canonical `(due, seq)` order — heap layout is
            // rebuild-dependent and never observable.
            let mut lane: Vec<&Pending> = queue.iter().map(|item| &item.0).collect();
            lane.sort_by_key(|pending| (pending.due, pending.seq));
            enc.write_count(lane.len());
            for pending in lane {
                write_event_entry(enc, &pending.event);
            }
        }
        enc.write_count(self.blocked.len());
        for event in self.blocked.values() {
            write_event_entry(enc, event);
        }
        enc.write_count(self.held.len());
        for pending in self.held.values() {
            write_event_entry(enc, &pending.event);
        }
        enc.write_count(self.ledger.len());
        for record in &self.ledger {
            write_record_entry(enc, record);
        }
    }

    /// The compacted spine section (`spine-history/2`): the
    /// authoritative active state in the full section's exact encoding
    /// (clock through held), then the compacted-history fields — the
    /// CARRIED derived max (the pre-cut records are summarized, so the
    /// load cannot derive it from the tail alone), the authenticated
    /// prefix summary, the cumulative summarized count, and the
    /// retained tail in the full form's per-record encoding.
    fn save_spine_history_section(&self, enc: &mut Encoder, compacted: &CompactedHistory) {
        enc.write_str(SPINE_HISTORY_DOMAIN);
        enc.write_tick(self.clock);
        enc.write_u64(self.next_seq);
        enc.write_u32(self.budget);
        enc.write_u32(self.near_reserve);
        enc.write_u64(self.stats.scheduled);
        enc.write_u64(self.stats.executed);
        enc.write_u64(self.stats.late_arrivals);
        enc.write_u64(self.stats.late_executions);
        enc.write_u64(self.stats.blocked_transitions);
        enc.write_u64(self.stats.invalid_dropped);
        enc.write_u64(self.stats.budget_exhausted_ticks);
        for queue in [&self.near_queue, &self.far_queue] {
            // Canonical `(due, seq)` order — heap layout is
            // rebuild-dependent and never observable.
            let mut lane: Vec<&Pending> = queue.iter().map(|item| &item.0).collect();
            lane.sort_by_key(|pending| (pending.due, pending.seq));
            enc.write_count(lane.len());
            for pending in lane {
                write_event_entry(enc, &pending.event);
            }
        }
        enc.write_count(self.blocked.len());
        for event in self.blocked.values() {
            write_event_entry(enc, event);
        }
        enc.write_count(self.held.len());
        for pending in self.held.values() {
            write_event_entry(enc, &pending.event);
        }
        enc.write_u8(u8::from(self.max_executed_index.is_some()));
        if let Some(max) = self.max_executed_index {
            enc.write_tick(max);
        }
        enc.write_bytes(&compacted.prefix_summary);
        enc.write_u64(compacted.summarized_records);
        enc.write_count(self.ledger.len());
        for record in &self.ledger {
            write_record_entry(enc, record);
        }
    }
}

fn write_event_entry(enc: &mut Encoder, event: &Event) {
    enc.write_u64(event.id.seq);
    enc.write_u32(u32::from(event.class.0));
    enc.write_u8(field_byte(event.field));
    match event.schedule {
        Schedule::AtLogicalTime { due } => {
            enc.write_u8(0);
            enc.write_tick(due);
        }
        Schedule::ExecutionRelative { origin, delay } => {
            enc.write_u8(1);
            enc.write_tick(origin);
            enc.write_u64(delay);
        }
    }
    enc.write_u64(event.payload);
    enc.write_count(event.depends_on.len());
    for dep in &event.depends_on {
        enc.write_u64(dep.seq);
    }
}

fn write_record_entry(enc: &mut Encoder, record: &ExecutionRecord) {
    enc.write_u64(record.id.seq);
    enc.write_u32(u32::from(record.class.0));
    enc.write_u8(field_byte(record.field));
    enc.write_tick(record.intended_due);
    enc.write_tick(record.executed_tick);
    enc.write_u8(match record.schedule_kind {
        ScheduleKind::AtLogicalTime => 0,
        ScheduleKind::ExecutionRelative => 1,
    });
    enc.write_u8(u8::from(record.late));
}

fn read_event_entry(dec: &mut Decoder, section: &'static str) -> Result<Event, SaveError> {
    let seq = dec.read_u64()?;
    let wide_class = dec.read_u32()?;
    let class = u16::try_from(wide_class).map_err(|_| SaveError::Limit {
        section,
        kind: "class",
        value: u64::from(wide_class),
        max: u64::from(u16::MAX),
    })?;
    let field = read_field(dec, section)?;
    let schedule = read_schedule(dec, section)?;
    let payload = dec.read_u64()?;
    let dep_count = dec.read_count(MAX_COLLECTION_COUNT, section, "dependency-count")?;
    if dep_count > dec.remaining() as u64 / DEP_BYTES {
        return Err(SaveError::Limit {
            section,
            kind: "dependency-count",
            value: dep_count,
            max: dec.remaining() as u64 / DEP_BYTES,
        });
    }
    let mut depends_on = Vec::with_capacity(dep_count as usize);
    for _ in 0..dep_count {
        depends_on.push(EventId {
            seq: dec.read_u64()?,
        });
    }
    Ok(Event {
        id: EventId { seq },
        class: EventClass::new(class),
        schedule,
        payload,
        field,
        depends_on,
    })
}

fn read_field(dec: &mut Decoder, section: &'static str) -> Result<Field, SaveError> {
    match dec.read_u8()? {
        0 => Ok(Field::Near),
        1 => Ok(Field::Far),
        discriminant => Err(SaveError::Enum {
            section,
            discriminant: u64::from(discriminant),
        }),
    }
}

fn read_schedule(dec: &mut Decoder, section: &'static str) -> Result<Schedule, SaveError> {
    match dec.read_u8()? {
        0 => Ok(Schedule::AtLogicalTime {
            due: dec.read_tick()?,
        }),
        1 => Ok(Schedule::ExecutionRelative {
            origin: dec.read_tick()?,
            delay: dec.read_u64()?,
        }),
        discriminant => Err(SaveError::Enum {
            section,
            discriminant: u64::from(discriminant),
        }),
    }
}

fn read_schedule_kind(dec: &mut Decoder, section: &'static str) -> Result<ScheduleKind, SaveError> {
    match dec.read_u8()? {
        0 => Ok(ScheduleKind::AtLogicalTime),
        1 => Ok(ScheduleKind::ExecutionRelative),
        discriminant => Err(SaveError::Enum {
            section,
            discriminant: u64::from(discriminant),
        }),
    }
}

fn read_late(dec: &mut Decoder, section: &'static str) -> Result<bool, SaveError> {
    match dec.read_u8()? {
        0 => Ok(false),
        1 => Ok(true),
        discriminant => Err(SaveError::Enum {
            section,
            discriminant: u64::from(discriminant),
        }),
    }
}

/// Reads a lane section: entries in canonical `(due, seq)` order with
/// unique identities and the lane's own field on every entry — the
/// live spine routes each obligation to the queue matching its field,
/// so a serialized disagreement is not a live state and is rejected,
/// never silently re-laned. Duplicates or disorder are integrity
/// failures, never repaired.
fn read_lane(
    dec: &mut Decoder,
    section: &'static str,
    lane: Field,
) -> Result<Vec<Event>, SaveError> {
    let count = dec.read_count(MAX_COLLECTION_COUNT, section, "entry-count")?;
    if count > dec.remaining() as u64 / ENTRY_MIN_BYTES {
        return Err(SaveError::Limit {
            section,
            kind: "entry-count",
            value: count,
            max: dec.remaining() as u64 / ENTRY_MIN_BYTES,
        });
    }
    let mut events = Vec::with_capacity(count as usize);
    let mut seen = BTreeSet::new();
    let mut prev: Option<(u64, u64)> = None;
    for _ in 0..count {
        let event = read_event_entry(dec, section)?;
        if event.field != lane {
            return Err(SaveError::Integrity("spine-save/lane-field-coherence"));
        }
        let key = (event.schedule.due().as_u64(), event.id.seq);
        if let Some(prior) = prev
            && key <= prior
        {
            return Err(SaveError::Order { section });
        }
        if !seen.insert(event.id.seq) {
            return Err(SaveError::Duplicate {
                section,
                seq: event.id.seq,
            });
        }
        prev = Some(key);
        events.push(event);
    }
    Ok(events)
}

/// Reads a seq-ordered section (blocked or held): strictly ascending
/// identities — the BTreeMap canonical order.
fn read_seq_section(dec: &mut Decoder, section: &'static str) -> Result<Vec<Event>, SaveError> {
    let count = dec.read_count(MAX_COLLECTION_COUNT, section, "entry-count")?;
    if count > dec.remaining() as u64 / ENTRY_MIN_BYTES {
        return Err(SaveError::Limit {
            section,
            kind: "entry-count",
            value: count,
            max: dec.remaining() as u64 / ENTRY_MIN_BYTES,
        });
    }
    let mut events = Vec::with_capacity(count as usize);
    let mut prev_seq: Option<u64> = None;
    for _ in 0..count {
        let event = read_event_entry(dec, section)?;
        if let Some(prior) = prev_seq {
            if event.id.seq == prior {
                return Err(SaveError::Duplicate {
                    section,
                    seq: prior,
                });
            }
            if event.id.seq < prior {
                return Err(SaveError::Order { section });
            }
        }
        prev_seq = Some(event.id.seq);
        events.push(event);
    }
    Ok(events)
}

/// Reads a ledger-form section (the full ledger, or the compacted
/// form's retained tail): the authoritative execution order (append
/// order), with unique identities.
fn read_ledger_section(
    dec: &mut Decoder,
    section: &'static str,
) -> Result<Vec<ExecutionRecord>, SaveError> {
    let count = dec.read_count(MAX_COLLECTION_COUNT, section, "entry-count")?;
    if count > dec.remaining() as u64 / RECORD_BYTES {
        return Err(SaveError::Limit {
            section,
            kind: "entry-count",
            value: count,
            max: dec.remaining() as u64 / RECORD_BYTES,
        });
    }
    let mut records = Vec::with_capacity(count as usize);
    let mut seen = BTreeSet::new();
    for _ in 0..count {
        let seq = dec.read_u64()?;
        let wide_class = dec.read_u32()?;
        let class = u16::try_from(wide_class).map_err(|_| SaveError::Limit {
            section,
            kind: "class",
            value: u64::from(wide_class),
            max: u64::from(u16::MAX),
        })?;
        let field = read_field(dec, section)?;
        let intended_due = dec.read_tick()?;
        let executed_tick = dec.read_tick()?;
        let schedule_kind = read_schedule_kind(dec, section)?;
        let late = read_late(dec, section)?;
        if !seen.insert(seq) {
            return Err(SaveError::Duplicate { section, seq });
        }
        records.push(ExecutionRecord {
            id: EventId { seq },
            class: EventClass::new(class),
            field,
            intended_due,
            executed_tick,
            schedule_kind,
            late,
        });
    }
    Ok(records)
}

/// The history payload [`Spine::restore`] read: the full ledger, or
/// the compacted form's fields.
enum HistoryPayload {
    Full {
        ledger: Vec<ExecutionRecord>,
    },
    Compacted {
        max_executed_index: Option<Tick>,
        prefix_summary: [u8; 32],
        summarized_records: u64,
        tail: Vec<ExecutionRecord>,
    },
}

/// The per-record ledger checks shared by both history forms:
/// frontier bounds, disjointness from unfinished work, per-record
/// clock coherence with the encoded lateness flag, and chronological
/// order (live execution appends within nondecreasing campaign ticks
/// — one clock advance per tick, every execution of that tick at it —
/// so a regressing `executed_tick` is not a live state).
fn check_record_section(
    records: &[ExecutionRecord],
    unfinished: &BTreeSet<u64>,
    next_seq: u64,
    clock: Tick,
) -> Result<(), SaveError> {
    let mut prev_executed_tick: Option<u64> = None;
    for record in records {
        let seq = record.id.seq;
        if seq >= next_seq {
            return Err(SaveError::Integrity("spine-save/frontier"));
        }
        if unfinished.contains(&seq) {
            return Err(SaveError::Integrity(
                "spine-save/ledger-overlaps-unfinished",
            ));
        }
        if record.intended_due > record.executed_tick
            || record.executed_tick > clock
            || record.late != (record.executed_tick > record.intended_due)
        {
            return Err(SaveError::Integrity("spine-save/clock-coherence"));
        }
        if let Some(prior) = prev_executed_tick
            && record.executed_tick.as_u64() < prior
        {
            return Err(SaveError::Integrity("spine-save/ledger-order"));
        }
        prev_executed_tick = Some(record.executed_tick.as_u64());
    }
    Ok(())
}

fn count_late(records: &[ExecutionRecord]) -> u64 {
    u64::try_from(records.iter().filter(|record| record.late).count())
        .expect("counts are bounded by the input length")
}

/// The fixed-width canonical record bytes — the exact 31 bytes
/// [`write_record_entry`] emits (pinned equal by test) — hashed into
/// the authenticated prefix-summary chain. Fixed-width by
/// construction: seq 8 + class 4 + field 1 + intended due 8 +
/// executed tick 8 + schedule kind 1 + late flag 1.
fn record_wire_bytes(record: &ExecutionRecord) -> [u8; 31] {
    let mut bytes = [0u8; 31];
    bytes[0..8].copy_from_slice(&record.id.seq.to_le_bytes());
    bytes[8..12].copy_from_slice(&u32::from(record.class.0).to_le_bytes());
    bytes[12] = field_byte(record.field);
    bytes[13..21].copy_from_slice(&record.intended_due.as_u64().to_le_bytes());
    bytes[21..29].copy_from_slice(&record.executed_tick.as_u64().to_le_bytes());
    bytes[29] = match record.schedule_kind {
        ScheduleKind::AtLogicalTime => 0,
        ScheduleKind::ExecutionRelative => 1,
    };
    bytes[30] = u8::from(record.late);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The prefix-summary chain's per-record bytes are the save
    /// encoding's per-record bytes — pinned exactly, so the summary
    /// hashes what the wire format specifies.
    #[test]
    fn record_wire_bytes_matches_the_save_encoding() {
        let record = ExecutionRecord {
            id: EventId { seq: 42 },
            class: EventClass::new(7),
            field: Field::Far,
            intended_due: Tick(5),
            executed_tick: Tick(9),
            schedule_kind: ScheduleKind::ExecutionRelative,
            late: true,
        };
        let mut enc = Encoder::new();
        write_record_entry(&mut enc, &record);
        assert_eq!(enc.into_bytes(), record_wire_bytes(&record));
    }
}
