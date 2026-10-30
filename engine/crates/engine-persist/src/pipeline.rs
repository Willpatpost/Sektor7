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

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use engine_core::{Decoder, Encoder, SaveError};

use crate::checkpoint::{CHECKPOINT_METADATA_DOMAIN, CheckpointMetadata};
use crate::consequence::ConsequenceId;
use crate::envelope::{ClassifiedSpan, ConsequenceEnvelope, DurabilityClass, SourcePosition};
use crate::hash::Hash;
use crate::journal::{
    CancellationReason, FrontierEntry, JournalError, JournalReceipt, T1Journal, TransactionId,
    TransactionInput, WriteReconciliation,
};
use crate::medium::{
    AppendOutcome, CanonicalMedium, MediumError, ObjectClass, Reconciliation, StoredObject,
    SyncTier, UnknownReason,
};
use crate::record::{AuthorityMode, RecordKind, RevisionRecord};
use crate::recovery::ConfirmedEvidence;

/// The canonical multi-consequence batch's domain (versioned; the
/// content object referenced via the revision's `content_refs`).
/// **v2 (the third review's effect-equivalence corrective):** each
/// entry additionally carries the producing envelope's EFFECT
/// COMMITMENT — the SHA-256 of its canonical bytes — binding the
/// confirmed consequence to its actual authoritative effects. v1
/// carried only (identity, transaction, coverage), which cannot
/// distinguish two valid payloads sharing the identity-level
/// `TransactionId` and coverage (e.g. +2 pending vs +2 captured
/// under the same identity, span, classification, and plan). v1
/// never shipped in an accepted build (the B3 delivery remained on
/// HOLD), so there is no on-disk compatibility surface: the walk
/// reads v2 only, and this constant is the documented migration
/// boundary.
pub const CONSEQUENCE_BATCH_DOMAIN: &str = "sektor7/engine-persist/consequence-batch/2";
pub const CONSEQUENCE_BATCH_SCHEMA: u8 = 2;

/// Documented maximum for one batch's entries (bounded decode — each
/// entry is at least 16 + 40 + 8 bytes, also bounded by the remaining
/// input).
pub const MAX_BATCH_ENTRIES: u64 = 1 << 16;
/// Documented maximum for one entry's coverage spans.
pub const MAX_BATCH_SPANS: u64 = 1 << 16;

