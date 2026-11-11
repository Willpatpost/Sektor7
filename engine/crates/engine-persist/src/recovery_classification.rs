//! (V2X-CP3-B-R1a1) The retained-evidence recovery-state classifier
//! — a reader-generic, head-relative, read-only classification of
//! the recovery evidence retained above the covering checkpoint,
//! consuming the accepted B-R1a0 discovery observations. The
//! classifier validates the COHERENCE of the retained evidence;
//! its positive output establishes **consistency of retained
//! evidence**, never globally authenticated retirement.
//!
//! **The observation-seed discipline.** The caller-supplied HEAD
//! remains an observation seed: no provider-coordinated
//! canonical-head claim is permitted or made. Every result
//! carries the explicit [`UnresolvedAuthority`] statement — the
//! provider canonical head, the founding-derived seed, the
//! original range digest, the original object closures, physical
//! retirement, journal retirement, provider durability, older
//! checkpoints, completed-Range authority, and startup safety are
//! each NOT established, and the type system offers no value that
//! could claim otherwise. "Not observed" always means **not
//! observed on the inspected head-relative segment**, never
//! "absent from the physical store".
//!
//! **The mandatory authentication sequence** (each stage consumes
//! only earlier stages and pure checks over `BeginReads` reads of
//! evidence ABOVE the covering checkpoint):
//! - **L0** — the accepted [`discover_recovery_evidence`] result,
//!   unchanged: every discovery error and the unresolved boundary
//!   preserved.
//! - **L2** — the covering checkpoint: the MK's regular shape,
//!   single metadata reference, `parent == metadata.sk` (all
//!   discovery-enforced), plus the exact deterministic checkpoint
//!   key, a nonempty indexed retirement membership, and the unique
//!   indexed identities/epochs (the accepted
//!   [`validate_index_shape`]). The MK's presence on the
//!   caller-supplied head-relative path is not independent
//!   provider canonicality.
//! - **L3** — the plan: the exact MK-bound key, the decoded
//!   bindings (MK/SK/range digest/seed), the ledger byte limit,
//!   and the flattened plan membership compared position-by-
//!   position against the index-derived newest-first order —
//!   rejecting omissions, injections, duplicates, and
//!   reorderings. Retained-evidence consistency, not historical
//!   correctness.
//! - **L4** — the begin: the exact key, the strict payload, the
//!   source equal to the observed plan RECORD identity, the
//!   digest/seed agreement, and the strict chronological ordering
//!   (the plan and every required audit precede the begin on the
//!   observed ancestry). No provider durability is reported. The
//!   existing begin-authority verifier (whose founding descent
//!   may cross a partially reclaimed range) is NEVER invoked.
//! - **L5** — every plan-defined batch's observed audit: the
//!   exact key, the strict `ReclamationLedgerV2` decode, the
//!   MK/SK/batch/digest/seed bindings, the exact ordered
//!   unit-record membership, the admitted plan ledger byte limit,
//!   and the recomputed `reclaimed_keys` from the retained
//!   checkpoint index's keyed-member information (the accepted
//!   retained key-count invariant). An audit whose original
//!   object closure cannot presently be rederived is never
//!   represented as fully historically authenticated.
//! - **L6** — the V2 `/2` markers: the exact MK and batch key,
//!   valid plan membership, the C2 structural reader's outcome
//!   preserved, contextual plan/audit/begin consistency required
//!   before counting a structurally matched marker. Markers not
//!   observed above the MK are distinguished from markers observed
//!   but contradicted (the latter degrade the evidence). No
//!   completion authority is granted from a structural match. An
//!   off-chain marker absent from the observed ancestry cannot
//!   count as present.
//! - **L7** — the witness evidence, classified SEPARATELY (the
//!   orthogonal [`WitnessEvidenceObservation`]): none observed;
//!   manifest absent with shard evidence observed; manifest and
//!   shard structures coherently observed (bindings, count
//!   coherence, deterministic keys, strict decodes); or degraded.
//!   The full A1-3 `Verified` contract is NEVER claimed — only
//!   individually decoded structures, never authenticated witness
//!   coverage.
//!
//! **Noncircularity (mandatory).** This classifier never invokes
//! `verify_v2_begin_authority_evidence_via`,
//! `verify_v2_execution_readiness_via`,
//! `verify_installed_ancestry_witness`, `account_batch_retirement`,
//! or any gap-aware HEAD-to-founding authority descent — those
//! routines may rely on range-crossing behavior that is not yet
//! safe after partial retirement. The founding identity is never
//! inferred from a missing parent; no unauthenticated whole-range
//! checkpoint self-skip is ever used; nothing below the covering
//! checkpoint is ever read.
//!
//! **Bounds.** The traversal ceiling is the accepted discovery's
//! (`MAX_DISCOVERY_RECORDS`). Lookups use bounded maps of the
//! discovered record identities and keys built once (no
//! O(batches × ancestry) rescans; the index lookup map is also
//! built once). Decoded aggregate payloads are retained one at a
//! time (a single audit ≤ the plan's ledger byte limit; a single
//! witness shard ≤ the accepted payload ceiling) and dropped
//! after their checks. Worst case: O(A + W) object reads beyond
//! the discovery's own (A = observed audits, W = observed witness
//! structures); memory O(R) plus one payload at a time (R =
//! records consulted).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::ancestry_witness::{
    AncestryWitnessManifest, AncestryWitnessShard, RECLAMATION_WITNESS_SHARD_KEY_DOMAIN,
    reclamation_witness_manifest_idempotency_key, reclamation_witness_shard_idempotency_key,
    validate_index_shape,
};
use crate::checkpoint::{CheckpointMetadata, ReclaimedIndexEntry, checkpoint_mk_idempotency_key};
use crate::hash::Hash;
use crate::reclaim::{
    BeginReads, MAX_RECLAMATION_LEDGER_BYTES, ReclaimPlan, ReclamationBegin, ReclamationLedgerV2,
    reclamation_begin_idempotency_key, reclamation_ledger_idempotency_key,
    reclamation_ledger_v2_idempotency_key, reclamation_plan_idempotency_key,
};
use crate::recovery_discovery::{
    CompletionStructural, CoveringCheckpoint, CoveringObservation, DiscoveredFamily,
    DiscoveredPayload, DiscoveredRecord, DiscoveryError, DiscoveryLimits,
    RecoveryEvidenceDiscovery, discover_recovery_evidence,
};

// ============================================================= the errors

/// The classifier's error channel: the L0 discovery failures
/// propagated unchanged (every evidence-level incoherence is an
/// OBSERVATION carried in the result's states, never an error).
#[derive(Debug)]
pub enum ClassificationError {
    /// The discovery walk refused (the typed traversal failure,
    /// preserved exactly).
    Discovery(DiscoveryError),
}

impl fmt::Display for ClassificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClassificationError::Discovery(error) => {
                write!(f, "recovery-classification/discovery: {error}")
            }
        }
    }
}

impl std::error::Error for ClassificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClassificationError::Discovery(error) => Some(error),
        }
    }
}

// ================================================ the unresolved authority

/// The marker for every authority requirement this classifier
/// deliberately does NOT establish. The field names on
/// [`UnresolvedAuthority`] enumerate them; no value exists that
/// could claim the positive.
#[derive(Clone, Copy)]
pub struct NotEstablished;

impl fmt::Debug for NotEstablished {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NotEstablished")
    }
}

/// The older-checkpoint status — the only unresolved-authority
/// item with more than one accurate value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OlderRangeStatus {
    /// A covering checkpoint was identified and classified; the
    /// state below its boundary (the range and every older
    /// checkpoint) was never inspected and remains unresolved —
    /// never a startup permission.
    UnresolvedBelowCoveringBoundary,
    /// The inspected ancestry reached the founding record with no
    /// covering checkpoint: no retirement obligation is invented
    /// for it.
    NoCoveringCheckpointObserved,
}

/// The explicit unresolved-authority statement carried by EVERY
/// classification result. Each field names one limitation; the
/// values cannot claim the positive.
#[derive(Debug, Clone)]
pub struct UnresolvedAuthority {
    /// The supplied HEAD's provider-coordinated canonical identity
    /// — not established (the head is an observation seed).
    pub provider_canonical_head: NotEstablished,
    /// The founding-derived seed — not independently rederived
    /// (the retained evidence's seed consistency was checked; the
    /// founding was never crossed).
    pub founding_derived_seed: NotEstablished,
    /// The original range digest — not independently reverified
    /// against original bytes.
    pub original_range_digest: NotEstablished,
    /// The original object closures — not fully reauthenticated.
    pub original_object_closures: NotEstablished,
    /// Physical retirement — not established.
    pub physical_retirement: NotEstablished,
    /// Journal retirement — not established.
    pub journal_retirement: NotEstablished,
    /// Provider durability (the begin's durability grade) — not
    /// established and not reported.
    pub provider_durability: NotEstablished,
    /// The older-checkpoint state (the one item with a variable
    /// accurate value).
    pub older_checkpoints: OlderRangeStatus,
    /// Completed-Range authority — not established (a structural
    /// marker match is never a completion receipt).
    pub completed_range_authority: NotEstablished,
    /// A startup-safe verdict — not established and not offered.
    pub startup_safety: NotEstablished,
}

// ================================================== the observed evidence

/// The plan observation: the record identity and the strictly
/// decoded payload that passed the L3 retained-context checks
/// (the exact key, the four-field binding to the covering
/// metadata, the valid ledger byte limit, and the
/// position-by-position flattened membership against the
/// index-derived newest-first order). What this establishes:
/// retained-evidence consistency — not the physical presence of
/// the original records and not historical correctness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedPlan {
    /// The plan revision's identity.
    pub record: Hash,
    /// The strictly decoded plan.
    pub plan: ReclaimPlan,
}

/// The begin observation: the record identity and the strictly
/// decoded payload that passed the L4 retained-context checks
/// (the exact key, the source equal to the observed plan RECORD
/// identity, the digest/seed agreement, and the strict
/// chronological ordering with the plan and every required audit
/// older than the begin on the observed ancestry). What this
/// establishes: retained-evidence consistency — never provider
/// durability and never execution status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedBegin {
    /// The begin revision's identity.
    pub record: Hash,
    /// The strictly decoded begin.
    pub begin: ReclamationBegin,
}

/// The V1 begin observation (the source bound to the observed V1
/// audit revision).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedV1Begin {
    /// The begin revision's identity.
    pub record: Hash,
    /// The strictly decoded begin.
    pub begin: ReclamationBegin,
}

// ========================================================= the states

/// The retained-evidence recovery-state ladder — OBSERVATION
/// terminology throughout: each state names what was OBSERVED and
/// which retained-context checks SUCCEEDED, never what was
/// proven about execution, installation canonicity, durability,
/// or historical retirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryState {
    /// The inspected ancestry reached the founding record with no
    /// covering checkpoint observed. No retirement obligation is
    /// invented.
    NoCoveringCheckpoint,
    /// A covering checkpoint was observed; no reclamation
    /// evidence (no plan, no V1 audit) was observed above it.
    NoRetirementEvidenceObserved {
        /// The covering checkpoint (the L2 checks passed).
        covering: Box<CoveringCheckpoint>,
    },
    /// A plan was observed and passed the L3 checks; no begin was
    /// observed above the covering checkpoint. The audit lists
    /// are per plan batch — "not observed" means not observed on
    /// the inspected head-relative segment. No claim is made
    /// about whether physical retirement ever happened.
    PlanObservedWithoutBegin {
        /// The covering checkpoint.
        covering: Box<CoveringCheckpoint>,
        /// The L3-consistent plan.
        plan: Box<ObservedPlan>,
        /// The plan batches with an observed audit record.
        audits_observed: Vec<u64>,
        /// The plan batches with no audit record observed.
        audits_not_observed: Vec<u64>,
    },
    /// V1-form evidence was observed (the V1 audit and, when
    /// present, the V1 begin bound to it). The existing V1
    /// verifiers own everything further; no V2 obligation is
    /// invented.
    V1EvidenceObserved {
        /// The covering checkpoint.
        covering: Box<CoveringCheckpoint>,
        /// The V1 audit revision's identity.
        v1_audit: Hash,
        /// The V1 begin, when observed (the source bound to the
        /// observed V1 audit).
        begin: Option<ObservedV1Begin>,
    },
    /// A V2 begin was observed and passed the L4 checks, with
    /// every plan batch's audit observed and L5-consistent; no
    /// V2 `/2` marker was observed (on the inspected segment).
    V2BeginObserved {
        /// The covering checkpoint.
        covering: Box<CoveringCheckpoint>,
        /// The L3-consistent plan.
        plan: Box<ObservedPlan>,
        /// The L4-consistent begin.
        begin: Box<ObservedBegin>,
        /// The plan batches with no marker record observed (on the
        /// inspected head-relative segment).
        markers_not_observed: Vec<u64>,
    },
    /// A V2 begin was observed (L4-consistent, all audits
    /// L5-consistent) and SOME but not all plan batches have
    /// structurally matched `/2` markers (the exact MK/batch key,
    /// valid plan membership, the C2 structural reader's positive
    /// result, and the contextual plan/audit/begin consistency).
    /// A structural match is NOT a durable completion receipt and
    /// NOT historical retirement proof.
    SomeBatchMarkersObserved {
        /// The covering checkpoint.
        covering: Box<CoveringCheckpoint>,
        /// The L3-consistent plan.
        plan: Box<ObservedPlan>,
        /// The L4-consistent begin.
        begin: Box<ObservedBegin>,
        /// The plan batches with a structurally matched marker.
        markers_structurally_matched: Vec<u64>,
        /// The plan batches with no marker record observed.
        markers_not_observed: Vec<u64>,
    },
    /// A V2 begin was observed (L4-consistent, all audits
    /// L5-consistent) and EVERY plan batch has a structurally
    /// matched `/2` marker. This is the structural matching of
    /// every batch marker — NOT the authentication of every
    /// batch's historical retirement, NOT completed-Range
    /// authority, and NOT a startup-safe verdict.
    AllBatchMarkersStructurallyMatched {
        /// The covering checkpoint.
        covering: Box<CoveringCheckpoint>,
        /// The L3-consistent plan.
        plan: Box<ObservedPlan>,
        /// The L4-consistent begin.
        begin: Box<ObservedBegin>,
        /// The plan batches with a structurally matched marker
        /// (all of them).
        markers_structurally_matched: Vec<u64>,
    },
    /// The retained evidence is corrupt, contradictory, or
    /// unprovable — the typed reason names the exact obstruction
    /// and the position where the retained-context checks
    /// diverged.
    EvidenceDegraded {
        /// The typed degradation reason.
        reason: EvidenceDegradation,
    },
}

