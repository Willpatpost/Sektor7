//! The commit pipeline (`pipeline.rs`, the fourth of the six Batch 3
//! modules) — the five-state durability model's per-key view over the
//! T1 journal and the canonical medium (§4.2–4.3, §5.5).
//!
//! **The per-key phases** (the GO's four statuses, one lattice):
//! `InMemory` (staged accrual during play — no I/O touches this
//! path) → `JournaledT1` (the boundary capture journaled and fsynced
//! — durable, replayable) → `AppliedToLivePending` (the application
//! reports the effect in the live authoritative state while the
//! canonical commit is pending — never conflated with durability) →
//! `InConfirmedChain` (canonically confirmed). An effect may exist in
//! live state while its canonical commit is pending; that is never
//! presented as durability.
//!
//! **Canonical confirmation (the rule, never relaxed):** a
//! consequence is confirmed iff (1) the revision carrying its
//! `consequence-batch/1` content object is **included in the verified
//! chain** — re-read and hash-verified through the medium, never a
//! bare `Accepted`, never inferred from time, local receipts,
//! counter matches, T1 fsync, or caches; (2) the batch object is read
//! back **by its content identity** and **every `ConsequenceId` is
//! individually validated** — present, with exactly the attempted
//! occurrence coverage and producing transaction, and no unattempted
//! keys; and (3) the chain walk placed each key at the confirming
//! revision. Merely storing a batch object or confirming its
//! aggregate revision is **insufficient** — a mismatch is
//! [`PipelineError::ConfirmationInvalid`], never a false T3.
//!
//! **The canonical batch (requirement A):** the multi-key batch is a
//! versioned `consequence-batch/1` content object referenced via the
//! revision's `content_refs` — carrying the complete validated key
//! set with per-consequence occurrence coverage — never an opaque
//! batch idempotency key. The revision's idempotency key **is** the
//! batch's content hash (chain-unique, enforced by the medium), so a
//! stale-parent rebase or an idempotent retry of the same logical
//! batch can never double-install. Duplicate keys across revisions
//! never duplicate: chain-confirmed keys are never re-batched, and a
//! conflicting identity reuse fails closed.
//!
//! **Stale parents and Unknown outcomes:** the pipeline appends
//! against its own verified head (the trusted-cursor model — the
//! session's view, not a fresh read). A `RejectedStaleParent` means
//! another writer advanced the chain: the pipeline re-walks the
//! observed suffix (determining the now-confirmed keys — a key
//! confirmed by another writer's batch is confirmed, never
//! re-submitted), rebases the remaining keys **with identical
//! per-key identities and coverages** on the observed parent, and
//! retries once; a second rejection retains the attempt for
//! [`Self::retry_pending`]. An `Unknown` outcome retains the exact
//! revision, record bytes, and key set; [`Self::reconcile_pending`]
//! resolves it under the medium's lock — `Installed` still requires
//! the full read-back walk and individual validation before
//! confirmation; `NotInstalled` is retry-safe under the same content
//! identity; `Unverifiable` stays pending, never falsely confirmed.
//!
//! **Honesty:** the confirmation receipt carries the exact revision,
//! the verified chain position (epoch), every individually validated
//! key, the actual provider tier, and the authority mode **from the
//! founding record** (never assumed): standalone campaigns label
//! `T3-local (standalone)` — never bare T3; shared campaigns confirm
//! via the shared medium. Windows carries the process-crash tier,
//! never presented as power-loss durability. The chain walk is
//! O(new revisions) per commit (the session-open walk is O(chain),
//! documented honestly — never per tick); staging performs no I/O;
//! the pipeline never decides gameplay policy.

use std::collections::BTreeMap;
use std::fmt;

use engine_core::wire::MAX_TAG_LEN;
use engine_core::{Decoder, Encoder, SaveError};

use crate::consequence::ConsequenceId;
use crate::envelope::{ClassifiedSpan, ConsequenceEnvelope, DurabilityClass, SourcePosition};
use crate::hash::Hash;
use crate::journal::{
    CancellationReason, FrontierEntry, JournalError, JournalReceipt, T1Journal, TransactionId,
    TransactionInput, WriteReconciliation,
};
use crate::medium::{
    AppendOutcome, CanonicalMedium, MediumError, Reconciliation, StoredObject, SyncTier,
    UnknownReason,
};
use crate::record::{AuthorityMode, RecordKind, RevisionRecord};

/// The canonical multi-consequence batch's domain (versioned; the
/// content object referenced via the revision's `content_refs`).
pub const CONSEQUENCE_BATCH_DOMAIN: &str = "sektor7/engine-persist/consequence-batch/1";
pub const CONSEQUENCE_BATCH_SCHEMA: u8 = 1;

/// Documented maximum for one batch's entries (bounded decode — each
/// entry is at least 16 + 40 + 8 bytes, also bounded by the remaining
/// input).
pub const MAX_BATCH_ENTRIES: u64 = 1 << 16;
/// Documented maximum for one entry's coverage spans.
pub const MAX_BATCH_SPANS: u64 = 1 << 16;

/// One consequence's entry in the canonical batch — the complete
/// per-consequence evidence: the identity, the producing transaction,
/// and the occurrence coverage. Confirmation validates each entry
/// individually; the aggregate alone is never sufficient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchEntry {
    pub id: ConsequenceId,
    pub transaction: TransactionId,
    /// The class-homogeneous occurrence coverage this consequence
    /// commits (mirrors the envelope's sub-spans).
    pub coverage: Vec<ClassifiedSpan>,
}

/// The versioned canonical batch content object (requirement A —
/// never an opaque batch key). Entries are strictly ascending by
/// identity, unique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsequenceBatch {
    pub entries: Vec<BatchEntry>,
}

impl ConsequenceBatch {
    /// Canonical bytes (strict: only ordered, unique, well-formed
    /// batches serialize).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, SaveError> {
        const SECTION: &str = "consequence-batch";
        if self.entries.len() as u64 > MAX_BATCH_ENTRIES {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "entry-count",
                value: self.entries.len() as u64,
                max: MAX_BATCH_ENTRIES,
            });
        }
        let mut enc = Encoder::new();
        enc.write_str(CONSEQUENCE_BATCH_DOMAIN);
        enc.write_u8(CONSEQUENCE_BATCH_SCHEMA);
        enc.write_count(self.entries.len());
        let mut prev: Option<&ConsequenceId> = None;
        for entry in &self.entries {
            if let Some(prior) = prev
                && entry.id <= *prior
            {
                return Err(SaveError::Order { section: SECTION });
            }
            prev = Some(&entry.id);
            enc.write_u32(entry.id.region);
            enc.write_u64(entry.id.episode);
            enc.write_u32(entry.id.npc);
            enc.write_bytes(entry.transaction.0.as_bytes());
            write_coverage(&mut enc, &entry.coverage)?;
        }
        Ok(enc.into_bytes())
    }

    /// Strict decode (fail-closed: domain, schema, ascending unique
    /// identities, per-span validity and canonical order, bounded
    /// counts, no trailing).
    pub fn decode(bytes: &[u8]) -> Result<Self, SaveError> {
        const SECTION: &str = "consequence-batch";
        let mut dec = Decoder::new(bytes);
        dec.read_domain(CONSEQUENCE_BATCH_DOMAIN)?;
        let schema = dec.read_u8()?;
        if schema != CONSEQUENCE_BATCH_SCHEMA {
            return Err(SaveError::Version {
                observed: u32::from(schema),
                supported: u32::from(CONSEQUENCE_BATCH_SCHEMA),
            });
        }
        let count = dec.read_count(MAX_BATCH_ENTRIES, SECTION, "entry-count")?;
        // Each entry is at least 16 (identity) + 40 (transaction) +
        // 8 (coverage count) bytes.
        let remaining = dec.remaining() as u64;
        if count > remaining / 64 {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "entry-count",
                value: count,
                max: remaining / 64,
            });
        }
        let mut entries: Vec<BatchEntry> = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let id = ConsequenceId {
                region: dec.read_u32()?,
                episode: dec.read_u64()?,
                npc: dec.read_u32()?,
            };
            if let Some(prior) = entries.last()
                && id <= prior.id
            {
                return Err(SaveError::Order { section: SECTION });
            }
            let transaction_bytes = dec.read_bytes(Hash::WIDTH, SECTION)?;
            let transaction = TransactionId(
                Hash::from_bytes(transaction_bytes)
                    .ok_or(SaveError::Integrity("consequence-batch/hash-width"))?,
            );
            let coverage = read_coverage(&mut dec)?;
            entries.push(BatchEntry {
                id,
                transaction,
                coverage,
            });
        }
        dec.finish()?;
        Ok(ConsequenceBatch { entries })
    }
}

