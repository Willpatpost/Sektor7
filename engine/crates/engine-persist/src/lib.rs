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

pub mod checkpoint;
pub mod consequence;
pub mod encounter;
pub mod envelope;
pub mod fault;
pub mod hash;
pub mod journal;
pub mod local;
pub mod medium;
pub mod migrate;
pub mod pipeline;
pub mod reclaim;
pub mod record;
pub mod recovery;
pub mod rotation;
pub mod sqlite;

#[cfg(test)]
mod migrate_tests;
#[cfg(test)]
mod sqlite_tests;
#[cfg(test)]
mod tests;

pub use checkpoint::{
    CHECKPOINT_METADATA_DOMAIN, CHECKPOINT_METADATA_VERSION, CHECKPOINT_MK_KEY_DOMAIN,
    CheckpointMetadata, ReclaimedIndexEntry, SkipEntry, chain_range_digest,
    checkpoint_mk_idempotency_key, compute_digest_seed, compute_range_digest,
    publish_checkpoint_mk,
};
pub use consequence::{
    AwardCheckpoint, AwardState, ConsequenceId, Coverage, OccurrenceSpan, PackedKeyError, SpanError,
};
pub use encounter::{EncounterError, EncounterJournal};
pub use envelope::{
    ClassifiedSpan, ConsequenceEnvelope, CounterDelta, CounterValues, DurabilityClass,
    PartialApplication, SignalLane, SourceBaseline, SourcePosition,
};
pub use fault::{FaultAction, FaultControl, FaultPoint};
pub use hash::Hash;
pub use journal::{
    CancellationReason, CommitStage, CommittedTransaction, FrontierEntry, JournalError,
    JournalFaultControl, JournalFaultPoint, JournalReceipt, JournalScan, RecordedCancellation,
    T1Journal, TailKind, TailState, TransactionDurability, TransactionId, TransactionInput,
    WriteReconciliation,
};
pub use local::{AppendTimings, LocalFileMedium, WalkStats};
pub use medium::{
    AppendOutcome, CanonicalMedium, ChainSummary, MediumError, ObjectClass, Reconciliation,
    StoredObject, SyncTier, UnknownReason,
};
pub use migrate::{
    CopiedChain, MigrationError, MigrationReport, archive_legacy, copy_legacy_chain,
    cutover_seal_activate, migrate_local_to_sqlite,
};
pub use pipeline::{
    BatchEntry, CONSEQUENCE_BATCH_DOMAIN, CaptureReceipt, CommitOutcome, CommitPipeline,
    ConfirmationLabel, ConfirmationReceipt, ConsequenceBatch, KeyPhase, KeyStatus, PipelineError,
    ReconcileOutcome,
};
pub use reclaim::{
    RECLAMATION_LEDGER_DOMAIN, RECLAMATION_LEDGER_KEY_DOMAIN, RECLAMATION_LEDGER_VERSION,
    ReclaimUnit, ReclamationLedger, publish_reclamation_ledger, reclamation_ledger_idempotency_key,
};
pub use record::{AuthorityMode, RECORD_DOMAIN, RECORD_VERSION, RecordKind, RevisionRecord};
pub use recovery::{
    AppliedEffect, ConfirmedEvidence, RecoveredParticipant, RecoveryAnchor, RecoveryError,
    RecoveryOutcome, RecoveryReport, TransitionClassification, reconcile,
    reconstruct_episode_frontier,
};
pub use rotation::{
    GENERATIONS_IO_FAULT, JOURNAL_ACTIVE_SUFFIX, JournalRotation, POINTER_DIR_SYNC_FAULT,
    PointerDurability, active_journal_path, read_journal_generations,
    reestablish_pointer_durability, retire_journal_generations, rotate_journal,
};
pub use sqlite::{
    SqliteFaultAction, SqliteFaultControl, SqliteFaultPoint, SqliteMedium, SqliteMediumConfig,
};