/// The typed degradation reasons — every incoherence the
/// retained-context checks can find above the covering
/// checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceDegradation {
    /// The covering MK does not carry the exact deterministic
    /// checkpoint key.
    CheckpointKeyMismatch { record: Hash },
    /// The covering checkpoint's indexed retirement membership is
    /// empty.
    EmptyRetirementIndex { record: Hash },
    /// The indexed identities or epochs are not unique (the
    /// accepted index-shape rule).
    IndexShapeInvalid { record: Hash },
    /// The plan payload refused strict decode.
    MalformedPlanPayload { record: Hash, reason: String },
    /// The plan's decoded bindings disagree with the covering
    /// metadata (the rule names the field).
    PlanBindingMismatch { record: Hash, rule: &'static str },
    /// The plan's ledger byte limit exceeds the accepted ceiling.
    PlanLedgerByteLimitInvalid { record: Hash, limit: u64 },
    /// The flattened plan membership diverges from the
    /// index-derived newest-first order at the carried position
    /// (an omission, injection, duplicate, or reordering).
    PlanMembershipMismatch { record: Hash, position: usize },
    /// The begin payload refused strict decode.
    MalformedBeginPayload { record: Hash, reason: String },
    /// The begin's decoded bindings disagree (the rule names the
    /// field; the source must equal the observed plan RECORD
    /// identity).
    BeginBindingMismatch { record: Hash, rule: &'static str },
    /// The strict chronological ordering failed: the plan or a
    /// required audit does not precede the begin on the observed
    /// ancestry.
    BeginTemporalViolation { record: Hash, rule: &'static str },
    /// A plan batch's audit record was not observed although a
    /// begin was (the all-audits-first retained invariant).
    RequiredAuditNotObserved { batch: u64 },
    /// The audit payload could not be read.
    AuditEvidenceUnreadable { batch: u64, detail: String },
    /// The audit payload refused strict decode.
    MalformedAuditPayload { batch: u64, reason: String },
    /// The audit's decoded bindings disagree (the rule names the
    /// field).
    AuditBindingMismatch { batch: u64, rule: &'static str },
    /// The audit's encoded size exceeds the admitted plan ledger
    /// byte limit.
    AuditByteLimitExceeded {
        batch: u64,
        observed: u64,
        limit: u64,
    },
    /// The audit's ordered unit-record membership diverges from
    /// the plan's batch at the carried position.
    AuditUnitsMismatch { batch: u64, position: usize },
    /// The audit's `reclaimed_keys` disagrees with the recomputed
    /// keyed-member count from the retained checkpoint index.
    AuditKeyedCountMismatch {
        batch: u64,
        observed: u64,
        expected: u64,
    },
    /// A marker record was observed but contradicted (the typed
    /// rule carried; a batch of `None` means the key did not bind
    /// to the covering checkpoint).
    MarkerContradicted {
        record: Hash,
        batch: Option<u64>,
        rule: String,
    },
    /// A marker's batch index is outside the plan's membership.
    MarkerOutsidePlanMembership { record: Hash, batch: u64 },
    /// A begin was observed without any plan or V1 audit to bind
    /// its source to.
    BeginSourceNotObserved { record: Hash },
    /// V1 and V2 reclamation evidence were both observed above
    /// the same covering checkpoint (the accepted mutual
    /// exclusion).
    MixedV1AndV2Evidence { v1_record: Hash, v2_record: Hash },
    /// A witness manifest was observed without the plan and begin
    /// it must bind to (the publication order makes an orphaned
    /// manifest incoherent retained evidence).
    OrphanedWitnessManifest { record: Hash },
    /// A recognized reclamation-family record whose DECODED
    /// payload binds the covering MK was observed carrying an
    /// incorrect deterministic key — same-checkpoint evidence
    /// with a wrong key is a contradiction, never an absence (the
    /// family names the expected key's family).
    SameCheckpointWrongKeyEvidence { record: Hash, family: &'static str },
}

// ==================================================== the witness evidence

/// The witness-evidence observation — classified SEPARATELY from
/// the retirement-evidence states (a degraded witness blocks the
/// FUTURE traversal without contradicting the receipt facts, and
/// a coherent witness observation enables nothing by itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WitnessEvidenceObservation {
    /// No witness manifest and no shard evidence observed above
    /// the covering checkpoint.
    NoneObserved,
    /// No manifest observed, but shard evidence observed (the
    /// parsed deterministic shard indices carried — the
    /// manifestless prefix shape; no completeness claim).
    ManifestAbsentWithShardEvidence { shard_indices: Vec<u64> },
    /// The manifest and shard structures were coherently
    /// observed: the manifest decoded and bound (the plan/begin/
    /// metadata bindings, the member-count coherence), the shard
    /// set exactly complete and consecutive, and every shard
    /// strictly decoded with its bindings. This is the
    /// observational grade — individually decoded structures,
    /// NOT the accepted A1-3 `Verified` contract (no member
    /// preimage authentication, no coverage-digest verification,
    /// no installed-family completeness).
    CoherentlyObserved {
        /// The manifest record's identity.
        record: Hash,
        /// The strictly decoded, bound manifest.
        manifest: Box<AncestryWitnessManifest>,
        /// The observed shard records in index order.
        shard_records: Vec<Hash>,
    },
    /// The witness evidence is malformed, contradictory, or
    /// incomplete (the typed reason carried).
    Degraded { reason: WitnessDegradation },
}

/// The typed witness degradation reasons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WitnessDegradation {
    /// The manifest payload could not be read.
    ManifestUnreadable { detail: String },
    /// The manifest payload refused strict decode.
    MalformedManifestPayload { reason: String },
    /// The manifest's bindings disagree with the observed
    /// retirement evidence (the rule names the field; a plan or
    /// begin bound by the manifest was not observed, or the
    /// metadata object disagrees).
    ManifestBindingMismatch { rule: &'static str },
    /// The manifest's member count disagrees with the retained
    /// checkpoint index.
    ManifestCountIncoherent { manifest: u64, index: u64 },
    /// The observed shard set is not the exact consecutive
    /// manifest-declared set (missing or unexpected indices).
    ShardSetIncoherent {
        expected_count: u64,
        observed: Vec<u64>,
    },
    /// A shard payload could not be read.
    ShardUnreadable { index: u64, detail: String },
    /// A shard payload refused strict decode.
    MalformedShardPayload { index: u64, reason: String },
    /// A shard's bindings disagree with the manifest (the rule
    /// names the field).
    ShardBindingMismatch { index: u64, rule: &'static str },
    /// An observed shard record's key is not the exact canonical
    /// deterministic key for its parsed index (a noncanonical
    /// decimal spelling, a malformed suffix, or another
    /// same-checkpoint key form) — refused without normalization,
    /// never silently treated as absent.
    ShardKeyNoncanonical { detail: String },
    /// A recognized manifest-family record whose strictly decoded
    /// payload binds the covering MK but whose record key differs
    /// from the expected deterministic manifest key — a
    /// same-checkpoint contradiction, never an absence (F6).
    ManifestKeyNoncanonical { detail: String },
    /// The accumulated shard census overflowed checked
    /// arithmetic.
    ShardCensusOverflow,
    /// The accumulated per-shard member census disagrees with the
    /// manifest's total member count (the census is the SUM of
    /// the per-shard counts — an individual shard is never
    /// required to carry the complete census).
    ShardCensusMismatch { manifest: u64, accumulated: u64 },
    /// The observed ancestry order does not preserve the accepted
    /// publication order (the shards in ascending index order,
    /// then the manifest LAST — the rule names the violation).
    PublicationOrderViolation { rule: &'static str },
}

// ======================================================== the result

/// The classification result — the composite, total-destructure
/// observation. The classifier exposes no authorization
/// capability, cursor, skip permit, deletion permit, or
/// durability certificate: the destructuring of this type names
/// everything it holds.
#[derive(Debug, Clone)]
pub struct RecoveryClassification {
    /// The caller-supplied head — THE OBSERVATION SEED, never an
    /// authority.
    pub head: Hash,
    /// The accepted L0 discovery result, carried unchanged (the
    /// full head-relative observation set, including the
    /// unresolved boundary).
    pub discovery: RecoveryEvidenceDiscovery,
    /// The retained-evidence recovery state.
    pub state: RecoveryState,
    /// The orthogonal witness-evidence observation.
    pub witness: WitnessEvidenceObservation,
    /// The explicit unresolved-authority statement (present on
    /// every result, positive or degraded).
    pub unresolved_authority: UnresolvedAuthority,
}

// ========================================================= the classifier

/// The retained-evidence recovery-state classification: consumes
/// the accepted B-R1a0 discovery over the supplied head and
/// validates the coherence of the recovery evidence retained
/// above the covering checkpoint — the mandatory L2–L7 sequence,
/// purely and noncircularly (see the module documentation).
///
/// **What a positive result establishes:** the CONSISTENCY of the
/// retained evidence — never globally authenticated retirement,
/// never provider durability, never physical retirement, never a
/// startup-safe verdict (see [`UnresolvedAuthority`], carried by
/// every result).
pub fn classify_recovery_evidence<R: BeginReads>(
    reads: &mut R,
    head: &Hash,
    limits: &DiscoveryLimits,
) -> Result<RecoveryClassification, ClassificationError> {
    let discovery =
        discover_recovery_evidence(reads, head, limits).map_err(ClassificationError::Discovery)?;
    let older_checkpoints = match &discovery.covering {
        CoveringObservation::Identified(_) => OlderRangeStatus::UnresolvedBelowCoveringBoundary,
        CoveringObservation::NoneOnAncestry { .. } => {
            OlderRangeStatus::NoCoveringCheckpointObserved
        }
    };
    let unresolved_authority = UnresolvedAuthority {
        provider_canonical_head: NotEstablished,
        founding_derived_seed: NotEstablished,
        original_range_digest: NotEstablished,
        original_object_closures: NotEstablished,
        physical_retirement: NotEstablished,
        journal_retirement: NotEstablished,
        provider_durability: NotEstablished,
        older_checkpoints,
        completed_range_authority: NotEstablished,
        startup_safety: NotEstablished,
    };
    // The checkpoint-less ancestry: no retirement obligation is
    // invented. Witness-family records cannot coherently exist
    // without a covering checkpoint to bind to — observed ones
    // degrade the (necessarily separate) witness observation.
    let CoveringObservation::Identified(covering) = &discovery.covering else {
        let witness = if discovery.records.iter().any(|record| {
            matches!(
                record.family,
                DiscoveredFamily::AncestryWitnessManifest | DiscoveredFamily::AncestryWitnessShard
            )
        }) {
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ManifestBindingMismatch {
                    rule: "no-covering-checkpoint",
                },
            }
        } else {
            WitnessEvidenceObservation::NoneObserved
        };
        return Ok(RecoveryClassification {
            head: *head,
            state: RecoveryState::NoCoveringCheckpoint,
            witness,
            discovery,
            unresolved_authority,
        });
    };

    // ---- the bounded lookup maps (built once; no rescans) ----
    let key_records: BTreeMap<Vec<u8>, &DiscoveredRecord> = discovery
        .records
        .iter()
        .filter_map(|record| record.key.as_ref().map(|key| (key.clone(), record)))
        .collect();
    let positions: BTreeMap<Hash, usize> = discovery
        .records
        .iter()
        .enumerate()
        .map(|(index, record)| (record.identity, index))
        .collect();

    // The raw key-matched records (the identity bindings are the
    // same records the state ladder verifies).
    let plan_record = key_records
        .get(reclamation_plan_idempotency_key(&covering.record).as_slice())
        .copied();
    let v1_audit_record = key_records
        .get(reclamation_ledger_idempotency_key(&covering.record).as_slice())
        .copied();
    let begin_record = key_records
        .get(reclamation_begin_idempotency_key(&covering.record).as_slice())
        .copied();

    // L7 first (the orthogonal witness observation, carried by
    // EVERY result — positive or degraded).
    let witness = classify_witness_evidence(
        reads,
        &discovery,
        &key_records,
        &positions,
        covering,
        plan_record,
        begin_record,
    );
    let context = LadderContext {
        key_records: &key_records,
        positions: &positions,
        covering,
        plan_record,
        v1_audit_record,
        begin_record,
    };
    let state = classify_state(reads, &discovery, &context);
    Ok(RecoveryClassification {
        head: *head,
        state,
        witness,
        discovery,
        unresolved_authority,
    })
}

/// The bundled lookup maps and raw records the L2–L6 ladder
/// consumes (built once in the outer function).
struct LadderContext<'a> {
    key_records: &'a BTreeMap<Vec<u8>, &'a DiscoveredRecord>,
    positions: &'a BTreeMap<Hash, usize>,
    covering: &'a CoveringCheckpoint,
    plan_record: Option<&'a DiscoveredRecord>,
    v1_audit_record: Option<&'a DiscoveredRecord>,
    begin_record: Option<&'a DiscoveredRecord>,
}

// ------------------------------------------------------- the state ladder

/// The L2–L6 state ladder (the inner classification over the
/// built maps and raw records; every incoherence is a degraded
/// STATE, never an error).
fn classify_state<R: BeginReads>(
    reads: &mut R,
    discovery: &RecoveryEvidenceDiscovery,
    context: &LadderContext<'_>,
) -> RecoveryState {
    let LadderContext {
        key_records,
        covering,
        plan_record,
        v1_audit_record,
        begin_record,
        ..
    } = *context;
    // ---- L2: the covering checkpoint ----
    let mk = covering.record;
    let metadata = &covering.metadata;
    if covering.key.as_deref() != Some(checkpoint_mk_idempotency_key(&metadata.sk).as_slice()) {
        return RecoveryState::EvidenceDegraded {
            reason: EvidenceDegradation::CheckpointKeyMismatch { record: mk },
        };
    }
    if metadata.index.is_empty() {
        return RecoveryState::EvidenceDegraded {
            reason: EvidenceDegradation::EmptyRetirementIndex { record: mk },
        };
    }
    if validate_index_shape(&metadata.index).is_err() {
        return RecoveryState::EvidenceDegraded {
            reason: EvidenceDegradation::IndexShapeInvalid { record: mk },
        };
    }

    // ---- L3/L4: the plan and begin lanes ----
    match (plan_record, v1_audit_record) {
        (Some(plan), Some(v1)) => RecoveryState::EvidenceDegraded {
            reason: EvidenceDegradation::MixedV1AndV2Evidence {
                v1_record: v1.identity,
                v2_record: plan.identity,
            },
        },
        (None, None) => {
            // An orphaned witness manifest (observed without the
            // plan and begin it must bind to) is incoherent.
            if let Some(manifest) =
                key_records.get(reclamation_witness_manifest_idempotency_key(&mk).as_slice())
            {
                return RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::OrphanedWitnessManifest {
                        record: manifest.identity,
                    },
                };
            }
            match begin_record {
                Some(begin) => RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::BeginSourceNotObserved {
                        record: begin.identity,
                    },
                },
                None => {
                    // The same-MK wrong-key scan: recognized
                    // reclamation-family evidence binding the
                    // covering MK on an incorrect key is a
                    // contradiction, never an absence — it must
                    // not classify as "no retirement evidence".
                    if let Err(reason) =
                        scan_same_checkpoint_wrong_key_evidence(reads, discovery, covering, None)
                    {
                        return RecoveryState::EvidenceDegraded { reason };
                    }
                    RecoveryState::NoRetirementEvidenceObserved {
                        covering: Box::new(covering.clone()),
                    }
                }
            }
        }
        (None, Some(v1)) => {
            // The V1 lane: a manifest is V2-form evidence —
            // incoherent here; the begin, when observed, must
            // bind the V1 audit revision.
            if let Some(manifest) =
                key_records.get(reclamation_witness_manifest_idempotency_key(&mk).as_slice())
            {
                return RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::OrphanedWitnessManifest {
                        record: manifest.identity,
                    },
                };
            }
            let begin_observation = match begin_record {
                Some(begin) => match &begin.payload {
                    DiscoveredPayload::Begin(decoded)
                        if decoded.mk == mk && decoded.source == v1.identity =>
                    {
                        Some(ObservedV1Begin {
                            record: begin.identity,
                            begin: decoded.clone(),
                        })
                    }
                    DiscoveredPayload::Begin(_) => {
                        return RecoveryState::EvidenceDegraded {
                            reason: EvidenceDegradation::BeginBindingMismatch {
                                record: begin.identity,
                                rule: "v1-source-or-mk",
                            },
                        };
                    }
                    DiscoveredPayload::MalformedPayload { reason } => {
                        return RecoveryState::EvidenceDegraded {
                            reason: EvidenceDegradation::MalformedBeginPayload {
                                record: begin.identity,
                                reason: reason.clone(),
                            },
                        };
                    }
                    _ => {
                        return RecoveryState::EvidenceDegraded {
                            reason: EvidenceDegradation::MalformedBeginPayload {
                                record: begin.identity,
                                reason: String::from("unexpected-payload-degree"),
                            },
                        };
                    }
                },
                None => None,
            };
            RecoveryState::V1EvidenceObserved {
                covering: Box::new(covering.clone()),
                v1_audit: v1.identity,
                begin: begin_observation,
            }
        }
        (Some(plan), None) => classify_v2_lane(reads, discovery, context, plan),
    }
}