fn class_discriminant(class: DurabilityClass) -> u8 {
    match class {
        DurabilityClass::DurableEligible => 0,
        DurabilityClass::EncounterProvisional => 1,
    }
}

fn read_class(dec: &mut Decoder<'_>) -> Result<DurabilityClass, SaveError> {
    match dec.read_u8()? {
        0 => Ok(DurabilityClass::DurableEligible),
        1 => Ok(DurabilityClass::EncounterProvisional),
        discriminant => Err(SaveError::Enum {
            section: "consequence-batch",
            discriminant: u64::from(discriminant),
        }),
    }
}

fn write_coverage(enc: &mut Encoder, coverage: &[ClassifiedSpan]) -> Result<(), SaveError> {
    const SECTION: &str = "consequence-batch";
    if coverage.len() as u64 > MAX_BATCH_SPANS {
        return Err(SaveError::Limit {
            section: SECTION,
            kind: "coverage-span-count",
            value: coverage.len() as u64,
            max: MAX_BATCH_SPANS,
        });
    }
    enc.write_count(coverage.len());
    for classified in coverage {
        enc.write_u64(classified.span.lo);
        enc.write_u64(classified.span.hi);
        enc.write_u8(class_discriminant(classified.class));
    }
    Ok(())
}

fn read_coverage(dec: &mut Decoder<'_>) -> Result<Vec<ClassifiedSpan>, SaveError> {
    const SECTION: &str = "consequence-batch";
    let count = dec.read_count(MAX_BATCH_SPANS, SECTION, "coverage-span-count")?;
    let remaining = dec.remaining() as u64;
    if count > remaining / 17 {
        return Err(SaveError::Limit {
            section: SECTION,
            kind: "coverage-span-count",
            value: count,
            max: remaining / 17,
        });
    }
    let mut coverage: Vec<ClassifiedSpan> = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let lo = dec.read_u64()?;
        let hi = dec.read_u64()?;
        if lo >= hi {
            return Err(SaveError::Integrity(
                "consequence-batch/coverage-span-inverted",
            ));
        }
        if let Some(prior) = coverage.last()
            && lo <= prior.span.hi
        {
            return Err(SaveError::Integrity(
                "consequence-batch/coverage-span-noncanonical",
            ));
        }
        let class = read_class(dec)?;
        coverage.push(ClassifiedSpan {
            span: crate::consequence::OccurrenceSpan { lo, hi },
            class,
        });
    }
    Ok(coverage)
}

/// The per-key phase lattice (the GO's four statuses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPhase {
    /// Staged in memory only, before the journal write — never
    /// guaranteed across a crash, never claimed to be.
    InMemory,
    /// Journaled and fsynced (T1) — durable and replayable; pending
    /// canonical confirmation.
    JournaledT1,
    /// The application reports the effect in the live authoritative
    /// state while the canonical commit is pending.
    AppliedToLivePending,
    /// Individually validated inside the verified canonical chain.
    InConfirmedChain,
}

/// One key's pipeline view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyStatus {
    pub phase: KeyPhase,
    /// The journal transaction that captured the key.
    pub transaction: Option<TransactionId>,
    /// The chain revision that confirmed it.
    pub revision: Option<Hash>,
}

/// The capture boundary's receipt (returned only after the journal's
/// fsync — requirement C; a reconciled capture recovered a valid
/// committed frame without a fabricated acknowledgment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureReceipt {
    pub journal: JournalReceipt,
    pub reconciled: bool,
}

/// What a canonical commit produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitOutcome {
    /// Every key individually validated inside the verified chain.
    Confirmed(ConfirmationReceipt),
    /// No actionable, unconfirmed journaled keys existed.
    NothingPending,
    /// The append's outcome is uncertain — the exact revision, record
    /// bytes, and key set are retained; reconcile via
    /// [`CommitPipeline::reconcile_pending`].
    Unknown {
        revision: Hash,
        reason: UnknownReason,
    },
    /// The rebase-and-retry was rejected stale again — the attempt is
    /// retained for [`CommitPipeline::retry_pending`].
    StaleParent { observed: Option<Hash> },
}

/// What reconciling a retained unknown attempt established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// Installed **and** fully verified (the read-back walk plus the
    /// individual per-key validation) — confirmed.
    Confirmed(ConfirmationReceipt),
    /// Not installed — the attempt is retained; an idempotent retry
    /// under the same content identity is safe.
    NotInstalled,
    /// Verification could not establish the result — the attempt
    /// stays pending, never falsely confirmed.
    Unverifiable { reason: String },
    /// No retained attempt exists — nothing to reconcile.
    NothingPending,
}

/// The confirmation receipt — the exact revision, the verified chain
/// position, every individually validated key, the actual tier, and
/// the authority mode from the founding record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmationReceipt {
    pub revision: Hash,
    pub epoch: u64,
    pub keys: Vec<ConsequenceId>,
    pub tier: SyncTier,
    pub authority: AuthorityMode,
    pub label: ConfirmationLabel,
}

/// The honest confirmation label (§3.3 — no silent mode
/// presentations).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationLabel {
    /// A standalone campaign's local canonical confirmation —
    /// legitimate, never presented as replication or power-loss
    /// protection.
    T3LocalStandalone,
    /// A shared campaign confirmed through the shared medium.
    SharedCanonical,
}

impl fmt::Display for ConfirmationLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfirmationLabel::T3LocalStandalone => write!(f, "T3-local (standalone)"),
            ConfirmationLabel::SharedCanonical => write!(f, "shared canonical"),
        }
    }
}

/// Typed pipeline failures.
#[derive(Debug)]
pub enum PipelineError {
    Medium(MediumError),
    Journal(JournalError),
    Format(SaveError),
    /// The campaign store is unfounded — the founding record is the
    /// application's setup act, never a per-commit decision.
    UnfoundedMedium,
    /// A tracked identity re-staged or re-journaled with different
    /// content — conflicting reuse fails closed.
    ConflictingReuse {
        id: ConsequenceId,
    },
    /// The journal boundary aborted before the commit record —
    /// nothing durable was written; the staged set is retained and
    /// the retry is safe.
    CaptureAborted,
    /// An unresolved canonical attempt exists — reconcile or retry it
    /// before starting a new one (identity exactness).
    AttemptUnresolved {
        revision: Hash,
    },
    /// An untracked key was reported applied.
    UnknownKey {
        id: ConsequenceId,
    },
    /// An unstaged, unjournaled key was reported applied to live
    /// state — authoritative application before durability violates
    /// durable-before-discard.
    AppliedBeforeJournal {
        id: ConsequenceId,
    },
    /// The confirmed batch failed individual per-key validation —
    /// never a false T3.
    ConfirmationInvalid {
        detail: &'static str,
    },
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PipelineError::Medium(error) => write!(f, "canonical medium failure: {error}"),
            PipelineError::Journal(error) => write!(f, "journal failure: {error}"),
            PipelineError::Format(error) => write!(f, "pipeline format failure: {error}"),
            PipelineError::UnfoundedMedium => {
                write!(
                    f,
                    "the canonical medium is unfounded — found the campaign first"
                )
            }
            PipelineError::ConflictingReuse { id } => {
                write!(f, "conflicting reuse of consequence identity {id}")
            }
            PipelineError::CaptureAborted => write!(
                f,
                "the capture boundary aborted before the commit record — retry-safe"
            ),
            PipelineError::AttemptUnresolved { revision } => write!(
                f,
                "an unresolved canonical attempt ({revision}) exists — reconcile it first"
            ),
            PipelineError::UnknownKey { id } => {
                write!(f, "key {id} is not tracked by the pipeline")
            }
            PipelineError::AppliedBeforeJournal { id } => write!(
                f,
                "key {id} was reported applied before it was journaled — \
                 durable-before-discard violated"
            ),
            PipelineError::ConfirmationInvalid { detail } => write!(
                f,
                "canonical confirmation failed individual validation ({detail}) — \
                 never a false T3"
            ),
        }
    }
}

impl std::error::Error for PipelineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PipelineError::Medium(error) => Some(error),
            PipelineError::Journal(error) => Some(error),
            PipelineError::Format(error) => Some(error),
            _ => None,
        }
    }
}

