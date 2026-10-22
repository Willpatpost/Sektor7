//! The durable T1 journal (`journal/1`) — §5.5.1's atomic transaction
//! frames and §5.5.2's lifecycle discipline, the third of the six
//! Batch 3 modules.
//!
//! **The frame protocol.** One append-only file per campaign: a
//! versioned header, then length-framed records forming a
//! hash-chained stream (each record carries the SHA-256 of the
//! previous record's bytes, anchored at the header's hash —
//! tamper-evidence for every byte, including commit and cancellation
//! records no digest otherwise covers). A transaction is **capture
//! records then an explicit transaction-commit record** carrying the
//! transaction identity, the ordered journal sequence, the record
//! count, the SHA-256 digest over the full capture record set, the
//! consequential identities, the source-position binding, the
//! durability classification, the causal dependencies, the monotonic
//! identity-frontier entries, and the complete plan data for the
//! future B4 prepared-transition integration. A transaction is
//! **actionable iff its commit record is present, complete, verified
//! (count, digest, sequence, identity derivation), and durably
//! acknowledged** — capture records without a valid trailing commit
//! are incomplete preparation, inert by construction.
//!
//! **The four durability states (the review's binding distinction —
//! never interchangeable):**
//!
//! 1. **Readable transaction data** — frames exist on disk and parse
//!    (an aborted attempt's captures, still inert).
//! 2. **Valid committed frame** — the complete transaction with its
//!    trailing commit record, count, sequence, and digest verified.
//! 3. **Acknowledged T1 durability** — the required filesystem
//!    synchronization completed **and the caller received the
//!    receipt**. The acknowledgment is a *caller-observed event in
//!    one session*: it is never persisted, and a reloaded journal
//!    never claims a historical acknowledgment its caller never
//!    observed. Recovery may make a discovered committed frame
//!    actionable per its evidence — that is not a fabricated ack.
//! 4. **Confirmed canonical T3** — established only by the pipeline
//!    from verified chain inclusion under the applicable
//!    storage-provider contract; the journal alone can never confirm
//!    it, and [`TransactionDurability`] keeps the four states one
//!    non-interchangeable type so the pipeline cannot conflate them.
//!
//! **One fsync per completed transaction, at the commit record.** A
//! receipt never returns before the sync completes (requirement C);
//! a commit-phase write or sync failure is the typed
//! [`JournalError::AmbiguousCommit`] — never a guessed outcome — and
//! the documented next step is [`T1Journal::reconcile_write`]:
//! reread the file, verify the frame (§5.5.2 case 9 — present and
//! valid ⇒ the intent is durable; absent ⇒ not durable, retry-safe).
//! The append handle opens with WRITE access so `sync_all` works on
//! Windows (`FlushFileBuffers` requires it — the B2 lesson). The
//! capture-phase failure (`AbortedBeforeCommit`) is definitive — no
//! commit record was attempted, so nothing actionable exists.
//!
//! **Torn tails and committed-history corruption (the documented
//! rules).** The writer never appends past an unresolved tail
//! (open-for-write truncates the inert region to the last valid
//! commit/cancellation boundary first; the observed tail — kind and
//! byte length — is reported for diagnostics). Therefore, under the
//! single-writer discipline, at the moment a commit record was
//! written every byte before it was exactly what the writer wrote.
//! It follows that:
//!
//! - A **decodable** commit or cancellation record whose semantic
//!   checks fail (count, digest, sequence continuity, the identity
//!   set, the identity derivation, a duplicate transaction id, an
//!   unknown or double cancellation target) is **post-write
//!   corruption of committed history: fail closed, evidence
//!   preserved** — never truncated, never skipped, never
//!   reinterpreted as an ordinary incomplete tail. The writer
//!   computes every one of these values from the bytes it just
//!   wrote, so a mismatch proves later modification.
//! - A torn, truncated, or undecodable trailing region **after the
//!   last valid commit** is the crash window's inert tail (a
//!   filesystem zero-fill is the documented artifact and classifies
//!   the same way): reported, truncated by the next open-for-write,
//!   never replayed. The crash matrix's W1 — nothing there was ever
//!   actionable or acknowledged.
//! - Damage detected anywhere **before a later validating commit**
//!   is, by the same discipline, corruption within committed
//!   history: fail closed (the record-chain and the walk continue
//!   past damaged frames to find the later anchor).
//!
//! **Supersession is causal, never positional.** A later journal
//!   sequence alone never supersedes an earlier durable, unapplied
//!   consequence. A transaction stops being actionable only through
//!   the contractually valid, durable, explicitly reason-coded
//!   [`CancellationRecord`] (validated before suppression: the
//!   target must be committed and not already canceled); the
//!   causally-valid-later-transition path arrives with B4's
//!   RegionDirector integration and is carried, not implemented, by
//!   the reason codes here.
//!
//! **Transaction identity** (§5.5.2 case 8): the
//! [`TransactionId`] is the SHA-256 over the stable identity
//! material — the consequential identities, the classification, the
//! plan bytes — deliberately excluding the position, tick, spans,
//! and deltas a retry legitimately re-derives. A retry therefore
//! re-derives the same id, the journal recognizes it, and a
//! duplicate append returns the existing committed state
//! idempotently (no second frame, no second application).
//!
//! **Placement and efficiency:** the journal operates at
//! deactivation, milestone, and encounter boundaries — never in
//! per-NPC hot paths, never per tick; exactly one fsync per
//! completed transaction (and one at creation); bounded parsing
//! throughout (every count and blob is checked against its
//! documented maximum and the remaining input before allocation);
//! synchronous-first. The journal's own tier mirrors the medium's:
//! Linux carries the full fsync contract, Windows the process-crash
//! tier — never presented as power-loss durability (the SQLite
//! fallback decision remains the lead's).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use engine_core::{Decoder, Encoder, SaveError};

use crate::consequence::ConsequenceId;
use crate::envelope::{ConsequenceEnvelope, DurabilityClass, SourcePosition};
use crate::fault::FaultAction;
use crate::hash::{Hash, hash_object};
use crate::medium::SyncTier;

/// The journal-file header's domain (versioned).
pub const JOURNAL_DOMAIN: &str = "sektor7/engine-persist/journal/1";
/// The framed-record domain (versioned; the kind byte follows).
pub const JOURNAL_RECORD_DOMAIN: &str = "sektor7/engine-persist/journal-record/1";
/// The transaction-identity derivation domain (versioned).
pub const TRANSACTION_ID_DOMAIN: &str = "sektor7/engine-persist/transaction-id/1";

pub const JOURNAL_SCHEMA: u8 = 1;
pub const JOURNAL_RECORD_SCHEMA: u8 = 1;
pub const TRANSACTION_ID_SCHEMA: u8 = 1;

/// Documented maxima (bounded decode — every count is also bounded
/// by the remaining input before any allocation may trust it).
pub const MAX_JOURNAL_ENVELOPES: u64 = 1 << 16;
pub const MAX_JOURNAL_IDENTITIES: u64 = 1 << 16;
pub const MAX_JOURNAL_DEPENDENCIES: u64 = 1 << 16;
pub const MAX_JOURNAL_FRONTIER: u64 = 1 << 16;
pub const MAX_JOURNAL_ENVELOPE_BYTES: u64 = 1 << 20;
pub const MAX_JOURNAL_PLAN_BYTES: u64 = 1 << 20;
pub const MAX_JOURNAL_DETAIL_BYTES: u64 = 1 << 16;
/// Documented maximum for the journal file itself (the scan reads it
/// once per open/recovery — bounded, never in a hot path; growth
/// beyond this fails closed rather than allocating unbounded).
pub const MAX_JOURNAL_FILE_BYTES: u64 = 1 << 32;

/// The stable transaction identity — the SHA-256 over the stable
/// identity material (consequential identities, classification,
/// plan). A retry re-derives the same id; a genuinely new operation
/// receives a fresh one; no issued durable identity is ever reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TransactionId(pub Hash);

/// One monotonic identity-frontier entry: the advanced episode for a
/// region, journaled with the commit that advanced it (the frontier
/// advances only when a transaction commits durably — §5.5.1 case 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrontierEntry {
    pub region: u32,
    pub episode: u64,
}

/// The explicit, reason-coded cancellation cause (validated before
/// suppression; a higher journal sequence alone never supersedes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancellationReason {
    /// The application's explicit policy decision.
    ApplicationPolicy,
    /// A causally valid later transition covers the target (the B4
    /// RegionDirector path; carried here as the reason code).
    CausalSupersession,
    /// A governed resolution after divergence evidence.
    GovernedResolution,
}

/// Which phase of the commit boundary an ambiguous failure occurred
/// in (the receipt never returned; the on-disk state is unknown
/// pending reconciliation — never guessed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitStage {
    Write,
    Sync,
}

/// One transaction submitted to the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionInput {
    /// The complete consequential-state envelopes (the capture
    /// records — one per envelope, in order).
    pub envelopes: Vec<ConsequenceEnvelope>,
    /// The transaction-level source-position binding.
    pub position: SourcePosition,
    /// The transaction-level durability classification (the
    /// envelopes' per-span classes refine it — the transaction lane
    /// composes with mixed spans).
    pub classification: DurabilityClass,
    /// Causal dependencies on prior transactions (normalized to
    /// ascending canonical order, unique).
    pub dependencies: Vec<TransactionId>,
    /// The monotonic identity-frontier entries this commit carries
    /// (strictly ascending by region, unique).
    pub frontier: Vec<FrontierEntry>,
    /// Complete plan data for the future prepared-transition
    /// integration (B4 owns the schema; carried transparently,
    /// covered by the transaction identity).
    pub plan: Vec<u8>,
}

impl TransactionInput {
    /// The stable transaction identity — SHA-256 over the stable
    /// identity material (sorted unique consequential identities +
    /// classification + plan), excluding everything a retry
    /// legitimately re-derives (position, tick, spans, deltas,
    /// dependencies). Duplicate identities within one transaction
    /// are a conflicting reuse and fail closed.
    pub fn transaction_id(&self) -> Result<TransactionId, SaveError> {
        let ids = sorted_unique_identities(&self.envelopes)?;
        let mut enc = Encoder::new();
        enc.write_str(TRANSACTION_ID_DOMAIN);
        enc.write_u8(TRANSACTION_ID_SCHEMA);
        enc.write_u8(durability_discriminant(self.classification));
        enc.write_count(ids.len());
        for id in ids {
            enc.write_u32(id.region);
            enc.write_u64(id.episode);
            enc.write_u32(id.npc);
        }
        enc.write_bytes(&self.plan);
        Ok(TransactionId(hash_object(&enc.into_bytes())))
    }
}

/// The receipt returned only after the transaction's durability
/// completed (requirement C: never before the fsync).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalReceipt {
    pub id: TransactionId,
    pub sequence: u64,
    pub record_count: u64,
    pub digest: Hash,
    pub classification: DurabilityClass,
}

/// One committed, verified transaction — actionable unless canceled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedTransaction {
    pub id: TransactionId,
    pub sequence: u64,
    pub record_count: u64,
    pub digest: Hash,
    /// The complete envelopes, in journal order.
    pub envelopes: Vec<ConsequenceEnvelope>,
    pub position: SourcePosition,
    pub classification: DurabilityClass,
    pub dependencies: Vec<TransactionId>,
    pub frontier: Vec<FrontierEntry>,
    pub plan: Vec<u8>,
}

impl CommittedTransaction {
    /// The receipt form (the same fields the appending caller
    /// observed — reconstruction never fabricates an acknowledgment).
    pub fn receipt(&self) -> JournalReceipt {
        JournalReceipt {
            id: self.id,
            sequence: self.sequence,
            record_count: self.record_count,
            digest: self.digest,
            classification: self.classification,
        }
    }
}

/// One durable, validated cancellation record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCancellation {
    pub sequence: u64,
    pub target: TransactionId,
    pub reason: CancellationReason,
    pub detail: Vec<u8>,
}