/// The V2 lane: L3 (the plan consistency), L4 (the begin), L5
/// (every audit), and L6 (the markers).
fn classify_v2_lane<R: BeginReads>(
    reads: &mut R,
    discovery: &RecoveryEvidenceDiscovery,
    context: &LadderContext<'_>,
    plan: &DiscoveredRecord,
) -> RecoveryState {
    let LadderContext {
        key_records,
        positions,
        covering,
        begin_record,
        ..
    } = *context;
    let mk = covering.record;
    let metadata = &covering.metadata;

    // ---- L3: the plan consistency ----
    let decoded_plan = match &plan.payload {
        DiscoveredPayload::Plan(decoded) => decoded,
        DiscoveredPayload::MalformedPayload { reason } => {
            return RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::MalformedPlanPayload {
                    record: plan.identity,
                    reason: reason.clone(),
                },
            };
        }
        _ => {
            return RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::MalformedPlanPayload {
                    record: plan.identity,
                    reason: String::from("unexpected-payload-degree"),
                },
            };
        }
    };
    if decoded_plan.mk != mk {
        return RecoveryState::EvidenceDegraded {
            reason: EvidenceDegradation::PlanBindingMismatch {
                record: plan.identity,
                rule: "mk",
            },
        };
    }
    if decoded_plan.sk != metadata.sk {
        return RecoveryState::EvidenceDegraded {
            reason: EvidenceDegradation::PlanBindingMismatch {
                record: plan.identity,
                rule: "sk",
            },
        };
    }
    if decoded_plan.range_digest != metadata.range_digest {
        return RecoveryState::EvidenceDegraded {
            reason: EvidenceDegradation::PlanBindingMismatch {
                record: plan.identity,
                rule: "range-digest",
            },
        };
    }
    if decoded_plan.seed != metadata.seed {
        return RecoveryState::EvidenceDegraded {
            reason: EvidenceDegradation::PlanBindingMismatch {
                record: plan.identity,
                rule: "seed",
            },
        };
    }
    if decoded_plan.ledger_byte_limit > MAX_RECLAMATION_LEDGER_BYTES {
        return RecoveryState::EvidenceDegraded {
            reason: EvidenceDegradation::PlanLedgerByteLimitInvalid {
                record: plan.identity,
                limit: decoded_plan.ledger_byte_limit,
            },
        };
    }
    // The local plan-index membership comparison (the owner's
    // directive: implemented locally, equivalence-pinned by the
    // focused regression fixtures).
    if let Err(position) = plan_membership_matches_index(&plan.payload, metadata) {
        return RecoveryState::EvidenceDegraded {
            reason: EvidenceDegradation::PlanMembershipMismatch {
                record: plan.identity,
                position,
            },
        };
    }
    let plan_observation = Box::new(ObservedPlan {
        record: plan.identity,
        plan: decoded_plan.clone(),
    });

    // ---- L4: the begin ----
    let Some(begin) = begin_record else {
        // No begin observed: every OBSERVED plan-defined audit is
        // still validated in full (an invalid observed audit is
        // never a harmless publication-in-progress observation);
        // missing audits remain not-observed (never degradations
        // — the publication may legitimately be mid-flight).
        let index_by_hash: BTreeMap<Hash, &ReclaimedIndexEntry> = metadata
            .index
            .iter()
            .map(|entry| (entry.hash, entry))
            .collect();
        let mut audits_observed = Vec::new();
        let mut audits_not_observed = Vec::new();
        for batch in &plan_observation.plan.batches {
            let audit_key = reclamation_ledger_v2_idempotency_key(&mk, batch.index);
            match key_records.get(audit_key.as_slice()).copied() {
                Some(audit) => {
                    if let Err(reason) = validate_observed_audit(
                        reads,
                        audit,
                        covering,
                        &plan_observation.plan,
                        &index_by_hash,
                        batch,
                    ) {
                        return RecoveryState::EvidenceDegraded { reason };
                    }
                    audits_observed.push(batch.index);
                }
                None => audits_not_observed.push(batch.index),
            }
        }
        // An observed covering-MK Batch marker whose context
        // requires a begin that is not observed is a
        // CONTRADICTION, never harmless incomplete publication.
        for candidate in &discovery.completion_candidates {
            let rule = match &candidate.structural {
                CompletionStructural::V1Marker => continue,
                CompletionStructural::NotBoundToCoveringCheckpoint => {
                    String::from("not-bound-to-covering-checkpoint")
                }
                CompletionStructural::ContextIncomplete { .. } => {
                    String::from("begin-not-observed")
                }
                CompletionStructural::StructuralRefusal { rule } => rule.clone(),
                // Unreachable without an observed begin in the
                // discovery's marker context — refused closed.
                CompletionStructural::StructuralCandidate(_) => {
                    String::from("structural-candidate-without-begin")
                }
            };
            return RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::MarkerContradicted {
                    record: candidate.record,
                    batch: candidate.batch_index,
                    rule,
                },
            };
        }
        // The same-MK wrong-key scan: recognized reclamation-family
        // evidence binding the covering MK on an incorrect key is
        // a contradiction, never an absence.
        if let Err(reason) = scan_same_checkpoint_wrong_key_evidence(
            reads,
            discovery,
            covering,
            Some(&plan_observation.plan),
        ) {
            return RecoveryState::EvidenceDegraded { reason };
        }
        return RecoveryState::PlanObservedWithoutBegin {
            covering: Box::new(covering.clone()),
            plan: plan_observation,
            audits_observed,
            audits_not_observed,
        };
    };
    let decoded_begin = match &begin.payload {
        DiscoveredPayload::Begin(decoded) => decoded,
        DiscoveredPayload::MalformedPayload { reason } => {
            return RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::MalformedBeginPayload {
                    record: begin.identity,
                    reason: reason.clone(),
                },
            };
        }
        _ => {
            return RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::MalformedBeginPayload {
                    record: begin.identity,
                    reason: String::from("unexpected-payload-degree"),
                },
            };
        }
    };
    if decoded_begin.mk != mk {
        return RecoveryState::EvidenceDegraded {
            reason: EvidenceDegradation::BeginBindingMismatch {
                record: begin.identity,
                rule: "mk",
            },
        };
    }
    if decoded_begin.source != plan.identity {
        return RecoveryState::EvidenceDegraded {
            reason: EvidenceDegradation::BeginBindingMismatch {
                record: begin.identity,
                rule: "source-is-not-the-plan-record",
            },
        };
    }
    if decoded_begin.range_digest != metadata.range_digest {
        return RecoveryState::EvidenceDegraded {
            reason: EvidenceDegradation::BeginBindingMismatch {
                record: begin.identity,
                rule: "range-digest",
            },
        };
    }
    if decoded_begin.seed != metadata.seed {
        return RecoveryState::EvidenceDegraded {
            reason: EvidenceDegradation::BeginBindingMismatch {
                record: begin.identity,
                rule: "seed",
            },
        };
    }
    // The strict chronological ordering: the plan and every
    // required audit precede the begin on the observed ancestry
    // (the discovery order is newest first, so the begin's
    // position must be the smallest).
    let begin_position = positions[&begin.identity];
    let plan_position = positions[&plan.identity];
    if begin_position > plan_position {
        return RecoveryState::EvidenceDegraded {
            reason: EvidenceDegradation::BeginTemporalViolation {
                record: begin.identity,
                rule: "plan-above-begin",
            },
        };
    }

    // ---- L5: every plan batch's audit (the all-audits-required
    // rule and the strict audit-before-begin temporal requirement
    // are begin-lane-only; the payload validation itself is the
    // shared per-batch helper) ----
    // The index lookup map (built once — no per-batch rescans).
    let index_by_hash: BTreeMap<Hash, &ReclaimedIndexEntry> = metadata
        .index
        .iter()
        .map(|entry| (entry.hash, entry))
        .collect();
    for batch in &plan_observation.plan.batches {
        let audit_key = reclamation_ledger_v2_idempotency_key(&mk, batch.index);
        let Some(audit) = key_records.get(audit_key.as_slice()).copied() else {
            return RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::RequiredAuditNotObserved { batch: batch.index },
            };
        };
        let audit_position = positions[&audit.identity];
        if begin_position > audit_position {
            return RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::BeginTemporalViolation {
                    record: begin.identity,
                    rule: "audit-above-begin",
                },
            };
        }
        if let Err(reason) = validate_observed_audit(
            reads,
            audit,
            covering,
            &plan_observation.plan,
            &index_by_hash,
            batch,
        ) {
            return RecoveryState::EvidenceDegraded { reason };
        }
    }
    let begin_observation = Box::new(ObservedBegin {
        record: begin.identity,
        begin: decoded_begin.clone(),
    });

    // ---- L6: the V2 markers ----
    let plan_batch_count = plan_observation.plan.batches.len() as u64;
    let mut matched: Vec<u64> = Vec::new();
    let mut not_observed: Vec<u64> = plan_observation
        .plan
        .batches
        .iter()
        .map(|batch| batch.index)
        .collect();
    for candidate in &discovery.completion_candidates {
        match &candidate.structural {
            CompletionStructural::V1Marker => {
                // The V1 marker family in a V2-lane store: observed
                // and carried by the discovery; the existing V1
                // verifiers own it. No V2 counting.
            }
            CompletionStructural::NotBoundToCoveringCheckpoint => {
                return RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::MarkerContradicted {
                        record: candidate.record,
                        batch: None,
                        rule: String::from("not-bound-to-covering-checkpoint"),
                    },
                };
            }
            CompletionStructural::ContextIncomplete { .. } => {
                // Unreachable in the begin-observed lane (the L4/L5
                // checks established the plan, the begin, and every
                // audit the discovery used for the context) —
                // refused closed.
                return RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::MarkerContradicted {
                        record: candidate.record,
                        batch: candidate.batch_index,
                        rule: String::from("context-incomplete"),
                    },
                };
            }
            CompletionStructural::StructuralRefusal { rule } => {
                return RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::MarkerContradicted {
                        record: candidate.record,
                        batch: candidate.batch_index,
                        rule: rule.clone(),
                    },
                };
            }
            CompletionStructural::StructuralCandidate(_) => {
                let Some(batch) = candidate.batch_index else {
                    return RecoveryState::EvidenceDegraded {
                        reason: EvidenceDegradation::MarkerContradicted {
                            record: candidate.record,
                            batch: None,
                            rule: String::from("unparsed-batch-key"),
                        },
                    };
                };
                if batch >= plan_batch_count {
                    return RecoveryState::EvidenceDegraded {
                        reason: EvidenceDegradation::MarkerOutsidePlanMembership {
                            record: candidate.record,
                            batch,
                        },
                    };
                }
                matched.push(batch);
                not_observed.retain(|index| *index != batch);
            }
        }
    }
    if matched.is_empty() {
        RecoveryState::V2BeginObserved {
            covering: Box::new(covering.clone()),
            plan: plan_observation,
            begin: begin_observation,
            markers_not_observed: not_observed,
        }
    } else if matched.len() as u64 == plan_batch_count {
        RecoveryState::AllBatchMarkersStructurallyMatched {
            covering: Box::new(covering.clone()),
            plan: plan_observation,
            begin: begin_observation,
            markers_structurally_matched: matched,
        }
    } else {
        RecoveryState::SomeBatchMarkersObserved {
            covering: Box::new(covering.clone()),
            plan: plan_observation,
            begin: begin_observation,
            markers_structurally_matched: matched,
            markers_not_observed: not_observed,
        }
    }
}

// -------------------------------------------- the shared audit validation

/// The L5 audit validation for ONE observed plan batch — shared
/// by the begin and no-begin lanes (every OBSERVED audit is
/// validated in full regardless of the begin; only the
/// all-audits-required rule and the strict audit-before-begin
/// temporal requirement are begin-lane-only, enforced by the
/// callers).
fn validate_observed_audit<R: BeginReads>(
    reads: &mut R,
    audit: &DiscoveredRecord,
    covering: &CoveringCheckpoint,
    decoded_plan: &ReclaimPlan,
    index_by_hash: &BTreeMap<Hash, &ReclaimedIndexEntry>,
    batch: &crate::reclaim::PlanBatch,
) -> Result<(), EvidenceDegradation> {
    let mk = covering.record;
    let metadata = &covering.metadata;
    let Some(audit_object) = audit.first_object else {
        return Err(EvidenceDegradation::AuditEvidenceUnreadable {
            batch: batch.index,
            detail: String::from("no-first-object"),
        });
    };
    let bytes = match reads.read_object_bytes(&audit_object) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(EvidenceDegradation::AuditEvidenceUnreadable {
                batch: batch.index,
                detail: format!("{error}"),
            });
        }
    };
    if bytes.len() as u64 > decoded_plan.ledger_byte_limit {
        return Err(EvidenceDegradation::AuditByteLimitExceeded {
            batch: batch.index,
            observed: bytes.len() as u64,
            limit: decoded_plan.ledger_byte_limit,
        });
    }
    let ledger = match ReclamationLedgerV2::decode(&bytes) {
        Ok(ledger) => ledger,
        Err(error) => {
            return Err(EvidenceDegradation::MalformedAuditPayload {
                batch: batch.index,
                reason: format!("{error:?}"),
            });
        }
    };
    if ledger.mk != mk
        || ledger.sk != metadata.sk
        || ledger.batch != batch.index
        || ledger.range_digest != metadata.range_digest
        || ledger.seed != metadata.seed
    {
        return Err(EvidenceDegradation::AuditBindingMismatch {
            batch: batch.index,
            rule: "bindings",
        });
    }
    // The exact ordered unit-record membership.
    let mut diverged: Option<usize> = None;
    if ledger.units.len() != batch.records.len() {
        diverged = Some(batch.records.len().min(ledger.units.len()));
    } else {
        for (position, (unit, record)) in ledger.units.iter().zip(&batch.records).enumerate() {
            if unit.record != *record {
                diverged = Some(position);
                break;
            }
        }
    }
    if let Some(position) = diverged {
        return Err(EvidenceDegradation::AuditUnitsMismatch {
            batch: batch.index,
            position,
        });
    }
    // The recomputed retained key count (the accepted invariant):
    // the keyed members of this batch according to the retained
    // checkpoint index.
    let expected_keys = batch
        .records
        .iter()
        .filter(|record| {
            index_by_hash
                .get(*record)
                .is_some_and(|entry| entry.key.is_some())
        })
        .count() as u64;
    if ledger.reclaimed_keys != expected_keys {
        return Err(EvidenceDegradation::AuditKeyedCountMismatch {
            batch: batch.index,
            observed: ledger.reclaimed_keys,
            expected: expected_keys,
        });
    }
    Ok(())
}

// ------------------------------------------ the F5 same-MK wrong-key scan