impl From<MediumError> for PipelineError {
    fn from(error: MediumError) -> Self {
        PipelineError::Medium(error)
    }
}

impl From<JournalError> for PipelineError {
    fn from(error: JournalError) -> Self {
        PipelineError::Journal(error)
    }
}

/// One chain-confirmed key's evidence (the suppression authority).
struct ConfirmedEntry {
    coverage: Vec<ClassifiedSpan>,
    revision: Hash,
}

/// A retained canonical attempt — the exact revision, the batch's
/// content identity, and the per-key identities (the batch bytes
/// re-derive deterministically from the keys; never rebuilt from
/// guesswork).
struct PendingAttempt {
    record: RevisionRecord,
    batch_hash: Hash,
    /// The attempted per-key evidence: identity → (coverage,
    /// producing transaction).
    keys: BTreeMap<ConsequenceId, (Vec<ClassifiedSpan>, TransactionId)>,
}

/// The commit pipeline over a canonical medium and the durable T1
/// journal.
pub struct CommitPipeline<M: CanonicalMedium> {
    medium: M,
    journal: T1Journal,
    /// The staged in-memory accrual — no I/O touches this path; a
    /// crash loses it honestly (P0-5).
    staged: Vec<ConsequenceEnvelope>,
    keys: BTreeMap<ConsequenceId, KeyStatus>,
    /// The chain-derived confirmed key set (the session's verified
    /// view — rebuilt at open, extended by the walks).
    confirmed: BTreeMap<ConsequenceId, ConfirmedEntry>,
    /// The last revision the walk processed (the append basis — the
    /// trusted-cursor model; `None` on an unfounded medium).
    verified_head: Option<Hash>,
    verified_epoch: u64,
    /// The authority mode and campaign id from the founding record.
    founding: Option<(AuthorityMode, String)>,
    /// The retained unresolved canonical attempt, if any.
    pending: Option<PendingAttempt>,
    tier: SyncTier,
}

impl<M: CanonicalMedium> CommitPipeline<M> {
    /// Opens the pipeline: the journal's committed transactions seed
    /// the per-key tracking (the T1 state is durable across sessions),
    /// then the session-open chain walk (O(chain), documented honestly
    /// — never per tick) collects every chain-confirmed key from the
    /// `consequence-batch/1` objects, promotes them, and establishes
    /// the founding authority.
    pub fn open(medium: M, journal: T1Journal) -> Result<Self, PipelineError> {
        let tier = medium.durability_tier();
        let mut pipeline = CommitPipeline {
            medium,
            journal,
            staged: Vec::new(),
            keys: BTreeMap::new(),
            confirmed: BTreeMap::new(),
            verified_head: None,
            verified_epoch: 0,
            founding: None,
            pending: None,
            tier,
        };
        for transaction in pipeline.journal.transactions() {
            for envelope in &transaction.envelopes {
                pipeline.keys.entry(envelope.id).or_insert(KeyStatus {
                    phase: KeyPhase::JournaledT1,
                    transaction: Some(transaction.id),
                    revision: None,
                });
            }
        }
        pipeline.walk_chain()?;
        Ok(pipeline)
    }

    /// Stages one consequential envelope — the in-memory accrual
    /// (never guaranteed across a crash). A tracked identity staged
    /// again with different content is a conflicting reuse and fails
    /// closed; a byte-identical restage is a no-op.
    pub fn stage(&mut self, envelope: ConsequenceEnvelope) -> Result<(), PipelineError> {
        envelope.canonical_bytes().map_err(PipelineError::Format)?;
        if self.keys.contains_key(&envelope.id) {
            if self.staged.iter().any(|staged| staged == &envelope) {
                return Ok(());
            }
            return Err(PipelineError::ConflictingReuse { id: envelope.id });
        }
        self.keys.insert(
            envelope.id,
            KeyStatus {
                phase: KeyPhase::InMemory,
                transaction: None,
                revision: None,
            },
        );
        self.staged.push(envelope);
        Ok(())
    }

    /// The capture boundary: journals the staged consequences as one
    /// T1 transaction (the receipt returns only after the journal's
    /// fsync). An ambiguous commit outcome is reconciled immediately
    /// — reread and verified, never guessed; the recovery of a valid
    /// committed frame is **not** a fabricated acknowledgment. An
    /// aborted capture is retry-safe with the staged set retained.
    pub fn capture(
        &mut self,
        position: SourcePosition,
        classification: DurabilityClass,
        dependencies: Vec<TransactionId>,
        frontier: Vec<FrontierEntry>,
        plan: Vec<u8>,
    ) -> Result<CaptureReceipt, PipelineError> {
        let input = TransactionInput {
            envelopes: self.staged.clone(),
            position,
            classification,
            dependencies,
            frontier,
            plan,
        };
        let receipt = match self.journal.append_transaction(&input) {
            Ok(receipt) => CaptureReceipt {
                journal: receipt,
                reconciled: false,
            },
            Err(JournalError::AmbiguousCommit { .. }) => {
                let reconciled = match self.journal.reconcile_write(&input)? {
                    WriteReconciliation::Committed { receipt } => receipt,
                    WriteReconciliation::NotCommitted => return Err(PipelineError::CaptureAborted),
                };
                CaptureReceipt {
                    journal: reconciled,
                    reconciled: true,
                }
            }
            Err(JournalError::AbortedBeforeCommit { .. }) => {
                return Err(PipelineError::CaptureAborted);
            }
            Err(other) => return Err(PipelineError::Journal(other)),
        };
        for envelope in self.staged.drain(..) {
            self.keys.insert(
                envelope.id,
                KeyStatus {
                    phase: KeyPhase::JournaledT1,
                    transaction: Some(receipt.journal.id),
                    revision: None,
                },
            );
        }
        Ok(receipt)
    }

    /// Appends a validated, durable, reason-coded cancellation for a
    /// journaled transaction (the pipeline proxies the journal —
    /// supersession is the explicit durable record, never a higher
    /// sequence).
    pub fn cancel_transaction(
        &mut self,
        target: TransactionId,
        reason: CancellationReason,
        detail: &[u8],
    ) -> Result<u64, PipelineError> {
        let sequence = self.journal.append_cancellation(target, reason, detail)?;
        Ok(sequence)
    }

    /// The application reports effects applied to the live
    /// authoritative state (the pending-vs-confirmed distinction —
    /// never conflated with durability). A key must be journaled
    /// first (durable-before-discard).
    pub fn mark_applied(&mut self, keys: &[ConsequenceId]) -> Result<(), PipelineError> {
        for id in keys {
            let Some(status) = self.keys.get(id) else {
                return Err(PipelineError::UnknownKey { id: *id });
            };
            match status.phase {
                KeyPhase::InMemory => {
                    return Err(PipelineError::AppliedBeforeJournal { id: *id });
                }
                KeyPhase::JournaledT1 => {}
                KeyPhase::AppliedToLivePending | KeyPhase::InConfirmedChain => {}
            }
        }
        for id in keys {
            if let Some(status) = self.keys.get_mut(id)
                && status.phase == KeyPhase::JournaledT1
            {
                status.phase = KeyPhase::AppliedToLivePending;
            }
        }
        Ok(())
    }

    /// Prepares the canonical batch from every actionable,
    /// unconfirmed journaled key and submits it against the verified
    /// head. Canceled transactions are excluded (superseded — never
    /// canonically committed); chain-confirmed keys are never
    /// re-batched.
    pub fn commit_canonical(&mut self) -> Result<CommitOutcome, PipelineError> {
        if let Some(pending) = &self.pending {
            return Err(PipelineError::AttemptUnresolved {
                revision: pending.record.id(),
            });
        }
        let Some(parent) = self.verified_head else {
            return Err(PipelineError::UnfoundedMedium);
        };
        let keys = self.collect_pending_keys()?;
        if keys.is_empty() {
            return Ok(CommitOutcome::NothingPending);
        }
        self.submit_keys(keys, parent)
    }

    /// Reconciles a retained unknown attempt under the medium's lock:
    /// `Installed` still requires the full read-back walk and the
    /// individual per-key validation before confirmation; otherwise
    /// the attempt is retained (retry-safe or unverifiable — never
    /// falsely confirmed).
    pub fn reconcile_pending(&mut self) -> Result<ReconcileOutcome, PipelineError> {
        let Some(pending) = &self.pending else {
            return Ok(ReconcileOutcome::NothingPending);
        };
        let candidate = pending.record.id();
        match self.medium.reconcile_unknown(&candidate)? {
            Reconciliation::Installed { .. } => {
                let attempt = self.pending.take().expect("checked above");
                let CommitOutcome::Confirmed(receipt) = self.confirm(attempt)? else {
                    unreachable!("confirm returns Confirmed or errors");
                };
                Ok(ReconcileOutcome::Confirmed(receipt))
            }
            Reconciliation::NotInstalled { .. } => Ok(ReconcileOutcome::NotInstalled),
            Reconciliation::Unverifiable { reason } => {
                Ok(ReconcileOutcome::Unverifiable { reason })
            }
        }
    }

