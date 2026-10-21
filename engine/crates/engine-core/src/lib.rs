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
//! * **Causal dependencies and ordering** — within a class, `(due, seq)`
//!   defines deterministic queue priority (minimum key among currently
//!   queued, due events) — not a globally monotonic dequeue or execution
//!   guarantee; the ledger records deviations without identifying their
//!   causes (see [`EventClass`]). Across classes, ordering exists only
//!   through explicit `depends_on` links. Dependency ids must reference
//!   already-scheduled or already-executed events (unknown ids are rejected
//!   at schedule time), which makes dependency cycles structurally
//!   impossible.
//! * **Event vs execution timestamps** — [`Schedule::AtLogicalTime`] fixes
//!   the due tick at schedule time; [`Schedule::ExecutionRelative`] anchors
//!   a follow-up to the tick at which its scheduling event actually
//!   executed. Lateness (`executed_tick > intended_due`) is first-class
//!   ledger data.
//!
//! Deliberately absent (anti-abstraction ledger, §8): regions, LOD levels,
//! persistence, gating, networking, rendering, threading, and any
//! subsystem not exercised by this crate's own tests or the `engine-slice`
//! application. (Near/far lanes arrived with Stage 3 — §2.4's three-phase
//! budget policy — while regions and LOD levels remain gated.)
//!
//! Determinism rules: fingerprints use FNV-1a (stable across platforms and
//! compiler versions — `std`'s hashers are not); fingerprinted state never
//! derives from unordered iteration; wall-clock time never enters logical
//! state.

pub mod clock;
pub mod diag;
pub mod event;
pub mod fingerprint;
pub mod lod;
pub mod spine;
pub mod wire;

pub use clock::Tick;
pub use diag::{Diagnostics, Level, Record};
pub use event::{
    Event, EventClass, EventId, EventSpec, ExecutionRecord, Field, Schedule, ScheduleKind,
};
pub use fingerprint::Fingerprinter;
pub use lod::{LodConfig, LodConfigError, LodController, LodLevel, PresenceSample};
pub use spine::{
    ExecCtx, LaneReport, LaneTotals, ObligationLocation, ObligationView, ScheduleError, Spine,
    Stats, SuspensionStats, World,
};
pub use wire::{Decoder, Encoder, SaveError, SaveWorld};