/// One consequence's entry in the canonical batch — the complete
/// per-consequence evidence: the identity, the producing transaction,
/// the occurrence coverage, and the effect commitment. Confirmation
/// validates each entry individually; the aggregate alone is never
/// sufficient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchEntry {
    pub id: ConsequenceId,
    pub transaction: TransactionId,
    /// The class-homogeneous occurrence coverage this consequence
    /// commits (mirrors the envelope's sub-spans).
    pub coverage: Vec<ClassifiedSpan>,
    /// The producing envelope's effect commitment — the SHA-256 of
    /// its canonical bytes — binding this confirmed consequence to
    /// its actual authoritative effects (the uniform signal lane,
    /// the deltas, the classes). The identity-level `TransactionId`
    /// is deliberately content-blind; THIS is the semantic binding
    /// that distinguishes effect equivalence from identity equality.
    pub effect: Hash,
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
            enc.write_bytes(entry.effect.as_bytes());
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
        // 32 (effect commitment) + 8 (coverage count) bytes.
        let remaining = dec.remaining() as u64;
        if count > remaining / 96 {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "entry-count",
                value: count,
                max: remaining / 96,
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
            let effect_bytes = dec.read_bytes(Hash::WIDTH, SECTION)?;
            let effect = Hash::from_bytes(effect_bytes)
                .ok_or(SaveError::Integrity("consequence-batch/hash-width"))?;
            let coverage = read_coverage(&mut dec)?;
            entries.push(BatchEntry {
                id,
                transaction,
                coverage,
                effect,
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

/// One coverage list's canonical form: per-span validity, sorted,
/// non-overlapping; same-class touching spans would have composed
/// (non-canonical); **different-class adjacency is the legitimate
/// class-homogeneous partition boundary** — a mixed aggregate's
/// sub-spans meet exactly there (envelope/1's rule, carried into the
/// canonical batch).
fn validate_coverage(coverage: &[ClassifiedSpan]) -> Result<(), SaveError> {
    for classified in coverage {
        if classified.span.lo >= classified.span.hi {
            return Err(SaveError::Integrity(
                "consequence-batch/coverage-span-inverted",
            ));
        }
    }
    for pair in coverage.windows(2) {
        let (prior, next) = (&pair[0], &pair[1]);
        if next.span.lo < prior.span.hi {
            return Err(SaveError::Integrity(
                "consequence-batch/coverage-span-overlap",
            ));
        }
        if next.span.lo == prior.span.hi && prior.class == next.class {
            return Err(SaveError::Integrity(
                "consequence-batch/coverage-span-noncanonical",
            ));
        }
    }
    Ok(())
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
    validate_coverage(coverage)?;
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
        let class = read_class(dec)?;
        coverage.push(ClassifiedSpan {
            span: crate::consequence::OccurrenceSpan { lo, hi },
            class,
        });
    }
    validate_coverage(&coverage)?;
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
    /// Conflicting reuse of a `ConsequenceId` across valid canonical
    /// batches: two chain revisions claim the same identity. The
    /// exact-provenance standard rejects the second — fail closed,
    /// never overwriting the first revision's evidence, never
    /// guessing which is authoritative.
    CanonicalIdentityConflict {
        id: ConsequenceId,
        first_revision: Hash,
        second_revision: Hash,
    },
    /// Foreign-confirmed keys span distinct confirming revisions —
    /// one honest receipt cannot report them; fail closed rather
    /// than misreport any key's provenance.
    ForeignProvenanceDivergent {
        id: ConsequenceId,
        first_revision: Hash,
        second_revision: Hash,
    },
    /// The local attempt's provenance conflicts with the canonical
    /// entry claiming the identity — a different producing
    /// transaction or occurrence coverage. Exact-provenance
    /// equivalence is required before a foreign confirmation
    /// satisfies the local attempt; a mismatch fails closed, never
    /// silently absorbed.
    ConfirmedProvenanceMismatch {
        id: ConsequenceId,
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
            PipelineError::CanonicalIdentityConflict {
                id,
                first_revision,
                second_revision,
            } => write!(
                f,
                "consequence identity {id} is claimed by two valid canonical batches \
                 ({first_revision} and {second_revision}) — conflicting reuse fails \
                 closed"
            ),
            PipelineError::ForeignProvenanceDivergent {
                id,
                first_revision,
                second_revision,
            } => write!(
                f,
                "foreign-confirmed keys span distinct confirming revisions \
                 ({first_revision} and {second_revision}, first at {id}) — one \
                 honest receipt cannot report them"
            ),
            PipelineError::ConfirmedProvenanceMismatch { id } => write!(
                f,
                "the local attempt for consequence identity {id} conflicts with \
                 the canonical entry claiming it — exact provenance is required, \
                 a mismatch fails closed"
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

/// One chain-confirmed key's evidence (the suppression authority):
/// the exact coverage, the producing transaction, the effect
/// commitment binding the confirmed consequence to its actual
/// authoritative effects, and the confirming revision with its epoch
/// — the full provenance the receipt reports and the conflict checks
/// compare.
pub struct ConfirmedEntry {
    pub coverage: Vec<ClassifiedSpan>,
    pub transaction: TransactionId,
    pub effect: Hash,
    pub revision: Hash,
    pub epoch: u64,
}

/// The read-only confirmed-evidence view the actionable-key
/// derivation, the remaining-keys filter, and the key-phase promotion
/// run over. The verifier's truth map implements it (the synchronous
/// facade passes the verifier itself); a future actor composition
/// passes a receipt-fed mirror instead — over-inclusion is always
/// corrected by the verifier's own filter, never trusted from the
/// view.
pub trait ConfirmedView {
    /// The chain-confirmed evidence for one identity, if any.
    fn confirmed(&self, id: &ConsequenceId) -> Option<&ConfirmedEntry>;
}

/// One attempted key's exact-provenance evidence: the occurrence
/// coverage, the producing transaction, and the envelope's effect
/// commitment (identity-level equality is content-blind; the
/// commitment binds the actual authoritative effects — the tuple the
/// foreign-confirmation and confirmed-recovery equivalence compares).
pub type AttemptedKey = (Vec<ClassifiedSpan>, TransactionId, Hash);

/// One immutable commit request: the actionable keys' frozen evidence
/// plus the journal-derived position ticks — everything the
/// submission derives from the journal, so the verifier itself is
/// journal-blind (the authority half remains the journal's sole
/// owner).
#[derive(Clone)]
pub struct CommitRequest {
    /// The actionable, unconfirmed journaled keys: identity →
    /// (coverage, producing transaction, effect commitment).
    pub keys: BTreeMap<ConsequenceId, AttemptedKey>,
    /// The producing transactions' journal position ticks (a
    /// transaction's position is fixed at capture, so the frozen
    /// snapshot equals any later live lookup).
    pub ticks: BTreeMap<TransactionId, u64>,
}

/// A retained canonical attempt — the exact revision, the batch's
/// content identity, and the per-key identities (the batch bytes
/// re-derive deterministically from the keys; never rebuilt from
/// guesswork).
#[derive(Clone)]
struct PendingAttempt {
    record: RevisionRecord,
    batch_hash: Hash,
    /// The attempted per-key evidence: identity → (coverage,
    /// producing transaction, effect commitment).
    keys: BTreeMap<ConsequenceId, AttemptedKey>,
    /// The producing transactions' journal position ticks, frozen at
    /// submission (the retry's re-derivation uses the retained
    /// snapshot — identical to a live lookup by position
    /// immutability).
    ticks: BTreeMap<TransactionId, u64>,
}

/// The authority-owned half of the commit pipeline: the T1 journal,
/// the staged in-memory accrual, and the per-key phase tracking.
/// Journal-only — no medium I/O lives here. This is the half a future
/// verification actor leaves on the authority thread.
pub struct CommitAuthority {
    journal: T1Journal,
    /// The staged in-memory accrual — no I/O touches this path; a
    /// crash loses it honestly (P0-5).
    staged: Vec<ConsequenceEnvelope>,
    keys: BTreeMap<ConsequenceId, KeyStatus>,
}

/// The medium-owned half of the commit pipeline: the canonical
/// medium, the chain-derived confirmed key set (the session's
/// verified view), the trusted cursor, and the retained unresolved
/// canonical attempts. No journal access — everything journal-derived
/// arrives inside the immutable request. This is the half a future
/// verification actor owns on its own thread.
pub struct CommitVerifier<M: CanonicalMedium> {
    medium: M,
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
    /// Completed walks (incremented only when `walk_chain` runs to
    /// its end). The facade promotes tracked keys exactly when this
    /// advances during an operation — today's walk-closing promotion
    /// loop ran precisely per completed walk.
    walk_rounds: u64,
}

/// The synchronous composition — behaviorally identical to the
/// accepted single-struct pipeline. Every published operation
/// delegates to the half that owns the state; the commit-family
/// operations compose: the authority derives the immutable request
/// (journal + the verifier's confirmed truth), the verifier processes
/// it (append → confirm → walk — strict full-object verification
/// throughout), and the authority promotes tracked keys from the
/// verifier's post-operation truth exactly when a walk completed. A
/// future verification actor replaces this composition's middle step
/// with a queue and a receipt; the halves themselves are unchanged.
pub struct CommitPipeline<M: CanonicalMedium> {
    authority: CommitAuthority,
    verifier: CommitVerifier<M>,
}

impl CommitAuthority {
    /// Opens the authority half: the journal's committed transactions
    /// seed the per-key tracking (the T1 state is durable across
    /// sessions).
    pub fn open(journal: T1Journal) -> Self {
        let mut authority = CommitAuthority {
            journal,
            staged: Vec::new(),
            keys: BTreeMap::new(),
        };
        for transaction in authority.journal.transactions() {
            for envelope in &transaction.envelopes {
                authority.keys.entry(envelope.id).or_insert(KeyStatus {
                    phase: KeyPhase::JournaledT1,
                    transaction: Some(transaction.id),
                    revision: None,
                });
            }
        }
        authority
    }

    /// The authority's journal (read-only access — the rotation's
    /// classification and the diagnostics read the durable T1
    /// state; the instance and its sidecar lock stay owned here).
    pub fn journal(&self) -> &T1Journal {
        &self.journal
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

    /// Discards staged (in-memory, uncaptured) envelopes for the given
    /// identities — the failed-boundary path: effects staged but never
    /// captured are in-memory pending only; discarding them never
    /// touches journaled or confirmed keys (P0-5 — in-memory state is
    /// honestly lost). A retried boundary re-stages identical bytes.
    pub fn discard_staged(&mut self, ids: &[ConsequenceId]) {
        self.staged.retain(|envelope| !ids.contains(&envelope.id));
        for id in ids {
            if let Some(status) = self.keys.get(id)
                && status.phase == KeyPhase::InMemory
            {
                self.keys.remove(id);
            }
        }
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
                    // A different consequential payload is committed
                    // under this identity — the attempt is NOT durable
                    // and can never be: fail closed, never retry as
                    // though it were the same attempt.
                    WriteReconciliation::ConflictingPayload { id } => {
                        return Err(PipelineError::Journal(JournalError::ConflictingPayload {
                            id,
                        }));
                    }
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
    /// journaled transaction (the authority proxies the journal —
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

    /// One key's pipeline view.
    pub fn status(&self, id: ConsequenceId) -> Option<KeyStatus> {
        self.keys.get(&id).cloned()
    }

    /// Collects the actionable, unconfirmed journaled keys — the
    /// batch candidates — over a confirmed VIEW (the verifier's truth
    /// through the synchronous facade; a receipt-fed mirror in a
    /// future actor composition — the verifier's own remaining-keys
    /// filter corrects any over-inclusion, never trusting the view).
    /// A key claimed by two journal transactions is a conflicting
    /// reuse and fails closed.
    pub fn collect_actionable(
        &self,
        confirmed: &dyn ConfirmedView,
    ) -> Result<BTreeMap<ConsequenceId, AttemptedKey>, PipelineError> {
        let mut keys: BTreeMap<ConsequenceId, AttemptedKey> = BTreeMap::new();
        for transaction in self.journal.transactions() {
            if !self.journal.is_actionable(transaction.id) {
                continue;
            }
            for envelope in &transaction.envelopes {
                let effect = envelope
                    .effect_commitment()
                    .map_err(PipelineError::Format)?;
                if let Some(entry) = confirmed.confirmed(&envelope.id) {
                    // Exact provenance: the journal envelope must
                    // match the canonical entry — the same producing
                    // transaction, coverage, and EFFECT COMMITMENT
                    // (identity-level equality is content-blind; the
                    // effect commitment binds the actual authoritative
                    // effects); a mismatch is a conflicting claim,
                    // never silently skipped.
                    if entry.transaction == transaction.id
                        && entry.coverage == envelope.sub_spans
                        && entry.effect == effect
                    {
                        continue;
                    }
                    return Err(PipelineError::ConfirmedProvenanceMismatch { id: envelope.id });
                }
                if keys.contains_key(&envelope.id) {
                    return Err(PipelineError::ConflictingReuse { id: envelope.id });
                }
                keys.insert(
                    envelope.id,
                    (envelope.sub_spans.clone(), transaction.id, effect),
                );
            }
        }
        Ok(keys)
    }

    /// The producing transactions' journal position ticks for one
    /// collected key set — everything the submission derives from the
    /// journal, frozen into the immutable request (a transaction's
    /// position is fixed at capture, so this snapshot equals any
    /// later live lookup).
    pub fn transaction_ticks(
        &self,
        keys: &BTreeMap<ConsequenceId, AttemptedKey>,
    ) -> BTreeMap<TransactionId, u64> {
        let mut ticks: BTreeMap<TransactionId, u64> = BTreeMap::new();
        for (_, transaction, _) in keys.values() {
            if let Some(committed) = self
                .journal
                .transactions()
                .iter()
                .find(|committed| committed.id == *transaction)
            {
                ticks.insert(*transaction, committed.position.tick);
            }
        }
        ticks
    }

    /// Promotes every tracked key the view holds confirmed evidence
    /// for — today's walk-closing promotion loop, run by the facade
    /// exactly when a walk completed (idempotent; the entry's
    /// confirming revision is the validated one).
    pub fn promote_from(&mut self, confirmed: &dyn ConfirmedView) {
        for (id, status) in &mut self.keys {
            if let Some(entry) = confirmed.confirmed(id) {
                status.phase = KeyPhase::InConfirmedChain;
                status.revision = Some(entry.revision);
            }
        }
    }
}

impl<M: CanonicalMedium> ConfirmedView for CommitVerifier<M> {
    fn confirmed(&self, id: &ConsequenceId) -> Option<&ConfirmedEntry> {
        self.confirmed.get(id)
    }
}

/// The attempted keys still unconfirmed after a re-filter (the rebase
/// set — identical identities and coverages) over a confirmed VIEW. A
/// foreign confirmation satisfies the local attempt only under EXACT
/// provenance equivalence: the canonical entry must carry the
/// attempt's own producing transaction, coverage, and EFFECT
/// COMMITMENT — a mismatched transaction, coverage, or
/// consequential payload is a conflicting claim, never silently
/// absorbed.
fn remaining_keys(
    attempted: BTreeMap<ConsequenceId, AttemptedKey>,
    confirmed: &dyn ConfirmedView,
) -> Result<BTreeMap<ConsequenceId, AttemptedKey>, PipelineError> {
    let mut remaining = BTreeMap::new();
    for (id, (coverage, transaction, effect)) in attempted {
        if let Some(entry) = confirmed.confirmed(&id) {
            if entry.transaction == transaction
                && entry.coverage == coverage
                && entry.effect == effect
            {
                // Confirmed by another writer's batch carrying
                // exactly this attempt — never re-submitted; the
                // per-key status promotes.
                continue;
            }
            return Err(PipelineError::ConfirmedProvenanceMismatch { id });
        }
        remaining.insert(id, (coverage, transaction, effect));
    }
    Ok(remaining)
}

impl<M: CanonicalMedium> CommitVerifier<M> {
    /// Opens the verifier: the session-open chain walk (O(chain),
    /// documented honestly — never per tick) collects every
    /// chain-confirmed key from the `consequence-batch/1` objects and
    /// establishes the founding authority.
    pub fn open(medium: M) -> Result<Self, PipelineError> {
        let tier = medium.durability_tier();
        let mut verifier = CommitVerifier {
            medium,
            confirmed: BTreeMap::new(),
            verified_head: None,
            verified_epoch: 0,
            founding: None,
            pending: None,
            tier,
            walk_rounds: 0,
        };
        verifier.walk_chain()?;
        Ok(verifier)
    }

    /// The retained unresolved canonical attempt's revision, if any.
    pub fn pending_revision(&self) -> Option<Hash> {
        self.pending.as_ref().map(|attempt| attempt.record.id())
    }

    /// The trusted cursor (the last walk-processed revision — the
    /// append basis; `None` on an unfounded medium).
    pub fn verified_head(&self) -> Option<Hash> {
        self.verified_head
    }

    /// Completed walks — the facade's promotion trigger (today's
    /// walk-closing promotion loop ran precisely per completed walk).
    pub fn completed_walks(&self) -> u64 {
        self.walk_rounds
    }

    /// Processes one immutable commit request to a terminal outcome:
    /// the remaining-keys filter (exact provenance against THIS
    /// truth — a view that over-included is corrected here, never
    /// trusted), then the submit/rebase machinery.
    pub fn submit(&mut self, request: CommitRequest) -> Result<CommitOutcome, PipelineError> {
        if let Some(pending) = &self.pending {
            return Err(PipelineError::AttemptUnresolved {
                revision: pending.record.id(),
            });
        }
        let Some(parent) = self.verified_head else {
            return Err(PipelineError::UnfoundedMedium);
        };
        let attempted: Vec<ConsequenceId> = request.keys.keys().copied().collect();
        let keys = remaining_keys(request.keys, self)?;
        if keys.is_empty() {
            // Every requested key is already chain-confirmed under
            // exact provenance (a lagged view's over-inclusion,
            // corrected here — unreachable through the synchronous
            // facade, whose collection runs over this same truth and
            // reports NothingPending first).
            let receipt = self.foreign_receipt(attempted)?;
            return Ok(CommitOutcome::Confirmed(receipt));
        }
        self.submit_keys(keys, parent, &request.ticks)
    }

    /// Reconciles a retained unknown attempt under the medium's lock:
    /// `Installed` still requires the full read-back walk and the
    /// individual per-key validation before confirmation; otherwise
    /// the attempt is retained (retry-safe or unverifiable — never
    /// falsely confirmed).
    ///
    /// PP-B4 checkpoint 2's exception safety: the verification runs on
    /// a CLONE — the slot keeps the candidate until confirmation
    /// succeeds, so a readback failure (worker death included) retains
    /// it, never a discarded unresolved obligation.
    pub fn reconcile(&mut self) -> Result<ReconcileOutcome, PipelineError> {
        let Some(attempt) = self.pending.clone() else {
            return Ok(ReconcileOutcome::NothingPending);
        };
        let candidate = attempt.record.id();
        match self.medium.reconcile_unknown(&candidate)? {
            Reconciliation::Installed { .. } => {
                let CommitOutcome::Confirmed(receipt) = self.confirm(attempt)? else {
                    unreachable!("confirm returns Confirmed or errors");
                };
                self.pending = None;
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
    ///
    /// PP-B4 checkpoint 2's exception safety: the ORIGINAL candidate
    /// stays in the slot through every pre-submission fallible step
    /// (the walk, the remaining-keys derivation, the foreign receipt —
    /// the slot clears only after that receipt verifies); the
    /// re-submission then owns the transition — a definite pre-commit
    /// failure of candidate B leaves the original A (its
    /// `NotInstalled` disposition stands; a later retry re-derives),
    /// while any possibly-installed B REPLACES A (append_for's
    /// write-at-uncertainty-or-acceptance) and is never overwritten
    /// back.
    pub fn retry(&mut self) -> Result<CommitOutcome, PipelineError> {
        let Some(attempt) = self.pending.clone() else {
            return Ok(CommitOutcome::NothingPending);
        };
        self.walk_chain()?;
        let attempted: Vec<ConsequenceId> = attempt.keys.keys().copied().collect();
        let remaining = remaining_keys(attempt.keys, self)?;
        if remaining.is_empty() {
            // Every attempted key is now chain-confirmed (another
            // writer's batch) — confirmed without our revision. The
            // slot clears ONLY after the receipt's verification.
            let receipt = self.foreign_receipt(attempted)?;
            self.pending = None;
            return Ok(CommitOutcome::Confirmed(receipt));
        }
        let parent = self.verified_head.ok_or(PipelineError::UnfoundedMedium)?;
        self.submit_keys(remaining, parent, &attempt.ticks)
    }

    /// Whether the identity is individually validated inside the
    /// verified canonical chain (the suppression authority).
    pub fn is_confirmed(&self, id: ConsequenceId) -> bool {
        self.confirmed.contains_key(&id)
    }

    /// One confirmed key's exact occurrence coverage (the suppression
    /// authority's span-exact form).
    pub fn confirmed_coverage(&self, id: ConsequenceId) -> Option<&[ClassifiedSpan]> {
        self.confirmed
            .get(&id)
            .map(|entry| entry.coverage.as_slice())
    }

    /// One confirmed key's producing transaction.
    pub fn confirmed_transaction(&self, id: ConsequenceId) -> Option<TransactionId> {
        self.confirmed.get(&id).map(|entry| entry.transaction)
    }

    /// The confirmed evidence export (the recovery composition's
    /// chain-side input).
    pub fn confirmed_evidence(&self) -> BTreeMap<ConsequenceId, ConfirmedEvidence> {
        self.confirmed
            .iter()
            .map(|(id, entry)| {
                (
                    *id,
                    ConfirmedEvidence {
                        coverage: entry.coverage.clone(),
                        transaction: entry.transaction,
                        effect: entry.effect,
                    },
                )
            })
            .collect()
    }

    /// Every chain-confirmed identity.
    pub fn confirmed_keys(&self) -> Vec<ConsequenceId> {
        self.confirmed.keys().copied().collect()
    }

    /// What `Accepted` establishes on this medium.
    pub fn sync_tier(&self) -> SyncTier {
        self.tier
    }

    /// The founding authority mode and campaign id.
    pub fn authority(&self) -> Option<(AuthorityMode, &str)> {
        self.founding
            .as_ref()
            .map(|(mode, campaign)| (*mode, campaign.as_str()))
    }

    // ---------------------------------------------------------- internals

    /// Builds the batch for `keys`, submits it against `parent`, and
    /// handles the stale-rebase once. The idempotency key is the
    /// batch's content hash — the rebase preserves per-key
    /// identities, and the medium's chain-wide key uniqueness makes a
    /// double-install unrepresentable.
    fn submit_keys(
        &mut self,
        keys: BTreeMap<ConsequenceId, AttemptedKey>,
        parent: Hash,
        ticks: &BTreeMap<TransactionId, u64>,
    ) -> Result<CommitOutcome, PipelineError> {
        let (attempt, outcome) = self.append_for(keys.clone(), parent, ticks)?;
        match outcome {
            AppendOutcome::Accepted { id, tier: _ } => {
                if id != attempt.record.id() {
                    return Err(PipelineError::ConfirmationInvalid {
                        detail: "accepted-revision-identity-mismatch",
                    });
                }
                self.confirm_retained(attempt)
            }
            AppendOutcome::RejectedStaleParent { observed } => {
                // Revalidate: the observed chain was validated by the
                // medium before the rejection; the suffix walk
                // determines the now-confirmed keys.
                self.walk_chain()?;
                let attempted: Vec<ConsequenceId> = keys.keys().copied().collect();
                let remaining = remaining_keys(keys, self)?;
                if remaining.is_empty() {
                    // Every attempted key is now chain-confirmed (another
                    // writer's batch) — confirmed without our revision. The
                    // exact foreign provenance verifies FIRST; the pending
                    // slot clears only after the receipt succeeds (C2-F1: a
                    // verified foreign confirmation must clear the retained
                    // candidate — never leave the caller's next
                    // commit_canonical refusing AttemptUnresolved; a
                    // fallible verification failure preserves it).
                    let receipt = self.foreign_receipt(attempted)?;
                    self.pending = None;
                    return Ok(CommitOutcome::Confirmed(receipt));
                }
                let observed_parent = observed
                    .or(self.verified_head)
                    .ok_or(PipelineError::UnfoundedMedium)?;
                let (attempt, outcome) = self.append_for(remaining, observed_parent, ticks)?;
                match outcome {
                    AppendOutcome::Accepted { id, tier: _ } => {
                        if id != attempt.record.id() {
                            return Err(PipelineError::ConfirmationInvalid {
                                detail: "accepted-revision-identity-mismatch",
                            });
                        }
                        self.confirm_retained(attempt)
                    }
                    // The rebase was rejected again — append_for already
                    // retains the last built attempt for retry_pending.
                    AppendOutcome::RejectedStaleParent { observed } => {
                        Ok(CommitOutcome::StaleParent { observed })
                    }
                    AppendOutcome::Unknown { reason } => Ok(CommitOutcome::Unknown {
                        revision: attempt.record.id(),
                        reason,
                    }),
                }
            }
            AppendOutcome::Unknown { reason } => Ok(CommitOutcome::Unknown {
                revision: attempt.record.id(),
                reason,
            }),
        }
    }

    /// The confirmation procedure's exception-safe entry (PP-B4
    /// checkpoint 2): `confirm` runs on the retained clone — the
    /// pending slot already holds the candidate (append_for's
    /// write-at-uncertainty-or-acceptance) and is cleared ONLY on
    /// verified confirmation. A worker-transport failure during the
    /// readback returns `CommitOutcome::Unknown` with
    /// `WorkerReplyLost` — the append was accepted (its disposition
    /// stands) while the verification could not complete: never a
    /// definite failure, never a false confirmation. Any other
    /// readback error propagates with the candidate retained for
    /// reconciliation.
    fn confirm_retained(
        &mut self,
        attempt: PendingAttempt,
    ) -> Result<CommitOutcome, PipelineError> {
        let revision = attempt.record.id();
        match self.confirm(attempt) {
            Ok(outcome) => {
                self.pending = None;
                Ok(outcome)
            }
            Err(PipelineError::Medium(MediumError::WorkerUnavailable { .. })) => {
                Ok(CommitOutcome::Unknown {
                    revision,
                    reason: UnknownReason::WorkerReplyLost,
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Builds the batch content object and the revision record for
    /// `keys` on `parent`, and appends. The tick derives
    /// deterministically from the batched transactions' journaled
    /// positions (retry stability — carried frozen in the request and
    /// the retained attempt).
    ///
    /// THE CANDIDATE-STATE RULE (PP-B4 checkpoint 2, the preflight's
    /// four-state lifecycle): a DEFINITE pre-commit failure (the
    /// medium's own contract — and the worker proxy's discipline:
    /// uncertain dispositions arrive as `Unknown` outcomes, never
    /// errors) returns WITHOUT writing the pending slot, preserving
    /// the accepted direct-retry behavior; every outcome that leaves
    /// the candidate possibly installed (`Accepted`, `Unknown`, or a
    /// stale-parent rebase target) writes the slot BEFORE the caller
    /// takes any further fallible step, so the reconciliation
    /// identity can never be lost to a readback failure. The slot is
    /// cleared only by verified confirmation.
    fn append_for(
        &mut self,
        keys: BTreeMap<ConsequenceId, AttemptedKey>,
        parent: Hash,
        ticks: &BTreeMap<TransactionId, u64>,
    ) -> Result<(PendingAttempt, AppendOutcome), PipelineError> {
        let mut entries: Vec<BatchEntry> = Vec::with_capacity(keys.len());
        let mut tick: u64 = 0;
        for (id, (coverage, transaction, effect)) in &keys {
            entries.push(BatchEntry {
                id: *id,
                transaction: *transaction,
                coverage: coverage.clone(),
                effect: *effect,
            });
            if let Some(position) = ticks.get(transaction) {
                tick = tick.max(*position);
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
            ticks: ticks.clone(),
        };
        // Write-at-uncertainty-or-acceptance: the retained candidate
        // precedes every further fallible step (the readback included);
        // cleared only by verified confirmation.
        self.pending = Some(attempt.clone());
        Ok((attempt, outcome))
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
        for (id, (coverage, transaction, effect)) in &attempt.keys {
            let Some(entry) = batch.entries.iter().find(|entry| entry.id == *id) else {
                return Err(PipelineError::ConfirmationInvalid {
                    detail: "key-missing-from-confirmed-batch",
                });
            };
            if entry.coverage != *coverage
                || entry.transaction != *transaction
                || entry.effect != *effect
            {
                return Err(PipelineError::ConfirmationInvalid {
                    detail: "payload-mismatch-in-confirmed-batch",
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
        // The attempted keys' phase promotion (today's in-confirm
        // loop) is the facade's post-operation promote_from — the
        // walk completed, so it runs; the entry's confirming revision
        // is the validated one.
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
    /// confirmed. Per-key exact provenance: the receipt reports the
    /// revision that ACTUALLY confirmed the keys — never the current
    /// (possibly unrelated, later) HEAD. Keys confirmed by distinct
    /// revisions cannot collapse into one honest receipt: fail
    /// closed rather than misreport any key's provenance.
    fn foreign_receipt(
        &self,
        keys: Vec<ConsequenceId>,
    ) -> Result<ConfirmationReceipt, PipelineError> {
        let (authority, _) = self
            .founding
            .as_ref()
            .ok_or(PipelineError::UnfoundedMedium)?;
        let mut confirming: Option<(Hash, u64)> = None;
        for key in &keys {
            let entry = self
                .confirmed
                .get(key)
                .ok_or(PipelineError::UnknownKey { id: *key })?;
            match confirming {
                None => confirming = Some((entry.revision, entry.epoch)),
                Some((revision, _)) if revision == entry.revision => {}
                Some((revision, _)) => {
                    return Err(PipelineError::ForeignProvenanceDivergent {
                        id: *key,
                        first_revision: revision,
                        second_revision: entry.revision,
                    });
                }
            }
        }
        let (revision, epoch) = confirming.ok_or(PipelineError::UnfoundedMedium)?;
        Ok(ConfirmationReceipt {
            revision,
            epoch,
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
        // THE COMPACTED MODE (PP-B3): an absent parent is a gap —
        // legitimate only under the most recent on-chain MK's skip
        // table (recognized by the record's first content object
        // carrying the checkpoint-metadata domain); anything else is
        // hard corruption, fail closed. The gap-aware descent lets
        // the confirmed-evidence collection reach the batches BELOW
        // reclaimed ranges through the permanent skips.
        let mut skip_table: Option<(Hash, CheckpointMetadata)> = None;
        // THE S6-C4 READ-PATH CORRECTIVE (the within-walk verified
        // classification memo): each referenced object is examined —
        // and its FULL content hash verified through the medium's
        // verified-classification operation — AT MOST ONCE per walk;
        // the same walk's later examination of the same object reuses
        // the classification. The memo is walk-local: EVERY walk
        // re-verifies every object it examines (a corruption
        // introduced between two walks is detected by the second
        // walk — the accepted timing guarantee preserved); objects
        // whose content the walk CONSUMES (consequence batches,
        // checkpoint metadata) always receive the full verified read.
        let mut classified: HashMap<Hash, ObjectClass> = HashMap::new();
        loop {
            if self.verified_head == Some(cursor) {
                break;
            }
            let record = self.medium.read_record(&cursor)?;
            let parent = record.parent;
            let current = cursor;
            if let Some(object) = record.content_refs.first() {
                let class = self.classify_once(object, &mut classified)?;
                if class.domain.as_deref() == Some(CHECKPOINT_METADATA_DOMAIN) {
                    let bytes = self.read_verified(object)?;
                    let metadata =
                        CheckpointMetadata::decode(&bytes).map_err(PipelineError::Format)?;
                    skip_table = Some((current, metadata));
                }
            }
            chain.push((cursor, record));
            match parent {
                Some(parent) => match self.medium.read_record(&parent) {
                    Ok(_) => cursor = parent,
                    Err(MediumError::MissingObject { .. }) => {
                        let Some((owner, table)) = skip_table.as_ref() else {
                            return Err(PipelineError::Medium(MediumError::MissingObject {
                                hash: parent,
                            }));
                        };
                        let target = if current == *owner {
                            table
                                .self_skip()
                                .map(|entry| entry.nearest_retained_ancestor)
                        } else {
                            table
                                .skip_for(&current)
                                .map(|entry| entry.nearest_retained_ancestor)
                        };
                        let Some(target) = target else {
                            return Err(PipelineError::Medium(MediumError::MissingObject {
                                hash: parent,
                            }));
                        };
                        cursor = target;
                    }
                    Err(other) => return Err(other.into()),
                },
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
                let class = self.classify_once(content, &mut classified)?;
                if class.domain.as_deref() == Some(CONSEQUENCE_BATCH_DOMAIN) {
                    let bytes = self.read_verified(content)?;
                    let batch = ConsequenceBatch::decode(&bytes).map_err(PipelineError::Format)?;
                    for entry in batch.entries {
                        if let Some(existing) = self.confirmed.get(&entry.id) {
                            // Conflicting reuse of a ConsequenceId
                            // across valid canonical batches: two
                            // chain revisions claim the same identity.
                            // The exact-provenance standard rejects
                            // the second — never overwriting the first
                            // revision's evidence, never guessing
                            // which is authoritative.
                            return Err(PipelineError::CanonicalIdentityConflict {
                                id: entry.id,
                                first_revision: existing.revision,
                                second_revision: id,
                            });
                        }
                        self.confirmed.insert(
                            entry.id,
                            ConfirmedEntry {
                                coverage: entry.coverage,
                                transaction: entry.transaction,
                                effect: entry.effect,
                                revision: id,
                                epoch: record.epoch,
                            },
                        );
                    }
                }
            }
            self.verified_head = Some(id);
            self.verified_epoch = record.epoch;
        }
        // A completed walk (today's walk-closing promotion trigger —
        // the facade promotes tracked keys exactly when this
        // advances).
        self.walk_rounds += 1;
        Ok(())
    }

    /// One walk's verified classification of a referenced object,
    /// memoized within that walk (the S6-C4 read-path corrective):
    /// the FIRST examination performs the medium's verified
    /// classification — the full object bytes hash-checked against
    /// the identity, exactly as `read_object` does — and later
    /// examinations of the same object within the SAME walk reuse
    /// the result. The memo is walk-local by construction: the next
    /// walk starts empty, so every object it examines is re-verified
    /// (the corruption-between-walks timing guarantee preserved).
    fn classify_once(
        &mut self,
        hash: &Hash,
        memo: &mut HashMap<Hash, ObjectClass>,
    ) -> Result<ObjectClass, PipelineError> {
        if let Some(class) = memo.get(hash) {
            return Ok(class.clone());
        }
        let class = self.medium.classify_object(hash)?;
        memo.insert(*hash, class.clone());
        Ok(class)
    }

    /// The full verified read for an object whose content this walk
    /// CONSUMES (a consequence batch or checkpoint metadata) — never
    /// memoized, never bypassed: the complete bytes are read back and
    /// hash-verified, and the existing semantic validation runs over
    /// them exactly as before.
    fn read_verified(&mut self, hash: &Hash) -> Result<Vec<u8>, PipelineError> {
        Ok(self.medium.read_object(hash)?)
    }
}

impl<M: CanonicalMedium> CommitPipeline<M> {
    /// Opens the pipeline: the journal's committed transactions seed
    /// the per-key tracking (the T1 state is durable across sessions),
    /// then the session-open chain walk (O(chain), documented honestly
    /// — never per tick) collects every chain-confirmed key from the
    /// `consequence-batch/1` objects, promotes them, and establishes
    /// the founding authority.
    pub fn open(medium: M, journal: T1Journal) -> Result<Self, PipelineError> {
        let mut authority = CommitAuthority::open(journal);
        let verifier = CommitVerifier::open(medium)?;
        authority.promote_from(&verifier);
        Ok(CommitPipeline {
            authority,
            verifier,
        })
    }

    /// The pipeline's journal (read-only access — the rotation's
    /// classification and the diagnostics read the durable T1
    /// state; the instance and its sidecar lock stay owned here).
    pub fn journal(&self) -> &T1Journal {
        self.authority.journal()
    }

    /// Stages one consequential envelope — the in-memory accrual
    /// (never guaranteed across a crash). A tracked identity staged
    /// again with different content is a conflicting reuse and fails
    /// closed; a byte-identical restage is a no-op.
    pub fn stage(&mut self, envelope: ConsequenceEnvelope) -> Result<(), PipelineError> {
        self.authority.stage(envelope)
    }

    /// Discards staged (in-memory, uncaptured) envelopes for the given
    /// identities — the failed-boundary path: effects staged but never
    /// captured are in-memory pending only; discarding them never
    /// touches journaled or confirmed keys (P0-5 — in-memory state is
    /// honestly lost). A retried boundary re-stages identical bytes.
    pub fn discard_staged(&mut self, ids: &[ConsequenceId]) {
        self.authority.discard_staged(ids)
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
        self.authority
            .capture(position, classification, dependencies, frontier, plan)
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
        self.authority.cancel_transaction(target, reason, detail)
    }

    /// The application reports effects applied to the live
    /// authoritative state (the pending-vs-confirmed distinction —
    /// never conflated with durability). A key must be journaled
    /// first (durable-before-discard).
    pub fn mark_applied(&mut self, keys: &[ConsequenceId]) -> Result<(), PipelineError> {
        self.authority.mark_applied(keys)
    }

    /// Prepares the canonical batch from every actionable,
    /// unconfirmed journaled key and submits it against the verified
    /// head. Canceled transactions are excluded (superseded — never
    /// canonically committed); chain-confirmed keys are never
    /// re-batched.
    pub fn commit_canonical(&mut self) -> Result<CommitOutcome, PipelineError> {
        // Today's walk-closing promotion: the authority promotes
        // tracked keys exactly when a walk completed during the
        // operation (the truth only advances inside completed walks,
        // so an unchanged count means no promotion is due — today's
        // error paths before any walk promoted nothing).
        let before = self.verifier.completed_walks();
        let result = self.commit_canonical_inner();
        if self.verifier.completed_walks() != before {
            self.authority.promote_from(&self.verifier);
        }
        result
    }

    fn commit_canonical_inner(&mut self) -> Result<CommitOutcome, PipelineError> {
        if let Some(revision) = self.verifier.pending_revision() {
            return Err(PipelineError::AttemptUnresolved { revision });
        }
        if self.verifier.verified_head().is_none() {
            return Err(PipelineError::UnfoundedMedium);
        }
        let keys = self.authority.collect_actionable(&self.verifier)?;
        if keys.is_empty() {
            return Ok(CommitOutcome::NothingPending);
        }
        let ticks = self.authority.transaction_ticks(&keys);
        self.verifier.submit(CommitRequest { keys, ticks })
    }

    /// Reconciles a retained unknown attempt under the medium's lock:
    /// `Installed` still requires the full read-back walk and the
    /// individual per-key validation before confirmation; otherwise
    /// the attempt is retained (retry-safe or unverifiable — never
    /// falsely confirmed).
    ///
    /// PP-B4 checkpoint 2's exception safety: the verification runs on
    /// a CLONE — the slot keeps the candidate until confirmation
    /// succeeds, so a readback failure (worker death included) retains
    /// it, never a discarded unresolved obligation.
    pub fn reconcile_pending(&mut self) -> Result<ReconcileOutcome, PipelineError> {
        let before = self.verifier.completed_walks();
        let result = self.verifier.reconcile();
        if self.verifier.completed_walks() != before {
            self.authority.promote_from(&self.verifier);
        }
        result
    }

    /// Retries a retained attempt (after `NotInstalled`, or a stale
    /// parent): re-walks the chain first (determining keys another
    /// writer may have confirmed — never re-submitted), then
    /// re-submits the remaining per-key identities — the same record
    /// bytes when the parent still matches, the rebase (identical
    /// identities and coverages, new parent) when it does not.
    ///
    /// PP-B4 checkpoint 2's exception safety: the ORIGINAL candidate
    /// stays in the slot through every pre-submission fallible step
    /// (the walk, the remaining-keys derivation, the foreign receipt —
    /// the slot clears only after that receipt verifies); the
    /// re-submission then owns the transition — a definite pre-commit
    /// failure of candidate B leaves the original A (its
    /// `NotInstalled` disposition stands; a later retry re-derives),
    /// while any possibly-installed B REPLACES A (append_for's
    /// write-at-uncertainty-or-acceptance) and is never overwritten
    /// back.
    pub fn retry_pending(&mut self) -> Result<CommitOutcome, PipelineError> {
        let before = self.verifier.completed_walks();
        let result = self.verifier.retry();
        if self.verifier.completed_walks() != before {
            self.authority.promote_from(&self.verifier);
        }
        result
    }

    /// One key's pipeline view.
    pub fn status(&self, id: ConsequenceId) -> Option<KeyStatus> {
        self.authority.status(id)
    }

    /// Whether the identity is individually validated inside the
    /// verified canonical chain (the suppression authority).
    pub fn is_confirmed(&self, id: ConsequenceId) -> bool {
        self.verifier.is_confirmed(id)
    }

    /// The chain-confirmed occurrence coverage for one identity (the
    /// per-key evidence — never a batch aggregate alone).
    pub fn confirmed_coverage(&self, id: ConsequenceId) -> Option<&[ClassifiedSpan]> {
        self.verifier.confirmed_coverage(id)
    }

    /// The producing transaction for one chain-confirmed identity
    /// (the per-key provenance — the transaction whose captured
    /// effects the confirming batch installed).
    pub fn confirmed_transaction(&self, id: ConsequenceId) -> Option<TransactionId> {
        self.verifier.confirmed_transaction(id)
    }

    /// The chain-confirmed canonical evidence per identity — the
    /// recovery interface's provenance input (the coverage, producing
    /// transaction, and effect commitment; a bare identity set cannot
    /// validate journal envelopes against the canonical entries).
    pub fn confirmed_evidence(&self) -> BTreeMap<ConsequenceId, ConfirmedEvidence> {
        self.verifier.confirmed_evidence()
    }

    /// The confirmed key set (the pipeline's session view).
    pub fn confirmed_keys(&self) -> Vec<ConsequenceId> {
        self.verifier.confirmed_keys()
    }

    /// The actual durability tier of the underlying medium (the
    /// honest platform boundary — Windows carries the process-crash
    /// tier, never presented as power-loss durability).
    pub fn sync_tier(&self) -> SyncTier {
        self.verifier.sync_tier()
    }

    /// The authority mode from the founding record (never assumed).
    pub fn authority(&self) -> Option<(AuthorityMode, &str)> {
        self.verifier.authority()
    }
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
                effect: staged_envelope(1).effect_commitment().unwrap(),
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
                staged_envelope(1).effect_commitment().unwrap(),
            ),
        );
        let attempt = PendingAttempt {
            record: RevisionRecord::regular(confirmation.revision, confirmation.epoch, 101),
            batch_hash: object.hash(),
            keys,
            ticks: BTreeMap::new(),
        };
        assert!(matches!(
            pipeline.verifier.confirm(attempt),
            Err(PipelineError::ConfirmationInvalid {
                detail: "payload-mismatch-in-confirmed-batch"
            })
        ));
        // (b) A key the confirmed batch never carried.
        let mut keys = BTreeMap::new();
        keys.insert(
            ConsequenceId::new(1, 1, 1),
            (
                staged_envelope(1).sub_spans.clone(),
                transaction,
                staged_envelope(1).effect_commitment().unwrap(),
            ),
        );
        keys.insert(
            ConsequenceId::new(1, 5, 1),
            (
                staged_envelope(5).sub_spans.clone(),
                transaction,
                staged_envelope(5).effect_commitment().unwrap(),
            ),
        );
        let batch_hash = object.hash();
        let attempt = PendingAttempt {
            record: RevisionRecord::regular(confirmation.revision, confirmation.epoch, 101),
            batch_hash,
            keys,
            ticks: BTreeMap::new(),
        };
        assert!(matches!(
            pipeline.verifier.confirm(attempt),
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
        let staged = pipeline.authority.staged.len();
        pipeline.stage(staged_envelope(2)).unwrap();
        assert_eq!(pipeline.authority.staged.len(), staged + 1);
        let before = pipeline.authority.staged.len();
        pipeline.stage(staged_envelope(2)).unwrap();
        assert_eq!(
            pipeline.authority.staged.len(),
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
    fn conflicting_canonical_identities_across_batches_rejected() {
        // The review's finding: two valid canonical batches claiming
        // the SAME ConsequenceId — the exact-provenance standard
        // rejects the second, never overwriting the first revision's
        // evidence. The normal pipeline flow cannot produce the
        // conflict (the stale-path re-walk filters confirmed keys
        // before any re-submission), so the second batch is a
        // hand-crafted valid revision — exactly the defensive scope
        // of the walk's contract: ANY valid chain.
        let dir = TestDir::new("canonical-conflict");
        ensure_founded(&dir);
        let identity = ConsequenceId::new(1, 1, 1);
        // The first canonical batch, through the pipeline.
        let journal = T1Journal::open(dir.path().join("journal-a.t1")).unwrap();
        let mut writer =
            CommitPipeline::open(LocalFileMedium::open(dir.store()).unwrap(), journal).unwrap();
        let transaction = capture_index(&mut writer, 1);
        let CommitOutcome::Confirmed(first) = writer.commit_canonical().unwrap() else {
            panic!("expected the first confirmation");
        };
        assert_eq!(first.keys, vec![identity]);
        let confirming = first.revision;
        let head_epoch = first.epoch;
        drop(writer);
        // The conflicting second batch: a valid revision on top of
        // the confirming one, carrying the same identity under a
        // different coverage (a different content hash — the medium
        // accepts it; only the pipeline's provenance standard
        // rejects the claim).
        let batch = ConsequenceBatch {
            entries: vec![BatchEntry {
                id: identity,
                transaction,
                coverage: vec![ClassifiedSpan {
                    span: OccurrenceSpan::new(2, 4).unwrap(),
                    class: DurabilityClass::DurableEligible,
                }],
                effect: staged_envelope(1).effect_commitment().unwrap(),
            }],
        };
        let batch_bytes = batch.canonical_bytes().unwrap();
        let batch_object = StoredObject::new(batch_bytes).unwrap();
        let batch_hash = batch_object.hash();
        let record = RevisionRecord::regular(confirming, head_epoch, 0)
            .with_idempotency_key(batch_hash.as_bytes().to_vec())
            .with_content_refs(vec![batch_hash]);
        let mut medium = LocalFileMedium::open(dir.store()).unwrap();
        match medium
            .append(
                &record,
                std::slice::from_ref(&batch_object),
                Some(confirming),
            )
            .unwrap()
        {
            AppendOutcome::Accepted { .. } => {}
            other => panic!("the conflicting revision must append: {other:?}"),
        }
        drop(medium);
        // A fresh pipeline over the conflicted chain fails closed at
        // its session-open walk — the second batch's claim on the
        // identity is rejected, the first's evidence preserved.
        let journal = T1Journal::open(dir.path().join("journal-b.t1")).unwrap();
        let error = match CommitPipeline::open(LocalFileMedium::open(dir.store()).unwrap(), journal)
        {
            Ok(_) => panic!("the conflicted chain must fail the open"),
            Err(error) => error,
        };
        assert!(
            matches!(
                &error,
                PipelineError::CanonicalIdentityConflict {
                    id,
                    first_revision,
                    second_revision: _,
                } if *id == identity && *first_revision == confirming
            ),
            "the conflicting canonical identity fails closed: {error}"
        );
    }

    #[test]
    fn foreign_receipt_reports_the_confirming_revision_not_the_head() {
        // The review's finding: the foreign receipt must report each
        // key's ACTUAL confirming revision and epoch — never the
        // current (unrelated, later) HEAD. Writer B confirms the
        // identity at revision R1; an unrelated revision R2 lands on
        // top; writer A's stale-path re-walk then returns the foreign
        // confirmation — which must name R1, not R2.
        let dir = TestDir::new("foreign-receipt");
        ensure_founded(&dir);
        let identity = ConsequenceId::new(1, 1, 1);
        // Writer A journals the identity and does NOT commit.
        let journal_a = T1Journal::open(dir.path().join("journal-a.t1")).unwrap();
        let mut writer_a =
            CommitPipeline::open(LocalFileMedium::open(dir.store()).unwrap(), journal_a).unwrap();
        capture_index(&mut writer_a, 1);
        // Writer B (a separate journal) confirms the identity at R1.
        let journal_b = T1Journal::open(dir.path().join("journal-b.t1")).unwrap();
        let mut writer_b =
            CommitPipeline::open(LocalFileMedium::open(dir.store()).unwrap(), journal_b).unwrap();
        capture_index(&mut writer_b, 1);
        let CommitOutcome::Confirmed(foreign) = writer_b.commit_canonical().unwrap() else {
            panic!("expected writer B's confirmation");
        };
        assert_eq!(foreign.keys, vec![identity]);
        let confirming = foreign.revision;
        let confirming_epoch = foreign.epoch;
        // An unrelated revision lands on top of the confirming one.
        capture_index(&mut writer_b, 2);
        let CommitOutcome::Confirmed(unrelated) = writer_b.commit_canonical().unwrap() else {
            panic!("expected the unrelated confirmation");
        };
        assert_ne!(unrelated.revision, confirming);
        drop(writer_b);
        // Writer A's commit: the parent is stale (the head moved past
        // the confirming revision); the suffix re-walk finds the
        // identity confirmed by R1 — the foreign receipt names R1
        // and its epoch, never the unrelated HEAD.
        let CommitOutcome::Confirmed(receipt) = writer_a.commit_canonical().unwrap() else {
            panic!("expected the foreign confirmation");
        };
        assert_eq!(receipt.keys, vec![identity]);
        assert_eq!(receipt.revision, confirming);
        assert_eq!(receipt.epoch, confirming_epoch);
        assert_ne!(receipt.revision, unrelated.revision);
    }

    #[test]
    fn foreign_confirmation_with_conflicting_coverage_fails_closed() {
        // The re-review's finding: remaining_keys previously skipped
        // an attempted identity merely because it existed in the
        // confirmed map. Competing writers claiming the same identity
        // under DIFFERENT coverage: the foreign confirmation does not
        // satisfy the local attempt — exact-provenance equivalence
        // fails closed, never silently absorbed.
        let dir = TestDir::new("foreign-coverage-conflict");
        ensure_founded(&dir);
        let identity = ConsequenceId::new(1, 1, 1);
        // Writer A journals the identity with coverage (0, 2].
        let journal_a = T1Journal::open(dir.path().join("journal-a.t1")).unwrap();
        let mut writer_a =
            CommitPipeline::open(LocalFileMedium::open(dir.store()).unwrap(), journal_a).unwrap();
        writer_a.stage(staged_envelope(1)).unwrap();
        let boundary_a = boundary(1);
        writer_a
            .capture(
                boundary_a.position,
                boundary_a.classification,
                boundary_a.dependencies,
                boundary_a.frontier,
                boundary_a.plan,
            )
            .unwrap();
        // Writer B (a separate journal) claims the same identity under
        // DIFFERENT coverage (2, 4] and confirms it.
        let journal_b = T1Journal::open(dir.path().join("journal-b.t1")).unwrap();
        let mut writer_b =
            CommitPipeline::open(LocalFileMedium::open(dir.store()).unwrap(), journal_b).unwrap();
        writer_b
            .stage(
                ConsequenceEnvelope::capture(
                    identity,
                    vec![ClassifiedSpan {
                        span: OccurrenceSpan::new(2, 4).unwrap(),
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
                        tick: 110,
                    },
                )
                .unwrap(),
            )
            .unwrap();
        let boundary_b = boundary(1);
        writer_b
            .capture(
                boundary_b.position,
                boundary_b.classification,
                boundary_b.dependencies,
                boundary_b.frontier,
                boundary_b.plan,
            )
            .unwrap();
        let CommitOutcome::Confirmed(_) = writer_b.commit_canonical().unwrap() else {
            panic!("expected writer B's confirmation");
        };
        drop(writer_b);
        // Writer A's commit: the stale-path re-walk finds the identity
        // confirmed under a DIFFERENT coverage — the local attempt
        // conflicts with the canonical entry: fail closed.
        let error = writer_a.commit_canonical().unwrap_err();
        assert!(
            matches!(
                &error,
                PipelineError::ConfirmedProvenanceMismatch { id } if *id == identity
            ),
            "the conflicting coverage fails closed: {error}"
        );
    }

    #[test]
    fn foreign_confirmation_with_conflicting_transaction_fails_closed() {
        // The same identity and coverage, but a DIFFERENT producing
        // transaction (a different plan): the canonical entry is not
        // the local attempt — exact provenance fails closed, never
        // silently absorbed.
        let dir = TestDir::new("foreign-transaction-conflict");
        ensure_founded(&dir);
        let identity = ConsequenceId::new(1, 1, 1);
        // Writer A journals the identity under plan-1.
        let journal_a = T1Journal::open(dir.path().join("journal-a.t1")).unwrap();
        let mut writer_a =
            CommitPipeline::open(LocalFileMedium::open(dir.store()).unwrap(), journal_a).unwrap();
        writer_a.stage(staged_envelope(1)).unwrap();
        let boundary_a = boundary(1);
        writer_a
            .capture(
                boundary_a.position,
                boundary_a.classification,
                boundary_a.dependencies,
                boundary_a.frontier,
                boundary_a.plan,
            )
            .unwrap();
        // Writer B claims the same identity + the same coverage under
        // a different plan (a different producing transaction) and
        // confirms it.
        let journal_b = T1Journal::open(dir.path().join("journal-b.t1")).unwrap();
        let mut writer_b =
            CommitPipeline::open(LocalFileMedium::open(dir.store()).unwrap(), journal_b).unwrap();
        writer_b.stage(staged_envelope(1)).unwrap();
        let boundary_b = boundary(2);
        writer_b
            .capture(
                boundary_b.position,
                boundary_b.classification,
                boundary_b.dependencies,
                boundary_b.frontier,
                boundary_b.plan,
            )
            .unwrap();
        let CommitOutcome::Confirmed(_) = writer_b.commit_canonical().unwrap() else {
            panic!("expected writer B's confirmation");
        };
        drop(writer_b);
        // Writer A's commit: the producing transaction differs — the
        // local attempt conflicts with the canonical entry: fail
        // closed.
        let error = writer_a.commit_canonical().unwrap_err();
        assert!(
            matches!(
                &error,
                PipelineError::ConfirmedProvenanceMismatch { id } if *id == identity
            ),
            "the conflicting transaction fails closed: {error}"
        );
    }

    #[test]
    fn foreign_confirmation_with_matching_ids_but_conflicting_effects_fails_closed() {
        // The third re-review's finding: `TransactionId` + occurrence
        // coverage is insufficient to prove effect equivalence — the
        // same identity, span, classification, and plan (the same
        // content-blind TransactionId inputs) admits two valid
        // uniform lanes (+2 pending vs +2 captured). A foreign
        // confirmation carrying the same transaction and coverage
        // but a DIFFERENT consequential effect does not satisfy the
        // local attempt: exact effect provenance fails closed.
        let dir = TestDir::new("foreign-effect-conflict");
        ensure_founded(&dir);
        let identity = ConsequenceId::new(1, 1, 1);
        // Writer A journals the identity with the pending lane —
        // byte-identical to staged_envelope(1) but for the lane.
        let journal_a = T1Journal::open(dir.path().join("journal-a.t1")).unwrap();
        let mut writer_a =
            CommitPipeline::open(LocalFileMedium::open(dir.store()).unwrap(), journal_a).unwrap();
        writer_a
            .stage(
                ConsequenceEnvelope::capture(
                    identity,
                    vec![ClassifiedSpan {
                        span: OccurrenceSpan::new(0, 2).unwrap(),
                        class: DurabilityClass::DurableEligible,
                    }],
                    SignalLane::Pending,
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
                        tick: 101,
                    },
                )
                .unwrap(),
            )
            .unwrap();
        let boundary_a = boundary(1);
        writer_a
            .capture(
                boundary_a.position,
                boundary_a.classification,
                boundary_a.dependencies,
                boundary_a.frontier,
                boundary_a.plan,
            )
            .unwrap();
        // Writer B claims the same identity, span, class, and plan —
        // the same TransactionId and coverage — under the captured
        // lane, and confirms it.
        let journal_b = T1Journal::open(dir.path().join("journal-b.t1")).unwrap();
        let mut writer_b =
            CommitPipeline::open(LocalFileMedium::open(dir.store()).unwrap(), journal_b).unwrap();
        writer_b.stage(staged_envelope(1)).unwrap();
        let boundary_b = boundary(1);
        writer_b
            .capture(
                boundary_b.position,
                boundary_b.classification,
                boundary_b.dependencies,
                boundary_b.frontier,
                boundary_b.plan,
            )
            .unwrap();
        let CommitOutcome::Confirmed(_) = writer_b.commit_canonical().unwrap() else {
            panic!("expected writer B's confirmation");
        };
        drop(writer_b);
        // Writer A's commit: the stale-path re-walk finds the
        // identity confirmed under the same transaction and coverage
        // but a DIFFERENT effect commitment — the local attempt
        // conflicts with the canonical entry: fail closed.
        let error = writer_a.commit_canonical().unwrap_err();
        assert!(
            matches!(
                &error,
                PipelineError::ConfirmedProvenanceMismatch { id } if *id == identity
            ),
            "the conflicting effect fails closed: {error}"
        );
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
                    effect: Hash::from_bytes(&[11u8; 32]).unwrap(),
                },
                BatchEntry {
                    id: ConsequenceId::new(1, 2, 4),
                    transaction: TransactionId(Hash::from_bytes(&[9u8; 32]).unwrap()),
                    coverage: Vec::new(),
                    effect: Hash::from_bytes(&[13u8; 32]).unwrap(),
                },
            ],
        };
        let bytes = batch.canonical_bytes().unwrap();
        assert_eq!(ConsequenceBatch::decode(&bytes).unwrap(), batch);
        // The class-homogeneous partition boundary: cross-class
        // adjacent sub-spans are legitimate (a mixed aggregate — the
        // encounter envelope's shape); same-class adjacent spans
        // would have composed.
        let partitioned = ConsequenceBatch {
            entries: vec![BatchEntry {
                id: ConsequenceId::new(1, 2, 3),
                transaction: TransactionId(Hash::from_bytes(&[7u8; 32]).unwrap()),
                coverage: vec![
                    ClassifiedSpan {
                        span: OccurrenceSpan::new(0, 2).unwrap(),
                        class: DurabilityClass::DurableEligible,
                    },
                    ClassifiedSpan {
                        span: OccurrenceSpan::new(2, 4).unwrap(),
                        class: DurabilityClass::EncounterProvisional,
                    },
                ],
                effect: Hash::from_bytes(&[21u8; 32]).unwrap(),
            }],
        };
        let bytes = partitioned.canonical_bytes().unwrap();
        assert_eq!(ConsequenceBatch::decode(&bytes).unwrap(), partitioned);
        let mut composed = partitioned;
        composed.entries[0].coverage[1].class = DurabilityClass::DurableEligible;
        assert!(composed.canonical_bytes().is_err());
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

    // ---------------------------------------- the S6-C4 read-path corrective

    /// A counting medium wrapper (the S6-C4 test seam): every
    /// `read_object` and `classify_object` is counted per content
    /// hash, distinguishing the CALLER-VISIBLE byte transfer (the
    /// bytes read_object returns) from the implied internal read
    /// volume (the classified lengths — each classification performs
    /// the full verified read provider-side). A reduction in the
    /// transfer is never misreported as a reduction in the physical
    /// reads.
    struct CountingMedium<M: CanonicalMedium> {
        inner: M,
        read_calls: std::collections::HashMap<Hash, u32>,
        read_bytes: u64,
        classify_calls: std::collections::HashMap<Hash, u32>,
        classify_volume: u64,
    }

    impl<M: CanonicalMedium> CountingMedium<M> {
        fn wrap(inner: M) -> Self {
            CountingMedium {
                inner,
                read_calls: std::collections::HashMap::new(),
                read_bytes: 0,
                classify_calls: std::collections::HashMap::new(),
                classify_volume: 0,
            }
        }
    }

    impl<M: CanonicalMedium> CanonicalMedium for CountingMedium<M> {
        fn head(&mut self) -> Result<Option<Hash>, MediumError> {
            self.inner.head()
        }
        fn append_admitted(
            &mut self,
            admitted: crate::medium::AdmittedAppend,
            expected_parent: Option<Hash>,
        ) -> Result<AppendOutcome, MediumError> {
            self.inner.append_admitted(admitted, expected_parent)
        }
        fn read_record(&mut self, id: &Hash) -> Result<RevisionRecord, MediumError> {
            self.inner.read_record(id)
        }
        fn read_object(&mut self, hash: &Hash) -> Result<Vec<u8>, MediumError> {
            let bytes = self.inner.read_object(hash)?;
            *self.read_calls.entry(*hash).or_insert(0) += 1;
            self.read_bytes += bytes.len() as u64;
            Ok(bytes)
        }
        fn classify_object(&mut self, hash: &Hash) -> Result<ObjectClass, MediumError> {
            let class = self.inner.classify_object(hash)?;
            *self.classify_calls.entry(*hash).or_insert(0) += 1;
            self.classify_volume += class.length;
            Ok(class)
        }
        fn validate(&mut self) -> Result<crate::medium::ChainSummary, MediumError> {
            self.inner.validate()
        }
        fn reconcile_unknown(&mut self, candidate: &Hash) -> Result<Reconciliation, MediumError> {
            self.inner.reconcile_unknown(candidate)
        }
        fn durability_tier(&self) -> SyncTier {
            self.inner.durability_tier()
        }
        fn reclaim(&mut self, units: &[crate::reclaim::ReclaimUnit]) -> Result<u64, MediumError> {
            self.inner.reclaim(units)
        }
        fn reclaim_pending(
            &mut self,
            units: &[crate::reclaim::ReclaimUnit],
        ) -> Result<bool, MediumError> {
            self.inner.reclaim_pending(units)
        }
    }

    /// One publication revision's content object: a well-formed
    /// domain-tagged NON-batch payload (the session-snapshot shape —
    /// a domain string distinct from every batch or
    /// checkpoint-metadata domain, followed by filler).
    fn publication_object(index: u64, size: usize) -> StoredObject {
        let mut enc = Encoder::new();
        enc.write_str(&format!("s6c4-publication-clone-{index}"));
        enc.write_bytes(&vec![index as u8; size]);
        StoredObject::new(enc.into_bytes()).unwrap()
    }

    fn append_publication<M: CanonicalMedium>(
        medium: &mut M,
        parent: Option<Hash>,
        index: u64,
        epoch: u64,
        size: usize,
    ) -> Hash {
        let object = publication_object(index, size);
        let parent = parent.expect("the chain is founded before the cluster");
        let record = RevisionRecord::regular(parent, epoch, 100 + index)
            .with_content_refs(vec![object.hash()]);
        match medium.append(&record, &[object], Some(parent)).unwrap() {
            AppendOutcome::Accepted { id, .. } => id,
            other => panic!("the publication must accept: {other:?}"),
        }
    }

    /// A publication cluster over a founded medium: `count` snapshot
    /// revisions appended to the chain (the deferred-walk shape the
    /// hardware finding reproduced), each with a well-formed
    /// domain-tagged non-batch content object of `size` bytes.
    fn append_publications<M: CanonicalMedium>(medium: &mut M, count: u64, size: usize) {
        let mut parent = medium.head().unwrap();
        for index in 1..=count {
            // The parent's epoch: the founding is epoch 0, each
            // publication's own epoch is parent + 1.
            parent = Some(append_publication(medium, parent, index, index - 1, size));
        }
    }

    /// The staged capture + boundary fixture over ANY medium (the
    /// `capture_index` helper's medium-generic form).
    fn capture_index_on<M: CanonicalMedium>(
        pipeline: &mut CommitPipeline<M>,
        index: u64,
    ) -> TransactionId {
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

    /// The stored file path of a content object in the local medium
    /// (the `objects/<hex>` layout).
    fn object_file(dir: &TestDir, object: &StoredObject) -> std::path::PathBuf {
        dir.store().join("objects").join(object.hash().hex())
    }

    /// The S6-C4 corrective's read-amplification gate: after a
    /// publication cluster, the confirmation's chain walk verifies
    /// every referenced object (the classification counts) but
    /// transfers only the CONSUMED content (the batch) — the
    /// caller-visible read volume excludes the snapshot bodies, and
    /// no object is classified more than once within the walk.
    #[test]
    fn verified_classification_verifies_each_object_once_per_walk() {
        let dir = TestDir::new("s6c4-amplification");
        let mut medium = CountingMedium::wrap(clean_medium(&dir));
        // The publication cluster: three large snapshot revisions on
        // the chain (the deferred-walk shape the hardware finding
        // reproduced).
        append_publications(&mut medium, 3, 24 * 1024);
        // The pending consequence + the confirmation.
        let journal = T1Journal::open(dir.path().join("journal.t1")).unwrap();
        let mut pipeline = CommitPipeline::open(medium, journal).unwrap();
        capture_index_on(&mut pipeline, 1);
        let outcome = pipeline.commit_canonical().unwrap();
        let CommitOutcome::Confirmed(receipt) = outcome else {
            panic!("the commit must confirm: {outcome:?}")
        };
        assert_eq!(receipt.keys.len(), 1);
        // Every referenced object was verified EXACTLY once per
        // examination walk (the cold open walk and the commit walk
        // each classify each object they examine at most once).
        assert!(
            pipeline
                .verifier
                .medium
                .classify_calls
                .values()
                .all(|count| *count == 1),
            "no object classified more than once per walk: {:?}",
            pipeline.verifier.medium.classify_calls
        );
        // The classification covered the three snapshots (the
        // internal verified reads) — the implied internal volume is
        // honest, NOT zero.
        assert!(pipeline.verifier.medium.classify_volume >= 3 * 24 * 1024);
        // The caller-visible transfer excludes the snapshot bodies:
        // only the consequence batch (small) was read back in full.
        assert!(
            pipeline.verifier.medium.read_bytes < 8 * 1024,
            "the snapshot bodies must not transfer to the caller: {} bytes",
            pipeline.verifier.medium.read_bytes
        );
        assert_eq!(pipeline.verifier.medium.read_calls.len(), 1);
    }

    /// The deferred-walk shape done properly (the hardware finding's
    /// exact structure): the pipeline opens over the founded chain,
    /// THEN a concurrent writer appends publication revisions, and
    /// the next confirmation's incremental walk examines them.
    fn open_then_publish(
        dir: &TestDir,
        publications: u64,
        size: usize,
    ) -> CommitPipeline<LocalFileMedium> {
        let journal = T1Journal::open(dir.path().join("journal.t1")).unwrap();
        let pipeline = CommitPipeline::open(clean_medium(dir), journal).unwrap();
        let mut writer = clean_medium(dir);
        append_publications(&mut writer, publications, size);
        pipeline
    }

    /// The strict integrity guarantee: a corrupted snapshot object —
    /// corrupted BEYOND any domain prefix, mid-body — is detected AT
    /// the walk that examines it (the classification's full-object
    /// hash check, never a prefix-only read).
    #[test]
    fn corrupted_snapshot_detected_at_the_first_examination() {
        let dir = TestDir::new("s6c4-corrupt-first");
        let mut pipeline = open_then_publish(&dir, 2, 16 * 1024);
        // Corrupt the newest snapshot's stored bytes on disk,
        // mid-body (the domain prefix stays intact).
        let target = publication_object(2, 16 * 1024);
        let path = object_file(&dir, &target);
        let mut bytes = fs::read(&path).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] = bytes[mid].wrapping_add(1);
        fs::write(&path, bytes).unwrap();
        capture_index_on(&mut pipeline, 1);
        let error = pipeline.commit_canonical().unwrap_err();
        assert!(
            error.to_string().contains("hash") || error.to_string().contains("Hash"),
            "the corrupted object is detected at the walk: {error}"
        );
    }

    /// The corruption-between-walks guarantee: an object referenced
    /// by a revision that a LATER walk examines for the first time,
    /// corrupted after the earlier walk, is detected by that later
    /// walk (the memo is walk-local — every walk re-verifies what it
    /// examines; no session-wide cache exists).
    #[test]
    fn corruption_between_walks_detected_by_the_second_walk() {
        let dir = TestDir::new("s6c4-corrupt-second");
        let mut pipeline = open_then_publish(&dir, 1, 16 * 1024);
        capture_index_on(&mut pipeline, 1);
        let outcome = pipeline.commit_canonical().unwrap();
        assert!(matches!(outcome, CommitOutcome::Confirmed { .. }));
        // A second publication appended after the first walk — its
        // object corrupted before the second walk examines it. (The
        // parent is the first confirmation: epoch 2 of the chain.)
        let mut writer = clean_medium(&dir);
        let head = writer.head().unwrap();
        append_publication(&mut writer, head, 2, 2, 16 * 1024);
        let target = publication_object(2, 16 * 1024);
        let path = object_file(&dir, &target);
        let mut bytes = fs::read(&path).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] = bytes[mid].wrapping_add(1);
        fs::write(&path, bytes).unwrap();
        capture_index_on(&mut pipeline, 2);
        let error = pipeline.commit_canonical().unwrap_err();
        assert!(
            error.to_string().contains("hash") || error.to_string().contains("Hash"),
            "the second walk detects the intervening corruption: {error}"
        );
    }

    /// A missing referenced object fails the walk closed (the
    /// classification's existence check — the existing
    /// `MissingObject`, through the new path).
    #[test]
    fn missing_referenced_object_fails_the_walk() {
        let dir = TestDir::new("s6c4-missing");
        let mut pipeline = open_then_publish(&dir, 2, 16 * 1024);
        let target = publication_object(1, 16 * 1024);
        fs::remove_file(object_file(&dir, &target)).unwrap();
        capture_index_on(&mut pipeline, 1);
        let error = pipeline.commit_canonical().unwrap_err();
        assert!(
            error.to_string().contains("missing") || error.to_string().contains("Missing"),
            "the missing object fails the walk: {error}"
        );
    }

    // ------------------------------------------------ the 1-A split composition
    //
    // The five focused proofs that the authority/verifier composition
    // produces the same decisions and outputs as the accepted
    // single-struct pipeline — each drives the HALVES explicitly
    // (the future actor's exact protocol: derive the immutable
    // request on the authority, process it in the verifier, promote
    // from the verifier's post-operation truth), then checks the
    // facade over the same state agrees.

    /// A captured transaction subsequently canceled before any
    /// canonical submission is never collected, never submitted, and
    /// stays T1-pending — through the explicit composition and the
    /// facade alike.
    #[test]
    fn split_composition_captured_then_canceled_never_submitted() {
        let dir = TestDir::new("split-canceled");
        let journal = T1Journal::open(dir.path().join("journal.t1")).unwrap();
        let mut authority = CommitAuthority::open(journal);
        let verifier = CommitVerifier::open(clean_medium(&dir)).unwrap();
        authority.promote_from(&verifier);
        authority.stage(staged_envelope(1)).unwrap();
        let boundary = boundary(1);
        let receipt = authority
            .capture(
                boundary.position,
                boundary.classification,
                boundary.dependencies,
                boundary.frontier,
                boundary.plan,
            )
            .unwrap();
        authority
            .cancel_transaction(
                receipt.journal.id,
                CancellationReason::ApplicationPolicy,
                b"policy",
            )
            .unwrap();
        // The authority's actionable derivation excludes the canceled
        // transaction: nothing to submit.
        let keys = authority.collect_actionable(&verifier).unwrap();
        assert!(keys.is_empty(), "the canceled capture is not actionable");
        // The facade over the SAME halves agrees — and the canceled
        // key's phase never advances past T1.
        let mut pipeline = CommitPipeline {
            authority,
            verifier,
        };
        assert!(matches!(
            pipeline.commit_canonical().unwrap(),
            CommitOutcome::NothingPending
        ));
        assert_eq!(
            pipeline.status(ConsequenceId::new(1, 1, 1)).unwrap().phase,
            KeyPhase::JournaledT1,
            "the canceled transaction's key stays T1-pending, never submitted"
        );
    }

    /// Multiple actionable transactions compose into one batch
    /// through the split; two journal transactions claiming ONE
    /// identity fail the collection closed — the halves and the
    /// facade produce the same decisions.
    #[test]
    fn split_composition_overlapping_and_conflicting_transactions() {
        let dir = TestDir::new("split-overlap");
        let mut pipeline = fresh_pipeline(&dir);
        // Two actionable transactions (adjacent coverages, distinct
        // identities): the explicit derivation collects BOTH and
        // freezes BOTH producing transactions' ticks into the
        // request.
        capture_index(&mut pipeline, 1);
        capture_index(&mut pipeline, 2);
        let keys = pipeline
            .authority
            .collect_actionable(&pipeline.verifier)
            .unwrap();
        assert_eq!(keys.len(), 2);
        let ticks = pipeline.authority.transaction_ticks(&keys);
        assert_eq!(
            ticks.len(),
            2,
            "each producing transaction's tick is frozen"
        );
        let CommitOutcome::Confirmed(receipt) = pipeline.commit_canonical().unwrap() else {
            panic!("expected confirmation");
        };
        assert_eq!(
            receipt.keys,
            vec![ConsequenceId::new(1, 1, 1), ConsequenceId::new(1, 2, 1)]
        );
        assert!(pipeline.is_confirmed(ConsequenceId::new(1, 1, 1)));
        assert!(pipeline.is_confirmed(ConsequenceId::new(1, 2, 1)));

        // Conflicting identities: a second journal transaction
        // claiming an already-captured (unconfirmed) identity — built
        // directly through the journal (the pipeline is dropped
        // first: the journal's flock is held for the instance
        // lifetime). Both the authority's derivation and the facade
        // fail closed.
        capture_index(&mut pipeline, 3);
        drop(pipeline);
        let mut raw = T1Journal::open(dir.path().join("journal.t1")).unwrap();
        let boundary = boundary(9);
        let input = TransactionInput {
            envelopes: vec![staged_envelope(3)],
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
            pipeline.authority.collect_actionable(&pipeline.verifier),
            Err(PipelineError::ConflictingReuse { .. })
        ));
        assert!(matches!(
            pipeline.commit_canonical(),
            Err(PipelineError::ConflictingReuse { .. })
        ));
    }

    /// A retained unknown attempt survives the failed verification
    /// (the lost acknowledgment) in the VERIFIER half — visible
    /// through the halves, refusing new submissions — and the retry
    /// re-derives the record from the FROZEN request ticks (the
    /// verifier is journal-blind; the retained snapshot must carry
    /// everything).
    #[test]
    fn split_composition_unknown_attempt_survives_and_retries_from_frozen_ticks() {
        let dir = TestDir::new("split-unknown-retry");
        // Founded through a clean instance; the pipeline's medium
        // fails BEFORE the rename — Unknown, nothing installed.
        ensure_founded(&dir);
        let journal = T1Journal::open(dir.path().join("journal.t1")).unwrap();
        let medium = LocalFileMedium::open_with(
            dir.store(),
            Some(FaultControl::new().arm(FaultPoint::HeadTmpWritten, FaultAction::InjectError)),
        )
        .unwrap();
        let mut pipeline = CommitPipeline::open(medium, journal).unwrap();
        capture_index(&mut pipeline, 1);
        let revision = match pipeline.commit_canonical().unwrap() {
            CommitOutcome::Unknown { revision, .. } => revision,
            other => panic!("expected Unknown, observed {other:?}"),
        };
        // The attempt is RETAINED in the verifier half — never
        // dropped by the failed verification.
        assert_eq!(pipeline.verifier.pending_revision(), Some(revision));
        // A new submission is refused while the attempt is
        // unresolved (identity exactness) — through the facade AND
        // the verifier's own submit.
        assert!(matches!(
            pipeline.commit_canonical(),
            Err(PipelineError::AttemptUnresolved { revision: pending }) if pending == revision
        ));
        assert!(matches!(
            pipeline.verifier.submit(CommitRequest {
                keys: BTreeMap::new(),
                ticks: BTreeMap::new(),
            }),
            Err(PipelineError::AttemptUnresolved { revision: pending }) if pending == revision
        ));
        // The verified absence, then the retry FROM THE RETAINED
        // ATTEMPT: on the still-faulted medium the retry stays
        // ambiguous — and the re-derived record is IDENTITY-STABLE
        // (the same parent, epoch, and FROZEN request ticks re-derive
        // the identical record bytes — the verifier is journal-blind;
        // the retained snapshot carries everything).
        assert_eq!(
            pipeline.reconcile_pending().unwrap(),
            ReconcileOutcome::NotInstalled
        );
        match pipeline.retry_pending().unwrap() {
            CommitOutcome::Unknown {
                revision: retried, ..
            } => {
                assert_eq!(
                    retried, revision,
                    "the retry re-derives the identical record from the frozen ticks"
                );
            }
            other => {
                panic!("expected the retry to stay ambiguous on the faulted medium: {other:?}")
            }
        }
        // The attempt SURVIVED the second failed verification too —
        // never dropped, never confirmed.
        assert_eq!(pipeline.verifier.pending_revision(), Some(revision));
        assert_eq!(
            pipeline.reconcile_pending().unwrap(),
            ReconcileOutcome::NotInstalled
        );
        // Recovery through a clean instance (the crash-recovery
        // shape): the same content identity installs exactly once —
        // the same journal evidence, one revision.
        drop(pipeline);
        let mut recovered = fresh_pipeline(&dir);
        let CommitOutcome::Confirmed(confirmation) = recovered.commit_canonical().unwrap() else {
            panic!("expected confirmation");
        };
        assert_eq!(confirmation.keys, vec![ConsequenceId::new(1, 1, 1)]);
        assert_eq!(
            recovered.status(ConsequenceId::new(1, 1, 1)).unwrap().phase,
            KeyPhase::InConfirmedChain
        );
        let mut check = LocalFileMedium::open(dir.store()).unwrap();
        assert_eq!(check.validate().unwrap().records, 2, "one revision only");
    }

    /// No confirmation exists before the full individual-key
    /// verification: a lying attempt (a coverage the confirmed batch
    /// never carried) fails the per-key validation through the
    /// verifier half, and the authority's tracked phase never
    /// advances — never a false T3.
    #[test]
    fn split_composition_no_receipt_before_individual_key_verification() {
        let dir = TestDir::new("split-no-false-receipt");
        let mut pipeline = fresh_pipeline(&dir);
        let transaction = capture_index(&mut pipeline, 1);
        let CommitOutcome::Confirmed(confirmation) = pipeline.commit_canonical().unwrap() else {
            panic!("expected confirmation");
        };
        // A second captured key stays pending while the lying attempt
        // is rejected — the honest key is never conflated.
        let pending = capture_index(&mut pipeline, 2);
        let _ = pending;
        // The lying attempt: the identity the confirmed batch DID
        // carry, but with a coverage it never did.
        let real = ConsequenceBatch {
            entries: vec![BatchEntry {
                id: ConsequenceId::new(1, 1, 1),
                transaction,
                coverage: vec![ClassifiedSpan {
                    span: OccurrenceSpan::new(0, 2).unwrap(),
                    class: DurabilityClass::DurableEligible,
                }],
                effect: staged_envelope(1).effect_commitment().unwrap(),
            }],
        };
        let object = StoredObject::new(real.canonical_bytes().unwrap()).unwrap();
        let mut keys = BTreeMap::new();
        keys.insert(
            ConsequenceId::new(1, 1, 1),
            (
                vec![ClassifiedSpan {
                    span: OccurrenceSpan::new(99, 100).unwrap(),
                    class: DurabilityClass::DurableEligible,
                }],
                transaction,
                staged_envelope(1).effect_commitment().unwrap(),
            ),
        );
        let attempt = PendingAttempt {
            record: RevisionRecord::regular(confirmation.revision, confirmation.epoch, 101),
            batch_hash: object.hash(),
            keys,
            ticks: BTreeMap::new(),
        };
        // The individual validation fails closed through the verifier
        // half — no Confirmed receipt, no promotion on the authority
        // side.
        assert!(matches!(
            pipeline.verifier.confirm(attempt),
            Err(PipelineError::ConfirmationInvalid {
                detail: "payload-mismatch-in-confirmed-batch"
            })
        ));
        assert_eq!(
            pipeline.status(ConsequenceId::new(1, 2, 1)).unwrap().phase,
            KeyPhase::JournaledT1,
            "the honestly captured key is never promoted by a rejected attempt"
        );
        assert!(!pipeline.is_confirmed(ConsequenceId::new(1, 2, 1)));
    }

    /// The explicit composition protocol end-to-end: the authority
    /// derives the immutable request, the verifier processes it to a
    /// verified Confirmed, and the authority's promote_from advances
    /// every tracked key to InConfirmedChain at exactly the
    /// confirming revision — the facade's semantics, driven by hand.
    #[test]
    fn split_composition_promotes_confirmed_keys_after_verification() {
        let dir = TestDir::new("split-promotion");
        let journal = T1Journal::open(dir.path().join("journal.t1")).unwrap();
        let mut authority = CommitAuthority::open(journal);
        let mut verifier = CommitVerifier::open(clean_medium(&dir)).unwrap();
        authority.promote_from(&verifier);
        // Two captured keys, one reported applied to live state (the
        // pending-vs-confirmed distinction survives the split).
        authority.stage(staged_envelope(1)).unwrap();
        let first = boundary(1);
        authority
            .capture(
                first.position,
                first.classification,
                first.dependencies,
                first.frontier,
                first.plan,
            )
            .unwrap();
        authority.stage(staged_envelope(2)).unwrap();
        let second = boundary(2);
        authority
            .capture(
                second.position,
                second.classification,
                second.dependencies,
                second.frontier,
                second.plan,
            )
            .unwrap();
        authority
            .mark_applied(&[ConsequenceId::new(1, 1, 1)])
            .unwrap();
        assert_eq!(
            authority.status(ConsequenceId::new(1, 1, 1)).unwrap().phase,
            KeyPhase::AppliedToLivePending
        );
        // The composition: derive → submit → promote.
        let keys = authority.collect_actionable(&verifier).unwrap();
        assert_eq!(keys.len(), 2);
        let ticks = authority.transaction_ticks(&keys);
        let CommitOutcome::Confirmed(receipt) =
            verifier.submit(CommitRequest { keys, ticks }).unwrap()
        else {
            panic!("expected confirmation");
        };
        assert_eq!(
            receipt.keys,
            vec![ConsequenceId::new(1, 1, 1), ConsequenceId::new(1, 2, 1)]
        );
        assert_eq!(
            verifier.completed_walks(),
            2,
            "the open walk plus the confirmation walk"
        );
        authority.promote_from(&verifier);
        // Both keys promoted at exactly the confirming revision.
        for id in [ConsequenceId::new(1, 1, 1), ConsequenceId::new(1, 2, 1)] {
            let status = authority.status(id).unwrap();
            assert_eq!(status.phase, KeyPhase::InConfirmedChain);
            assert_eq!(status.revision, Some(receipt.revision));
            assert!(verifier.is_confirmed(id));
        }
        // A SECOND promotion pass is idempotent (the facade runs it
        // after every operation).
        authority.promote_from(&verifier);
        assert_eq!(
            authority.status(ConsequenceId::new(1, 1, 1)).unwrap().phase,
            KeyPhase::InConfirmedChain
        );
    }
}
