//! Stage 0 reusable engine core for the Sektor7 original engine.
//!
//! Scope (`docs/TECH-ARCHITECTURE.md` §6, Stage 0 — lead authorization
//! 2026-10-19): the canonical clock seed, the event spine carrying the three
//! contract semantics the architecture review required, deterministic
//! logical-state fingerprints, and basic diagnostics.
//!
//! Implemented contract semantics (`docs/TECH-ARCHITECTURE.md` §3.2, v0.2):
//!
//! * **Safe watermark** — [`Spine::processed_through`] is distinct from
//!   [`Spine::max_executed_due`] and never claims completeness while earlier
//!   required events remain queued, executing, or blocked by unresolved
//!   causal dependencies; it returns `None` when nothing is provably
//!   complete (including a pending event at tick 0). It is computed live,
//!   may retreat when late events are scheduled, and is a progress measure
//!   — not irreversible finality.
//! * **Causal dependencies and ordering** — the spine's queue priority is
//!   globally `(due, seq)`; execution follows it except under causal
//!   blocking or late arrival (both recorded in the ledger — see
//!   [`EventClass`]). Across classes, ordering exists only through explicit
//!   `depends_on` links. Dependency ids must reference already-scheduled or
//!   already-executed events (unknown ids are rejected at schedule time),
//!   which makes dependency cycles structurally impossible.
//! * **Event vs execution timestamps** — [`Schedule::AtLogicalTime`] fixes
//!   the due tick at schedule time; [`Schedule::ExecutionRelative`] anchors
//!   a follow-up to the tick at which its scheduling event actually
//!   executed. Lateness (`executed_tick > intended_due`) is first-class
//!   ledger data.
//!
//! Deliberately absent (anti-abstraction ledger, §8): regions, LOD levels,
//! near/far lanes, persistence, gating, networking, rendering, threading,
//! and any subsystem not exercised by this crate's own tests or the
//! `engine-slice` application.
//!
//! Determinism rules: fingerprints use FNV-1a (stable across platforms and
//! compiler versions — `std`'s hashers are not); fingerprinted state never
//! derives from unordered iteration; wall-clock time never enters logical
//! state.

pub mod clock;
pub mod diag;
pub mod event;
pub mod fingerprint;
pub mod spine;

pub use clock::Tick;
pub use diag::{Diagnostics, Level, Record};
pub use event::{Event, EventClass, EventId, EventSpec, ExecutionRecord, Schedule, ScheduleKind};
pub use fingerprint::Fingerprinter;
pub use spine::{ExecCtx, ScheduleError, Spine, Stats, World};