/// The same-MK wrong-key scan: recognized reclamation-family
/// records whose DECODED payload binds the covering MK but whose
/// record carries an incorrect deterministic key — a
/// contradiction, never an absence. Foreign-checkpoint evidence
/// (a payload binding another MK) is never promoted. `plan`, when
/// present, names the expected audit keys to exclude (the
/// correctly-keyed audits are validated by their own lane). The
/// expected audit keys are built ONCE into a bounded ordered set
/// and tested by membership — the scan is O((B + R) log B) over
/// B plan batches and R discovered records, never the
/// batch-count × ancestry linear rescan (F7).
fn scan_same_checkpoint_wrong_key_evidence<R: BeginReads>(
    reads: &mut R,
    discovery: &RecoveryEvidenceDiscovery,
    covering: &CoveringCheckpoint,
    plan: Option<&ReclaimPlan>,
) -> Result<(), EvidenceDegradation> {
    let mk = covering.record;
    let plan_key = reclamation_plan_idempotency_key(&mk);
    let begin_key = reclamation_begin_idempotency_key(&mk);
    let expected_audit_keys: BTreeSet<Vec<u8>> = plan
        .map(|plan| {
            plan.batches
                .iter()
                .map(|batch| reclamation_ledger_v2_idempotency_key(&mk, batch.index))
                .collect()
        })
        .unwrap_or_default();
    for record in &discovery.records {
        let Some(key) = record.key.as_deref() else {
            continue;
        };
        match record.family {
            DiscoveredFamily::ReclamationPlan => {
                if key == plan_key.as_slice() {
                    continue;
                }
                if let DiscoveredPayload::Plan(decoded) = &record.payload
                    && decoded.mk == mk
                {
                    return Err(EvidenceDegradation::SameCheckpointWrongKeyEvidence {
                        record: record.identity,
                        family: "plan",
                    });
                }
            }
            DiscoveredFamily::ReclamationBegin => {
                if key == begin_key.as_slice() {
                    continue;
                }
                if let DiscoveredPayload::Begin(decoded) = &record.payload
                    && decoded.mk == mk
                {
                    return Err(EvidenceDegradation::SameCheckpointWrongKeyEvidence {
                        record: record.identity,
                        family: "begin",
                    });
                }
            }
            DiscoveredFamily::ReclamationAuditV2 => {
                if expected_audit_keys.contains(key) {
                    continue;
                }
                // The audit payload is not decoded by the
                // discovery — read and decode it (bounded by the
                // accepted ceiling) solely to test the MK binding;
                // an unreadable or undecodable foreign-keyed audit
                // is not attributable to the covering checkpoint
                // and is never promoted.
                let Some(object) = record.first_object else {
                    continue;
                };
                let Ok(bytes) = reads.read_object_bytes(&object) else {
                    continue;
                };
                if bytes.len() as u64 > MAX_RECLAMATION_LEDGER_BYTES {
                    continue;
                }
                let Ok(ledger) = ReclamationLedgerV2::decode(&bytes) else {
                    continue;
                };
                if ledger.mk == mk {
                    return Err(EvidenceDegradation::SameCheckpointWrongKeyEvidence {
                        record: record.identity,
                        family: "audit",
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

// ------------------------------------------------- the L3 local partition

/// The local plan-index membership comparison (the owner's
/// directive: implemented locally, equivalence-pinned by the
/// focused regression fixtures): the flattened plan batches, in
/// the plan's deterministic order, must equal the index-derived
/// newest-first order (the entries sorted by epoch descending —
/// the epochs unique per the accepted index shape)
/// position-by-position. `Err(position)` carries the first
/// divergence (an omission, injection, duplicate, or
/// reordering).
fn plan_membership_matches_index(
    payload: &DiscoveredPayload,
    metadata: &CheckpointMetadata,
) -> Result<(), usize> {
    let DiscoveredPayload::Plan(plan) = payload else {
        return Err(0);
    };
    let mut indexed: Vec<&ReclaimedIndexEntry> = metadata.index.iter().collect();
    // Newest first (the epochs are unique per the L2 shape check).
    indexed.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.epoch));
    let mut position = 0usize;
    for batch in &plan.batches {
        for record in &batch.records {
            match indexed.get(position) {
                Some(entry) if entry.hash == *record => position += 1,
                _ => return Err(position),
            }
        }
    }
    if position != indexed.len() {
        return Err(position);
    }
    Ok(())
}

// ------------------------------------------------------ the L7 witness

/// The orthogonal witness classification, carried by every
/// result. `plan`/`begin` are the raw key-matched records (None
/// in the lanes where they were not observed — a manifest
/// binding a plan or begin that was not observed degrades the
/// witness evidence).
fn classify_witness_evidence<R: BeginReads>(
    reads: &mut R,
    discovery: &RecoveryEvidenceDiscovery,
    key_records: &BTreeMap<Vec<u8>, &DiscoveredRecord>,
    positions: &BTreeMap<Hash, usize>,
    covering: &CoveringCheckpoint,
    plan: Option<&DiscoveredRecord>,
    begin: Option<&DiscoveredRecord>,
) -> WitnessEvidenceObservation {
    let mk = covering.record;
    let manifest_key = reclamation_witness_manifest_idempotency_key(&mk);
    let manifest_record = key_records.get(manifest_key.as_slice()).copied();
    // The F6 same-MK manifest-family scan: a recognized
    // manifest-family record whose strictly decoded payload binds
    // the covering MK but whose key differs from the expected
    // deterministic manifest key is a same-checkpoint
    // contradiction — never a silent absence, never
    // NoneObserved. A legitimate foreign-checkpoint manifest
    // (the payload binding another MK) remains a foreign
    // observation, never a covering-MK degradation. The payload
    // is read and strictly decoded solely to test the MK binding
    // (the manifest is a small fixed-width structure); an
    // unreadable or undecodable foreign-keyed manifest is never
    // attributed.
    for record in &discovery.records {
        if record.family != DiscoveredFamily::AncestryWitnessManifest {
            continue;
        }
        let Some(key) = record.key.as_deref() else {
            continue;
        };
        if key == manifest_key.as_slice() {
            continue;
        }
        let Some(object) = record.first_object else {
            continue;
        };
        let Ok(bytes) = reads.read_object_bytes(&object) else {
            continue;
        };
        if let Ok(decoded) = AncestryWitnessManifest::decode(&bytes)
            && decoded.mk == mk
        {
            return WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ManifestKeyNoncanonical {
                    detail: String::from(
                        "the same-checkpoint manifest payload is carried under an incorrect key",
                    ),
                },
            };
        }
    }
    // The observed shard records, keyed by the EXACT canonical
    // deterministic key for the covering MK: the index is parsed
    // and bounded WITHOUT lossy normalization, the canonical key
    // is reconstructed, and byte equality is required — a
    // noncanonical decimal spelling, a malformed suffix (INCLUDING
    // a same-MK key whose trailing `:<index>` portion is missing
    // or otherwise not index-separable), an overflowing index, or
    // another same-checkpoint key form refuses typed (F2/F6: a
    // malformed observed shard is never silently treated as
    // absent). A shard-family record keyed for ANOTHER checkpoint
    // is foreign evidence — never promoted, never a degradation.
    let shard_prefix = format!("{RECLAMATION_WITNESS_SHARD_KEY_DOMAIN}:{}:", mk.hex());
    let shard_mk_binding = format!("{RECLAMATION_WITNESS_SHARD_KEY_DOMAIN}:{}", mk.hex());
    let mut observed_shards: Vec<(u64, &DiscoveredRecord)> = Vec::new();
    for record in &discovery.records {
        if record.family != DiscoveredFamily::AncestryWitnessShard {
            continue;
        }
        let Some(key) = record.key.as_deref() else {
            continue;
        };
        let key_str = String::from_utf8_lossy(key);
        let tail = match key_str.strip_prefix(shard_prefix.as_str()) {
            Some(tail) => tail,
            None => {
                // A same-MK shard-family key that does not carry
                // the index separator is a malformed observed
                // shard (the mk binding is exact — the fixed-width
                // hex identities cannot be prefixes of one
                // another, so a foreign checkpoint's key never
                // matches this arm).
                if key_str.starts_with(shard_mk_binding.as_str()) {
                    return WitnessEvidenceObservation::Degraded {
                        reason: WitnessDegradation::ShardKeyNoncanonical {
                            detail: String::from(
                                "the same-checkpoint shard key carries no index suffix",
                            ),
                        },
                    };
                }
                continue;
            }
        };
        let index = match tail.parse::<u64>() {
            Ok(index) => index,
            Err(_) => {
                return WitnessEvidenceObservation::Degraded {
                    reason: WitnessDegradation::ShardKeyNoncanonical {
                        detail: format!("malformed shard index suffix: {tail}"),
                    },
                };
            }
        };
        let canonical_key = reclamation_witness_shard_idempotency_key(&mk, index);
        if key != canonical_key.as_slice() {
            return WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ShardKeyNoncanonical {
                    detail: format!("noncanonical key spelling for shard index {index}"),
                },
            };
        }
        observed_shards.push((index, record));
    }
    observed_shards.sort_by_key(|(index, _)| *index);

    let Some(manifest) = manifest_record else {
        if observed_shards.is_empty() {
            return WitnessEvidenceObservation::NoneObserved;
        }
        return WitnessEvidenceObservation::ManifestAbsentWithShardEvidence {
            shard_indices: observed_shards.iter().map(|(index, _)| *index).collect(),
        };
    };
    let Some(manifest_object) = manifest.first_object else {
        return WitnessEvidenceObservation::Degraded {
            reason: WitnessDegradation::ManifestUnreadable {
                detail: String::from("no-first-object"),
            },
        };
    };
    let bytes = match reads.read_object_bytes(&manifest_object) {
        Ok(bytes) => bytes,
        Err(error) => {
            return WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ManifestUnreadable {
                    detail: format!("{error}"),
                },
            };
        }
    };
    let decoded = match AncestryWitnessManifest::decode(&bytes) {
        Ok(decoded) => decoded,
        Err(error) => {
            return WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::MalformedManifestPayload {
                    reason: format!("{error:?}"),
                },
            };
        }
    };
    if decoded.mk != mk {
        return WitnessEvidenceObservation::Degraded {
            reason: WitnessDegradation::ManifestBindingMismatch { rule: "mk" },
        };
    }
    match (plan, begin) {
        (Some(plan), Some(begin)) => {
            if decoded.plan_record != plan.identity {
                return WitnessEvidenceObservation::Degraded {
                    reason: WitnessDegradation::ManifestBindingMismatch {
                        rule: "plan-record",
                    },
                };
            }
            if decoded.begin_record != begin.identity {
                return WitnessEvidenceObservation::Degraded {
                    reason: WitnessDegradation::ManifestBindingMismatch {
                        rule: "begin-record",
                    },
                };
            }
        }
        _ => {
            return WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ManifestBindingMismatch {
                    rule: "plan-or-begin-not-observed",
                },
            };
        }
    }
    if decoded.metadata_object != covering.object {
        return WitnessEvidenceObservation::Degraded {
            reason: WitnessDegradation::ManifestBindingMismatch {
                rule: "metadata-object",
            },
        };
    }
    if decoded.member_count != covering.metadata.index.len() as u64 {
        return WitnessEvidenceObservation::Degraded {
            reason: WitnessDegradation::ManifestCountIncoherent {
                manifest: decoded.member_count,
                index: covering.metadata.index.len() as u64,
            },
        };
    }
    // The exact consecutive shard set.
    let expected_count = decoded.shard_count;
    let observed_indices: Vec<u64> = observed_shards.iter().map(|(index, _)| *index).collect();
    let expected_indices: Vec<u64> = (0..expected_count).collect();
    if observed_indices != expected_indices {
        return WitnessEvidenceObservation::Degraded {
            reason: WitnessDegradation::ShardSetIncoherent {
                expected_count,
                observed: observed_indices,
            },
        };
    }
    // Every shard: read, strict-decode, bindings. One payload at
    // a time (the accepted single-shard ceiling). The per-shard
    // member count is the shard's OWN entry census (the strict
    // decode enforces it against the entries) — an individual
    // shard is never required to carry the manifest's complete
    // census; the census is accumulated and compared ONCE at the
    // end (F1).
    let mut census: u64 = 0;
    let mut shard_records = Vec::new();
    for &(index, record) in &observed_shards {
        let Some(object) = record.first_object else {
            return WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ShardUnreadable {
                    index,
                    detail: String::from("no-first-object"),
                },
            };
        };
        let bytes = match reads.read_object_bytes(&object) {
            Ok(bytes) => bytes,
            Err(error) => {
                return WitnessEvidenceObservation::Degraded {
                    reason: WitnessDegradation::ShardUnreadable {
                        index,
                        detail: format!("{error}"),
                    },
                };
            }
        };
        let shard = match AncestryWitnessShard::decode(&bytes) {
            Ok(shard) => shard,
            Err(error) => {
                return WitnessEvidenceObservation::Degraded {
                    reason: WitnessDegradation::MalformedShardPayload {
                        index,
                        reason: format!("{error:?}"),
                    },
                };
            }
        };
        if shard.mk != mk || shard.shard_index != index || shard.shard_count != decoded.shard_count
        {
            return WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ShardBindingMismatch {
                    index,
                    rule: "bindings",
                },
            };
        }
        census = match census.checked_add(shard.member_count) {
            Some(sum) => sum,
            None => {
                return WitnessEvidenceObservation::Degraded {
                    reason: WitnessDegradation::ShardCensusOverflow,
                };
            }
        };
        shard_records.push(record.identity);
    }
    if census != decoded.member_count {
        return WitnessEvidenceObservation::Degraded {
            reason: WitnessDegradation::ShardCensusMismatch {
                manifest: decoded.member_count,
                accumulated: census,
            },
        };
    }
    // The accepted publication order (F3): the shard records are
    // installed in ascending shard-index order, followed by the
    // manifest LAST — on the newest-first discovery order, the
    // manifest's position must be the smallest and the shard
    // positions must strictly increase as the indices decrease.
    // The bounded position map is used; nothing below the
    // covering MK is ever read.
    let manifest_position = positions[&manifest.identity];
    let mut previous_shard_position: Option<usize> = None;
    for (_, record) in &observed_shards {
        let position = positions[&record.identity];
        if position < manifest_position {
            return WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::PublicationOrderViolation {
                    rule: "manifest-not-newest",
                },
            };
        }
        if let Some(previous) = previous_shard_position
            && position >= previous
        {
            return WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::PublicationOrderViolation {
                    rule: "shards-out-of-order",
                },
            };
        }
        previous_shard_position = Some(position);
    }
    WitnessEvidenceObservation::CoherentlyObserved {
        record: manifest.identity,
        manifest: Box::new(decoded),
        shard_records,
    }
}

// ============================================================== the tests
//
// The owner's B-R1a1 required adversarial matrix (1-24) — pure
// in-memory synthetic-reader tests over genuine canonical codecs
// (CheckpointMetadata with the SK-inclusive index, ReclaimPlan,
// ReclamationLedgerV2, ReclamationBegin, the C1 markers via
// commit_classification, and the A0 witness manifest/shard with
// genuine member preimages), no filesystem, no providers, no
// publication, and no destructive execution of any kind.

#[cfg(test)]
mod classification_tests {
    use super::*;
    use crate::ancestry_accounting::StorageRole;
    use crate::ancestry_witness::WitnessEntry;
    use crate::batch_completion::{
        BatchCompletionContext, BatchCompletionMarkerV2, ClassificationEntry, EntryClassification,
        commit_classification, reclamation_complete_v2_idempotency_key,
    };
    use crate::checkpoint::{SkipEntry, chain_range_digest, compute_digest_seed};
    use crate::reclaim::{
        PlanBatch, RECLAMATION_BEGIN_DOMAIN, RECLAMATION_COMPLETE_DOMAIN,
        RECLAMATION_LEDGER_DOMAIN, RECLAMATION_LEDGER_V2_DOMAIN, RECLAMATION_PLAN_DOMAIN,
        ReclaimUnit, reclamation_complete_idempotency_key, reclamation_ledger_idempotency_key,
    };
    use crate::record::AuthorityMode;
    use engine_core::Encoder;
    use std::collections::BTreeMap;

    const ORDINARY_DOMAIN: &str = "sektor7/test/ordinary";
    const SNAPSHOT_DOMAIN: &str = "engine-slice/session-snapshot/2";

    /// The in-memory synthetic reader (the accepted pattern): two
    /// byte maps and NOTHING else.
    struct SyntheticReads {
        records: BTreeMap<Hash, Vec<u8>>,
        objects: BTreeMap<Hash, Vec<u8>>,
    }

    impl SyntheticReads {
        fn new() -> Self {
            SyntheticReads {
                records: BTreeMap::new(),
                objects: BTreeMap::new(),
            }
        }

        fn install_record(&mut self, record: &crate::record::RevisionRecord) -> Hash {
            let bytes = record.canonical_bytes();
            let id = crate::hash::hash_object(&bytes);
            self.records.insert(id, bytes);
            id
        }

        fn install_object(&mut self, bytes: &[u8]) -> Hash {
            let id = crate::hash::hash_object(bytes);
            self.objects.insert(id, bytes.to_vec());
            id
        }
    }

    impl BeginReads for SyntheticReads {
        fn read_record_bytes(&mut self, id: &Hash) -> Result<Vec<u8>, crate::medium::MediumError> {
            self.records
                .get(id)
                .cloned()
                .ok_or(crate::medium::MediumError::MissingObject { hash: *id })
        }
        fn read_object_bytes(
            &mut self,
            hash: &Hash,
        ) -> Result<Vec<u8>, crate::medium::MediumError> {
            self.objects
                .get(hash)
                .cloned()
                .ok_or(crate::medium::MediumError::MissingObject { hash: *hash })
        }
    }