    /// Retries a retained attempt (after `NotInstalled`, or a stale
    /// parent): re-walks the chain first (determining keys another
    /// writer may have confirmed — never re-submitted), then
    /// re-submits the remaining per-key identities — the same record
    /// bytes when the parent still matches, the rebase (identical
    /// identities and coverages, new parent) when it does not.
    pub fn retry_pending(&mut self) -> Result<CommitOutcome, PipelineError> {
        let Some(attempt) = self.pending.take() else {
            return Ok(CommitOutcome::NothingPending);
        };
        self.walk_chain()?;
        let attempted: Vec<ConsequenceId> = attempt.keys.keys().copied().collect();
        let remaining = self.remaining_keys(attempt.keys)?;
        if remaining.is_empty() {
            // Every attempted key is now chain-confirmed (another
            // writer's batch) — confirmed without our revision.
            return Ok(CommitOutcome::Confirmed(self.foreign_receipt(attempted)?));
        }
        let parent = self.verified_head.ok_or(PipelineError::UnfoundedMedium)?;
        self.submit_keys(remaining, parent)
    }

    /// One key's pipeline view.
    pub fn status(&self, id: ConsequenceId) -> Option<KeyStatus> {
        self.keys.get(&id).cloned()
    }

    /// Whether the identity is individually validated inside the
    /// verified canonical chain (the suppression authority).
    pub fn is_confirmed(&self, id: ConsequenceId) -> bool {
        self.confirmed.contains_key(&id)
    }

    /// The chain-confirmed occurrence coverage for one identity (the
    /// per-key evidence — never a batch aggregate alone).
    pub fn confirmed_coverage(&self, id: ConsequenceId) -> Option<&[ClassifiedSpan]> {
        self.confirmed
            .get(&id)
            .map(|entry| entry.coverage.as_slice())
    }

    /// The confirmed key set (the pipeline's session view).
    pub fn confirmed_keys(&self) -> Vec<ConsequenceId> {
        self.confirmed.keys().copied().collect()
    }

    /// The actual durability tier of the underlying medium (the
    /// honest platform boundary — Windows carries the process-crash
    /// tier, never presented as power-loss durability).
    pub fn sync_tier(&self) -> SyncTier {
        self.tier
    }

    /// The authority mode from the founding record (never assumed).
    pub fn authority(&self) -> Option<(AuthorityMode, &str)> {
        self.founding
            .as_ref()
            .map(|(mode, campaign)| (*mode, campaign.as_str()))
    }

    // ---------------------------------------------------------- internals

    /// Collects the actionable, unconfirmed journaled keys — the
    /// batch candidates. A key claimed by two journal transactions is
    /// a conflicting reuse and fails closed.
    fn collect_pending_keys(
        &self,
    ) -> Result<BTreeMap<ConsequenceId, (Vec<ClassifiedSpan>, TransactionId)>, PipelineError> {
        let mut keys: BTreeMap<ConsequenceId, (Vec<ClassifiedSpan>, TransactionId)> =
            BTreeMap::new();
        for transaction in self.journal.transactions() {
            if !self.journal.is_actionable(transaction.id) {
                continue;
            }
            for envelope in &transaction.envelopes {
                if self.confirmed.contains_key(&envelope.id) {
                    continue;
                }
                if keys.contains_key(&envelope.id) {
                    return Err(PipelineError::ConflictingReuse { id: envelope.id });
                }
                keys.insert(envelope.id, (envelope.sub_spans.clone(), transaction.id));
            }
        }
        Ok(keys)
    }

    /// The attempted keys still unconfirmed after a re-walk (the
    /// rebase set — identical identities and coverages).
    fn remaining_keys(
        &self,
        attempted: BTreeMap<ConsequenceId, (Vec<ClassifiedSpan>, TransactionId)>,
    ) -> Result<BTreeMap<ConsequenceId, (Vec<ClassifiedSpan>, TransactionId)>, PipelineError> {
        let mut remaining = BTreeMap::new();
        for (id, evidence) in attempted {
            if self.confirmed.contains_key(&id) {
                // Confirmed by another writer's batch — never
                // re-submitted; the per-key status promotes.
                continue;
            }
            remaining.insert(id, evidence);
        }
        Ok(remaining)
    }

    /// Builds the batch for `keys`, submits it against `parent`, and
    /// handles the stale-rebase once. The idempotency key is the
    /// batch's content hash — the rebase preserves per-key
    /// identities, and the medium's chain-wide key uniqueness makes a
    /// double-install unrepresentable.
    fn submit_keys(
        &mut self,
        keys: BTreeMap<ConsequenceId, (Vec<ClassifiedSpan>, TransactionId)>,
        parent: Hash,
    ) -> Result<CommitOutcome, PipelineError> {
        let (attempt, outcome) = self.append_for(keys.clone(), parent)?;
        match outcome {
            AppendOutcome::Accepted { id, tier: _ } => {
                let attempt = attempt.expect("accepted carries the attempt");
                if id != attempt.record.id() {
                    return Err(PipelineError::ConfirmationInvalid {
                        detail: "accepted-revision-identity-mismatch",
                    });
                }
                self.confirm(attempt)
            }
            AppendOutcome::RejectedStaleParent { observed } => {
                // Revalidate: the observed chain was validated by the
                // medium before the rejection; the suffix walk
                // determines the now-confirmed keys.
                self.walk_chain()?;
                let attempted: Vec<ConsequenceId> = keys.keys().copied().collect();
                let remaining = self.remaining_keys(keys)?;
                if remaining.is_empty() {
                    return Ok(CommitOutcome::Confirmed(self.foreign_receipt(attempted)?));
                }
                let observed_parent = observed
                    .or(self.verified_head)
                    .ok_or(PipelineError::UnfoundedMedium)?;
                let (attempt, outcome) = self.append_for(remaining, observed_parent)?;
                match outcome {
                    AppendOutcome::Accepted { id, tier: _ } => {
                        let attempt = attempt.expect("accepted carries the attempt");
                        if id != attempt.record.id() {
                            return Err(PipelineError::ConfirmationInvalid {
                                detail: "accepted-revision-identity-mismatch",
                            });
                        }
                        self.confirm(attempt)
                    }
                    AppendOutcome::RejectedStaleParent { observed } => {
                        // The rebase was rejected again — retain the
                        // last built attempt for retry_pending.
                        if let Some(attempt) = attempt {
                            self.pending = Some(attempt);
                        }
                        Ok(CommitOutcome::StaleParent { observed })
                    }
                    AppendOutcome::Unknown { reason } => {
                        let revision = attempt
                            .as_ref()
                            .map(|attempt| attempt.record.id())
                            .ok_or(PipelineError::UnfoundedMedium)?;
                        if let Some(attempt) = attempt {
                            self.pending = Some(attempt);
                        }
                        Ok(CommitOutcome::Unknown { revision, reason })
                    }
                }
            }
            AppendOutcome::Unknown { reason } => {
                let revision = attempt
                    .as_ref()
                    .map(|attempt| attempt.record.id())
                    .ok_or(PipelineError::UnfoundedMedium)?;
                if let Some(attempt) = attempt {
                    self.pending = Some(attempt);
                }
                Ok(CommitOutcome::Unknown { revision, reason })
            }
        }
    }