/// The four durability states — the review's binding distinction.
/// [`TransactionDurability::ConfirmedT3`] is established only by the
/// pipeline from verified canonical-chain inclusion; the journal
/// itself never returns it (a journal-only view can never confirm
/// T3), and the single type keeps the states non-interchangeable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionDurability {
    /// Frames exist on disk and parse (an aborted attempt's capture
    /// records — inert, never replayed).
    Readable,
    /// The complete transaction with its trailing commit record,
    /// count, sequence, and digest verified — actionable unless
    /// canceled.
    Committed,
    /// The synchronization completed and **this session's caller**
    /// received the receipt. Session-scoped, never persisted; a
    /// reloaded journal reports at most `Committed`.
    AcknowledgedT1,
    /// Confirmed through the verified canonical revision chain under
    /// the applicable storage-provider contract (pipeline-owned).
    ConfirmedT3,
}

/// The outcome of reconciling an ambiguous append (§5.5.2 case 9 —
/// reread and verify, never guess).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteReconciliation {
    /// The transaction's commit frame is present and fully valid —
    /// the intent is durable and actionable. This is **not** a
    /// fabricated acknowledgment: the receipt is reconstructed from
    /// the verified frame, and the acknowledgment bit stays unset.
    Committed { receipt: JournalReceipt },
    /// No valid commit frame exists for the attempted identity —
    /// nothing actionable was written; an idempotent retry is safe
    /// (the inert tail, if any, is resolved first).
    NotCommitted,
    /// A transaction with this identity IS committed — but with a
    /// DIFFERENT consequential payload. The identity-level
    /// `TransactionId` matched because identity is content-blind by
    /// design; the committed effects differ from the attempt, so
    /// this is never the attempted committed transaction and never a
    /// matching receipt. Fail closed — never retry as though it were
    /// the same attempt.
    ConflictingPayload { id: TransactionId },
}

/// What the trailing inert region beyond the last valid record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailKind {
    /// The file ends inside a frame's length prefix or record bytes
    /// (the classical torn write), or a committed transaction's
    /// record was truncated away.
    TornFrame,
    /// Capture frames parse but no commit record follows —
    /// incomplete preparation, inert by construction.
    MissingCommit,
    /// A complete frame region that does not decode, or a
    /// zero-filled region (the documented filesystem artifact).
    Undecodable,
    /// Record-chain damage in the trailing (post-last-commit)
    /// region — inert because no later valid commit anchors it.
    ChainDamage,
    /// The header itself is a torn creation prefix (re-initialized
    /// by open-for-write; nothing could ever have been journaled).
    TornCreation,
}

/// The journal's trailing state beyond the last valid record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailState {
    Clean,
    /// The inert region (never actionable, never acknowledged):
    /// reported for diagnostics, truncated by the next
    /// open-for-write, never replayed. Committed history is never
    /// touched.
    Inert {
        byte_len: u64,
        kind: TailKind,
    },
}

/// A complete read-only classification of a journal file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalScan {
    /// The committed transactions, in journal-sequence order.
    pub committed: Vec<CommittedTransaction>,
    /// The validated cancellation records, in sequence order.
    pub cancellations: Vec<RecordedCancellation>,
    /// The trailing state observed by the scan.
    pub tail: TailState,
}

/// Typed journal failures. `AmbiguousCommit` and
/// `AbortedBeforeCommit` carry the attempted identity semantics;
/// `CorruptCommitted` is fail-closed with the evidence preserved.
#[derive(Debug)]
pub enum JournalError {
    Io {
        op: &'static str,
        path: String,
        source: io::Error,
    },
    /// The header exists but is neither a valid header nor a torn
    /// creation prefix — fail closed.
    CorruptHeader { detail: String },
    /// Post-write corruption of committed history — the file is
    /// never truncated, skipped, or repaired; the rule names what
    /// failed and `at` is the byte offset of the evidence.
    CorruptCommitted { at: u64, rule: &'static str },
    /// A codec or input validation failure (typed, pre-write when
    /// raised by an append).
    Format(SaveError),
    /// A capture-phase I/O failure: no commit record was attempted,
    /// so the transaction is definitively not committed. The inert
    /// tail is resolved by the next open-for-write; retry-safe.
    AbortedBeforeCommit { source: io::Error },
    /// A commit-phase write or sync failure: the frame's presence
    /// and durability are unknown. Reconcile via
    /// [`T1Journal::reconcile_write`] — never a guessed outcome, and
    /// never a fabricated acknowledgment.
    AmbiguousCommit {
        id: TransactionId,
        stage: CommitStage,
        source: io::Error,
    },
    /// The cancellation target is not a committed transaction.
    UnknownTarget { id: TransactionId },
    /// The cancellation target is already canceled.
    AlreadyCanceled { id: TransactionId },
    /// A transaction with this identity is already committed with a
    /// DIFFERENT consequential payload — the identity-level
    /// `TransactionId` matched (identity is content-blind by design;
    /// retry-stable identities must not conflate with effect
    /// equivalence), but the committed effects differ from the
    /// attempt. Never mistaken for an idempotent duplicate: fail
    /// closed.
    ConflictingPayload { id: TransactionId },
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalError::Io { op, path, source } => {
                write!(f, "journal I/O failure during {op} at {path}: {source}")
            }
            JournalError::CorruptHeader { detail } => {
                write!(f, "the journal header is corrupt: {detail}")
            }
            JournalError::CorruptCommitted { at, rule } => write!(
                f,
                "committed journal history is corrupt at byte {at} ({rule}) — \
                 fail closed, evidence preserved"
            ),
            JournalError::Format(error) => write!(f, "journal format failure: {error}"),
            JournalError::AbortedBeforeCommit { source } => write!(
                f,
                "journal capture write aborted before the commit record \
                 (not committed; retry-safe): {source}"
            ),
            JournalError::AmbiguousCommit { id, stage, source } => write!(
                f,
                "ambiguous journal commit for {id:?} at the {stage:?} stage — \
                 reconcile by rereading, never guess: {source}"
            ),
            JournalError::UnknownTarget { id } => {
                write!(f, "cancellation target {id:?} is not committed")
            }
            JournalError::AlreadyCanceled { id } => {
                write!(f, "cancellation target {id:?} is already canceled")
            }
            JournalError::ConflictingPayload { id } => write!(
                f,
                "transaction {id:?} is already committed with a different \
                 consequential payload — identity equality is never effect \
                 equivalence; fail closed"
            ),
        }
    }
}

impl std::error::Error for JournalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JournalError::Io { source, .. }
            | JournalError::AbortedBeforeCommit { source, .. }
            | JournalError::AmbiguousCommit { source, .. } => Some(source),
            JournalError::Format(error) => Some(error),
            _ => None,
        }
    }
}

/// The journal's own crash-window fault seam (mirrors the B2 medium
/// seam; a journal constructed without a
/// [`JournalFaultControl`] never consults it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JournalFaultPoint {
    /// Before the first capture frame is written (pre-window — a
    /// clean, retryable failure; nothing written).
    BeforeCaptures,
    /// After the first capture frame (mid-transaction).
    MidCaptureWrite,
    /// All capture frames written, before the commit record
    /// (the incomplete-preparation window).
    CapturesWritten,
    /// The commit frame fully written, before the synchronization —
    /// the strongest ambiguity: the frame may be present and valid.
    CommitWritten,
    /// After the synchronization succeeded, before the receipt
    /// returns — durable but unacknowledged; a crash here is
    /// recovered as committed with **no** fabricated acknowledgment.
    AfterSync,
}

impl JournalFaultPoint {
    /// The stable name used by the process-test harness environment.
    pub fn name(&self) -> &'static str {
        match self {
            JournalFaultPoint::BeforeCaptures => "before-captures",
            JournalFaultPoint::MidCaptureWrite => "mid-capture-write",
            JournalFaultPoint::CapturesWritten => "captures-written",
            JournalFaultPoint::CommitWritten => "commit-written",
            JournalFaultPoint::AfterSync => "after-sync",
        }
    }

    /// Parses the harness name form.
    pub fn from_name(name: &str) -> Option<Self> {
        const POINTS: &[JournalFaultPoint] = &[
            JournalFaultPoint::BeforeCaptures,
            JournalFaultPoint::MidCaptureWrite,
            JournalFaultPoint::CapturesWritten,
            JournalFaultPoint::CommitWritten,
            JournalFaultPoint::AfterSync,
        ];
        POINTS.iter().copied().find(|point| point.name() == name)
    }
}

/// An armed map of journal point → action (test infrastructure).
#[derive(Debug, Clone, Default)]
pub struct JournalFaultControl {
    armed: BTreeMap<JournalFaultPoint, FaultAction>,
}

impl JournalFaultControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// Arms a point (builder form).
    pub fn arm(mut self, point: JournalFaultPoint, action: FaultAction) -> Self {
        self.armed.insert(point, action);
        self
    }

    /// Fires the armed action for `point`, if any: `Abort` terminates
    /// the process at the boundary; `InjectError` surfaces as the
    /// mapped failure for the phase.
    fn check(&self, point: JournalFaultPoint) -> Result<(), io::Error> {
        match self.armed.get(&point) {
            None => Ok(()),
            Some(FaultAction::Abort) => std::process::abort(),
            Some(FaultAction::InjectError) => Err(io::Error::other("fault-injected")),
        }
    }
}

// ------------------------------------------------------------ wire forms

fn durability_discriminant(class: DurabilityClass) -> u8 {
    match class {
        DurabilityClass::DurableEligible => 0,
        DurabilityClass::EncounterProvisional => 1,
    }
}

fn read_durability(dec: &mut Decoder<'_>) -> Result<DurabilityClass, SaveError> {
    match dec.read_u8()? {
        0 => Ok(DurabilityClass::DurableEligible),
        1 => Ok(DurabilityClass::EncounterProvisional),
        discriminant => Err(SaveError::Enum {
            section: "journal",
            discriminant: u64::from(discriminant),
        }),
    }
}

fn cancellation_discriminant(reason: CancellationReason) -> u8 {
    match reason {
        CancellationReason::ApplicationPolicy => 0,
        CancellationReason::CausalSupersession => 1,
        CancellationReason::GovernedResolution => 2,
    }
}

fn read_cancellation_reason(dec: &mut Decoder<'_>) -> Result<CancellationReason, SaveError> {
    match dec.read_u8()? {
        0 => Ok(CancellationReason::ApplicationPolicy),
        1 => Ok(CancellationReason::CausalSupersession),
        2 => Ok(CancellationReason::GovernedResolution),
        discriminant => Err(SaveError::Enum {
            section: "journal",
            discriminant: u64::from(discriminant),
        }),
    }
}

fn write_position(enc: &mut Encoder, position: &SourcePosition) {
    match position.head {
        Some(head) => {
            enc.write_u8(1);
            enc.write_bytes(head.as_bytes());
        }
        None => enc.write_u8(0),
    }
    enc.write_u64(position.tick);
}

fn read_position(dec: &mut Decoder<'_>) -> Result<SourcePosition, SaveError> {
    let head = match dec.read_u8()? {
        0 => None,
        1 => {
            let bytes = dec.read_bytes(Hash::WIDTH, "journal")?;
            Some(Hash::from_bytes(bytes).ok_or(SaveError::Integrity("journal/hash-width"))?)
        }
        discriminant => {
            return Err(SaveError::Enum {
                section: "journal",
                discriminant: u64::from(discriminant),
            });
        }
    };
    let tick = dec.read_u64()?;
    Ok(SourcePosition { head, tick })
}

fn write_identities(enc: &mut Encoder, ids: &[ConsequenceId]) {
    enc.write_count(ids.len());
    for id in ids {
        enc.write_u32(id.region);
        enc.write_u64(id.episode);
        enc.write_u32(id.npc);
    }
}

/// Strictly ascending, unique identity list (16 bytes per entry).
fn read_identities(
    dec: &mut Decoder<'_>,
    max: u64,
    kind: &'static str,
) -> Result<Vec<ConsequenceId>, SaveError> {
    const SECTION: &str = "journal";
    let count = dec.read_count(max, SECTION, kind)?;
    let remaining = dec.remaining() as u64;
    if count > remaining / 16 {
        return Err(SaveError::Limit {
            section: SECTION,
            kind,
            value: count,
            max: remaining / 16,
        });
    }
    let mut ids: Vec<ConsequenceId> = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let id = ConsequenceId {
            region: dec.read_u32()?,
            episode: dec.read_u64()?,
            npc: dec.read_u32()?,
        };
        if let Some(prior) = ids.last()
            && id <= *prior
        {
            return Err(SaveError::Order { section: SECTION });
        }
        ids.push(id);
    }
    Ok(ids)
}