    fn domain_object(domain: &str, tag: &[u8]) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.write_str(domain);
        let mut bytes = encoder.into_bytes();
        bytes.extend_from_slice(tag);
        bytes
    }

    /// The sequential chain builder; `push` returns the record's
    /// identity AND canonical bytes (the witness preimages need
    /// the genuine bytes of the on-chain members).
    struct ChainBuilder {
        reads: SyntheticReads,
        tip: Hash,
        tip_epoch: u64,
    }

    impl ChainBuilder {
        fn founded() -> Self {
            let record =
                crate::record::RevisionRecord::founding(AuthorityMode::Standalone, "b-r1a1", 0);
            let mut reads = SyntheticReads::new();
            let founding = reads.install_record(&record);
            ChainBuilder {
                reads,
                tip: founding,
                tip_epoch: 0,
            }
        }

        fn push(&mut self, key: Option<Vec<u8>>, object_bytes: &[u8]) -> (Hash, Vec<u8>) {
            let object = self.reads.install_object(object_bytes);
            let mut record = crate::record::RevisionRecord::regular(self.tip, self.tip_epoch, 1)
                .with_content_refs(vec![object]);
            if let Some(key) = key {
                record = record.with_idempotency_key(key);
            }
            let bytes = record.canonical_bytes();
            let identity = crate::hash::hash_object(&bytes);
            self.reads.records.insert(identity, bytes.clone());
            self.tip = identity;
            self.tip_epoch += 1;
            (identity, bytes)
        }

        fn push_domain(&mut self, domain: &str, tag: &[u8]) -> (Hash, Vec<u8>) {
            self.push(None, &domain_object(domain, tag))
        }
    }

    /// The covering-chain prefix: founding → o1 → anchor → o2 →
    /// SK → MK, with the SK-INCLUSIVE index (the real publication
    /// discipline: SK's own retirement entry) whose epoch-
    /// descending order [sk, m0, m1] matches the flattened plan.
    /// The off-chain members m0 (keyed, epoch 3) and m1 (epoch 2)
    /// exist as genuine records for the witness preimages but are
    /// never installed — nothing below the MK is ever read.
    struct CoveringChain {
        chain: ChainBuilder,
        sk: Hash,
        mk: Hash,
        mk_object: Hash,
        m0: Hash,
        m1: Hash,
        m0_bytes: Vec<u8>,
        m1_bytes: Vec<u8>,
        sk_bytes: Vec<u8>,
        metadata: CheckpointMetadata,
        range_digest: [u8; 32],
        seed: [u8; 32],
    }

    impl CoveringChain {
        fn default_plan(&self) -> ReclaimPlan {
            ReclaimPlan {
                mk: self.mk,
                sk: self.sk,
                range_digest: self.range_digest,
                seed: self.seed,
                ledger_byte_limit: 512 * 1024,
                batches: vec![
                    PlanBatch {
                        index: 0,
                        records: vec![self.sk, self.m0],
                    },
                    PlanBatch {
                        index: 1,
                        records: vec![self.m1],
                    },
                ],
                journal_files: Vec::new(),
            }
        }

        fn default_audit(&self, batch: u64) -> ReclamationLedgerV2 {
            let (records, reclaimed_keys, units) = match batch {
                0 => (
                    2,
                    1,
                    vec![
                        ReclaimUnit {
                            record: self.sk,
                            objects: Vec::new(),
                        },
                        ReclaimUnit {
                            record: self.m0,
                            objects: vec![crate::hash::hash_object(b"b-r1a1-m0-obj")],
                        },
                    ],
                ),
                _ => (
                    1,
                    0,
                    vec![ReclaimUnit {
                        record: self.m1,
                        objects: Vec::new(),
                    }],
                ),
            };
            ReclamationLedgerV2 {
                mk: self.mk,
                sk: self.sk,
                batch,
                range_digest: self.range_digest,
                seed: self.seed,
                records,
                reclaimed_keys,
                units,
            }
        }

        fn push_plan(&mut self, plan: &ReclaimPlan) -> Hash {
            let key = reclamation_plan_idempotency_key(&self.mk);
            self.chain.push(Some(key), &plan.canonical_bytes()).0
        }

        fn push_audit(&mut self, ledger: &ReclamationLedgerV2) -> Hash {
            let key = reclamation_ledger_v2_idempotency_key(&self.mk, ledger.batch);
            self.chain.push(Some(key), &ledger.canonical_bytes()).0
        }

        fn push_begin(&mut self, source: Hash) -> Hash {
            let begin = ReclamationBegin {
                mk: self.mk,
                source,
                range_digest: self.range_digest,
                seed: self.seed,
            };
            let key = reclamation_begin_idempotency_key(&self.mk);
            self.chain.push(Some(key), &begin.canonical_bytes()).0
        }

        fn default_manifest(&self, plan: Hash, begin: Hash) -> AncestryWitnessManifest {
            AncestryWitnessManifest {
                mk: self.mk,
                plan_record: plan,
                begin_record: begin,
                metadata_object: self.mk_object,
                member_count: 3,
                shard_count: 1,
                coverage_digest: [9u8; 32],
            }
        }

        fn default_shard(&self) -> AncestryWitnessShard {
            AncestryWitnessShard {
                mk: self.mk,
                shard_index: 0,
                shard_count: 1,
                member_count: 3,
                entries: vec![
                    WitnessEntry {
                        member: self.sk,
                        preimage: self.sk_bytes.clone(),
                    },
                    WitnessEntry {
                        member: self.m0,
                        preimage: self.m0_bytes.clone(),
                    },
                    WitnessEntry {
                        member: self.m1,
                        preimage: self.m1_bytes.clone(),
                    },
                ],
            }
        }

        /// The accepted PRODUCTION order: the shard records in
        /// ascending shard-index order, followed by the manifest
        /// LAST (F3 — the positive fixtures must match it).
        fn push_witness_family(
            &mut self,
            manifest: &AncestryWitnessManifest,
            shards: &[AncestryWitnessShard],
        ) -> Hash {
            for shard in shards {
                let shard_key = crate::ancestry_witness::reclamation_witness_shard_idempotency_key(
                    &self.mk,
                    shard.shard_index,
                );
                let _ = self.chain.push(Some(shard_key), &shard.canonical_bytes()).0;
            }
            let manifest_key =
                crate::ancestry_witness::reclamation_witness_manifest_idempotency_key(&self.mk);
            self.chain
                .push(Some(manifest_key), &manifest.canonical_bytes())
                .0
        }

        fn push_witness(
            &mut self,
            manifest: &AncestryWitnessManifest,
            shard: &AncestryWitnessShard,
        ) -> Hash {
            self.push_witness_family(manifest, std::slice::from_ref(shard))
        }

        /// The two-shard witness with UNEQUAL per-shard counts (the
        /// F1 census fixture): shard 0 carries [sk, m0], shard 1
        /// carries [m1] — the census accumulates 2 + 1 = 3 against
        /// the manifest's total of 3.
        fn default_two_shard_witness(
            &self,
            plan: Hash,
            begin: Hash,
        ) -> (AncestryWitnessManifest, Vec<AncestryWitnessShard>) {
            let manifest = AncestryWitnessManifest {
                mk: self.mk,
                plan_record: plan,
                begin_record: begin,
                metadata_object: self.mk_object,
                member_count: 3,
                shard_count: 2,
                coverage_digest: [9u8; 32],
            };
            let shard0 = AncestryWitnessShard {
                mk: self.mk,
                shard_index: 0,
                shard_count: 2,
                member_count: 2,
                entries: vec![
                    WitnessEntry {
                        member: self.sk,
                        preimage: self.sk_bytes.clone(),
                    },
                    WitnessEntry {
                        member: self.m0,
                        preimage: self.m0_bytes.clone(),
                    },
                ],
            };
            let shard1 = AncestryWitnessShard {
                mk: self.mk,
                shard_index: 1,
                shard_count: 2,
                member_count: 1,
                entries: vec![WitnessEntry {
                    member: self.m1,
                    preimage: self.m1_bytes.clone(),
                }],
            };
            (manifest, vec![shard0, shard1])
        }

        fn push_marker(&mut self, context: &BatchCompletionContext, key_batch: u64) -> Hash {
            let key = reclamation_complete_v2_idempotency_key(&self.mk, key_batch);
            self.chain
                .push(Some(key), &marker_for(context).canonical_bytes())
                .0
        }

        fn push_head(&mut self) -> Hash {
            self.chain.push_domain(ORDINARY_DOMAIN, b"head").0
        }
    }

    fn covering_chain() -> CoveringChain {
        let mut chain = ChainBuilder::founded();
        let founding = chain.tip;
        let _o1 = chain.push_domain(ORDINARY_DOMAIN, b"o1");
        let (anchor, _) = chain.push_domain(ORDINARY_DOMAIN, b"anchor");
        let _o2 = chain.push_domain(ORDINARY_DOMAIN, b"o2");
        let (sk, sk_bytes) = chain.push_domain(SNAPSHOT_DOMAIN, b"sk");
        // The off-chain range members (below the MK, never read).
        let m0_record = crate::record::RevisionRecord::regular(anchor, 2, 1)
            .with_idempotency_key(Vec::from(&b"b-r1a1-m0-key"[..]));
        let m0_bytes = m0_record.canonical_bytes();
        let m0 = crate::hash::hash_object(&m0_bytes);
        let m1_record = crate::record::RevisionRecord::regular(m0, 1, 1);
        let m1_bytes = m1_record.canonical_bytes();
        let m1 = crate::hash::hash_object(&m1_bytes);
        let seed = compute_digest_seed(&founding);
        let range_digest = chain_range_digest(&seed, &[b"m0-bytes", b"m1-bytes"]);
        let metadata = CheckpointMetadata::new(
            sk,
            anchor,
            vec![SkipEntry::self_skip(anchor)],
            vec![
                crate::checkpoint::ReclaimedIndexEntry::new(None, sk, 4),
                crate::checkpoint::ReclaimedIndexEntry::new(
                    Some(Vec::from(&b"b-r1a1-m0-key"[..])),
                    m0,
                    3,
                ),
                crate::checkpoint::ReclaimedIndexEntry::new(None, m1, 2),
            ],
            range_digest,
            seed,
        );
        let (mk, _) = chain.push(
            Some(checkpoint_mk_idempotency_key(&sk)),
            &metadata.canonical_bytes(),
        );
        CoveringChain {
            chain,
            sk,
            mk,
            mk_object: crate::hash::hash_object(&metadata.canonical_bytes()),
            m0,
            m1,
            m0_bytes,
            m1_bytes,
            sk_bytes,
            metadata,
            range_digest,
            seed,
        }
    }

    fn classification_entries() -> Vec<ClassificationEntry> {
        vec![
            ClassificationEntry {
                identity: crate::hash::hash_object(b"b-r1a1-SK0"),
                role: StorageRole::Record,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            },
            ClassificationEntry {
                identity: crate::hash::hash_object(b"b-r1a1-m0a"),
                role: StorageRole::Record,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            },
            ClassificationEntry {
                identity: crate::hash::hash_object(b"b-r1a1-o0a"),
                role: StorageRole::Object,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            },
            ClassificationEntry {
                identity: crate::hash::hash_object(b"b-r1a1-os"),
                role: StorageRole::Object,
                classification: EntryClassification::Deferred,
                conditional_on_prior_batch: false,
            },
        ]
    }

    fn marker_for(context: &BatchCompletionContext) -> BatchCompletionMarkerV2 {
        let commitment = commit_classification(context, classification_entries()).unwrap();
        BatchCompletionMarkerV2 {
            mk: context.mk,
            plan: context.plan,
            batch_index: context.batch_index,
            audit: context.audit,
            begin: context.begin,
            commitment: commitment.digest,
            counters: commitment.counters,
        }
    }

    /// The full V2 fixture: plan, both audits, begin, witness
    /// (manifest + shard), both markers, and a head.
    struct Fixture {
        reads: SyntheticReads,
        head: Hash,
    }

    fn build_fixture() -> Fixture {
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        let manifest = covering.default_manifest(plan, begin);
        let shard = covering.default_shard();
        let _witness = covering.push_witness(&manifest, &shard);
        let context0 = BatchCompletionContext {
            mk: covering.mk,
            plan,
            batch_index: 0,
            audit: audit0,
            begin,
        };
        let _marker0 = covering.push_marker(&context0, 0);
        let context1 = BatchCompletionContext {
            mk: covering.mk,
            plan,
            batch_index: 1,
            audit: audit1,
            begin,
        };
        let _marker1 = covering.push_marker(&context1, 1);
        let head = covering.push_head();
        Fixture {
            reads: covering.chain.reads,
            head,
        }
    }

    fn default_limits() -> DiscoveryLimits {
        DiscoveryLimits::default_limits()
    }

    fn classify(reads: &mut SyntheticReads, head: &Hash) -> RecoveryClassification {
        classify_recovery_evidence(reads, head, &default_limits()).unwrap()
    }

    // (1) No covering checkpoint.
    #[test]
    fn no_covering_checkpoint_ancestry_classifies_without_inventing_obligations() {
        let mut chain = ChainBuilder::founded();
        let (head, _) = chain.push_domain(ORDINARY_DOMAIN, b"head");
        let classification = classify(&mut chain.reads, &head);
        let RecoveryClassification {
            head: observed_head,
            state,
            witness,
            unresolved_authority,
            ..
        } = &classification;
        assert_eq!(*observed_head, head);
        assert!(matches!(state, RecoveryState::NoCoveringCheckpoint));
        assert!(matches!(witness, WitnessEvidenceObservation::NoneObserved));
        assert_eq!(
            unresolved_authority.older_checkpoints,
            OlderRangeStatus::NoCoveringCheckpointObserved
        );
    }

    // (2) Covering MK with no retirement evidence observed.
    #[test]
    fn covering_checkpoint_without_retirement_evidence_observes_none() {
        let mut covering = covering_chain();
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        let RecoveryState::NoRetirementEvidenceObserved { covering: observed } =
            &classification.state
        else {
            panic!(
                "expected NoRetirementEvidenceObserved, got {:?}",
                classification.state
            );
        };
        assert_eq!(observed.record, covering.mk);
        assert_eq!(observed.metadata, covering.metadata);
        assert!(matches!(
            classification.witness,
            WitnessEvidenceObservation::NoneObserved
        ));
        assert_eq!(
            classification.unresolved_authority.older_checkpoints,
            OlderRangeStatus::UnresolvedBelowCoveringBoundary
        );
    }

    // (3) + (4) Plan present, begin absent; partial audit
    // publication.
    #[test]
    fn plan_observed_without_begin_reports_the_audit_observations() {
        // No audits at all.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        let RecoveryState::PlanObservedWithoutBegin {
            plan: observed_plan,
            audits_observed,
            audits_not_observed,
            ..
        } = &classification.state
        else {
            panic!(
                "expected PlanObservedWithoutBegin, got {:?}",
                classification.state
            );
        };
        assert_eq!(observed_plan.record, plan);
        assert_eq!(observed_plan.plan, plan_payload);
        assert!(audits_observed.is_empty());
        assert_eq!(audits_not_observed.as_slice(), &[0, 1]);
        // Partial audit publication: one of two.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let _plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        let RecoveryState::PlanObservedWithoutBegin {
            audits_observed,
            audits_not_observed,
            ..
        } = &classification.state
        else {
            panic!("expected PlanObservedWithoutBegin");
        };
        assert_eq!(audits_observed.as_slice(), &[0]);
        assert_eq!(audits_not_observed.as_slice(), &[1]);
    }

    // (5) V1 legacy observations.
    #[test]
    fn v1_legacy_evidence_is_observed_without_v2_obligations() {
        let mut covering = covering_chain();
        let v1_audit_payload = domain_object(RECLAMATION_LEDGER_DOMAIN, b"v1-audit");
        let v1_audit_key = reclamation_ledger_idempotency_key(&covering.mk);
        let v1_audit = covering.chain.push(Some(v1_audit_key), &v1_audit_payload).0;
        let begin_payload = ReclamationBegin {
            mk: covering.mk,
            source: v1_audit,
            range_digest: covering.range_digest,
            seed: covering.seed,
        };
        let begin_key = reclamation_begin_idempotency_key(&covering.mk);
        let begin = covering
            .chain
            .push(Some(begin_key), &begin_payload.canonical_bytes())
            .0;
        let v1_marker_key = reclamation_complete_idempotency_key(&covering.mk, Some(0));
        let _v1_marker = covering
            .chain
            .push(
                Some(v1_marker_key),
                &domain_object(RECLAMATION_COMPLETE_DOMAIN, b"v1-marker"),
            )
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        let RecoveryState::V1EvidenceObserved {
            v1_audit: observed_audit,
            begin: observed_begin,
            ..
        } = &classification.state
        else {
            panic!(
                "expected V1EvidenceObserved, got {:?}",
                classification.state
            );
        };
        assert_eq!(*observed_audit, v1_audit);
        let observed_begin = observed_begin.as_ref().expect("the V1 begin was observed");
        assert_eq!(observed_begin.record, begin);
        assert_eq!(observed_begin.begin, begin_payload);
        assert!(matches!(
            classification.witness,
            WitnessEvidenceObservation::NoneObserved
        ));
    }

    // (6) V2 begin with no completion markers.
    #[test]
    fn v2_begin_with_no_markers_observed_reports_not_observed() {
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        let RecoveryState::V2BeginObserved {
            markers_not_observed,
            begin: observed_begin,
            ..
        } = &classification.state
        else {
            panic!("expected V2BeginObserved, got {:?}", classification.state);
        };
        assert_eq!(markers_not_observed.as_slice(), &[0, 1]);
        assert_eq!(observed_begin.record, begin);
        assert_eq!(observed_begin.begin.source, plan);
        assert!(matches!(
            classification.witness,
            WitnessEvidenceObservation::NoneObserved
        ));
    }

    // (7) One structurally matched marker out of two batches.
    #[test]
    fn one_of_two_markers_structurally_matched_is_reported_exactly() {
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        // Only batch 1's marker — the non-consecutive receipt set
        // is reported exactly, never interpreted.
        let context1 = BatchCompletionContext {
            mk: covering.mk,
            plan,
            batch_index: 1,
            audit: audit1,
            begin,
        };
        let _marker1 = covering.push_marker(&context1, 1);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        let RecoveryState::SomeBatchMarkersObserved {
            markers_structurally_matched,
            markers_not_observed,
            ..
        } = &classification.state
        else {
            panic!(
                "expected SomeBatchMarkersObserved, got {:?}",
                classification.state
            );
        };
        assert_eq!(markers_structurally_matched.as_slice(), &[1]);
        assert_eq!(markers_not_observed.as_slice(), &[0]);
    }

    // (8) All Batch markers structurally matched — without Range
    // authority.
    #[test]
    fn all_markers_structurally_matched_grants_no_range_authority() {
        let mut fixture = build_fixture();
        let classification = classify(&mut fixture.reads, &fixture.head);
        let RecoveryState::AllBatchMarkersStructurallyMatched {
            markers_structurally_matched,
            ..
        } = &classification.state
        else {
            panic!(
                "expected AllBatchMarkersStructurallyMatched, got {:?}",
                classification.state
            );
        };
        assert_eq!(
            markers_structurally_matched.as_slice(),
            &[1, 0],
            "the matched list is in discovery order (newest first)"
        );
        // The witness observation is coherent (the full fixture
        // carries the genuine manifest and shard).
        assert!(matches!(
            classification.witness,
            WitnessEvidenceObservation::CoherentlyObserved { .. }
        ));
        // The unresolved-authority statement names every non-grant
        // — destructured field by field.
        let UnresolvedAuthority {
            provider_canonical_head: NotEstablished,
            founding_derived_seed: NotEstablished,
            original_range_digest: NotEstablished,
            original_object_closures: NotEstablished,
            physical_retirement: NotEstablished,
            journal_retirement: NotEstablished,
            provider_durability: NotEstablished,
            older_checkpoints,
            completed_range_authority: NotEstablished,
            startup_safety: NotEstablished,
        } = &classification.unresolved_authority;
        assert_eq!(
            *older_checkpoints,
            OlderRangeStatus::UnresolvedBelowCoveringBoundary
        );
    }

    // (9) A correctly keyed but context-lying Batch marker.
    #[test]
    fn a_correctly_keyed_lying_marker_degrades_the_evidence() {
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        // Keyed for batch 1 while binding batch 0's context.
        let lying_context = BatchCompletionContext {
            mk: covering.mk,
            plan,
            batch_index: 0,
            audit: audit0,
            begin,
        };
        let lying = covering.push_marker(&lying_context, 1);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        let RecoveryState::EvidenceDegraded { reason } = &classification.state else {
            panic!("expected EvidenceDegraded, got {:?}", classification.state);
        };
        assert!(matches!(
            reason,
            EvidenceDegradation::MarkerContradicted { record, batch, rule }
                if *record == lying && *batch == Some(1) && rule == "batch-candidate/context-mismatch"
        ));
    }

    // (10) A marker whose batch index is outside the plan.
    #[test]
    fn a_marker_outside_the_plan_membership_degrades() {
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        // A genuine audit and marker for a batch the plan does not
        // define (index 5): the structural read succeeds against
        // the discovered context; the plan-membership check
        // refuses.
        let mut foreign_audit = covering.default_audit(5);
        foreign_audit.batch = 5;
        let audit5 = covering.push_audit(&foreign_audit);
        let context5 = BatchCompletionContext {
            mk: covering.mk,
            plan,
            batch_index: 5,
            audit: audit5,
            begin,
        };
        let outside = covering.push_marker(&context5, 5);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        let RecoveryState::EvidenceDegraded { reason } = &classification.state else {
            panic!("expected EvidenceDegraded, got {:?}", classification.state);
        };
        assert!(matches!(
            reason,
            EvidenceDegradation::MarkerOutsidePlanMembership { record, batch }
                if *record == outside && *batch == 5
        ));
    }

    // (11) A valid off-chain marker absent from the inspected
    // ancestry.
    #[test]
    fn an_off_chain_marker_absent_from_the_ancestry_cannot_count() {
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        // The marker's OBJECT is installed in the store, but its
        // RECORD is never appended to the ancestry — off-chain.
        let context = BatchCompletionContext {
            mk: covering.mk,
            plan,
            batch_index: 0,
            audit: audit0,
            begin,
        };
        let _marker_object = covering
            .chain
            .reads
            .install_object(&marker_for(&context).canonical_bytes());
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        let RecoveryState::V2BeginObserved {
            markers_not_observed,
            ..
        } = &classification.state
        else {
            panic!("expected V2BeginObserved, got {:?}", classification.state);
        };
        assert_eq!(markers_not_observed.as_slice(), &[0, 1]);
    }

    // (12) Duplicate deterministic keys.
    #[test]
    fn duplicate_deterministic_keys_refuse_at_the_discovery() {
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        // A second record under the SAME plan key (the chain-wide
        // uniqueness invariant, refused by the discovery) — ON the
        // inspected ancestry: the head's parent chain runs through
        // it.
        let duplicate_object = covering
            .chain
            .reads
            .install_object(&domain_object(ORDINARY_DOMAIN, b"duplicate"));
        let duplicate =
            crate::record::RevisionRecord::regular(covering.chain.tip, covering.chain.tip_epoch, 1)
                .with_idempotency_key(reclamation_plan_idempotency_key(&covering.mk))
                .with_content_refs(vec![duplicate_object]);
        let duplicate_id = covering.chain.reads.install_record(&duplicate);
        covering.chain.tip = duplicate_id;
        covering.chain.tip_epoch += 1;
        let head = covering.push_head();
        let error = classify_recovery_evidence(&mut covering.chain.reads, &head, &default_limits())
            .unwrap_err();
        assert!(matches!(
            error,
            ClassificationError::Discovery(DiscoveryError::KeyDuplicate { record })
                if record == plan
        ));
    }

    // (13) Malformed plan or begin payload; temporal violations.
    #[test]
    fn malformed_begin_payloads_and_temporal_violations_degrade() {
        // A malformed plan payload.
        let mut covering = covering_chain();
        let plan_key = reclamation_plan_idempotency_key(&covering.mk);
        let _plan = covering
            .chain
            .push(
                Some(plan_key),
                &domain_object(RECLAMATION_PLAN_DOMAIN, b"garbage"),
            )
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::MalformedPlanPayload { .. }
            }
        ));
        // A malformed begin payload.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let _plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin_key = reclamation_begin_idempotency_key(&covering.mk);
        let begin = covering
            .chain
            .push(
                Some(begin_key),
                &domain_object(RECLAMATION_BEGIN_DOMAIN, b"garbage"),
            )
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::MalformedBeginPayload { record, .. }
            } if *record == begin
        ));
        // A begin whose source is not the observed plan record.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let foreign_source = crate::hash::hash_object(b"b-r1a1-foreign-source");
        let begin = covering.push_begin(foreign_source);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::BeginBindingMismatch { record, rule }
            } if *record == begin && *rule == "source-is-not-the-plan-record"
        ));
        assert_ne!(foreign_source, plan);
        // An audit ABOVE the begin (the temporal violation).
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let _begin = covering.push_begin(plan);
        // The audit for batch 1 published AFTER the begin — newer
        // on the ancestry than the begin.
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::BeginTemporalViolation { rule, .. }
            } if *rule == "audit-above-begin"
        ));
    }

    // (14) Malformed or context-mismatched audit.
    #[test]
    fn malformed_or_context_mismatched_audits_degrade() {
        // A malformed audit payload.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0_key = reclamation_ledger_v2_idempotency_key(&covering.mk, 0);
        let _audit0 = covering
            .chain
            .push(
                Some(audit0_key),
                &domain_object(RECLAMATION_LEDGER_V2_DOMAIN, b"garbage"),
            )
            .0;
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let _begin = covering.push_begin(plan);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::MalformedAuditPayload { batch, .. }
            } if *batch == 0
        ));
        // A genuine ledger under the batch-0 key whose payload
        // binds batch 1 — pushed under the EXPLICIT batch-0 key so
        // the payload's batch binding is the lie (the key remains
        // the honest locator).
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let mut lying = covering.default_audit(0);
        lying.batch = 1;
        let lying_key = reclamation_ledger_v2_idempotency_key(&covering.mk, 0);
        let _audit0 = covering
            .chain
            .push(Some(lying_key), &lying.canonical_bytes())
            .0;
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let _begin = covering.push_begin(plan);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::AuditBindingMismatch { batch, .. }
            } if *batch == 0
        ));
        // Reordered units.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let mut reordered = covering.default_audit(0);
        reordered.units.reverse();
        let _audit0 = covering.push_audit(&reordered);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let _begin = covering.push_begin(plan);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::AuditUnitsMismatch { batch, position }
            } if *batch == 0 && *position == 0
        ));
    }

    // (15) Incorrect audit reclaimed_keys count.
    #[test]
    fn incorrect_reclaimed_keys_counts_degrade() {
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let mut miscounted = covering.default_audit(0);
        miscounted.reclaimed_keys = 2;
        let _audit0 = covering.push_audit(&miscounted);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let _begin = covering.push_begin(plan);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::AuditKeyedCountMismatch {
                    batch,
                    observed,
                    expected,
                }
            } if *batch == 0 && *observed == 2 && *expected == 1
        ));
    }

    // (16) Incorrect flattened plan membership order (and the
    // omission/injection variants — the local partition rule's
    // equivalence pinning).
    #[test]
    fn incorrect_flattened_plan_membership_degrades_at_the_position() {
        // Reordered batches: batch 0 names [m1] — divergence at
        // position 0.
        let mut covering = covering_chain();
        let mut reordered = covering.default_plan();
        reordered.batches = vec![
            PlanBatch {
                index: 0,
                records: vec![covering.m1],
            },
            PlanBatch {
                index: 1,
                records: vec![covering.sk, covering.m0],
            },
        ];
        let plan = covering.push_plan(&reordered);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::PlanMembershipMismatch { record, position }
            } if *record == plan && *position == 0
        ));
        // Omission: the plan flattens to [sk, m0] against the
        // three-entry index — divergence at position 2.
        let mut covering = covering_chain();
        let mut omitted = covering.default_plan();
        omitted.batches = vec![PlanBatch {
            index: 0,
            records: vec![covering.sk, covering.m0],
        }];
        let _plan = covering.push_plan(&omitted);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::PlanMembershipMismatch { position, .. }
            } if *position == 2
        ));
        // Injection: a foreign record inside batch 0 — divergence
        // at its position.
        let mut covering = covering_chain();
        let foreign = crate::hash::hash_object(b"b-r1a1-foreign-member");
        let mut injected = covering.default_plan();
        injected.batches[0].records.push(foreign);
        let _plan = covering.push_plan(&injected);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::PlanMembershipMismatch { position, .. }
            } if *position == 2
        ));
    }

    // (17) Duplicate indexed epochs.
    #[test]
    fn duplicate_indexed_epochs_degrade_via_the_index_shape() {
        let mut chain = ChainBuilder::founded();
        let founding = chain.tip;
        let _o1 = chain.push_domain(ORDINARY_DOMAIN, b"o1");
        let (anchor, _) = chain.push_domain(ORDINARY_DOMAIN, b"anchor");
        let _o2 = chain.push_domain(ORDINARY_DOMAIN, b"o2");
        let (sk, _) = chain.push_domain(SNAPSHOT_DOMAIN, b"sk");
        let m0 = crate::hash::hash_object(b"b-r1a1-dup-m0");
        let m9 = crate::hash::hash_object(b"b-r1a1-dup-m9");
        let seed = compute_digest_seed(&founding);
        let range_digest = chain_range_digest(&seed, &[b"m0-bytes", b"m9-bytes"]);
        // Two entries sharing epoch 3 — distinct hashes, so the
        // codec's sorted-unique-HASH rule passes; the accepted
        // index-shape rule refuses.
        let metadata = CheckpointMetadata::new(
            sk,
            anchor,
            vec![SkipEntry::self_skip(anchor)],
            vec![
                crate::checkpoint::ReclaimedIndexEntry::new(None, sk, 4),
                crate::checkpoint::ReclaimedIndexEntry::new(None, m0, 3),
                crate::checkpoint::ReclaimedIndexEntry::new(None, m9, 3),
            ],
            range_digest,
            seed,
        );
        let (mk, _) = chain.push(
            Some(checkpoint_mk_idempotency_key(&sk)),
            &metadata.canonical_bytes(),
        );
        let (head, _) = chain.push_domain(ORDINARY_DOMAIN, b"head");
        let classification = classify(&mut chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::IndexShapeInvalid { record }
            } if *record == mk
        ));
    }

    // (18) Malformed or context-mismatched witness evidence —
    // the witness observation degrades while the receipt-based
    // main state stands (the orthogonality).
    #[test]
    fn malformed_or_mismatched_witness_evidence_degrades_the_witness_observation() {
        // A manifest binding a foreign begin record.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        let mut manifest = covering.default_manifest(plan, begin);
        manifest.begin_record = crate::hash::hash_object(b"b-r1a1-foreign-begin");
        let shard = covering.default_shard();
        let _witness = covering.push_witness(&manifest, &shard);
        let context0 = BatchCompletionContext {
            mk: covering.mk,
            plan,
            batch_index: 0,
            audit: audit0,
            begin,
        };
        let _marker0 = covering.push_marker(&context0, 0);
        let context1 = BatchCompletionContext {
            mk: covering.mk,
            plan,
            batch_index: 1,
            audit: audit1,
            begin,
        };
        let _marker1 = covering.push_marker(&context1, 1);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::AllBatchMarkersStructurallyMatched { .. }
        ));
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ManifestBindingMismatch { rule }
            } if *rule == "begin-record"
        ));
        // A malformed manifest payload.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let _begin = covering.push_begin(plan);
        let manifest_key =
            crate::ancestry_witness::reclamation_witness_manifest_idempotency_key(&covering.mk);
        let _manifest = covering
            .chain
            .push(
                Some(manifest_key),
                &domain_object(
                    crate::ancestry_witness::RECLAMATION_WITNESS_MANIFEST_DOMAIN,
                    b"garbage",
                ),
            )
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::V2BeginObserved { .. }
        ));
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::MalformedManifestPayload { .. }
            }
        ));
        // A declared second shard that is not observed.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        let mut manifest = covering.default_manifest(plan, begin);
        manifest.shard_count = 2;
        let shard = covering.default_shard();
        let _witness = covering.push_witness(&manifest, &shard);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ShardSetIncoherent { expected_count, observed }
            } if *expected_count == 2 && observed.as_slice() == [0]
        ));
        // A malformed shard payload.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        let manifest = covering.default_manifest(plan, begin);
        let manifest_key =
            crate::ancestry_witness::reclamation_witness_manifest_idempotency_key(&covering.mk);
        let _manifest_record = covering
            .chain
            .push(Some(manifest_key), &manifest.canonical_bytes())
            .0;
        let shard_key =
            crate::ancestry_witness::reclamation_witness_shard_idempotency_key(&covering.mk, 0);
        let _shard = covering
            .chain
            .push(
                Some(shard_key),
                &domain_object(
                    crate::ancestry_witness::RECLAMATION_WITNESS_SHARD_DOMAIN,
                    b"garbage",
                ),
            )
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::MalformedShardPayload { index, .. }
            } if *index == 0
        ));
    }

    // (19) Partially observed witness shards without a manifest.
    #[test]
    fn a_shard_prefix_without_a_manifest_is_observed_honestly() {
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let _begin = covering.push_begin(plan);
        let shard = covering.default_shard();
        let shard_key =
            crate::ancestry_witness::reclamation_witness_shard_idempotency_key(&covering.mk, 0);
        let _shard_record = covering
            .chain
            .push(Some(shard_key), &shard.canonical_bytes())
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::V2BeginObserved { .. }
        ));
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::ManifestAbsentWithShardEvidence { shard_indices }
                if shard_indices.as_slice() == [0]
        ));
    }

    // (20) Mixed V1/V2 evidence refusal; a begin with no source
    // evidence.
    #[test]
    fn mixed_v1_and_v2_evidence_refuses() {
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let v1_audit_key = reclamation_ledger_idempotency_key(&covering.mk);
        let v1_audit = covering
            .chain
            .push(
                Some(v1_audit_key),
                &domain_object(RECLAMATION_LEDGER_DOMAIN, b"v1-audit"),
            )
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::MixedV1AndV2Evidence { v1_record, v2_record }
            } if *v1_record == v1_audit && *v2_record == plan
        ));
        // A begin with neither a plan nor a V1 audit to bind to.
        let mut covering = covering_chain();
        let begin = covering.push_begin(crate::hash::hash_object(b"b-r1a1-nowhere"));
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::BeginSourceNotObserved { record }
            } if *record == begin
        ));
    }

    // (21) Older checkpoint with missing physical range records.
    #[test]
    fn an_unresolved_older_range_never_becomes_permission() {
        // A stacked two-checkpoint fixture: the older range's
        // members are entirely absent from the reader (an
        // unresolved partial retirement below the older MK); the
        // covering classification succeeds without touching them.
        let mut chain = ChainBuilder::founded();
        let founding = chain.tip;
        let _o1 = chain.push_domain(ORDINARY_DOMAIN, b"o1");
        let (anchor0, _) = chain.push_domain(ORDINARY_DOMAIN, b"anchor-0");
        let (sk0, _) = chain.push_domain(SNAPSHOT_DOMAIN, b"sk-0");
        let metadata0 = CheckpointMetadata::new(
            sk0,
            anchor0,
            vec![SkipEntry::self_skip(anchor0)],
            vec![crate::checkpoint::ReclaimedIndexEntry::new(
                None,
                crate::hash::hash_object(b"b-r1a1-old-m0"),
                1,
            )],
            [1u8; 32],
            compute_digest_seed(&founding),
        );
        let mk0_key = checkpoint_mk_idempotency_key(&sk0);
        let _mk0 = chain.push(Some(mk0_key), &metadata0.canonical_bytes()).0;
        let (anchor1, _) = chain.push_domain(ORDINARY_DOMAIN, b"anchor-1");
        let (sk1, _) = chain.push_domain(SNAPSHOT_DOMAIN, b"sk-1");
        let sk1_epoch = chain.tip_epoch;
        let metadata1 = CheckpointMetadata::new(
            sk1,
            anchor1,
            vec![SkipEntry::self_skip(anchor1)],
            vec![crate::checkpoint::ReclaimedIndexEntry::new(
                None, sk1, sk1_epoch,
            )],
            [3u8; 32],
            [4u8; 32],
        );
        let mk1_key = checkpoint_mk_idempotency_key(&sk1);
        let mk1 = chain.push(Some(mk1_key), &metadata1.canonical_bytes()).0;
        let plan1 = ReclaimPlan {
            mk: mk1,
            sk: sk1,
            range_digest: [3u8; 32],
            seed: [4u8; 32],
            ledger_byte_limit: 512 * 1024,
            batches: vec![PlanBatch {
                index: 0,
                records: vec![sk1],
            }],
            journal_files: Vec::new(),
        };
        let plan1_key = reclamation_plan_idempotency_key(&mk1);
        let plan_record = chain.push(Some(plan1_key), &plan1.canonical_bytes()).0;
        let audit1 = ReclamationLedgerV2 {
            mk: mk1,
            sk: sk1,
            batch: 0,
            range_digest: [3u8; 32],
            seed: [4u8; 32],
            records: 1,
            reclaimed_keys: 0,
            units: vec![ReclaimUnit {
                record: sk1,
                objects: Vec::new(),
            }],
        };
        let audit1_key = reclamation_ledger_v2_idempotency_key(&mk1, 0);
        let _audit_record = chain.push(Some(audit1_key), &audit1.canonical_bytes()).0;
        let begin_payload = ReclamationBegin {
            mk: mk1,
            source: plan_record,
            range_digest: [3u8; 32],
            seed: [4u8; 32],
        };
        let begin1_key = reclamation_begin_idempotency_key(&mk1);
        let _begin_record = chain
            .push(Some(begin1_key), &begin_payload.canonical_bytes())
            .0;
        let (head, _) = chain.push_domain(ORDINARY_DOMAIN, b"head");
        let classification = classify(&mut chain.reads, &head);
        let RecoveryState::V2BeginObserved {
            markers_not_observed,
            ..
        } = &classification.state
        else {
            panic!("expected V2BeginObserved, got {:?}", classification.state);
        };
        assert_eq!(markers_not_observed.as_slice(), &[0]);
        // The older range is explicitly unresolved — never a
        // permission, never a completion claim.
        assert_eq!(
            classification.unresolved_authority.older_checkpoints,
            OlderRangeStatus::UnresolvedBelowCoveringBoundary
        );
        let boundary = classification
            .discovery
            .boundary
            .expect("the boundary is present");
        assert_eq!(boundary.sk, sk1);
    }

    // (22) Read-only byte-map invariance.
    #[test]
    fn the_classification_is_read_only_over_the_byte_maps() {
        let mut fixture = build_fixture();
        let records_before = fixture.reads.records.clone();
        let objects_before = fixture.reads.objects.clone();
        let _ = classify(&mut fixture.reads, &fixture.head);
        assert_eq!(fixture.reads.records, records_before);
        assert_eq!(fixture.reads.objects, objects_before);
    }

    // (23) Exact resource-limit boundaries.
    #[test]
    fn resource_limit_boundaries_are_exact() {
        // The admitted plan ledger byte limit at exactly the
        // audit's encoded size passes; one byte less refuses.
        let mut covering = covering_chain();
        let audit0_probe = covering.default_audit(0);
        let mut plan_payload = covering.default_plan();
        plan_payload.ledger_byte_limit = audit0_probe.canonical_bytes().len() as u64;
        let plan = covering.push_plan(&plan_payload);
        let _audit0 = covering.push_audit(&audit0_probe);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let _begin = covering.push_begin(plan);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::V2BeginObserved { .. }
        ));
        let mut covering = covering_chain();
        let audit0_probe = covering.default_audit(0);
        let mut plan_payload = covering.default_plan();
        plan_payload.ledger_byte_limit = audit0_probe.canonical_bytes().len() as u64 - 1;
        let plan = covering.push_plan(&plan_payload);
        let _audit0 = covering.push_audit(&audit0_probe);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let _begin = covering.push_begin(plan);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::AuditByteLimitExceeded { batch, .. }
            } if *batch == 0
        ));
        // The discovery traversal bound.
        let mut fixture = build_fixture();
        let error = classify_recovery_evidence(
            &mut fixture.reads,
            &fixture.head,
            &DiscoveryLimits { max_records: 4 },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ClassificationError::Discovery(DiscoveryError::TraversalBoundExhausted { .. })
        ));
    }

    // (24) No provider startup, deletion, publication, cursor, or
    // skip side effects — the total destructuring names every
    // field and every value; no capability type exists.
    #[test]
    fn the_result_exposes_no_authoritative_capability() {
        let mut fixture = build_fixture();
        let classification = classify(&mut fixture.reads, &fixture.head);
        // The total destructure: head, discovery, state, witness,
        // unresolved_authority — there is nothing else to hold,
        // and no cursor, permit, certificate, or verdict field
        // exists anywhere in the type.
        let RecoveryClassification {
            head,
            discovery,
            state,
            witness,
            unresolved_authority,
        } = &classification;
        assert_eq!(*head, fixture.head);
        assert!(matches!(
            state,
            RecoveryState::AllBatchMarkersStructurallyMatched { .. }
        ));
        assert!(matches!(
            witness,
            WitnessEvidenceObservation::CoherentlyObserved { .. }
        ));
        // The L0 observation set is carried unchanged.
        assert_eq!(discovery.head, fixture.head);
        assert_eq!(discovery.completion_candidates.len(), 2);
        // The unresolved-authority fields are the NotEstablished
        // markers — no positive claim value exists to construct.
        let UnresolvedAuthority {
            provider_canonical_head,
            founding_derived_seed,
            original_range_digest,
            original_object_closures,
            physical_retirement,
            journal_retirement,
            provider_durability,
            older_checkpoints,
            completed_range_authority,
            startup_safety,
        } = unresolved_authority;
        let _ = (
            provider_canonical_head,
            founding_derived_seed,
            original_range_digest,
            original_object_closures,
            physical_retirement,
            journal_retirement,
            provider_durability,
            completed_range_authority,
            startup_safety,
        );
        assert_eq!(
            *older_checkpoints,
            OlderRangeStatus::UnresolvedBelowCoveringBoundary
        );
    }

    // (F1 + F3) The two-shard witness census with UNEQUAL
    // per-shard counts, published in the accepted production
    // order (the shards ascending, then the manifest LAST).
    #[test]
    fn the_two_shard_witness_census_accumulates_unequal_counts() {
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        let (manifest, shards) = covering.default_two_shard_witness(plan, begin);
        let _manifest_record = covering.push_witness_family(&manifest, &shards);
        let context0 = BatchCompletionContext {
            mk: covering.mk,
            plan,
            batch_index: 0,
            audit: audit0,
            begin,
        };
        let _marker0 = covering.push_marker(&context0, 0);
        let context1 = BatchCompletionContext {
            mk: covering.mk,
            plan,
            batch_index: 1,
            audit: audit1,
            begin,
        };
        let _marker1 = covering.push_marker(&context1, 1);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::AllBatchMarkersStructurallyMatched { .. }
        ));
        let WitnessEvidenceObservation::CoherentlyObserved {
            shard_records,
            manifest: observed_manifest,
            ..
        } = &classification.witness
        else {
            panic!(
                "expected CoherentlyObserved, got {:?}",
                classification.witness
            );
        };
        assert_eq!(observed_manifest.member_count, 3);
        assert_eq!(observed_manifest.shard_count, 2);
        assert_eq!(shard_records.len(), 2);
    }

    // (F1) The accumulated census below and above the manifest's
    // total refuses typed.
    #[test]
    fn the_witness_census_mismatch_refuses_below_and_above() {
        // Below: shard 0 carries only [sk] — the census
        // accumulates 1 + 1 = 2 against the manifest's 3.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        let (mut manifest, mut shards) = covering.default_two_shard_witness(plan, begin);
        shards[0] = AncestryWitnessShard {
            mk: covering.mk,
            shard_index: 0,
            shard_count: 2,
            member_count: 1,
            entries: vec![WitnessEntry {
                member: covering.sk,
                preimage: covering.sk_bytes.clone(),
            }],
        };
        let _ = &mut manifest;
        let _manifest_record = covering.push_witness_family(&manifest, &shards);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ShardCensusMismatch { manifest, accumulated }
            } if *manifest == 3 && *accumulated == 2
        ));
        // Above: shard 1 carries an additional genuine off-chain
        // member — the census accumulates 2 + 2 = 4 against 3.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        let (manifest, mut shards) = covering.default_two_shard_witness(plan, begin);
        let m9_record = crate::record::RevisionRecord::regular(covering.m1, 1, 1);
        let m9_bytes = m9_record.canonical_bytes();
        let m9 = crate::hash::hash_object(&m9_bytes);
        shards[1] = AncestryWitnessShard {
            mk: covering.mk,
            shard_index: 1,
            shard_count: 2,
            member_count: 2,
            entries: vec![
                WitnessEntry {
                    member: covering.m1,
                    preimage: covering.m1_bytes.clone(),
                },
                WitnessEntry {
                    member: m9,
                    preimage: m9_bytes,
                },
            ],
        };
        let _manifest_record = covering.push_witness_family(&manifest, &shards);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ShardCensusMismatch { manifest, accumulated }
            } if *manifest == 3 && *accumulated == 4
        ));
    }

    // (F2) Noncanonical shard keys refuse without normalization;
    // the key/payload-index disagreement refuses at the bindings.
    #[test]
    fn noncanonical_shard_keys_refuse_without_normalization() {
        // A leading-zero index spelling under the covering MK.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let _begin = covering.push_begin(plan);
        let leading_zero_key = Vec::from(
            format!(
                "{}:{}:00",
                RECLAMATION_WITNESS_SHARD_KEY_DOMAIN,
                covering.mk.hex()
            )
            .as_bytes(),
        );
        let _shard = covering
            .chain
            .push(
                Some(leading_zero_key),
                &domain_object(
                    crate::ancestry_witness::RECLAMATION_WITNESS_SHARD_DOMAIN,
                    b"payload",
                ),
            )
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ShardKeyNoncanonical { .. }
            }
        ));
        // A malformed index suffix.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let _begin = covering.push_begin(plan);
        let malformed_key = Vec::from(
            format!(
                "{}:{}:x",
                RECLAMATION_WITNESS_SHARD_KEY_DOMAIN,
                covering.mk.hex()
            )
            .as_bytes(),
        );
        let _shard = covering
            .chain
            .push(
                Some(malformed_key),
                &domain_object(
                    crate::ancestry_witness::RECLAMATION_WITNESS_SHARD_DOMAIN,
                    b"payload",
                ),
            )
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ShardKeyNoncanonical { .. }
            }
        ));
        // A key/payload-index disagreement: the canonical key for
        // index 1 carrying a payload that binds shard index 0 (a
        // complete two-shard set so the binding check — not the
        // set coherence — is what fires).
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        let (manifest, shards) = covering.default_two_shard_witness(plan, begin);
        let shard_zero_key =
            crate::ancestry_witness::reclamation_witness_shard_idempotency_key(&covering.mk, 0);
        let _genuine = covering
            .chain
            .push(Some(shard_zero_key), &shards[0].canonical_bytes())
            .0;
        let key_one =
            crate::ancestry_witness::reclamation_witness_shard_idempotency_key(&covering.mk, 1);
        let _disagreeing = covering
            .chain
            .push(Some(key_one), &shards[0].canonical_bytes())
            .0;
        let manifest_key =
            crate::ancestry_witness::reclamation_witness_manifest_idempotency_key(&covering.mk);
        let _manifest = covering
            .chain
            .push(Some(manifest_key), &manifest.canonical_bytes())
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ShardBindingMismatch { index, rule }
            } if *index == 1 && *rule == "bindings"
        ));
    }

    // (F3) The publication order is enforced; the degradation is
    // orthogonal (the retirement state stands).
    #[test]
    fn the_witness_publication_order_is_enforced() {
        // The manifest published BEFORE the shard.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        let manifest = covering.default_manifest(plan, begin);
        let shard = covering.default_shard();
        let manifest_key =
            crate::ancestry_witness::reclamation_witness_manifest_idempotency_key(&covering.mk);
        let _manifest = covering
            .chain
            .push(Some(manifest_key), &manifest.canonical_bytes())
            .0;
        let shard_key =
            crate::ancestry_witness::reclamation_witness_shard_idempotency_key(&covering.mk, 0);
        let _shard = covering
            .chain
            .push(Some(shard_key), &shard.canonical_bytes())
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::V2BeginObserved { .. }
        ));
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::PublicationOrderViolation { rule }
            } if *rule == "manifest-not-newest"
        ));
        // The shards published in reversed order.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        let (manifest, shards) = covering.default_two_shard_witness(plan, begin);
        let shard_one_key =
            crate::ancestry_witness::reclamation_witness_shard_idempotency_key(&covering.mk, 1);
        let _reversed = covering
            .chain
            .push(Some(shard_one_key), &shards[1].canonical_bytes())
            .0;
        let shard_zero_key =
            crate::ancestry_witness::reclamation_witness_shard_idempotency_key(&covering.mk, 0);
        let _reversed = covering
            .chain
            .push(Some(shard_zero_key), &shards[0].canonical_bytes())
            .0;
        let manifest_key =
            crate::ancestry_witness::reclamation_witness_manifest_idempotency_key(&covering.mk);
        let _manifest = covering
            .chain
            .push(Some(manifest_key), &manifest.canonical_bytes())
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::V2BeginObserved { .. }
        ));
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::PublicationOrderViolation { rule }
            } if *rule == "shards-out-of-order"
        ));
    }

    // (F4) Observed audits validate in full without a begin.
    #[test]
    fn observed_audits_validate_without_a_begin() {
        // Valid observed audits: the state stands with the exact
        // observation lists.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let _plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        let RecoveryState::PlanObservedWithoutBegin {
            audits_observed,
            audits_not_observed,
            ..
        } = &classification.state
        else {
            panic!(
                "expected PlanObservedWithoutBegin, got {:?}",
                classification.state
            );
        };
        assert_eq!(audits_observed.as_slice(), &[0, 1]);
        assert!(audits_not_observed.is_empty());
        // A malformed observed audit degrades even without a
        // begin.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let _plan = covering.push_plan(&plan_payload);
        let audit0_key = reclamation_ledger_v2_idempotency_key(&covering.mk, 0);
        let _audit0 = covering
            .chain
            .push(
                Some(audit0_key),
                &domain_object(RECLAMATION_LEDGER_V2_DOMAIN, b"garbage"),
            )
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::MalformedAuditPayload { batch, .. }
            } if *batch == 0
        ));
        // A lying audit binding (batch 1 under the batch-0 key).
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let _plan = covering.push_plan(&plan_payload);
        let mut lying = covering.default_audit(0);
        lying.batch = 1;
        let lying_key = reclamation_ledger_v2_idempotency_key(&covering.mk, 0);
        let _audit0 = covering
            .chain
            .push(Some(lying_key), &lying.canonical_bytes())
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::AuditBindingMismatch { batch, .. }
            } if *batch == 0
        ));
        // Reordered units.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let _plan = covering.push_plan(&plan_payload);
        let mut reordered = covering.default_audit(0);
        reordered.units.reverse();
        let _audit0 = covering.push_audit(&reordered);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::AuditUnitsMismatch { batch, position }
            } if *batch == 0 && *position == 0
        ));
        // The reclaimed_keys miscount.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let _plan = covering.push_plan(&plan_payload);
        let mut miscounted = covering.default_audit(0);
        miscounted.reclaimed_keys = 2;
        let _audit0 = covering.push_audit(&miscounted);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::AuditKeyedCountMismatch {
                    batch,
                    observed,
                    expected,
                }
            } if *batch == 0 && *observed == 2 && *expected == 1
        ));
        // The admitted ledger byte-limit violation.
        let mut covering = covering_chain();
        let audit0_probe = covering.default_audit(0);
        let mut plan_payload = covering.default_plan();
        plan_payload.ledger_byte_limit = audit0_probe.canonical_bytes().len() as u64 - 1;
        let _plan = covering.push_plan(&plan_payload);
        let _audit0 = covering.push_audit(&audit0_probe);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::AuditByteLimitExceeded { batch, .. }
            } if *batch == 0
        ));
    }

    // (F4) A marker requiring an unobserved begin contradicts the
    // no-begin state — never harmless incomplete publication.
    #[test]
    fn a_marker_requiring_an_unobserved_begin_contradicts_the_no_begin_state() {
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let audit1 = covering.push_audit(&audit1);
        // A genuine marker for batch 0 — but NO begin is ever
        // pushed: the discovery's structural pass cannot assemble
        // the context, and the classifier refuses the
        // contradiction.
        let context0 = BatchCompletionContext {
            mk: covering.mk,
            plan,
            batch_index: 0,
            audit: audit0,
            begin: crate::hash::hash_object(b"b-r1a1-never-pushed-begin"),
        };
        let marker = covering.push_marker(&context0, 0);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        let RecoveryState::EvidenceDegraded { reason } = &classification.state else {
            panic!("expected EvidenceDegraded, got {:?}", classification.state);
        };
        assert!(matches!(
            reason,
            EvidenceDegradation::MarkerContradicted { record, batch: _, rule }
                if *record == marker && rule == "begin-not-observed"
        ));
        let _ = audit1;
    }

    // (F5) Same-checkpoint wrong-key evidence refuses; foreign
    // evidence is never promoted.
    #[test]
    fn same_checkpoint_wrong_key_evidence_refuses() {
        // A plan payload binding the covering MK on a foreign
        // checkpoint's plan key — in the no-evidence lane.
        let mut covering = covering_chain();
        let foreign_mk = crate::hash::hash_object(b"b-r1a1-foreign-mk");
        let wrong_key = reclamation_plan_idempotency_key(&foreign_mk);
        let plan_payload = covering.default_plan();
        let wrong_plan = covering
            .chain
            .push(Some(wrong_key), &plan_payload.canonical_bytes())
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        let RecoveryState::EvidenceDegraded { reason } = &classification.state else {
            panic!("expected EvidenceDegraded, got {:?}", classification.state);
        };
        assert!(matches!(
            reason,
            EvidenceDegradation::SameCheckpointWrongKeyEvidence { record, family }
                if *record == wrong_plan && *family == "plan"
        ));
        // A begin payload binding the covering MK on a foreign
        // begin key — in the plan-without-begin lane.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let foreign_begin_key = reclamation_begin_idempotency_key(&foreign_mk);
        let begin_payload = ReclamationBegin {
            mk: covering.mk,
            source: plan,
            range_digest: covering.range_digest,
            seed: covering.seed,
        };
        let wrong_begin = covering
            .chain
            .push(Some(foreign_begin_key), &begin_payload.canonical_bytes())
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::SameCheckpointWrongKeyEvidence { record, family }
            } if *record == wrong_begin && *family == "begin"
        ));
        // An audit payload binding the covering MK on a foreign
        // audit key — in the plan-without-begin lane (a wrong-key
        // audit is never an innocent missing audit).
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let _plan = covering.push_plan(&plan_payload);
        let foreign_audit_key = reclamation_ledger_v2_idempotency_key(&foreign_mk, 0);
        let audit_payload = covering.default_audit(0);
        let wrong_audit = covering
            .chain
            .push(Some(foreign_audit_key), &audit_payload.canonical_bytes())
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::SameCheckpointWrongKeyEvidence { record, family }
            } if *record == wrong_audit && *family == "audit"
        ));
        // Legitimate foreign-checkpoint evidence (the payload
        // binding the FOREIGN MK) is never promoted: the state
        // stands as the honest absence.
        let mut covering = covering_chain();
        let foreign_plan_payload = ReclaimPlan {
            mk: foreign_mk,
            sk: covering.sk,
            range_digest: covering.range_digest,
            seed: covering.seed,
            ledger_byte_limit: 512 * 1024,
            batches: vec![PlanBatch {
                index: 0,
                records: vec![covering.sk],
            }],
            journal_files: Vec::new(),
        };
        let foreign_plan_key = reclamation_plan_idempotency_key(&foreign_mk);
        let _foreign_plan = covering
            .chain
            .push(
                Some(foreign_plan_key),
                &foreign_plan_payload.canonical_bytes(),
            )
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::NoRetirementEvidenceObserved { .. }
        ));
    }

    // (F6.3 + F6.5) A same-MK shard key missing the `:<index>`
    // suffix degrades — alone AND alongside an otherwise complete
    // witness (never CoherentlyObserved, never a silent absence).
    #[test]
    fn same_mk_shard_keys_missing_the_index_suffix_degrade() {
        // Alone.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let _begin = covering.push_begin(plan);
        let missing_suffix_key = Vec::from(
            format!(
                "{}:{}",
                RECLAMATION_WITNESS_SHARD_KEY_DOMAIN,
                covering.mk.hex()
            )
            .as_bytes(),
        );
        let _shard = covering
            .chain
            .push(
                Some(missing_suffix_key),
                &domain_object(
                    crate::ancestry_witness::RECLAMATION_WITNESS_SHARD_DOMAIN,
                    b"payload",
                ),
            )
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ShardKeyNoncanonical { .. }
            }
        ));
        // Alongside the complete two-shard witness: the malformed
        // extra shard still degrades — never CoherentlyObserved.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        let (manifest, shards) = covering.default_two_shard_witness(plan, begin);
        let missing_suffix_key = Vec::from(
            format!(
                "{}:{}",
                RECLAMATION_WITNESS_SHARD_KEY_DOMAIN,
                covering.mk.hex()
            )
            .as_bytes(),
        );
        let _malformed = covering
            .chain
            .push(
                Some(missing_suffix_key),
                &domain_object(
                    crate::ancestry_witness::RECLAMATION_WITNESS_SHARD_DOMAIN,
                    b"payload",
                ),
            )
            .0;
        let _manifest_record = covering.push_witness_family(&manifest, &shards);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ShardKeyNoncanonical { .. }
            }
        ));
    }

    // (F6.6) A same-MK manifest payload under an incorrect
    // manifest key degrades — never a silent NoneObserved.
    #[test]
    fn same_mk_manifest_payloads_under_incorrect_keys_degrade() {
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        let manifest = covering.default_manifest(plan, begin);
        let foreign_mk = crate::hash::hash_object(b"b-r1a1-f6-foreign-mk");
        let wrong_key =
            crate::ancestry_witness::reclamation_witness_manifest_idempotency_key(&foreign_mk);
        let _wrong_manifest = covering
            .chain
            .push(Some(wrong_key), &manifest.canonical_bytes())
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ManifestKeyNoncanonical { .. }
            }
        ));
        // The same-MK payload under the correct key classifies
        // normally (the control).
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        let manifest = covering.default_manifest(plan, begin);
        let shard = covering.default_shard();
        let _manifest_record = covering.push_witness(&manifest, &shard);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::CoherentlyObserved { .. }
        ));
    }

    // (F6.7 + F6.8) Foreign witness evidence stays foreign — never
    // attributed to the covering MK — and the NoneObserved result
    // is CORRECT there (the contrast: every same-MK form in the
    // other F6 regressions degrades, so no genuinely-observed
    // same-MK witness evidence ever remains NoneObserved).
    #[test]
    fn foreign_witness_evidence_is_never_attributed_to_the_covering_mk() {
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let _begin = covering.push_begin(plan);
        let foreign_mk = crate::hash::hash_object(b"b-r1a1-f6-foreign-mk");
        // A foreign shard: the payload binding the FOREIGN MK
        // under the foreign checkpoint's own shard key.
        let foreign_shard = AncestryWitnessShard {
            mk: foreign_mk,
            shard_index: 0,
            shard_count: 1,
            member_count: 1,
            entries: vec![WitnessEntry {
                member: crate::hash::hash_object(b"b-r1a1-foreign-member"),
                preimage: crate::record::RevisionRecord::regular(
                    crate::hash::hash_object(b"b-r1a1-foreign-parent"),
                    1,
                    1,
                )
                .canonical_bytes(),
            }],
        };
        let foreign_shard_key =
            crate::ancestry_witness::reclamation_witness_shard_idempotency_key(&foreign_mk, 0);
        let _foreign_shard = covering
            .chain
            .push(Some(foreign_shard_key), &foreign_shard.canonical_bytes())
            .0;
        // A foreign manifest: the payload binding the FOREIGN MK
        // under the foreign checkpoint's own manifest key.
        let foreign_manifest = AncestryWitnessManifest {
            mk: foreign_mk,
            plan_record: crate::hash::hash_object(b"b-r1a1-foreign-plan"),
            begin_record: crate::hash::hash_object(b"b-r1a1-foreign-begin"),
            metadata_object: crate::hash::hash_object(b"b-r1a1-foreign-metadata"),
            member_count: 1,
            shard_count: 1,
            coverage_digest: [7u8; 32],
        };
        let foreign_manifest_key =
            crate::ancestry_witness::reclamation_witness_manifest_idempotency_key(&foreign_mk);
        let _foreign_manifest = covering
            .chain
            .push(
                Some(foreign_manifest_key),
                &foreign_manifest.canonical_bytes(),
            )
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.state,
            RecoveryState::V2BeginObserved { .. }
        ));
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::NoneObserved
        ));
    }

    // (F7) The wrong-key scan classifies correctly across several
    // batches and interleaved foreign audit-family records,
    // independent of record order (the set-membership lookup).
    #[test]
    fn the_wrong_key_scan_classifies_across_interleaved_foreign_audits() {
        let mut covering = covering_chain();
        // A three-batch plan (the maximum partition of the
        // three-member index): [sk], [m0], [m1] — newest first.
        let mut plan_payload = covering.default_plan();
        plan_payload.batches = vec![
            PlanBatch {
                index: 0,
                records: vec![covering.sk],
            },
            PlanBatch {
                index: 1,
                records: vec![covering.m0],
            },
            PlanBatch {
                index: 2,
                records: vec![covering.m1],
            },
        ];
        let plan = covering.push_plan(&plan_payload);
        let foreign_mk = crate::hash::hash_object(b"b-r1a1-f7-foreign-mk");
        // The first foreign audit, interleaved before the plan's
        // own audits.
        let foreign_audit = ReclamationLedgerV2 {
            mk: foreign_mk,
            sk: covering.sk,
            batch: 0,
            range_digest: covering.range_digest,
            seed: covering.seed,
            records: 1,
            reclaimed_keys: 0,
            units: vec![ReclaimUnit {
                record: covering.sk,
                objects: Vec::new(),
            }],
        };
        let foreign_key = reclamation_ledger_v2_idempotency_key(&foreign_mk, 0);
        let _foreign = covering
            .chain
            .push(Some(foreign_key), &foreign_audit.canonical_bytes())
            .0;
        // The three correctly-keyed one-unit audits (sk unkeyed,
        // m0 keyed, m1 unkeyed per the retained index), with the
        // second foreign audit interleaved between them.
        let audit0 = ReclamationLedgerV2 {
            mk: covering.mk,
            sk: covering.sk,
            batch: 0,
            range_digest: covering.range_digest,
            seed: covering.seed,
            records: 1,
            reclaimed_keys: 0,
            units: vec![ReclaimUnit {
                record: covering.sk,
                objects: Vec::new(),
            }],
        };
        let _audit0 = covering.push_audit(&audit0);
        let foreign_audit_two = ReclamationLedgerV2 {
            mk: foreign_mk,
            sk: covering.sk,
            batch: 1,
            range_digest: covering.range_digest,
            seed: covering.seed,
            records: 1,
            reclaimed_keys: 0,
            units: vec![ReclaimUnit {
                record: covering.m0,
                objects: Vec::new(),
            }],
        };
        let foreign_key_two = reclamation_ledger_v2_idempotency_key(&foreign_mk, 1);
        let _foreign_two = covering
            .chain
            .push(Some(foreign_key_two), &foreign_audit_two.canonical_bytes())
            .0;
        let audit1 = ReclamationLedgerV2 {
            mk: covering.mk,
            sk: covering.sk,
            batch: 1,
            range_digest: covering.range_digest,
            seed: covering.seed,
            records: 1,
            reclaimed_keys: 1,
            units: vec![ReclaimUnit {
                record: covering.m0,
                objects: vec![crate::hash::hash_object(b"b-r1a1-m0-obj")],
            }],
        };
        let _audit1 = covering.push_audit(&audit1);
        let audit2 = ReclamationLedgerV2 {
            mk: covering.mk,
            sk: covering.sk,
            batch: 2,
            range_digest: covering.range_digest,
            seed: covering.seed,
            records: 1,
            reclaimed_keys: 0,
            units: vec![ReclaimUnit {
                record: covering.m1,
                objects: Vec::new(),
            }],
        };
        let _audit2 = covering.push_audit(&audit2);
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        // The foreign audits are not attributed (no degradation)
        // and the three correctly-keyed audits are all observed —
        // the classification is correct independent of the
        // interleaved record order.
        let RecoveryState::PlanObservedWithoutBegin {
            audits_observed,
            audits_not_observed,
            ..
        } = &classification.state
        else {
            panic!(
                "expected PlanObservedWithoutBegin, got {:?}",
                classification.state
            );
        };
        assert_eq!(audits_observed.as_slice(), &[0, 1, 2]);
        assert!(audits_not_observed.is_empty());
        let _ = plan;
    }
}