    /// Builds the batch content object and the revision record for
    /// `keys` on `parent`, and appends. The tick derives
    /// deterministically from the batched transactions (retry
    /// stability).
    fn append_for(
        &mut self,
        keys: BTreeMap<ConsequenceId, (Vec<ClassifiedSpan>, TransactionId)>,
        parent: Hash,
    ) -> Result<(Option<PendingAttempt>, AppendOutcome), PipelineError> {
        let mut entries: Vec<BatchEntry> = Vec::with_capacity(keys.len());
        let mut tick: u64 = 0;
        for (id, (coverage, transaction)) in &keys {
            entries.push(BatchEntry {
                id: *id,
                transaction: *transaction,
                coverage: coverage.clone(),
            });
            if let Some(status) = self.journal_transactions_lookup(*transaction) {
                tick = tick.max(status);
            }
        }
        let batch = ConsequenceBatch { entries };
        let batch_bytes = batch.canonical_bytes().map_err(PipelineError::Format)?;
        let batch_object = StoredObject::new(batch_bytes)?;
        let batch_hash = batch_object.hash();
        // The idempotency key IS the batch's content identity —
        // chain-unique, so the same logical batch can never
        // double-install however often it is retried or rebased.
        let record = RevisionRecord::regular(parent, self.verified_epoch, tick)
            .with_idempotency_key(batch_hash.as_bytes().to_vec())
            .with_content_refs(vec![batch_hash]);
        let outcome =
            self.medium
                .append(&record, std::slice::from_ref(&batch_object), Some(parent))?;
        let attempt = PendingAttempt {
            record,
            batch_hash,
            keys,
        };
        Ok((Some(attempt), outcome))
    }

    /// The position tick of one journal transaction (retry-stable
    /// record derivation).
    fn journal_transactions_lookup(&self, transaction: TransactionId) -> Option<u64> {
        self.journal
            .transactions()
            .iter()
            .find(|committed| committed.id == transaction)
            .map(|committed| committed.position.tick)
    }

    /// The confirmation procedure: the verified read-back walk (a
    /// bare Accepted is never confirmation), the batch read back by
    /// its content identity, and the **individual validation** of
    /// every attempted key — present, with exactly the attempted
    /// coverage and producing transaction, placed by the walk at the
    /// confirming revision, with no unattempted keys.
    fn confirm(&mut self, attempt: PendingAttempt) -> Result<CommitOutcome, PipelineError> {
        self.walk_chain()?;
        let bytes = self.medium.read_object(&attempt.batch_hash)?;
        let batch = ConsequenceBatch::decode(&bytes).map_err(PipelineError::Format)?;
        if batch.entries.len() != attempt.keys.len() {
            return Err(PipelineError::ConfirmationInvalid {
                detail: "batch-key-set-mismatch",
            });
        }
        let revision = attempt.record.id();
        for (id, (coverage, transaction)) in &attempt.keys {
            let Some(entry) = batch.entries.iter().find(|entry| entry.id == *id) else {
                return Err(PipelineError::ConfirmationInvalid {
                    detail: "key-missing-from-confirmed-batch",
                });
            };
            if entry.coverage != *coverage || entry.transaction != *transaction {
                return Err(PipelineError::ConfirmationInvalid {
                    detail: "coverage-mismatch-in-confirmed-batch",
                });
            }
            let Some(confirmed) = self.confirmed.get(id) else {
                return Err(PipelineError::ConfirmationInvalid {
                    detail: "key-not-chain-included",
                });
            };
            if confirmed.revision != revision {
                return Err(PipelineError::ConfirmationInvalid {
                    detail: "key-confirmed-by-different-revision",
                });
            }
        }
        for id in attempt.keys.keys() {
            if let Some(status) = self.keys.get_mut(id) {
                status.phase = KeyPhase::InConfirmedChain;
                status.revision = Some(revision);
            }
        }
        let (authority, _) = self
            .founding
            .as_ref()
            .ok_or(PipelineError::UnfoundedMedium)?;
        let label = match authority {
            AuthorityMode::Standalone => ConfirmationLabel::T3LocalStandalone,
            AuthorityMode::Shared => ConfirmationLabel::SharedCanonical,
        };
        Ok(CommitOutcome::Confirmed(ConfirmationReceipt {
            revision,
            epoch: attempt.record.epoch,
            keys: attempt.keys.keys().copied().collect(),
            tier: self.tier,
            authority: *authority,
            label,
        }))
    }

    /// A confirmation receipt for keys another writer's revision
    /// confirmed (the re-walk placed them; per-key revisions are
    /// queryable via the confirmed set).
    fn foreign_receipt(
        &self,
        keys: Vec<ConsequenceId>,
    ) -> Result<ConfirmationReceipt, PipelineError> {
        let revision = self.verified_head.ok_or(PipelineError::UnfoundedMedium)?;
        let (authority, _) = self
            .founding
            .as_ref()
            .ok_or(PipelineError::UnfoundedMedium)?;
        Ok(ConfirmationReceipt {
            revision,
            epoch: self.verified_epoch,
            keys,
            tier: self.tier,
            authority: *authority,
            label: match authority {
                AuthorityMode::Standalone => ConfirmationLabel::T3LocalStandalone,
                AuthorityMode::Shared => ConfirmationLabel::SharedCanonical,
            },
        })
    }

    /// The verified chain walk: re-reads every new record
    /// (hash-verified by the medium) from the head down to the last
    /// processed revision, collects every `consequence-batch/1`
    /// entry per key (the suppression authority), and promotes
    /// tracked keys. Non-batch content objects are skipped — the
    /// store is domain-agnostic; only consequence batches carry
    /// keys. O(new revisions) within a session; O(chain) at open —
    /// never per tick.
    fn walk_chain(&mut self) -> Result<(), PipelineError> {
        let Some(head) = self.medium.head()? else {
            return Ok(());
        };
        let mut chain: Vec<(Hash, RevisionRecord)> = Vec::new();
        let mut cursor = head;
        loop {
            if self.verified_head == Some(cursor) {
                break;
            }
            let record = self.medium.read_record(&cursor)?;
            let parent = record.parent;
            chain.push((cursor, record));
            match parent {
                Some(parent) => cursor = parent,
                None => break,
            }
        }
        for (id, record) in chain.into_iter().rev() {
            if let RecordKind::Founding {
                authority,
                campaign_id,
            } = &record.kind
            {
                self.founding = Some((*authority, campaign_id.clone()));
            }
            for content in &record.content_refs {
                let bytes = self.medium.read_object(content)?;
                if peek_domain(&bytes).as_deref() == Some(CONSEQUENCE_BATCH_DOMAIN) {
                    let batch = ConsequenceBatch::decode(&bytes).map_err(PipelineError::Format)?;
                    for entry in batch.entries {
                        self.confirmed.insert(
                            entry.id,
                            ConfirmedEntry {
                                coverage: entry.coverage,
                                revision: id,
                            },
                        );
                    }
                }
            }
            self.verified_head = Some(id);
            self.verified_epoch = record.epoch;
        }
        for (id, status) in &mut self.keys {
            if let Some(confirmed) = self.confirmed.get(id) {
                status.phase = KeyPhase::InConfirmedChain;
                status.revision = Some(confirmed.revision);
            }
        }
        Ok(())
    }
}

