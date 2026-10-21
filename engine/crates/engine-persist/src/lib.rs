//! Stage 5 Batch 2: the content-addressed revision store and local
//! canonical medium (the accepted v1.8 architecture's §5.3 nine-step
//! CAS protocol, §5.3.2 validation cost model, and §5.3.3 v1.4 orphan
//! protocol; the implementation record is
//! `docs/STAGE5-B2-STORAGE-RECORD.md`).
//!
//! **Dependency direction (ratified):** `engine-core → engine-persist
//! → application` — this crate uses the Batch 1 wire primitives and
//! adds the single approved `sha2` dependency; engine-core stays
//! zero-dependency and is never reversed into persistence.
//!
//! **What lives here:** SHA-256 content addressing with structural
//! domain separation; the versioned hash-chained revision record;
//! immutable insert-once content-addressed objects with
//! verify-on-retry and the quarantine-never-delete orphan protocol;
//! the [`CanonicalMedium`] boundary with the three distinct append
//! outcomes — `Accepted` (carrying the platform's honest durability
//! tier), `RejectedStaleParent`, and `Unknown` (reconciled under the
//! lock, never guessed); and [`LocalFileMedium`] — the OS-managed
//! `HEAD.lock`, the trusted-cursor validation (O(1) walk work per
//! straightforward append, cold founding → HEAD walk at session open,
//! suffix walks on divergence, fail-closed on non-descendant
//! histories), and the crash-window fault seam the real-process test
//! matrix drives.
//!
//! **What deliberately does not live here (B3+):** the five-state
//! gameplay CommitPipeline, T1 encounter journals, NpcWorld/
//! RegionDirector codecs, networking, compaction/retention, and any
//! asynchronous storage. SHA-256 here is object integrity, never
//! signer authentication.
//!
//! **Windows durability (the central review risk):** the custom
//! provider is qualified for the process-crash tier only (the
//! documented std file-fsync + rename sequence); acknowledged-revision
//! persistence across OS crash or sudden power loss is **not claimed
//! from std** — the N−1-valid/N−lost scenario governs, and SQLite
//! remains the identified production fallback (an evidence-based
//! comparison is delivered at the Batch 2 review; the lead selects).

pub mod fault;
pub mod hash;
pub mod local;
pub mod medium;
pub mod record;

#[cfg(test)]
mod tests;

pub use fault::{FaultAction, FaultControl, FaultPoint};
pub use hash::Hash;
pub use local::{AppendTimings, LocalFileMedium, WalkStats};
pub use medium::{
    AppendOutcome, CanonicalMedium, ChainSummary, MediumError, Reconciliation, StoredObject,
    SyncTier, UnknownReason,
};
pub use record::{AuthorityMode, RECORD_DOMAIN, RECORD_VERSION, RecordKind, RevisionRecord};