fn write_transaction_ids(enc: &mut Encoder, ids: &[TransactionId]) {
    enc.write_count(ids.len());
    for id in ids {
        enc.write_bytes(id.0.as_bytes());
    }
}

/// Strictly ascending, unique transaction-id list (40 bytes per
/// entry: length prefix + hash).
fn read_transaction_ids(dec: &mut Decoder<'_>) -> Result<Vec<TransactionId>, SaveError> {
    const SECTION: &str = "journal";
    let count = dec.read_count(MAX_JOURNAL_DEPENDENCIES, SECTION, "dependency-count")?;
    let remaining = dec.remaining() as u64;
    if count > remaining / 40 {
        return Err(SaveError::Limit {
            section: SECTION,
            kind: "dependency-count",
            value: count,
            max: remaining / 40,
        });
    }
    let mut ids: Vec<TransactionId> = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let bytes = dec.read_bytes(Hash::WIDTH, SECTION)?;
        let hash = Hash::from_bytes(bytes).ok_or(SaveError::Integrity("journal/hash-width"))?;
        let id = TransactionId(hash);
        if let Some(prior) = ids.last()
            && id <= *prior
        {
            return Err(SaveError::Order { section: SECTION });
        }
        ids.push(id);
    }
    Ok(ids)
}

fn write_frontier(enc: &mut Encoder, frontier: &[FrontierEntry]) {
    enc.write_count(frontier.len());
    for entry in frontier {
        enc.write_u32(entry.region);
        enc.write_u64(entry.episode);
    }
}

/// Strictly ascending regions, unique (12 bytes per entry).
fn read_frontier(dec: &mut Decoder<'_>) -> Result<Vec<FrontierEntry>, SaveError> {
    const SECTION: &str = "journal";
    let count = dec.read_count(MAX_JOURNAL_FRONTIER, SECTION, "frontier-count")?;
    let remaining = dec.remaining() as u64;
    if count > remaining / 12 {
        return Err(SaveError::Limit {
            section: SECTION,
            kind: "frontier-count",
            value: count,
            max: remaining / 12,
        });
    }
    let mut entries: Vec<FrontierEntry> = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let entry = FrontierEntry {
            region: dec.read_u32()?,
            episode: dec.read_u64()?,
        };
        if let Some(prior) = entries.last()
            && entry.region <= prior.region
        {
            return Err(SaveError::Integrity("journal/frontier-region-order"));
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// The canonical empty-header bytes (the header is one write + one
/// fsync at creation — a crash can only leave a prefix).
fn header_bytes() -> Vec<u8> {
    let mut enc = Encoder::new();
    enc.write_str(JOURNAL_DOMAIN);
    enc.write_u8(JOURNAL_SCHEMA);
    enc.into_bytes()
}

fn read_hash_field(dec: &mut Decoder<'_>) -> Result<Hash, SaveError> {
    let bytes = dec.read_bytes(Hash::WIDTH, "journal")?;
    Hash::from_bytes(bytes).ok_or(SaveError::Integrity("journal/hash-width"))
}

/// One decoded framed record (strict; structural checks included).
enum JournalRecord {
    Capture {
        envelope_bytes: Vec<u8>,
    },
    Commit {
        id: TransactionId,
        sequence: u64,
        record_count: u64,
        digest: Hash,
        identities: Vec<ConsequenceId>,
        position: SourcePosition,
        classification: DurabilityClass,
        dependencies: Vec<TransactionId>,
        frontier: Vec<FrontierEntry>,
        plan: Vec<u8>,
    },
    Cancellation {
        target: TransactionId,
        sequence: u64,
        reason: CancellationReason,
        detail: Vec<u8>,
    },
}

/// Encodes a capture record (the chain field first, then the complete
/// envelope bytes).
fn encode_capture(prev: Hash, envelope_bytes: &[u8]) -> Vec<u8> {
    let mut enc = Encoder::new();
    enc.write_str(JOURNAL_RECORD_DOMAIN);
    enc.write_u8(JOURNAL_RECORD_SCHEMA);
    enc.write_u8(0);
    enc.write_bytes(prev.as_bytes());
    enc.write_bytes(envelope_bytes);
    enc.into_bytes()
}

/// The commit record's fields (bundled to keep the encoder's
/// signature small).
struct CommitFields<'a> {
    id: TransactionId,
    sequence: u64,
    record_count: u64,
    digest: Hash,
    identities: &'a [ConsequenceId],
    position: &'a SourcePosition,
    classification: DurabilityClass,
    dependencies: &'a [TransactionId],
    frontier: &'a [FrontierEntry],
    plan: &'a [u8],
}

fn encode_commit(prev: Hash, fields: &CommitFields<'_>) -> Vec<u8> {
    let mut enc = Encoder::new();
    enc.write_str(JOURNAL_RECORD_DOMAIN);
    enc.write_u8(JOURNAL_RECORD_SCHEMA);
    enc.write_u8(1);
    enc.write_bytes(prev.as_bytes());
    enc.write_bytes(fields.id.0.as_bytes());
    enc.write_u64(fields.sequence);
    enc.write_u64(fields.record_count);
    enc.write_bytes(fields.digest.as_bytes());
    write_identities(&mut enc, fields.identities);
    write_position(&mut enc, fields.position);
    enc.write_u8(durability_discriminant(fields.classification));
    write_transaction_ids(&mut enc, fields.dependencies);
    write_frontier(&mut enc, fields.frontier);
    enc.write_bytes(fields.plan);
    enc.into_bytes()
}

fn encode_cancellation(
    prev: Hash,
    target: TransactionId,
    sequence: u64,
    reason: CancellationReason,
    detail: &[u8],
) -> Vec<u8> {
    let mut enc = Encoder::new();
    enc.write_str(JOURNAL_RECORD_DOMAIN);
    enc.write_u8(JOURNAL_RECORD_SCHEMA);
    enc.write_u8(2);
    enc.write_bytes(prev.as_bytes());
    enc.write_bytes(target.0.as_bytes());
    enc.write_u64(sequence);
    enc.write_u8(cancellation_discriminant(reason));
    enc.write_bytes(detail);
    enc.into_bytes()
}

/// Strict decode of one framed record's bytes.
fn decode_record(bytes: &[u8]) -> Result<(Hash, JournalRecord), SaveError> {
    const SECTION: &str = "journal";
    let mut dec = Decoder::new(bytes);
    dec.read_domain(JOURNAL_RECORD_DOMAIN)?;
    let schema = dec.read_u8()?;
    if schema != JOURNAL_RECORD_SCHEMA {
        return Err(SaveError::Version {
            observed: u32::from(schema),
            supported: u32::from(JOURNAL_RECORD_SCHEMA),
        });
    }
    let kind = dec.read_u8()?;
    let prev = read_hash_field(&mut dec)?;
    let record = match kind {
        0 => JournalRecord::Capture {
            envelope_bytes: dec
                .read_bytes(MAX_JOURNAL_ENVELOPE_BYTES, SECTION)?
                .to_vec(),
        },
        1 => {
            let id = TransactionId(read_hash_field(&mut dec)?);
            let sequence = dec.read_u64()?;
            let record_count = dec.read_u64()?;
            let digest = read_hash_field(&mut dec)?;
            let identities = read_identities(&mut dec, MAX_JOURNAL_IDENTITIES, "identity-count")?;
            let position = read_position(&mut dec)?;
            let classification = read_durability(&mut dec)?;
            let dependencies = read_transaction_ids(&mut dec)?;
            let frontier = read_frontier(&mut dec)?;
            let plan = dec.read_bytes(MAX_JOURNAL_PLAN_BYTES, SECTION)?.to_vec();
            JournalRecord::Commit {
                id,
                sequence,
                record_count,
                digest,
                identities,
                position,
                classification,
                dependencies,
                frontier,
                plan,
            }
        }
        2 => {
            let target = TransactionId(read_hash_field(&mut dec)?);
            let sequence = dec.read_u64()?;
            let reason = read_cancellation_reason(&mut dec)?;
            let detail = dec.read_bytes(MAX_JOURNAL_DETAIL_BYTES, SECTION)?.to_vec();
            JournalRecord::Cancellation {
                target,
                sequence,
                reason,
                detail,
            }
        }
        discriminant => {
            return Err(SaveError::Enum {
                section: SECTION,
                discriminant: u64::from(discriminant),
            });
        }
    };
    dec.finish()?;
    Ok((prev, record))
}

/// Frames a record: `[u64 length][record bytes]`.
fn frame(record: &[u8]) -> Vec<u8> {
    let mut framed = Vec::with_capacity(8 + record.len());
    framed.extend_from_slice(&(record.len() as u64).to_le_bytes());
    framed.extend_from_slice(record);
    framed
}

fn sorted_unique_identities(
    envelopes: &[ConsequenceEnvelope],
) -> Result<Vec<ConsequenceId>, SaveError> {
    let mut ids: Vec<ConsequenceId> = envelopes.iter().map(|envelope| envelope.id).collect();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    if ids.len() != before {
        return Err(SaveError::Integrity(
            "journal/duplicate-identity-in-transaction",
        ));
    }
    Ok(ids)
}

// ------------------------------------------------------ scan/classify

enum HeaderState {
    /// Zero bytes (never initialized, or created empty).
    Fresh,
    Valid {
        header_hash: Hash,
    },
    /// A prefix of the canonical header — a crashed creation; nothing
    /// could ever have been journaled.
    TornCreation {
        byte_len: u64,
    },
}

fn read_header(bytes: &[u8]) -> Result<HeaderState, JournalError> {
    let header = header_bytes();
    if bytes.is_empty() {
        return Ok(HeaderState::Fresh);
    }
    if bytes.len() < header.len() {
        if bytes == &header[..bytes.len()] {
            return Ok(HeaderState::TornCreation {
                byte_len: bytes.len() as u64,
            });
        }
        return Err(JournalError::CorruptHeader {
            detail: "not a header prefix".into(),
        });
    }
    let mut dec = Decoder::new(bytes);
    dec.read_domain(JOURNAL_DOMAIN)
        .map_err(|error| JournalError::CorruptHeader {
            detail: error.to_string(),
        })?;
    let schema = dec.read_u8().map_err(|error| JournalError::CorruptHeader {
        detail: error.to_string(),
    })?;
    if schema != JOURNAL_SCHEMA {
        return Err(JournalError::CorruptHeader {
            detail: format!("unsupported journal schema {schema}"),
        });
    }
    Ok(HeaderState::Valid {
        header_hash: hash_object(&header),
    })
}

/// The classified in-memory state derived from one scan.
struct JournalState {
    committed: Vec<CommittedTransaction>,
    cancellations: Vec<RecordedCancellation>,
    committed_ids: BTreeSet<TransactionId>,
    canceled_ids: BTreeSet<TransactionId>,
    /// Identities visible in the inert tail's capture frames (the
    /// `Readable` state — data exists and parses; never actionable).
    inert_ids: BTreeSet<ConsequenceId>,
    /// The observed trailing state (diagnostic truth — retained even
    /// after the tail is resolved).
    observed_tail: TailState,
    /// The file offset where the next frame must be written (the end
    /// of the last valid commit or cancellation record).
    append_offset: u64,
    /// Whether every inert tail has been truncated away.
    tail_resolved: bool,
    next_sequence: u64,
    /// The hash of the last validly processed record (the chain
    /// anchor for the next append).
    chain_prev: Hash,
}

impl JournalState {
    fn empty(header_hash: Hash) -> Self {
        JournalState {
            committed: Vec::new(),
            cancellations: Vec::new(),
            committed_ids: BTreeSet::new(),
            canceled_ids: BTreeSet::new(),
            inert_ids: BTreeSet::new(),
            observed_tail: TailState::Clean,
            append_offset: header_bytes().len() as u64,
            tail_resolved: true,
            next_sequence: 1,
            chain_prev: header_hash,
        }
    }
}

/// One damage site detected during the walk (the first one stands).
struct Damage {
    offset: u64,
    kind: TailKind,
}

/// The classification walk — the documented rules of the module
/// docs, applied in one sequential pass.
fn classify(bytes: &[u8], header_hash: Hash) -> Result<JournalState, JournalError> {
    let mut state = JournalState::empty(header_hash);
    let mut offset = header_bytes().len();
    let mut expected_prev = header_hash;
    let mut chain_ok = true;
    let mut damage: Option<Damage> = None;
    // The pending (uncommitted) capture group.
    let mut pending: Vec<ConsequenceEnvelope> = Vec::new();
    let mut pending_ids: Vec<ConsequenceId> = Vec::new();
    let mut pending_concat: Vec<u8> = Vec::new();
    let mut last_valid_end = offset as u64;

    while offset < bytes.len() {
        let remaining = bytes.len() - offset;
        if remaining < 8 {
            // A partial length prefix — the classical torn tail.
            damage = Some(Damage {
                offset: offset as u64,
                kind: TailKind::TornFrame,
            });
            break;
        }
        let len = u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("8 bytes"));
        let frame_end = offset as u64 + 8 + len;
        if frame_end > bytes.len() as u64 {
            // The frame claims more bytes than remain — torn.
            damage = Some(Damage {
                offset: offset as u64,
                kind: TailKind::TornFrame,
            });
            break;
        }
        if len == 0 {
            // A zero-length frame is the documented zero-fill
            // artifact (or garbage): damage, walked past.
            if damage.is_none() {
                damage = Some(Damage {
                    offset: offset as u64,
                    kind: TailKind::Undecodable,
                });
            }
            chain_ok = false;
            offset += 8;
            continue;
        }
        let record_bytes = &bytes[offset + 8..frame_end as usize];
        let (prev, record) = match decode_record(record_bytes) {
            Ok(decoded) => decoded,
            Err(_) => {
                if damage.is_none() {
                    damage = Some(Damage {
                        offset: offset as u64,
                        kind: TailKind::Undecodable,
                    });
                }
                chain_ok = false;
                offset = frame_end as usize;
                continue;
            }
        };
        if chain_ok && prev != expected_prev {
            if damage.is_none() {
                damage = Some(Damage {
                    offset: offset as u64,
                    kind: TailKind::ChainDamage,
                });
            }
            chain_ok = false;
            offset = frame_end as usize;
            continue;
        }
        let record_hash = hash_object(record_bytes);
        match record {
            JournalRecord::Capture { envelope_bytes } => {
                match ConsequenceEnvelope::decode(&envelope_bytes) {
                    Ok(envelope) => {
                        state.inert_ids.insert(envelope.id);
                        pending_ids.push(envelope.id);
                        pending.push(envelope);
                        pending_concat.extend_from_slice(record_bytes);
                    }
                    Err(_) => {
                        // The frame decodes as a record but the
                        // envelope inside is damaged.
                        if damage.is_none() {
                            damage = Some(Damage {
                                offset: offset as u64,
                                kind: TailKind::Undecodable,
                            });
                        }
                        chain_ok = false;
                    }
                }
            }
            JournalRecord::Commit {
                id,
                sequence,
                record_count,
                digest,
                identities,
                position,
                classification,
                dependencies,
                frontier,
                plan,
            } => {
                // A decodable commit record whose semantic checks
                // fail is post-write corruption of committed history
                // (the single-writer, no-append-past-tail discipline
                // makes a mismatch unreachable at write time): fail
                // closed, evidence preserved.
                let frame_offset = offset as u64;
                let fail_closed = |rule: &'static str| {
                    Err(JournalError::CorruptCommitted {
                        at: frame_offset,
                        rule,
                    })
                };
                if sequence != state.next_sequence {
                    return fail_closed(if sequence < state.next_sequence {
                        "sequence-regression"
                    } else {
                        "sequence-gap"
                    });
                }
                if record_count != pending.len() as u64 {
                    return fail_closed("count-mismatch");
                }
                if digest != hash_object(&pending_concat) {
                    return fail_closed("digest-mismatch");
                }
                if identities != pending_ids {
                    return fail_closed("identity-set-mismatch");
                }
                let derived = derive_transaction_id(&identities, classification, &plan);
                if id != derived {
                    return fail_closed("identity-derivation");
                }
                if state.committed_ids.contains(&id) {
                    return fail_closed("duplicate-transaction-id");
                }
                if let Some(site) = damage {
                    // A fully validating commit after earlier damage:
                    // the writer wrote this commit over a clean file,
                    // so the damage modified committed history.
                    return Err(JournalError::CorruptCommitted {
                        at: site.offset,
                        rule: "damage-within-committed-history",
                    });
                }
                state.committed_ids.insert(id);
                state.committed.push(CommittedTransaction {
                    id,
                    sequence,
                    record_count,
                    digest,
                    envelopes: std::mem::take(&mut pending),
                    position,
                    classification,
                    dependencies,
                    frontier,
                    plan,
                });
                pending_ids.clear();
                pending_concat.clear();
                state.next_sequence += 1;
                last_valid_end = frame_end;
                state.append_offset = frame_end;
                // A validated commit proves its own bytes; the chain
                // re-anchors here — and the append chain anchors at
                // the last durable record (an unresolved tail's
                // captures are truncated before any append).
                chain_ok = true;
                expected_prev = record_hash;
                state.chain_prev = record_hash;
                offset = frame_end as usize;
                continue;
            }
            JournalRecord::Cancellation {
                target,
                sequence,
                reason,
                detail,
            } => {
                let frame_offset = offset as u64;
                let fail_closed = |rule: &'static str| {
                    Err(JournalError::CorruptCommitted {
                        at: frame_offset,
                        rule,
                    })
                };
                if sequence != state.next_sequence {
                    return fail_closed(if sequence < state.next_sequence {
                        "sequence-regression"
                    } else {
                        "sequence-gap"
                    });
                }
                if !state.committed_ids.contains(&target) {
                    return fail_closed("cancel-unknown-target");
                }
                if state.canceled_ids.contains(&target) {
                    return fail_closed("cancel-duplicate-target");
                }
                if let Some(site) = damage {
                    return Err(JournalError::CorruptCommitted {
                        at: site.offset,
                        rule: "damage-within-committed-history",
                    });
                }
                state.canceled_ids.insert(target);
                state.cancellations.push(RecordedCancellation {
                    sequence,
                    target,
                    reason,
                    detail,
                });
                state.next_sequence += 1;
                last_valid_end = frame_end;
                state.append_offset = frame_end;
                chain_ok = true;
                expected_prev = record_hash;
                state.chain_prev = record_hash;
                offset = frame_end as usize;
                continue;
            }
        }
        expected_prev = record_hash;
        offset = frame_end as usize;
    }

    // The trailing classification: damage or pending captures beyond
    // the last valid record are the inert tail (never actionable,
    // never acknowledged). Committed history ended at
    // `last_valid_end`.
    if let Some(site) = damage {
        state.observed_tail = TailState::Inert {
            byte_len: bytes.len() as u64 - last_valid_end,
            kind: site.kind,
        };
    } else if !pending.is_empty() {
        state.observed_tail = TailState::Inert {
            byte_len: bytes.len() as u64 - last_valid_end,
            kind: TailKind::MissingCommit,
        };
    } else {
        state.observed_tail = TailState::Clean;
    }
    state.tail_resolved = state.append_offset == bytes.len() as u64;
    // Identities that landed in committed transactions are not
    // `Readable`-tail state.
    for transaction in &state.committed {
        for identity in transaction.envelopes.iter().map(|e| e.id) {
            state.inert_ids.remove(&identity);
        }
    }
    Ok(state)
}