/// Peeks a content object's domain tag (the store is domain-agnostic;
/// only `consequence-batch/1` objects carry keys).
fn peek_domain(bytes: &[u8]) -> Option<String> {
    let mut dec = Decoder::new(bytes);
    dec.read_str(MAX_TAG_LEN, "content-peek")
        .ok()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consequence::{AwardCheckpoint, Coverage, OccurrenceSpan};
    use crate::envelope::{CounterValues, SignalLane, SourceBaseline};
    use crate::fault::{FaultAction, FaultControl, FaultPoint};
    use crate::local::LocalFileMedium;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("s7-p3-unit-{}-{label}-{seq}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create test root");
            TestDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn store(&self) -> PathBuf {
            self.0.join("store")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Idempotently founds the campaign store (the founding is the
    /// application's setup act, never a per-commit decision).
    fn ensure_founded(dir: &TestDir) {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        if medium.head().unwrap().is_some() {
            return;
        }
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "pipeline-campaign", 0);
        match medium.append(&founding, &[], None).unwrap() {
            AppendOutcome::Accepted { .. } => {}
            other => panic!("the founding must accept: {other:?}"),
        }
    }

    fn clean_medium(dir: &TestDir) -> LocalFileMedium {
        ensure_founded(dir);
        LocalFileMedium::open(dir.store()).unwrap()
    }

    struct Boundary {
        position: SourcePosition,
        classification: DurabilityClass,
        dependencies: Vec<TransactionId>,
        frontier: Vec<FrontierEntry>,
        plan: Vec<u8>,
    }

    fn boundary(index: u64) -> Boundary {
        Boundary {
            position: SourcePosition {
                head: None,
                tick: 100 + index,
            },
            classification: DurabilityClass::DurableEligible,
            dependencies: Vec::new(),
            frontier: vec![FrontierEntry {
                region: 1,
                episode: index,
            }],
            plan: format!("plan-{index}").into_bytes(),
        }
    }

    fn fresh_pipeline(dir: &TestDir) -> CommitPipeline<LocalFileMedium> {
        let journal = T1Journal::open(dir.path().join("journal.t1")).unwrap();
        CommitPipeline::open(clean_medium(dir), journal).unwrap()
    }

    fn capture_index(pipeline: &mut CommitPipeline<LocalFileMedium>, index: u64) -> TransactionId {
        pipeline.stage(staged_envelope(index)).unwrap();
        let boundary = boundary(index);
        let receipt = pipeline
            .capture(
                boundary.position,
                boundary.classification,
                boundary.dependencies,
                boundary.frontier,
                boundary.plan,
            )
            .unwrap();
        assert!(!receipt.reconciled);
        receipt.journal.id
    }

    fn staged_envelope(index: u64) -> ConsequenceEnvelope {
        ConsequenceEnvelope::capture(
            ConsequenceId::new(1, index, 1),
            vec![ClassifiedSpan {
                span: OccurrenceSpan::new(2 * (index - 1), 2 * index).unwrap(),
                class: DurabilityClass::DurableEligible,
            }],
            SignalLane::Captured,
            CounterValues::default(),
            SourceBaseline {
                counters: CounterValues::default(),
                award: AwardCheckpoint {
                    durable: Coverage::new(),
                    provisional: Coverage::new(),
                },
            },
            SourcePosition {
                head: None,
                tick: 100 + index,
            },
        )
        .unwrap()
    }

    // ------------------------------------------------------------ the flow

    #[test]
    fn stage_capture_commit_confirms_end_to_end() {
        let dir = TestDir::new("end-to-end");
        let mut pipeline = fresh_pipeline(&dir);
        for index in 1..=3u64 {
            pipeline.stage(staged_envelope(index)).unwrap();
            assert_eq!(
                pipeline
                    .status(ConsequenceId::new(1, index, 1))
                    .unwrap()
                    .phase,
                KeyPhase::InMemory
            );
        }
        let boundary = boundary(1);
        let receipt = pipeline
            .capture(
                boundary.position,
                boundary.classification,
                boundary.dependencies,
                boundary.frontier,
                boundary.plan,
            )
            .unwrap();
        for index in 1..=3u64 {
            assert_eq!(
                pipeline
                    .status(ConsequenceId::new(1, index, 1))
                    .unwrap()
                    .phase,
                KeyPhase::JournaledT1
            );
            assert_eq!(
                pipeline
                    .status(ConsequenceId::new(1, index, 1))
                    .unwrap()
                    .transaction,
                Some(receipt.journal.id)
            );
        }

        let CommitOutcome::Confirmed(confirmation) = pipeline.commit_canonical().unwrap() else {
            panic!("expected confirmation");
        };
        assert_eq!(confirmation.keys.len(), 3);
        assert_eq!(confirmation.epoch, 1, "founding epoch 0 + one revision");
        assert_eq!(confirmation.authority, AuthorityMode::Standalone);
        assert_eq!(confirmation.label, ConfirmationLabel::T3LocalStandalone);
        assert_eq!(confirmation.label.to_string(), "T3-local (standalone)");
        for index in 1..=3u64 {
            let id = ConsequenceId::new(1, index, 1);
            assert_eq!(
                pipeline.status(id).unwrap().phase,
                KeyPhase::InConfirmedChain
            );
            assert!(pipeline.is_confirmed(id));
            // The per-key coverage evidence — individually tracked,
            // never a batch aggregate alone.
            assert_eq!(
                pipeline.confirmed_coverage(id).unwrap(),
                staged_envelope(index).sub_spans.as_slice()
            );
        }
        // The chain holds founding + one revision; re-committing is a
        // no-op.
        assert_eq!(
            pipeline.commit_canonical().unwrap(),
            CommitOutcome::NothingPending
        );
        let mut check = LocalFileMedium::open(dir.store()).unwrap();
        assert_eq!(check.validate().unwrap().records, 2);
    }

    #[test]
    fn t1_pending_until_genuine_t3() {
        let dir = TestDir::new("t1-pending");
        let mut pipeline = fresh_pipeline(&dir);
        capture_index(&mut pipeline, 1);
        // Journaled (T1) — durable and replayable, but NOT confirmed:
        // the chain holds only the founding revision.
        assert_eq!(
            pipeline.status(ConsequenceId::new(1, 1, 1)).unwrap().phase,
            KeyPhase::JournaledT1
        );
        assert!(!pipeline.is_confirmed(ConsequenceId::new(1, 1, 1)));
        let mut check = LocalFileMedium::open(dir.store()).unwrap();
        assert_eq!(check.validate().unwrap().records, 1);
        // The confirmation flips the phase only through the verified
        // chain inclusion.
        let CommitOutcome::Confirmed(_) = pipeline.commit_canonical().unwrap() else {
            panic!("expected confirmation");
        };
        assert_eq!(
            pipeline.status(ConsequenceId::new(1, 1, 1)).unwrap().phase,
            KeyPhase::InConfirmedChain
        );
    }

    #[test]
    fn confirmation_rejects_mismatched_attempts() {
        let dir = TestDir::new("confirm-invalid");
        let mut pipeline = fresh_pipeline(&dir);
        let transaction = capture_index(&mut pipeline, 1);
        let CommitOutcome::Confirmed(confirmation) = pipeline.commit_canonical().unwrap() else {
            panic!("expected confirmation");
        };
        // The real batch object is content-addressed — rebuilding the
        // same entries reproduces the exact on-chain bytes (and
        // therefore the same hash), so a doctored attempt against it
        // reaches the individual validation rather than a missing
        // object.
        let real = ConsequenceBatch {
            entries: vec![BatchEntry {
                id: ConsequenceId::new(1, 1, 1),
                transaction,
                coverage: staged_envelope(1).sub_spans.clone(),
            }],
        };
        let object = StoredObject::new(real.canonical_bytes().unwrap()).unwrap();
        // (a) The coverage lies about what was attempted — the
        // individual validation must catch it and never report a
        // false T3.
        let mut keys = BTreeMap::new();
        keys.insert(
            ConsequenceId::new(1, 1, 1),
            (
                vec![ClassifiedSpan {
                    span: OccurrenceSpan::new(99, 100).unwrap(),
                    class: DurabilityClass::DurableEligible,
                }],
                transaction,
            ),
        );
        let attempt = PendingAttempt {
            record: RevisionRecord::regular(confirmation.revision, confirmation.epoch, 101),
            batch_hash: object.hash(),
            keys,
        };
        assert!(matches!(
            pipeline.confirm(attempt),
            Err(PipelineError::ConfirmationInvalid {
                detail: "coverage-mismatch-in-confirmed-batch"
            })
        ));
        // (b) A key the confirmed batch never carried.
        let mut keys = BTreeMap::new();
        keys.insert(
            ConsequenceId::new(1, 1, 1),
            (staged_envelope(1).sub_spans.clone(), transaction),
        );
        keys.insert(
            ConsequenceId::new(1, 5, 1),
            (staged_envelope(5).sub_spans.clone(), transaction),
        );
        let batch_hash = object.hash();
        let attempt = PendingAttempt {
            record: RevisionRecord::regular(confirmation.revision, confirmation.epoch, 101),
            batch_hash,
            keys,
        };
        assert!(matches!(
            pipeline.confirm(attempt),
            Err(PipelineError::ConfirmationInvalid {
                detail: "batch-key-set-mismatch"
            })
        ));
    }

    #[test]
    fn unknown_installed_reconciles_to_confirmed() {
        let dir = TestDir::new("unknown-installed");
        // The store is founded through a clean instance; the
        // pipeline's medium carries the fault — the rename happens,
        // the acknowledgment is lost.
        ensure_founded(&dir);
        let journal = T1Journal::open(dir.path().join("journal.t1")).unwrap();
        let medium = LocalFileMedium::open_with(
            dir.store(),
            Some(FaultControl::new().arm(FaultPoint::AfterHeadRename, FaultAction::InjectError)),
        )
        .unwrap();
        let mut pipeline = CommitPipeline::open(medium, journal).unwrap();
        capture_index(&mut pipeline, 1);
        let CommitOutcome::Unknown { revision, .. } = pipeline.commit_canonical().unwrap() else {
            panic!("expected unknown");
        };
        assert_eq!(
            pipeline.status(ConsequenceId::new(1, 1, 1)).unwrap().phase,
            KeyPhase::JournaledT1,
            "an unknown outcome is never a confirmation"
        );
        // A new attempt is refused until the unknown is resolved
        // (identity exactness).
        assert!(matches!(
            pipeline.commit_canonical(),
            Err(PipelineError::AttemptUnresolved { revision: pending }) if pending == revision
        ));
        let ReconcileOutcome::Confirmed(confirmation) = pipeline.reconcile_pending().unwrap()
        else {
            panic!("expected installed confirmation");
        };
        assert_eq!(confirmation.revision, revision);
        assert_eq!(
            pipeline.status(ConsequenceId::new(1, 1, 1)).unwrap().phase,
            KeyPhase::InConfirmedChain
        );
        let mut check = LocalFileMedium::open(dir.store()).unwrap();
        assert_eq!(check.validate().unwrap().records, 2);
    }

    #[test]
    fn unknown_absent_retries_idempotently() {
        let dir = TestDir::new("unknown-absent");
        // Founded through a clean instance; the pipeline's medium
        // fails before the rename — Unknown, not installed.
        ensure_founded(&dir);
        let journal = T1Journal::open(dir.path().join("journal.t1")).unwrap();
        let medium = LocalFileMedium::open_with(
            dir.store(),
            Some(FaultControl::new().arm(FaultPoint::HeadTmpWritten, FaultAction::InjectError)),
        )
        .unwrap();
        let mut pipeline = CommitPipeline::open(medium, journal).unwrap();
        capture_index(&mut pipeline, 1);
        let CommitOutcome::Unknown { .. } = pipeline.commit_canonical().unwrap() else {
            panic!("expected unknown");
        };
        assert_eq!(
            pipeline.reconcile_pending().unwrap(),
            ReconcileOutcome::NotInstalled
        );
        // The retry on the still-faulted medium is ambiguous again —
        // retained, never guessed.
        let CommitOutcome::Unknown { .. } = pipeline.retry_pending().unwrap() else {
            panic!("expected the retry to stay ambiguous on the faulted medium");
        };
        assert_eq!(
            pipeline.reconcile_pending().unwrap(),
            ReconcileOutcome::NotInstalled
        );
        // Recovery through a clean instance: the same content
        // identity (the same batch bytes, hash, and idempotency key)
        // installs exactly once — no duplicate revision, no duplicate
        // effect.
        drop(pipeline);
        let mut recovered = fresh_pipeline(&dir);
        let CommitOutcome::Confirmed(_) = recovered.commit_canonical().unwrap() else {
            panic!("expected confirmation");
        };
        assert_eq!(
            recovered.status(ConsequenceId::new(1, 1, 1)).unwrap().phase,
            KeyPhase::InConfirmedChain
        );
        let mut check = LocalFileMedium::open(dir.store()).unwrap();
        assert_eq!(check.validate().unwrap().records, 2, "one revision only");
    }

    #[test]
    fn stale_parent_rebases_preserving_identity() {
        let dir = TestDir::new("stale-rebase");
        // Two pipelines over one store, separate journals. B opens
        // before A commits, so B appends against the founding — the
        // stale-parent path.
        let journal_a = T1Journal::open(dir.path().join("journal-a.t1")).unwrap();
        let mut a = CommitPipeline::open(clean_medium(&dir), journal_a).unwrap();
        let journal_b = T1Journal::open(dir.path().join("journal-b.t1")).unwrap();
        let mut b = CommitPipeline::open(clean_medium(&dir), journal_b).unwrap();

        capture_index(&mut a, 1);
        let CommitOutcome::Confirmed(confirmation_a) = a.commit_canonical().unwrap() else {
            panic!("expected A confirmed");
        };

        // B stages its own identity (a different episode) and commits
        // against its verified head (the founding) — stale.
        pipeline_stage_b(&mut b, 2);
        let CommitOutcome::Confirmed(confirmation_b) = b.commit_canonical().unwrap() else {
            panic!("expected B confirmed through the rebase");
        };
        assert_ne!(confirmation_a.revision, confirmation_b.revision);
        // The per-key identity and coverage survived the rebase
        // unchanged.
        let id = ConsequenceId::new(1, 2, 1);
        assert_eq!(b.status(id).unwrap().phase, KeyPhase::InConfirmedChain);
        assert_eq!(
            b.confirmed_coverage(id).unwrap(),
            staged_envelope(2).sub_spans.as_slice()
        );
        // B's re-walk also collected A's keys (the chain's committed
        // set is global).
        assert!(b.is_confirmed(ConsequenceId::new(1, 1, 1)));
        let mut check = LocalFileMedium::open(dir.store()).unwrap();
        assert_eq!(check.validate().unwrap().records, 3, "founding + A + B");
    }

    fn pipeline_stage_b(pipeline: &mut CommitPipeline<LocalFileMedium>, index: u64) {
        pipeline.stage(staged_envelope(index)).unwrap();
        let boundary = boundary(index);
        pipeline
            .capture(
                boundary.position,
                boundary.classification,
                boundary.dependencies,
                boundary.frontier,
                boundary.plan,
            )
            .unwrap();
    }

    #[test]
    fn foreign_confirmation_never_duplicates() {
        let dir = TestDir::new("foreign-confirmed");
        // A commits key k1; B — with its own journal holding the SAME
        // deterministic identity — commits after A: the re-walk finds
        // k1 already chain-confirmed, so B confirms without
        // re-submitting (no duplicate revision, no duplicate effect).
        let journal_a = T1Journal::open(dir.path().join("journal-a.t1")).unwrap();
        let mut a = CommitPipeline::open(clean_medium(&dir), journal_a).unwrap();
        let journal_b = T1Journal::open(dir.path().join("journal-b.t1")).unwrap();
        let mut b = CommitPipeline::open(clean_medium(&dir), journal_b).unwrap();

        capture_index(&mut a, 1);
        let CommitOutcome::Confirmed(_) = a.commit_canonical().unwrap() else {
            panic!("expected A confirmed");
        };
        pipeline_stage_b(&mut b, 1);
        let CommitOutcome::Confirmed(receipt) = b.commit_canonical().unwrap() else {
            panic!("expected the foreign confirmation");
        };
        assert_eq!(receipt.revision, a_head(&dir), "the confirming chain tip");
        assert_eq!(
            b.status(ConsequenceId::new(1, 1, 1)).unwrap().phase,
            KeyPhase::InConfirmedChain
        );
        let mut check = LocalFileMedium::open(dir.store()).unwrap();
        assert_eq!(
            check.validate().unwrap().records,
            2,
            "no duplicate revision"
        );
    }

    fn a_head(dir: &TestDir) -> Hash {
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        medium.head().unwrap().expect("founded")
    }

    #[test]
    fn duplicate_identity_reuse_fails_closed() {
        let dir = TestDir::new("duplicate-reuse");
        let mut pipeline = fresh_pipeline(&dir);
        capture_index(&mut pipeline, 1);
        // A different envelope under the same identity — conflicting
        // reuse.
        let mut conflicting = staged_envelope(2);
        conflicting.id = ConsequenceId::new(1, 1, 1);
        assert!(matches!(
            pipeline.stage(conflicting),
            Err(PipelineError::ConflictingReuse { .. })
        ));
        // A byte-identical restage is a no-op.
        let staged = pipeline.staged.len();
        pipeline.stage(staged_envelope(2)).unwrap();
        assert_eq!(pipeline.staged.len(), staged + 1);
        let before = pipeline.staged.len();
        pipeline.stage(staged_envelope(2)).unwrap();
        assert_eq!(
            pipeline.staged.len(),
            before,
            "the identical restage is a no-op"
        );

        // Two journal transactions claiming one identity (built
        // directly through the journal — the pipeline is dropped
        // first: the journal's flock is held for the instance
        // lifetime) fail the batch collection.
        drop(pipeline);
        let mut raw = T1Journal::open(dir.path().join("journal.t1")).unwrap();
        let boundary = boundary(9);
        let input = TransactionInput {
            envelopes: vec![staged_envelope(1)],
            position: boundary.position,
            classification: boundary.classification,
            dependencies: boundary.dependencies,
            frontier: boundary.frontier,
            plan: b"plan-other".to_vec(),
        };
        raw.append_transaction(&input).unwrap();
        drop(raw);
        let mut pipeline = fresh_pipeline(&dir);
        assert!(matches!(
            pipeline.commit_canonical(),
            Err(PipelineError::ConflictingReuse { .. })
        ));
    }

    #[test]
    fn canceled_transactions_are_excluded_from_the_batch() {
        let dir = TestDir::new("canceled-excluded");
        let mut pipeline = fresh_pipeline(&dir);
        let first = capture_index(&mut pipeline, 1);
        let second = capture_index(&mut pipeline, 2);
        pipeline
            .cancel_transaction(first, CancellationReason::ApplicationPolicy, b"policy")
            .unwrap();
        let CommitOutcome::Confirmed(confirmation) = pipeline.commit_canonical().unwrap() else {
            panic!("expected confirmation");
        };
        assert_eq!(confirmation.keys, vec![ConsequenceId::new(1, 2, 1)]);
        assert_eq!(
            pipeline.status(ConsequenceId::new(1, 1, 1)).unwrap().phase,
            KeyPhase::JournaledT1,
            "the canceled transaction's key stays T1-pending, never submitted"
        );
        assert!(pipeline.is_confirmed(ConsequenceId::new(1, 2, 1)));
        assert!(!pipeline.is_confirmed(ConsequenceId::new(1, 1, 1)));
        let _ = second;
    }

    #[test]
    fn mark_applied_tracks_pending_vs_confirmed() {
        let dir = TestDir::new("applied-pending");
        let mut pipeline = fresh_pipeline(&dir);
        // Applying an unstaged key is refused (durable-before-discard).
        pipeline.stage(staged_envelope(1)).unwrap();
        assert!(matches!(
            pipeline.mark_applied(&[ConsequenceId::new(1, 1, 1)]),
            Err(PipelineError::AppliedBeforeJournal { .. })
        ));
        assert!(matches!(
            pipeline.mark_applied(&[ConsequenceId::new(9, 9, 9)]),
            Err(PipelineError::UnknownKey { .. })
        ));
        let boundary = boundary(1);
        pipeline
            .capture(
                boundary.position,
                boundary.classification,
                boundary.dependencies,
                boundary.frontier,
                boundary.plan,
            )
            .unwrap();
        // The effect exists in live state while the canonical commit
        // is pending — never conflated with durability.
        pipeline
            .mark_applied(&[ConsequenceId::new(1, 1, 1)])
            .unwrap();
        assert_eq!(
            pipeline.status(ConsequenceId::new(1, 1, 1)).unwrap().phase,
            KeyPhase::AppliedToLivePending
        );
        assert!(!pipeline.is_confirmed(ConsequenceId::new(1, 1, 1)));
        let CommitOutcome::Confirmed(_) = pipeline.commit_canonical().unwrap() else {
            panic!("expected confirmation");
        };
        assert_eq!(
            pipeline.status(ConsequenceId::new(1, 1, 1)).unwrap().phase,
            KeyPhase::InConfirmedChain
        );
    }

    #[test]
    fn reopen_derives_statuses_from_chain_and_journal() {
        let dir = TestDir::new("reopen");
        let mut pipeline = fresh_pipeline(&dir);
        capture_index(&mut pipeline, 1);
        capture_index(&mut pipeline, 2);
        let CommitOutcome::Confirmed(_) = pipeline.commit_canonical().unwrap() else {
            panic!("expected confirmation");
        };
        capture_index(&mut pipeline, 3);
        // Staged-but-uncaptured keys are honestly lost across
        // sessions (P0-5).
        pipeline.stage(staged_envelope(4)).unwrap();
        drop(pipeline);

        let reopened = fresh_pipeline(&dir);
        assert_eq!(
            reopened.status(ConsequenceId::new(1, 1, 1)).unwrap().phase,
            KeyPhase::InConfirmedChain
        );
        assert_eq!(
            reopened.status(ConsequenceId::new(1, 2, 1)).unwrap().phase,
            KeyPhase::InConfirmedChain
        );
        assert_eq!(
            reopened.status(ConsequenceId::new(1, 3, 1)).unwrap().phase,
            KeyPhase::JournaledT1
        );
        assert!(reopened.status(ConsequenceId::new(1, 4, 1)).is_none());
        assert!(reopened.is_confirmed(ConsequenceId::new(1, 1, 1)));
    }

    #[test]
    fn medium_failure_leaves_commits_pending() {
        let dir = TestDir::new("medium-failure");
        ensure_founded(&dir);
        let journal = T1Journal::open(dir.path().join("journal.t1")).unwrap();
        let medium = LocalFileMedium::open_with(
            dir.store(),
            Some(FaultControl::new().arm(FaultPoint::BeforeLock, FaultAction::InjectError)),
        )
        .unwrap();
        let mut pipeline = CommitPipeline::open(medium, journal).unwrap();
        capture_index(&mut pipeline, 1);
        // A pre-window medium failure is a clean error — the commit
        // stays pending and the gates hold.
        assert!(matches!(
            pipeline.commit_canonical(),
            Err(PipelineError::Medium(_))
        ));
        assert_eq!(
            pipeline.status(ConsequenceId::new(1, 1, 1)).unwrap().phase,
            KeyPhase::JournaledT1
        );
        // Recovery through a clean medium instance confirms.
        drop(pipeline);
        let mut recovered = fresh_pipeline(&dir);
        let CommitOutcome::Confirmed(_) = recovered.commit_canonical().unwrap() else {
            panic!("expected confirmation");
        };
    }

    #[test]
    fn nothing_pending_and_unfounded() {
        let dir = TestDir::new("nothing-pending");
        let mut pipeline = fresh_pipeline(&dir);
        assert_eq!(
            pipeline.commit_canonical().unwrap(),
            CommitOutcome::NothingPending
        );
        capture_index(&mut pipeline, 1);
        let CommitOutcome::Confirmed(_) = pipeline.commit_canonical().unwrap() else {
            panic!("expected confirmation");
        };
        assert_eq!(
            pipeline.commit_canonical().unwrap(),
            CommitOutcome::NothingPending
        );

        // An unfounded medium never reaches submission.
        let dir2 = TestDir::new("unfounded");
        let journal = T1Journal::open(dir2.path().join("journal.t1")).unwrap();
        let mut unfounded =
            CommitPipeline::open(LocalFileMedium::open(dir2.store()).unwrap(), journal).unwrap();
        capture_index(&mut unfounded, 1);
        assert!(matches!(
            unfounded.commit_canonical(),
            Err(PipelineError::UnfoundedMedium)
        ));
    }

    #[test]
    fn empty_boundary_capture_is_journal_only() {
        let dir = TestDir::new("empty-boundary");
        let mut pipeline = fresh_pipeline(&dir);
        let boundary = boundary(7);
        let receipt = pipeline
            .capture(
                boundary.position,
                boundary.classification,
                boundary.dependencies,
                boundary.frontier,
                boundary.plan,
            )
            .unwrap();
        assert_eq!(receipt.journal.record_count, 0, "a frontier-only boundary");
        assert_eq!(
            pipeline.commit_canonical().unwrap(),
            CommitOutcome::NothingPending
        );
    }

    // ------------------------------------------------------- batch codec

    #[test]
    fn batch_codec_round_trips_and_rejects_corruption() {
        let batch = ConsequenceBatch {
            entries: vec![
                BatchEntry {
                    id: ConsequenceId::new(1, 2, 3),
                    transaction: TransactionId(Hash::from_bytes(&[7u8; 32]).unwrap()),
                    coverage: vec![
                        ClassifiedSpan {
                            span: OccurrenceSpan::new(0, 2).unwrap(),
                            class: DurabilityClass::DurableEligible,
                        },
                        ClassifiedSpan {
                            span: OccurrenceSpan::new(5, 7).unwrap(),
                            class: DurabilityClass::EncounterProvisional,
                        },
                    ],
                },
                BatchEntry {
                    id: ConsequenceId::new(1, 2, 4),
                    transaction: TransactionId(Hash::from_bytes(&[9u8; 32]).unwrap()),
                    coverage: Vec::new(),
                },
            ],
        };
        let bytes = batch.canonical_bytes().unwrap();
        assert_eq!(ConsequenceBatch::decode(&bytes).unwrap(), batch);
        // Truncation at every boundary, trailing bytes, a wrong
        // schema, and descending entries all fail closed.
        for cut in 0..bytes.len() {
            assert!(ConsequenceBatch::decode(&bytes[..cut]).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(ConsequenceBatch::decode(&trailing).is_err());
        let mut wrong_schema = bytes.clone();
        wrong_schema[8 + CONSEQUENCE_BATCH_DOMAIN.len()] = 9;
        assert!(ConsequenceBatch::decode(&wrong_schema).is_err());
        let mut descending = batch.clone();
        descending.entries.reverse();
        assert!(descending.canonical_bytes().is_err());
    }
}