/// The stable-identity derivation (shared by the writer and the
/// scan's validation — the carried id must equal it).
fn derive_transaction_id(
    identities: &[ConsequenceId],
    classification: DurabilityClass,
    plan: &[u8],
) -> TransactionId {
    let mut enc = Encoder::new();
    enc.write_str(TRANSACTION_ID_DOMAIN);
    enc.write_u8(TRANSACTION_ID_SCHEMA);
    enc.write_u8(durability_discriminant(classification));
    enc.write_count(identities.len());
    for id in identities {
        enc.write_u32(id.region);
        enc.write_u64(id.episode);
        enc.write_u32(id.npc);
    }
    enc.write_bytes(plan);
    TransactionId(hash_object(&enc.into_bytes()))
}

/// The transaction's consequential payload — the canonical bytes of
/// each envelope, in input order. The identity-level `TransactionId`
/// is deliberately content-blind (retry-stable identities must not
/// conflate with effect equivalence), so duplicate recognition and
/// ambiguous-write reconciliation compare the PAYLOAD: a retry with
/// a different consequential payload is a conflicting claim under
/// the same identity, never an already-durable matching attempt.
fn envelope_payloads(envelopes: &[ConsequenceEnvelope]) -> Result<Vec<Vec<u8>>, SaveError> {
    envelopes
        .iter()
        .map(|envelope| envelope.canonical_bytes())
        .collect()
}

// ------------------------------------------------------------- the journal

/// The durable T1 journal. One instance owns the **sidecar lock
/// file's** exclusive OS-managed lock for its lifetime (process death
/// releases it); the single-writer discipline is the precondition of
/// the corruption-fail-closed rules. The lock never covers the
/// journal data file's bytes — see [`Self::open_with`] for why.
pub struct T1Journal {
    path: PathBuf,
    file: File,
    /// The sidecar lock handle, held for the instance's lifetime —
    /// dropping it (or process death) releases the exclusive lock.
    _lock: File,
    state: JournalState,
    /// The sessions's caller-observed acknowledgments — never
    /// persisted, never claimed for a prior session.
    acknowledged: BTreeSet<TransactionId>,
    fault: Option<JournalFaultControl>,
    sync_count: u64,
    platform_tier: SyncTier,
}

impl T1Journal {
    /// Opens (creating if absent) and classifies. An inert tail is
    /// resolved by truncation (the documented torn-tail rule —
    /// committed history is never touched); committed-history
    /// corruption fails the open with the evidence preserved.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, JournalError> {
        Self::open_with(path, None)
    }

    /// Test-infrastructure constructor with the crash-window fault
    /// seam armed (production uses [`Self::open`]).
    pub fn open_with(
        path: impl Into<PathBuf>,
        fault: Option<JournalFaultControl>,
    ) -> Result<Self, JournalError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| io_err("create journal dir", &path, e))?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| io_err("open journal", &path, e))?;
        // The exclusive OS-managed lock lives on a dedicated sidecar
        // lock file — NEVER on the journal data file, whose bytes
        // other handles must read (the open-time scan, `scan_file`,
        // and the ambiguous-write reread all read through fresh
        // handles): a Windows byte-range lock is enforced against
        // I/O through non-owning handles (ERROR_LOCK_VIOLATION),
        // while a Unix flock is advisory — the B2 `HEAD.lock`
        // precedent. The data handle carries full write access
        // because `set_len` and `FlushFileBuffers` both require
        // `FILE_WRITE_DATA` on Windows, which an append-only handle
        // lacks; every write seeks to the end first (the journal is
        // append-only by design under the single-writer lock).
        let lock = open_lock_file(&path)?;
        let bytes = fs::read(&path).map_err(|e| io_err("read journal", &path, e))?;
        if bytes.len() as u64 > MAX_JOURNAL_FILE_BYTES {
            return Err(JournalError::Format(SaveError::Limit {
                section: "journal",
                kind: "file-length",
                value: bytes.len() as u64,
                max: MAX_JOURNAL_FILE_BYTES,
            }));
        }
        let mut journal = T1Journal {
            path: path.clone(),
            file,
            _lock: lock,
            state: JournalState::empty(Hash::ZERO),
            acknowledged: BTreeSet::new(),
            fault,
            sync_count: 0,
            platform_tier: platform_tier(),
        };
        match read_header(&bytes)? {
            HeaderState::Fresh | HeaderState::TornCreation { .. } => {
                // (Re-)initialize: the header is one write + one fsync,
                // so a crash can only have left a prefix — nothing
                // could ever have been journaled or acknowledged.
                journal
                    .file
                    .set_len(0)
                    .map_err(|e| io_err("truncate journal", &path, e))?;
                let header = header_bytes();
                journal
                    .append_bytes(&header)
                    .map_err(|e| io_err("write journal header", &path, e))?;
                journal.sync()?;
                sync_parent_dir(&path).map_err(|e| io_err("sync journal dir", &path, e))?;
                journal.state = JournalState::empty(hash_object(&header));
            }
            HeaderState::Valid { header_hash } => {
                journal.state = classify(&bytes, header_hash)?;
                journal.resolve_tail()?;
            }
        }
        Ok(journal)
    }

    /// A read-only classification of a journal file (no lock, no
    /// truncation — diagnostics and recovery over a quiescent or
    /// abandoned journal; a concurrent append can at worst add an
    /// inert-tail classification; portable even against a live
    /// writer, because the writer's exclusive lock lives on the
    /// sidecar file — the data file's bytes are never range-locked).
    /// An absent file is an empty journal: nothing pending.
    pub fn scan_file(path: impl AsRef<Path>) -> Result<JournalScan, JournalError> {
        let path = path.as_ref();
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(JournalScan {
                    committed: Vec::new(),
                    cancellations: Vec::new(),
                    tail: TailState::Clean,
                });
            }
            Err(e) => return Err(io_err("read journal", path, e)),
        };
        if bytes.len() as u64 > MAX_JOURNAL_FILE_BYTES {
            return Err(JournalError::Format(SaveError::Limit {
                section: "journal",
                kind: "file-length",
                value: bytes.len() as u64,
                max: MAX_JOURNAL_FILE_BYTES,
            }));
        }
        match read_header(&bytes)? {
            HeaderState::Fresh => Ok(JournalScan {
                committed: Vec::new(),
                cancellations: Vec::new(),
                tail: TailState::Clean,
            }),
            HeaderState::TornCreation { byte_len } => Ok(JournalScan {
                committed: Vec::new(),
                cancellations: Vec::new(),
                tail: TailState::Inert {
                    byte_len,
                    kind: TailKind::TornCreation,
                },
            }),
            HeaderState::Valid { header_hash } => {
                let state = classify(&bytes, header_hash)?;
                Ok(JournalScan {
                    committed: state.committed,
                    cancellations: state.cancellations,
                    tail: state.observed_tail,
                })
            }
        }
    }

    /// Appends one transaction: the capture frames, then the commit
    /// record, then exactly one fsync — the receipt returns only
    /// after the synchronization (requirement C). A duplicate
    /// identity is recognized idempotently (the existing committed
    /// state returns; no second frame, no second application).
    pub fn append_transaction(
        &mut self,
        input: &TransactionInput,
    ) -> Result<JournalReceipt, JournalError> {
        // Pre-write validation — nothing written on failure.
        if input.envelopes.len() as u64 > MAX_JOURNAL_ENVELOPES {
            return Err(JournalError::Format(SaveError::Limit {
                section: "journal",
                kind: "envelope-count",
                value: input.envelopes.len() as u64,
                max: MAX_JOURNAL_ENVELOPES,
            }));
        }
        if input.plan.len() as u64 > MAX_JOURNAL_PLAN_BYTES {
            return Err(JournalError::Format(SaveError::Limit {
                section: "journal",
                kind: "plan-length",
                value: input.plan.len() as u64,
                max: MAX_JOURNAL_PLAN_BYTES,
            }));
        }
        let mut envelope_bytes: Vec<Vec<u8>> = Vec::with_capacity(input.envelopes.len());
        for envelope in &input.envelopes {
            envelope_bytes.push(envelope.canonical_bytes().map_err(JournalError::Format)?);
        }
        let identities =
            sorted_unique_identities(&input.envelopes).map_err(JournalError::Format)?;
        let mut dependencies = input.dependencies.clone();
        dependencies.sort_unstable();
        dependencies.dedup();
        if dependencies.len() as u64 > MAX_JOURNAL_DEPENDENCIES {
            return Err(JournalError::Format(SaveError::Limit {
                section: "journal",
                kind: "dependency-count",
                value: dependencies.len() as u64,
                max: MAX_JOURNAL_DEPENDENCIES,
            }));
        }
        let mut frontier = input.frontier.clone();
        frontier.sort_unstable_by_key(|entry| entry.region);
        // Same region twice (whatever the episode) is an incoherent
        // frontier — never silently repaired.
        for pair in frontier.windows(2) {
            if pair[0].region == pair[1].region {
                return Err(JournalError::Format(SaveError::Integrity(
                    "journal/frontier-region-order",
                )));
            }
        }
        if frontier.len() as u64 > MAX_JOURNAL_FRONTIER {
            return Err(JournalError::Format(SaveError::Limit {
                section: "journal",
                kind: "frontier-count",
                value: frontier.len() as u64,
                max: MAX_JOURNAL_FRONTIER,
            }));
        }
        let id = input.transaction_id().map_err(JournalError::Format)?;
        // Idempotent recognition: the journal knows this
        // transaction — the existing committed state returns (a
        // truthful present-moment receipt of an already-durable
        // transaction, never a fabricated historical one). Identity
        // equality is NOT effect equivalence: the TransactionId is
        // content-blind by design, so the committed payload must
        // match the attempt — a different consequential payload under
        // the same identity is a conflicting claim, never an
        // idempotent duplicate.
        if let Some(existing) = self
            .state
            .committed
            .iter()
            .find(|transaction| transaction.id == id)
        {
            if envelope_payloads(&existing.envelopes).map_err(JournalError::Format)?
                != envelope_payloads(&input.envelopes).map_err(JournalError::Format)?
            {
                return Err(JournalError::ConflictingPayload { id });
            }
            let receipt = existing.receipt();
            self.acknowledged.insert(id);
            return Ok(receipt);
        }
        self.resolve_tail()?;
        self.fault_pre(JournalFaultPoint::BeforeCaptures)?;

        // Capture frames (no sync — one barrier per transaction, at
        // the commit record).
        let mut chain_prev = self.state.chain_prev;
        let mut capture_concat: Vec<u8> = Vec::new();
        for (index, bytes) in envelope_bytes.iter().enumerate() {
            let record = encode_capture(chain_prev, bytes);
            chain_prev = hash_object(&record);
            capture_concat.extend_from_slice(&record);
            let framed = frame(&record);
            self.append_bytes(&framed)
                .map_err(|e| JournalError::AbortedBeforeCommit { source: e })?;
            self.state.append_offset += framed.len() as u64;
            if index == 0 {
                self.fault_pre(JournalFaultPoint::MidCaptureWrite)?;
            }
        }
        self.fault_pre(JournalFaultPoint::CapturesWritten)?;

        // The commit record.
        let sequence = self.state.next_sequence;
        let digest = hash_object(&capture_concat);
        let fields = CommitFields {
            id,
            sequence,
            record_count: envelope_bytes.len() as u64,
            digest,
            identities: &identities,
            position: &input.position,
            classification: input.classification,
            dependencies: &dependencies,
            frontier: &frontier,
            plan: &input.plan,
        };
        let commit_record = encode_commit(chain_prev, &fields);
        let commit_hash = hash_object(&commit_record);
        let framed = frame(&commit_record);
        self.append_bytes(&framed)
            .map_err(|e| JournalError::AmbiguousCommit {
                id,
                stage: CommitStage::Write,
                source: e,
            })?;
        self.state.append_offset += framed.len() as u64;
        self.fault_commit(JournalFaultPoint::CommitWritten, id)?;

        // The one durability barrier of the completed transaction.
        self.sync()?;
        self.fault_commit(JournalFaultPoint::AfterSync, id)?;

        let transaction = CommittedTransaction {
            id,
            sequence,
            record_count: envelope_bytes.len() as u64,
            digest,
            envelopes: input.envelopes.clone(),
            position: input.position,
            classification: input.classification,
            dependencies,
            frontier,
            plan: input.plan.clone(),
        };
        self.state.committed.push(transaction);
        self.state.committed_ids.insert(id);
        self.state.next_sequence += 1;
        self.state.chain_prev = commit_hash;
        self.acknowledged.insert(id);
        Ok(JournalReceipt {
            id,
            sequence,
            record_count: envelope_bytes.len() as u64,
            digest,
            classification: input.classification,
        })
    }

    /// Appends one validated, durable, reason-coded cancellation
    /// record (its own completed record — its own fsync). Suppression
    /// happens only here: a higher journal sequence alone never
    /// supersedes.
    pub fn append_cancellation(
        &mut self,
        target: TransactionId,
        reason: CancellationReason,
        detail: &[u8],
    ) -> Result<u64, JournalError> {
        if detail.len() as u64 > MAX_JOURNAL_DETAIL_BYTES {
            return Err(JournalError::Format(SaveError::Limit {
                section: "journal",
                kind: "detail-length",
                value: detail.len() as u64,
                max: MAX_JOURNAL_DETAIL_BYTES,
            }));
        }
        if !self.state.committed_ids.contains(&target) {
            return Err(JournalError::UnknownTarget { id: target });
        }
        if self.state.canceled_ids.contains(&target) {
            return Err(JournalError::AlreadyCanceled { id: target });
        }
        self.resolve_tail()?;
        let sequence = self.state.next_sequence;
        let record = encode_cancellation(self.state.chain_prev, target, sequence, reason, detail);
        let record_hash = hash_object(&record);
        let framed = frame(&record);
        self.append_bytes(&framed)
            .map_err(|e| JournalError::AmbiguousCommit {
                id: target,
                stage: CommitStage::Write,
                source: e,
            })?;
        self.state.append_offset += framed.len() as u64;
        self.sync()?;
        self.state.canceled_ids.insert(target);
        self.state.cancellations.push(RecordedCancellation {
            sequence,
            target,
            reason,
            detail: detail.to_vec(),
        });
        self.state.next_sequence += 1;
        self.state.chain_prev = record_hash;
        Ok(sequence)
    }

    /// Reconciles an ambiguous append (§5.5.2 case 9): reread the
    /// file, verify, never guess. `Committed` recovers a valid
    /// actionable frame **without** fabricating a caller
    /// acknowledgment (the acknowledgment bit stays unset); the
    /// in-memory state is refreshed from the file either way, and
    /// any inert tail is resolved so a retry appends cleanly.
    pub fn reconcile_write(
        &mut self,
        attempted: &TransactionInput,
    ) -> Result<WriteReconciliation, JournalError> {
        let id = attempted.transaction_id().map_err(JournalError::Format)?;
        let bytes = fs::read(&self.path).map_err(|e| io_err("read journal", &self.path, e))?;
        if bytes.len() as u64 > MAX_JOURNAL_FILE_BYTES {
            return Err(JournalError::Format(SaveError::Limit {
                section: "journal",
                kind: "file-length",
                value: bytes.len() as u64,
                max: MAX_JOURNAL_FILE_BYTES,
            }));
        }
        match read_header(&bytes)? {
            HeaderState::Valid { header_hash } => {
                self.state = classify(&bytes, header_hash)?;
                self.resolve_tail()?;
            }
            // The header cannot regress to torn/fresh while this
            // instance holds the lock and a commit was attempted.
            HeaderState::Fresh | HeaderState::TornCreation { .. } => {
                self.state = JournalState::empty(hash_object(&header_bytes()));
            }
        }
        if let Some(existing) = self
            .state
            .committed
            .iter()
            .find(|transaction| transaction.id == id)
        {
            // Identity equality is NOT effect equivalence: the
            // reread transaction satisfies the ATTEMPT only when its
            // consequential payload matches — a different payload
            // under the same identity is never the attempted
            // committed transaction (never a matching receipt, never
            // a silent retry).
            if envelope_payloads(&existing.envelopes).map_err(JournalError::Format)?
                != envelope_payloads(&attempted.envelopes).map_err(JournalError::Format)?
            {
                return Ok(WriteReconciliation::ConflictingPayload { id });
            }
            return Ok(WriteReconciliation::Committed {
                receipt: existing.receipt(),
            });
        }
        Ok(WriteReconciliation::NotCommitted)
    }

    /// The committed transactions, in journal-sequence order.
    pub fn transactions(&self) -> &[CommittedTransaction] {
        &self.state.committed
    }

    /// The validated cancellation records, in sequence order.
    pub fn cancellations(&self) -> &[RecordedCancellation] {
        &self.state.cancellations
    }

    /// The observed trailing state (diagnostic truth — retained even
    /// after the tail was resolved by truncation).
    pub fn tail(&self) -> TailState {
        self.state.observed_tail
    }

    pub fn next_sequence(&self) -> u64 {
        self.state.next_sequence
    }

    pub fn is_committed(&self, id: TransactionId) -> bool {
        self.state.committed_ids.contains(&id)
    }

    pub fn is_canceled(&self, id: TransactionId) -> bool {
        self.state.canceled_ids.contains(&id)
    }

    /// The supersession rule: actionable iff committed and not
    /// canceled — a later journal sequence alone never suppresses.
    pub fn is_actionable(&self, id: TransactionId) -> bool {
        self.is_committed(id) && !self.is_canceled(id)
    }

    /// The four-state durability view of one transaction. The journal
    /// never returns `ConfirmedT3` (the pipeline establishes it from
    /// verified chain inclusion); a reloaded journal reports at most
    /// `Committed` — acknowledgments are this-session events only.
    /// An aborted attempt leaves no durable trace of its transaction
    /// identity (reconciliation classifies it `NotCommitted`); the
    /// `Readable` state of its capture frames is visible per
    /// consequence identity via [`Self::identity_status`].
    pub fn transaction_status(&self, id: TransactionId) -> Option<TransactionDurability> {
        if self.state.committed_ids.contains(&id) {
            if self.acknowledged.contains(&id) {
                Some(TransactionDurability::AcknowledgedT1)
            } else {
                Some(TransactionDurability::Committed)
            }
        } else {
            None
        }
    }

    /// The four-state durability view of one consequence identity —
    /// the pipeline's per-key view. `Readable` reports capture frames
    /// that exist and parse in the inert tail (an aborted attempt —
    /// never actionable); committed identities report the state of
    /// their containing transaction.
    pub fn identity_status(&self, identity: ConsequenceId) -> Option<TransactionDurability> {
        for transaction in &self.state.committed {
            if transaction
                .envelopes
                .iter()
                .any(|envelope| envelope.id == identity)
            {
                return if self.acknowledged.contains(&transaction.id) {
                    Some(TransactionDurability::AcknowledgedT1)
                } else {
                    Some(TransactionDurability::Committed)
                };
            }
        }
        if self.state.inert_ids.contains(&identity) {
            Some(TransactionDurability::Readable)
        } else {
            None
        }
    }

    /// Whether **this session's** caller received a receipt for the
    /// transaction (never persisted; never claimed for prior
    /// sessions).
    pub fn acknowledged_this_session(&self, id: TransactionId) -> bool {
        self.acknowledged.contains(&id)
    }

    /// What `Accepted`-equivalent durability this platform's journal
    /// fsync establishes (the medium's honest tier boundary —
    /// Windows carries the process-crash tier, never presented as
    /// power-loss durability).
    pub fn sync_tier(&self) -> SyncTier {
        self.platform_tier
    }

    /// The number of fsync barriers performed (creation plus one per
    /// completed transaction or cancellation — the efficiency
    /// evidence for the one-fsync-per-transaction rule).
    pub fn sync_count(&self) -> u64 {
        self.sync_count
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    // ---------------------------------------------------------- internals

    /// Appends at the current end of file: the explicit seek keeps
    /// the full-write-access handle (required for `set_len` and the
    /// sync barrier on Windows) at append semantics — every write of
    /// this append-only format lands at the end under the single
    /// writer's sidecar lock.
    fn append_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(bytes)
    }

    fn sync(&mut self) -> Result<(), JournalError> {
        self.file
            .sync_all()
            .map(|()| {
                self.sync_count += 1;
            })
            .map_err(|e| JournalError::Io {
                op: "sync journal",
                path: self.path.display().to_string(),
                source: e,
            })
    }

    /// Truncates an unresolved inert tail to the last valid boundary
    /// (idempotent; committed history is never touched).
    fn resolve_tail(&mut self) -> Result<(), JournalError> {
        if self.state.tail_resolved {
            return Ok(());
        }
        self.file
            .set_len(self.state.append_offset)
            .map_err(|e| io_err("truncate inert tail", &self.path, e))?;
        self.state.tail_resolved = true;
        Ok(())
    }

    /// A pre-commit fault: an injected failure is a clean
    /// capture-phase abort (nothing actionable written, retry-safe).
    fn fault_pre(&self, point: JournalFaultPoint) -> Result<(), JournalError> {
        match &self.fault {
            Some(control) => control
                .check(point)
                .map_err(|source| JournalError::AbortedBeforeCommit { source }),
            None => Ok(()),
        }
    }

    /// A commit-phase fault: an injected failure is the ambiguous
    /// commit outcome (reconcile — never guess, never fabricate).
    fn fault_commit(
        &self,
        point: JournalFaultPoint,
        id: TransactionId,
    ) -> Result<(), JournalError> {
        match &self.fault {
            Some(control) => control
                .check(point)
                .map_err(|source| JournalError::AmbiguousCommit {
                    id,
                    stage: CommitStage::Sync,
                    source,
                }),
            None => Ok(()),
        }
    }
}

/// Opens and exclusively locks the journal's sidecar lock file (the
/// B2 `HEAD.lock` pattern): created once, never unlinked; the OS
/// releases ownership at handle close or process death — a crashed
/// writer never leaves a stale lock, and there is no application
/// lock lifecycle to corrupt. A same-process second open blocks here
/// until the first instance drops. The lock deliberately does NOT
/// cover the data file's bytes (see [`T1Journal::open_with`]).
fn open_lock_file(path: &Path) -> Result<File, JournalError> {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_else(|| std::ffi::OsString::from("journal"));
    name.push(".lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path.with_file_name(name))
        .map_err(|e| io_err("open journal lock", path, e))?;
    lock.lock().map_err(|e| io_err("lock journal", path, e))?;
    Ok(lock)
}

fn platform_tier() -> SyncTier {
    if cfg!(windows) {
        SyncTier::ProcessCrashOnly
    } else {
        SyncTier::FullPosixSequence
    }
}

fn io_err(op: &'static str, path: &Path, source: io::Error) -> JournalError {
    JournalError::Io {
        op,
        path: path.display().to_string(),
        source,
    }
}

/// The Unix parent-directory sync after creation (std exposes no
/// Windows directory flush — the tier carries the limitation).
#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consequence::{AwardCheckpoint, Coverage, OccurrenceSpan};
    use crate::envelope::{ClassifiedSpan, CounterValues, SignalLane, SourceBaseline};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("s7-j3-unit-{}-{label}-{seq}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create test root");
            TestDir(path)
        }

        fn journal(&self) -> PathBuf {
            self.0.join("journal.t1")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn tx_input(index: u64) -> TransactionInput {
        let identity = ConsequenceId::new(1, index, 1);
        let envelope = ConsequenceEnvelope::capture(
            identity,
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
        .unwrap();
        TransactionInput {
            envelopes: vec![envelope],
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

    /// The lane-conflict shape: the same identity, span,
    /// classification, plan, position, and baseline — the same
    /// content-blind `TransactionId` inputs — under a different
    /// valid uniform signal lane (+2 pending vs +2 captured).
    fn lane_input(lane: SignalLane) -> TransactionInput {
        let identity = ConsequenceId::new(1, 1, 1);
        let envelope = ConsequenceEnvelope::capture(
            identity,
            vec![ClassifiedSpan {
                span: OccurrenceSpan::new(0, 2).unwrap(),
                class: DurabilityClass::DurableEligible,
            }],
            lane,
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
        .unwrap();
        TransactionInput {
            envelopes: vec![envelope],
            position: SourcePosition {
                head: None,
                tick: 101,
            },
            classification: DurabilityClass::DurableEligible,
            dependencies: Vec::new(),
            frontier: vec![FrontierEntry {
                region: 1,
                episode: 1,
            }],
            plan: b"plan-lane-conflict".to_vec(),
        }
    }

    fn faulted(path: &Path, point: JournalFaultPoint) -> Result<T1Journal, JournalError> {
        T1Journal::open_with(
            path,
            Some(JournalFaultControl::new().arm(point, FaultAction::InjectError)),
        )
    }

    // ------------------------------------------------------------ basics

    #[test]
    fn open_creates_header_and_scans_clean() {
        let dir = TestDir::new("create");
        let path = dir.journal();
        let journal = T1Journal::open(&path).unwrap();
        assert!(path.exists());
        assert_eq!(journal.transactions().len(), 0);
        assert_eq!(journal.tail(), TailState::Clean);
        assert_eq!(journal.next_sequence(), 1);
        assert_eq!(journal.sync_count(), 1, "the creation barrier");
        let scan = T1Journal::scan_file(&path).unwrap();
        assert_eq!(scan.tail, TailState::Clean);
        assert!(scan.committed.is_empty());
    }

    #[test]
    fn append_and_rescan_round_trips_transactions() {
        let dir = TestDir::new("round-trip");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        let mut receipts = Vec::new();
        for index in 1..=3u64 {
            let receipt = journal.append_transaction(&tx_input(index)).unwrap();
            assert_eq!(receipt.sequence, index);
            receipts.push(receipt);
        }
        assert_eq!(journal.sync_count(), 1 + 3, "one fsync per transaction");

        let scan = T1Journal::scan_file(&path).unwrap();
        assert_eq!(scan.tail, TailState::Clean);
        assert_eq!(scan.committed.len(), 3);
        for (transaction, receipt) in scan.committed.iter().zip(&receipts) {
            assert_eq!(transaction.id, receipt.id);
            assert_eq!(transaction.sequence, receipt.sequence);
            assert_eq!(transaction.record_count, receipt.record_count);
            assert_eq!(transaction.digest, receipt.digest);
            assert_eq!(
                transaction.envelopes,
                tx_input(transaction.sequence).envelopes
            );
            assert_eq!(transaction.plan, tx_input(transaction.sequence).plan);
        }
        // Byte-identical re-scan (repeated recovery is stable).
        let again = T1Journal::scan_file(&path).unwrap();
        assert_eq!(again, scan);
    }

    #[test]
    fn acknowledged_is_session_scoped_never_fabricated() {
        let dir = TestDir::new("ack-session");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        let receipt = journal.append_transaction(&tx_input(1)).unwrap();
        assert_eq!(
            journal.transaction_status(receipt.id),
            Some(TransactionDurability::AcknowledgedT1)
        );
        assert!(journal.acknowledged_this_session(receipt.id));
        drop(journal);

        // A reload observes the committed frame — never a historical
        // acknowledgment its caller never observed.
        let reloaded = T1Journal::open(&path).unwrap();
        assert_eq!(
            reloaded.transaction_status(receipt.id),
            Some(TransactionDurability::Committed)
        );
        assert!(!reloaded.acknowledged_this_session(receipt.id));
        assert!(reloaded.is_actionable(receipt.id));
    }

    // ------------------------------------------------------ inert tails

    #[test]
    fn captures_without_commit_are_inert_readable_state() {
        let dir = TestDir::new("missing-commit");
        let path = dir.journal();
        let input = tx_input(1);
        let mut journal = faulted(&path, JournalFaultPoint::CapturesWritten).unwrap();
        let error = journal.append_transaction(&input).unwrap_err();
        assert!(matches!(error, JournalError::AbortedBeforeCommit { .. }));
        drop(journal);

        // The frames exist and parse — the Readable state, distinct
        // from Committed.
        let scan = T1Journal::scan_file(&path).unwrap();
        assert!(matches!(
            scan.tail,
            TailState::Inert {
                kind: TailKind::MissingCommit,
                ..
            }
        ));
        let id = input.transaction_id().unwrap();
        let identity = input.envelopes[0].id;
        let reloaded = T1Journal::open(&path).unwrap();
        assert_eq!(
            reloaded.identity_status(identity),
            Some(TransactionDurability::Readable)
        );
        assert!(!reloaded.is_committed(id));
        // The inert tail was resolved by the open-for-write; a retry
        // appends cleanly.
        let mut retry = reloaded;
        let receipt = retry.append_transaction(&input).unwrap();
        assert_eq!(receipt.id, id, "the retry preserves the identity");
        assert_eq!(retry.transactions().len(), 1);
        assert_eq!(
            retry.transaction_status(id),
            Some(TransactionDurability::AcknowledgedT1)
        );
    }

    #[test]
    fn torn_tail_is_inert_and_committed_history_survives() {
        let dir = TestDir::new("torn-tail");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        journal.append_transaction(&tx_input(1)).unwrap();
        journal.append_transaction(&tx_input(2)).unwrap();
        drop(journal);

        // A torn trailing frame after two complete transactions.
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&[0u8, 0, 0, 0, 0, 0, 0, 100]).unwrap();
        file.write_all(b"partial-bytes").unwrap();
        drop(file);

        let scan = T1Journal::scan_file(&path).unwrap();
        assert_eq!(scan.committed.len(), 2, "committed history survives");
        assert!(matches!(
            scan.tail,
            TailState::Inert {
                kind: TailKind::TornFrame,
                ..
            }
        ));

        // Open-for-write resolves the tail and appends cleanly.
        let mut journal = T1Journal::open(&path).unwrap();
        assert_eq!(journal.transactions().len(), 2);
        journal.append_transaction(&tx_input(3)).unwrap();
        let rescan = T1Journal::scan_file(&path).unwrap();
        assert_eq!(rescan.committed.len(), 3);
        assert_eq!(rescan.tail, TailState::Clean);
    }

    #[test]
    fn zero_filled_tail_is_the_documented_inert_artifact() {
        let dir = TestDir::new("zero-fill");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        journal.append_transaction(&tx_input(1)).unwrap();
        drop(journal);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&[0u8; 16]).unwrap();
        drop(file);

        let scan = T1Journal::scan_file(&path).unwrap();
        assert_eq!(scan.committed.len(), 1);
        assert!(matches!(
            scan.tail,
            TailState::Inert {
                kind: TailKind::Undecodable,
                ..
            }
        ));
        let mut journal = T1Journal::open(&path).unwrap();
        assert_eq!(journal.transactions().len(), 1);
        journal.append_transaction(&tx_input(2)).unwrap();
        assert_eq!(T1Journal::scan_file(&path).unwrap().committed.len(), 2);
    }

    #[test]
    fn truncated_committed_frame_is_a_torn_tail() {
        let dir = TestDir::new("truncated-frame");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        journal.append_transaction(&tx_input(1)).unwrap();
        journal.append_transaction(&tx_input(2)).unwrap();
        drop(journal);
        let full = fs::read(&path).unwrap();
        // Truncate into the second transaction's commit record — a
        // torn frame can never be actionable.
        let cut = full.len() - 5;
        fs::write(&path, &full[..cut]).unwrap();

        let scan = T1Journal::scan_file(&path).unwrap();
        assert_eq!(scan.committed.len(), 1, "the first transaction survives");
        assert!(matches!(
            scan.tail,
            TailState::Inert {
                kind: TailKind::TornFrame,
                ..
            }
        ));
    }

    // ------------------------------------------- committed corruption

    #[test]
    fn corrupted_committed_capture_fails_closed_with_evidence() {
        let dir = TestDir::new("corrupt-capture");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        journal.append_transaction(&tx_input(1)).unwrap();
        journal.append_transaction(&tx_input(2)).unwrap();
        drop(journal);
        let original = fs::read(&path).unwrap();

        // Flip a byte inside the first transaction's capture envelope.
        let mut corrupted = original.clone();
        corrupted[header_bytes().len() + 8 + 60] ^= 0xff;
        fs::write(&path, &corrupted).unwrap();

        let error = T1Journal::scan_file(&path).unwrap_err();
        assert!(
            matches!(error, JournalError::CorruptCommitted { .. }),
            "unexpected error: {error:?}"
        );
        // The open fails closed; the evidence is preserved
        // byte-for-byte.
        assert!(T1Journal::open(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), corrupted);
    }

    #[test]
    fn wrong_count_and_digest_fail_closed() {
        let dir = TestDir::new("wrong-count");
        let path = dir.journal();
        // Hand-build: one capture, then a commit claiming two.
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let header = header_bytes();
        file.write_all(&header).unwrap();
        let chain_prev = hash_object(&header);
        let input = tx_input(1);
        let envelope_bytes = input.envelopes[0].canonical_bytes().unwrap();
        let capture = encode_capture(chain_prev, &envelope_bytes);
        let capture_hash = hash_object(&capture);
        file.write_all(&frame(&capture)).unwrap();
        let ids = sorted_unique_identities(&input.envelopes).unwrap();
        let fields = CommitFields {
            id: derive_transaction_id(&ids, input.classification, &input.plan),
            sequence: 1,
            record_count: 2, // wrong: one capture exists
            digest: hash_object(&capture),
            identities: &ids,
            position: &input.position,
            classification: input.classification,
            dependencies: &[],
            frontier: &input.frontier,
            plan: &input.plan,
        };
        let commit = encode_commit(capture_hash, &fields);
        file.write_all(&frame(&commit)).unwrap();
        drop(file);

        let error = T1Journal::scan_file(&path).unwrap_err();
        assert!(
            matches!(
                error,
                JournalError::CorruptCommitted {
                    rule: "count-mismatch",
                    ..
                }
            ),
            "unexpected error: {error:?}"
        );

        // A wrong digest likewise (correct count this time).
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(&header).unwrap();
        file.write_all(&frame(&capture)).unwrap();
        let fields = CommitFields {
            id: derive_transaction_id(&ids, input.classification, &input.plan),
            sequence: 1,
            record_count: 1,
            digest: hash_object(b"not-the-captures"),
            identities: &ids,
            position: &input.position,
            classification: input.classification,
            dependencies: &[],
            frontier: &input.frontier,
            plan: &input.plan,
        };
        let commit = encode_commit(capture_hash, &fields);
        file.write_all(&frame(&commit)).unwrap();
        drop(file);
        let error = T1Journal::scan_file(&path).unwrap_err();
        assert!(
            matches!(
                error,
                JournalError::CorruptCommitted {
                    rule: "digest-mismatch",
                    ..
                }
            ),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn sequence_gap_fails_closed() {
        let dir = TestDir::new("sequence-gap");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        journal.append_transaction(&tx_input(1)).unwrap();
        drop(journal);

        // Hand-append a commit with sequence 3 (a gap at 2).
        let bytes = fs::read(&path).unwrap();
        let chain_prev = hash_object(&bytes[bytes.len() - 8 - commit_record_len(&bytes)..]);
        let input = tx_input(2);
        let envelope_bytes = input.envelopes[0].canonical_bytes().unwrap();
        let capture = encode_capture(chain_prev, &envelope_bytes);
        let capture_hash = hash_object(&capture);
        let ids = sorted_unique_identities(&input.envelopes).unwrap();
        let fields = CommitFields {
            id: derive_transaction_id(&ids, input.classification, &input.plan),
            sequence: 3,
            record_count: 1,
            digest: hash_object(&capture),
            identities: &ids,
            position: &input.position,
            classification: input.classification,
            dependencies: &[],
            frontier: &input.frontier,
            plan: &input.plan,
        };
        let commit = encode_commit(capture_hash, &fields);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&frame(&capture)).unwrap();
        file.write_all(&frame(&commit)).unwrap();
        drop(file);

        let error = T1Journal::scan_file(&path).unwrap_err();
        assert!(
            matches!(
                error,
                JournalError::CorruptCommitted {
                    rule: "sequence-gap",
                    ..
                }
            ),
            "unexpected error: {error:?}"
        );
    }

    /// The length of the last framed record in `bytes` (for chain
    /// continuation in hand-built fixtures).
    fn commit_record_len(bytes: &[u8]) -> usize {
        let mut offset = header_bytes().len();
        let mut last_record_len = 0;
        while offset < bytes.len() {
            let len = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            last_record_len = len as usize;
            offset += 8 + len as usize;
        }
        last_record_len
    }

    #[test]
    fn committed_region_tamper_fails_closed() {
        // (a) A flipped byte inside the first commit record's
        // identity field: the carried identity no longer matches the
        // derivation — fail closed, evidence preserved.
        let dir = TestDir::new("tamper-commit-id");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        journal.append_transaction(&tx_input(1)).unwrap();
        journal.append_transaction(&tx_input(2)).unwrap();
        drop(journal);
        let original = fs::read(&path).unwrap();
        let starts = frame_starts(&original);
        // Record layout: domain (46) + schema (1) + kind (1) = 48,
        // prev hash (8 + 32) = 48..88, identity length (8) =
        // 88..96, identity hash (32) = 96..128 — flip inside the
        // identity hash.
        let mut corrupted = original.clone();
        corrupted[starts[1] + 8 + 99] ^= 0xff;
        fs::write(&path, &corrupted).unwrap();
        let error = T1Journal::scan_file(&path).unwrap_err();
        assert!(
            matches!(
                error,
                JournalError::CorruptCommitted {
                    rule: "identity-derivation",
                    ..
                }
            ),
            "unexpected error: {error:?}"
        );
        assert_eq!(fs::read(&path).unwrap(), corrupted, "evidence preserved");

        // (b) A garbage frame inserted BETWEEN two committed
        // transactions: the second transaction still validates (its
        // own count, digest, and sequence are intact), so the damage
        // demonstrably lies within committed history — the walk
        // continues past damaged frames exactly to find the later
        // anchor.
        let dir = TestDir::new("inserted-garbage");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        journal.append_transaction(&tx_input(1)).unwrap();
        journal.append_transaction(&tx_input(2)).unwrap();
        drop(journal);
        let original = fs::read(&path).unwrap();
        let starts = frame_starts(&original);
        let insert_at = starts[2]; // the second transaction's first capture
        let mut spliced = Vec::new();
        spliced.extend_from_slice(&original[..insert_at]);
        spliced.extend_from_slice(&frame(b"garbage record"));
        spliced.extend_from_slice(&original[insert_at..]);
        fs::write(&path, &spliced).unwrap();
        let error = T1Journal::scan_file(&path).unwrap_err();
        assert!(
            matches!(
                error,
                JournalError::CorruptCommitted {
                    rule: "damage-within-committed-history",
                    ..
                }
            ),
            "unexpected error: {error:?}"
        );
    }

    /// The byte offsets of every framed record in `bytes`.
    fn frame_starts(bytes: &[u8]) -> Vec<usize> {
        let mut starts = Vec::new();
        let mut offset = header_bytes().len();
        while offset + 8 <= bytes.len() {
            let len =
                u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("8 bytes")) as usize;
            starts.push(offset);
            offset += 8 + len;
        }
        starts
    }

    #[test]
    fn corrupted_tail_after_last_commit_is_inert() {
        let dir = TestDir::new("corrupt-tail");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        journal.append_transaction(&tx_input(1)).unwrap();
        journal.append_transaction(&tx_input(2)).unwrap();
        drop(journal);
        let original = fs::read(&path).unwrap();
        // Garbage after the last commit: the trailing region, inert —
        // never a fail-closed reinterpretation of committed history.
        let mut corrupted = original.clone();
        corrupted.extend_from_slice(b"trailing garbage");
        fs::write(&path, &corrupted).unwrap();

        let scan = T1Journal::scan_file(&path).unwrap();
        assert_eq!(scan.committed.len(), 2);
        assert!(matches!(scan.tail, TailState::Inert { .. }));
        let mut journal = T1Journal::open(&path).unwrap();
        assert_eq!(journal.transactions().len(), 2);
        journal.append_transaction(&tx_input(3)).unwrap();
        assert_eq!(T1Journal::scan_file(&path).unwrap().committed.len(), 3);
    }

    // ------------------------------------------------- ambiguity + retry

    #[test]
    fn ambiguous_sync_reconciles_committed_without_fabricated_ack() {
        let dir = TestDir::new("ambiguous-sync");
        let path = dir.journal();
        let input = tx_input(1);
        let mut journal = faulted(&path, JournalFaultPoint::CommitWritten).unwrap();
        // The commit frame is fully written; the injected failure
        // fires before the sync acknowledgment returns.
        let error = journal.append_transaction(&input).unwrap_err();
        assert!(matches!(
            error,
            JournalError::AmbiguousCommit {
                stage: CommitStage::Sync,
                ..
            }
        ));
        let id = input.transaction_id().unwrap();

        // Reconciliation rereads and verifies: the frame is present
        // and valid — committed and actionable, with NO fabricated
        // acknowledgment.
        let outcome = journal.reconcile_write(&input).unwrap();
        let WriteReconciliation::Committed { receipt } = outcome else {
            panic!("expected Committed, got {outcome:?}");
        };
        assert_eq!(receipt.id, id);
        assert_eq!(
            journal.transaction_status(id),
            Some(TransactionDurability::Committed)
        );
        assert!(!journal.acknowledged_this_session(id));
        assert!(journal.is_actionable(id));
    }

    #[test]
    fn ambiguous_abort_retries_idempotently_without_duplicates() {
        let dir = TestDir::new("abort-retry");
        let path = dir.journal();
        let input = tx_input(1);
        let mut journal = faulted(&path, JournalFaultPoint::CapturesWritten).unwrap();
        let error = journal.append_transaction(&input).unwrap_err();
        assert!(matches!(error, JournalError::AbortedBeforeCommit { .. }));

        // Reconciliation: no valid commit frame — retry-safe.
        assert_eq!(
            journal.reconcile_write(&input).unwrap(),
            WriteReconciliation::NotCommitted
        );
        drop(journal);
        // The retry (a fresh session — the fault does not re-arm)
        // preserves the identity and commits exactly once.
        let mut journal = T1Journal::open(&path).unwrap();
        let receipt = journal.append_transaction(&input).unwrap();
        assert_eq!(receipt.id, input.transaction_id().unwrap());
        assert_eq!(journal.transactions().len(), 1);

        // A duplicate append of the same input is recognized
        // idempotently — no second frame, no second fsync.
        let file_len = fs::metadata(&path).unwrap().len();
        let syncs = journal.sync_count();
        let recognized = journal.append_transaction(&input).unwrap();
        assert_eq!(recognized.id, receipt.id);
        assert_eq!(recognized.sequence, receipt.sequence);
        assert_eq!(fs::metadata(&path).unwrap().len(), file_len);
        assert_eq!(journal.sync_count(), syncs);
        assert_eq!(journal.transactions().len(), 1);
    }

    #[test]
    fn same_id_different_lane_conflicting_payload_fails_closed() {
        // The third re-review's finding: the identity-level
        // `TransactionId` is content-blind — two valid envelopes
        // with the same identity, span, classification, and plan
        // share it while their uniform signal lanes differ (+2
        // pending vs +2 captured). A retry under the same identity
        // with a DIFFERENT consequential payload is a conflicting
        // claim, never an idempotent duplicate: it fails closed and
        // never returns the existing durable receipt.
        let dir = TestDir::new("lane-conflict");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        let receipt = journal
            .append_transaction(&lane_input(SignalLane::Pending))
            .unwrap();
        let id = receipt.id;
        // The same TransactionId and occurrence coverage, a
        // different valid lane: a conflicting payload.
        let error = journal
            .append_transaction(&lane_input(SignalLane::Captured))
            .unwrap_err();
        assert!(
            matches!(error, JournalError::ConflictingPayload { id: conflict } if conflict == id),
            "the lane conflict fails closed: {error}"
        );
        // Never a matching durable receipt — the original stands,
        // exactly one frame.
        assert_eq!(journal.transactions().len(), 1);
        assert!(journal.is_actionable(id));
        // Ambiguous-write reconciliation against the conflicting
        // payload never classifies as the attempted committed
        // transaction.
        assert_eq!(
            journal
                .reconcile_write(&lane_input(SignalLane::Captured))
                .unwrap(),
            WriteReconciliation::ConflictingPayload { id }
        );
        // The identical retry remains idempotently recognized — no
        // second frame, no second fsync.
        let file_len = fs::metadata(&path).unwrap().len();
        let syncs = journal.sync_count();
        let recognized = journal
            .append_transaction(&lane_input(SignalLane::Pending))
            .unwrap();
        assert_eq!(recognized.id, id);
        assert_eq!(recognized.sequence, receipt.sequence);
        assert_eq!(fs::metadata(&path).unwrap().len(), file_len);
        assert_eq!(journal.sync_count(), syncs);
        assert_eq!(journal.transactions().len(), 1);
    }

    #[test]
    fn repeated_recovery_cycles_are_stable() {
        let dir = TestDir::new("repeat-recovery");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        for index in 1..=3u64 {
            journal.append_transaction(&tx_input(index)).unwrap();
        }
        drop(journal);
        let mut reference = T1Journal::scan_file(&path).unwrap();
        for _ in 0..3 {
            let scan = T1Journal::scan_file(&path).unwrap();
            assert_eq!(scan, reference);
            let reopened = T1Journal::open(&path).unwrap();
            assert_eq!(reopened.transactions(), reference.committed);
            assert_eq!(reopened.next_sequence(), 4);
            reference = scan;
        }
    }

    // ----------------------------------------------------- cancellation

    #[test]
    fn cancellation_suppresses_only_its_target() {
        let dir = TestDir::new("cancel");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        let first = journal.append_transaction(&tx_input(1)).unwrap();
        let second = journal.append_transaction(&tx_input(2)).unwrap();
        let third = journal.append_transaction(&tx_input(3)).unwrap();

        // An unrelated later sequence never supersedes the earlier
        // durable transactions.
        assert!(journal.is_actionable(first.id));
        assert!(journal.is_actionable(second.id));

        let sequence = journal
            .append_cancellation(
                second.id,
                CancellationReason::ApplicationPolicy,
                b"policy decision",
            )
            .unwrap();
        assert_eq!(sequence, 4);
        assert!(
            !journal.is_actionable(second.id),
            "only the target is suppressed"
        );
        assert!(journal.is_actionable(first.id));
        assert!(journal.is_actionable(third.id));

        let scan = T1Journal::scan_file(&path).unwrap();
        assert_eq!(scan.cancellations.len(), 1);
        assert_eq!(scan.cancellations[0].target, second.id);
        assert_eq!(
            scan.cancellations[0].reason,
            CancellationReason::ApplicationPolicy
        );
        assert_eq!(scan.committed.len(), 3);
        assert_eq!(journal.sync_count(), 1 + 3 + 1, "one fsync per record");
    }

    #[test]
    fn cancellation_validation_is_pre_write() {
        let dir = TestDir::new("cancel-validate");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        let receipt = journal.append_transaction(&tx_input(1)).unwrap();
        let file_len = fs::metadata(&path).unwrap().len();

        let unknown = TransactionId(hash_object(b"never-committed"));
        assert!(matches!(
            journal.append_cancellation(unknown, CancellationReason::ApplicationPolicy, b""),
            Err(JournalError::UnknownTarget { .. })
        ));
        journal
            .append_cancellation(receipt.id, CancellationReason::GovernedResolution, b"")
            .unwrap();
        assert!(matches!(
            journal.append_cancellation(receipt.id, CancellationReason::ApplicationPolicy, b""),
            Err(JournalError::AlreadyCanceled { .. })
        ));
        assert!(fs::metadata(&path).unwrap().len() > file_len);
    }

    // ------------------------------------------------------- boundaries

    #[test]
    fn torn_creation_header_reinitializes() {
        let dir = TestDir::new("torn-header");
        let path = dir.journal();
        let header = header_bytes();
        // A 10-byte prefix of the canonical header: a crashed
        // creation — nothing could ever have been journaled.
        fs::write(&path, &header[..10]).unwrap();
        let scan = T1Journal::scan_file(&path).unwrap();
        assert!(matches!(
            scan.tail,
            TailState::Inert {
                kind: TailKind::TornCreation,
                byte_len: 10,
                ..
            }
        ));
        let mut journal = T1Journal::open(&path).unwrap();
        assert_eq!(journal.transactions().len(), 0);
        journal.append_transaction(&tx_input(1)).unwrap();
        assert_eq!(T1Journal::scan_file(&path).unwrap().committed.len(), 1);

        // Garbage shorter than the header is not a prefix: fail
        // closed.
        let other = dir.0.join("garbage-header.t1");
        fs::write(&other, b"not-a-header").unwrap();
        assert!(matches!(
            T1Journal::open(&other),
            Err(JournalError::CorruptHeader { .. })
        ));
    }

    #[test]
    fn oversized_inputs_rejected_before_any_write() {
        let dir = TestDir::new("oversized");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        let file_len = fs::metadata(&path).unwrap().len();
        let syncs = journal.sync_count();

        let mut oversized = tx_input(1);
        oversized.plan = vec![0u8; (MAX_JOURNAL_PLAN_BYTES + 1) as usize];
        assert!(matches!(
            journal.append_transaction(&oversized),
            Err(JournalError::Format(SaveError::Limit { .. }))
        ));
        assert_eq!(fs::metadata(&path).unwrap().len(), file_len);
        assert_eq!(journal.sync_count(), syncs);

        // Duplicate identities inside one transaction are a
        // conflicting reuse — fail closed, nothing written.
        let mut duplicated = tx_input(2);
        duplicated.envelopes.push(duplicated.envelopes[0].clone());
        assert!(matches!(
            journal.append_transaction(&duplicated),
            Err(JournalError::Format(SaveError::Integrity(
                "journal/duplicate-identity-in-transaction"
            )))
        ));
        assert_eq!(fs::metadata(&path).unwrap().len(), file_len);

        // A frontier naming one region twice is incoherent.
        let mut bad_frontier = tx_input(3);
        bad_frontier.frontier = vec![
            FrontierEntry {
                region: 2,
                episode: 5,
            },
            FrontierEntry {
                region: 2,
                episode: 9,
            },
        ];
        assert!(matches!(
            journal.append_transaction(&bad_frontier),
            Err(JournalError::Format(SaveError::Integrity(
                "journal/frontier-region-order"
            )))
        ));
        assert_eq!(fs::metadata(&path).unwrap().len(), file_len);
    }

    #[test]
    fn dependencies_and_frontier_round_trip() {
        let dir = TestDir::new("deps-frontier");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        let first = journal.append_transaction(&tx_input(1)).unwrap();
        let mut input = tx_input(2);
        input.dependencies = vec![TransactionId(hash_object(b"unrelated")), first.id];
        input.frontier = vec![
            FrontierEntry {
                region: 1,
                episode: 2,
            },
            FrontierEntry {
                region: 3,
                episode: 7,
            },
        ];
        let receipt = journal.append_transaction(&input).unwrap();
        drop(journal);

        let scan = T1Journal::scan_file(&path).unwrap();
        let transaction = scan
            .committed
            .iter()
            .find(|transaction| transaction.id == receipt.id)
            .unwrap();
        let mut expected_dependencies = input.dependencies.clone();
        expected_dependencies.sort_unstable();
        assert_eq!(transaction.dependencies, expected_dependencies);
        assert_eq!(transaction.frontier, input.frontier);
        // The position binding and classification round-trip too.
        assert_eq!(transaction.position, input.position);
        assert_eq!(transaction.classification, input.classification);
    }

    #[test]
    fn multi_envelope_transactions_digest_the_full_set() {
        let dir = TestDir::new("multi-envelope");
        let path = dir.journal();
        let mut journal = T1Journal::open(&path).unwrap();
        let mut input = tx_input(1);
        let second = ConsequenceEnvelope::capture(
            ConsequenceId::new(1, 1, 2),
            vec![ClassifiedSpan {
                span: OccurrenceSpan::new(0, 1).unwrap(),
                class: DurabilityClass::EncounterProvisional,
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
                tick: 101,
            },
        )
        .unwrap();
        input.envelopes.push(second);
        let receipt = journal.append_transaction(&input).unwrap();
        assert_eq!(receipt.record_count, 2);
        drop(journal);

        let scan = T1Journal::scan_file(&path).unwrap();
        assert_eq!(scan.committed.len(), 1);
        assert_eq!(scan.committed[0].envelopes, input.envelopes);
        assert_eq!(scan.committed[0].record_count, 2);
        assert_eq!(scan.committed[0].receipt(), receipt);
    }
}
