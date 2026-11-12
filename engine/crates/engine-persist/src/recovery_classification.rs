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
                // F8: a shard-family record under a foreign-key
                // form whose strictly decoded payload binds the
                // COVERING MK is contradictory same-checkpoint
                // evidence — never a silent absence. The payload
                // is read and strictly decoded solely to test the
                // MK binding (the strict decode enforces the
                // accepted per-shard payload ceiling; one payload
                // at a time, dropped immediately); a payload
                // binding another MK remains legitimate foreign
                // evidence, and an undecodable foreign-keyed
                // payload is never attributed.
                let Some(object) = record.first_object else {
                    continue;
                };
                let Ok(bytes) = reads.read_object_bytes(&object) else {
                    continue;
                };
                if let Ok(shard) = AncestryWitnessShard::decode(&bytes)
                    && shard.mk == mk
                {
                    return WitnessEvidenceObservation::Degraded {
                        reason: WitnessDegradation::ShardKeyNoncanonical {
                            detail: String::from(
                                "a foreign-keyed shard record carries a payload \
                                 binding the covering checkpoint",
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

    // (F8) A shard-family record under a foreign-MK key form
    // whose strictly decoded payload binds the COVERING MK is
    // contradictory same-checkpoint evidence — alone (without a
    // manifest), alongside an otherwise complete canonical
    // witness, and never confused with genuine foreign evidence.
    #[test]
    fn foreign_keyed_shard_payloads_binding_the_covering_mk_degrade() {
        // Alone, without a manifest: the payload inspection fires
        // in the manifest-absent path.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let _begin = covering.push_begin(plan);
        // A genuine covering-MK shard payload pushed under a
        // FOREIGN checkpoint's shard key.
        let foreign_mk = crate::hash::hash_object(b"b-r1a1-f8-foreign-mk");
        let covering_shard = covering.default_shard();
        let foreign_key =
            crate::ancestry_witness::reclamation_witness_shard_idempotency_key(&foreign_mk, 0);
        let _contradictory = covering
            .chain
            .push(Some(foreign_key), &covering_shard.canonical_bytes())
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::Degraded {
                reason: WitnessDegradation::ShardKeyNoncanonical { .. }
            }
        ));
        // Alongside an otherwise complete canonical witness: the
        // contradictory record prevents CoherentlyObserved.
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let begin = covering.push_begin(plan);
        let (manifest, shards) = covering.default_two_shard_witness(plan, begin);
        let covering_shard_payload = shards[0].canonical_bytes();
        let foreign_key =
            crate::ancestry_witness::reclamation_witness_shard_idempotency_key(&foreign_mk, 0);
        let _contradictory = covering
            .chain
            .push(Some(foreign_key), &covering_shard_payload)
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
        // A genuine foreign-keyed, foreign-MK shard remains
        // foreign (the control — the payload binding the FOREIGN
        // MK never degrades the covering observation).
        let mut covering = covering_chain();
        let plan_payload = covering.default_plan();
        let plan = covering.push_plan(&plan_payload);
        let audit0 = covering.default_audit(0);
        let _audit0 = covering.push_audit(&audit0);
        let audit1 = covering.default_audit(1);
        let _audit1 = covering.push_audit(&audit1);
        let _begin = covering.push_begin(plan);
        let foreign_shard = AncestryWitnessShard {
            mk: foreign_mk,
            shard_index: 0,
            shard_count: 1,
            member_count: 1,
            entries: vec![WitnessEntry {
                member: crate::hash::hash_object(b"b-r1a1-f8-foreign-member"),
                preimage: crate::record::RevisionRecord::regular(
                    crate::hash::hash_object(b"b-r1a1-f8-foreign-parent"),
                    1,
                    1,
                )
                .canonical_bytes(),
            }],
        };
        let foreign_key =
            crate::ancestry_witness::reclamation_witness_shard_idempotency_key(&foreign_mk, 0);
        let _foreign = covering
            .chain
            .push(Some(foreign_key), &foreign_shard.canonical_bytes())
            .0;
        let head = covering.push_head();
        let classification = classify(&mut covering.chain.reads, &head);
        assert!(matches!(
            &classification.witness,
            WitnessEvidenceObservation::NoneObserved
        ));
    }
}

// ===================================================================
// (B-R1a2) The production-path verification package — the owner's
// 2026-11-12 GO under the standing engineering delegation: the
// classifier exercised over GENUINE published evidence through the
// production provider adapters and publication APIs (the
// bound-journal campaign, the keyed checkpoint publication, the V2
// plan/audit/begin ladder, the B-P1 ancestry witness, and the C1
// batch markers through the in-crate admitted receipt), with ZERO
// production-code changes — every fixture below is a test-side
// composition of existing public and pub(crate) seams, and nothing
// here grants, weakens, or extends any authority.
// ===================================================================

#[cfg(test)]
mod r1a2_production_tests {
    use super::*;
    use crate::ancestry_accounting::StorageRole;
    use crate::ancestry_witness::{WitnessPublicationOutcome, publish_ancestry_witness};
    use crate::batch_completion::{
        BatchCompletionContext, BatchCompletionMarkerV2, ClassificationEntry, EntryClassification,
        commit_classification, reclamation_complete_v2_idempotency_key,
    };
    use crate::checkpoint::{
        SkipEntry, chain_range_digest_step, compute_digest_seed, publish_checkpoint_mk,
    };
    use crate::medium::{AdmittedAppend, CanonicalMedium, StoredObject};
    use crate::reclaim::{
        BeginPublicationOutcome, MediumReads, ReclaimUnit, ReclamationLedger,
        create_campaign_bound_journal, find_installed_plan, find_installed_v2_audit,
        journal_root_id, publish_journal_root_binding, publish_reclamation_ledger,
        publish_reclamation_ledger_v2, publish_reclamation_plan,
    };
    use crate::record::{AuthorityMode, RevisionRecord};

    // ------------------------------------------------------ the stores

    fn temp_path(prefix: &str, label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "s7-r1a2-{prefix}-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn plain_object(payload: &[u8]) -> StoredObject {
        StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(payload);
            enc.into_bytes()
        })
        .unwrap()
    }

    fn sqlite_store(label: &str) -> (std::path::PathBuf, crate::SqliteMedium) {
        let path = temp_path("sqlite", label).with_extension("db");
        let medium = crate::SqliteMedium::open(&path).unwrap();
        (path, medium)
    }

    fn cleanup(paths: &[std::path::PathBuf]) {
        for path in paths {
            let _ = std::fs::remove_dir_all(path);
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
            let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
        }
    }

    /// The complete sorted file inventory of a store root (every
    /// file with its size) — the read-only proof's observation
    /// subject. Unix-only like its sole caller (the LocalFile
    /// read-only proof): a Windows build must not carry the orphan.
    #[cfg(unix)]
    fn store_inventory(root: &std::path::Path) -> Vec<(std::path::PathBuf, u64)> {
        fn walk(dir: &std::path::Path, files: &mut Vec<(std::path::PathBuf, u64)>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, files);
                } else {
                    files.push((path, entry.metadata().unwrap().len()));
                }
            }
        }
        let mut files = Vec::new();
        walk(root, &mut files);
        files.sort();
        files
    }

    // ------------------------------------------------ the campaign facts

    /// Phase A's products: the founding, p1, the two keyed retiring
    /// members, SK, and the shared content object's identity.
    struct StagedPrefix {
        founding: Hash,
        p1: Hash,
        members: Vec<Hash>,
        sk: Hash,
        shared_object: Hash,
    }

    /// The staged V2 campaign's retained facts — every identity a
    /// later phase or assertion needs, all from genuine production
    /// returns.
    struct StagedCampaign {
        members: Vec<Hash>,
        sk: Hash,
        shared_object: Hash,
        mk: Hash,
        plan_record: Hash,
        plan: ReclaimPlan,
        audits: Vec<Hash>,
        begin: Hash,
    }

    fn assemble_campaign(
        prefix: StagedPrefix,
        mk: Hash,
        plan_record: Hash,
        plan: ReclaimPlan,
        audits: Vec<Hash>,
        begin: Hash,
    ) -> StagedCampaign {
        StagedCampaign {
            members: prefix.members,
            sk: prefix.sk,
            shared_object: prefix.shared_object,
            mk,
            plan_record,
            plan,
            audits,
            begin,
        }
    }

    // ------------------------------------------------- the staged phases

    /// (Phase A) The founding, the bound journal with its root
    /// binding, p1, two keyed retiring members, and SK — the bp1
    /// fixture shape, entirely through the production append path.
    fn stage_pre_checkpoint(
        medium: &mut dyn CanonicalMedium,
        label: &str,
        journal_root: &std::path::Path,
    ) -> StagedPrefix {
        let shared = plain_object(b"r1a2-shared-payload");
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, label, 0)
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        let founding_id = founding.id();
        let location = crate::reclaim::JournalRootBinding::normalize_location(journal_root);
        let root_id = journal_root_id(&founding_id, &location);
        drop(create_campaign_bound_journal(medium, journal_root, &founding_id).unwrap());
        let (_binding, _outcome) =
            publish_journal_root_binding(medium, &founding_id, &root_id, &location, 1).unwrap();
        let step = |medium: &mut dyn CanonicalMedium, tick: u64| -> Hash {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record =
                RevisionRecord::regular(head, epoch, tick).with_content_refs(vec![shared.hash()]);
            medium
                .append(&record, std::slice::from_ref(&shared), Some(head))
                .unwrap();
            record.id()
        };
        let p1 = step(medium, 2);
        let mut members = Vec::new();
        for index in 0..2u32 {
            let object = plain_object(format!("r1a2-{label}-payload-{index}").as_bytes());
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = RevisionRecord::regular(head, epoch, 3)
                .with_idempotency_key(format!("r1a2-{label}-member-{index}").into_bytes())
                .with_content_refs(vec![object.hash()]);
            medium
                .append(&record, std::slice::from_ref(&object), Some(head))
                .unwrap();
            members.push(record.id());
        }
        let sk = step(medium, 4);
        StagedPrefix {
            founding: founding_id,
            p1,
            members,
            sk,
            shared_object: shared.hash(),
        }
    }

    /// (Phase B) The checkpoint metadata over the actual chain and
    /// the KEYED MK publication — the production route
    /// (`publish_checkpoint_mk` installs the deterministic
    /// checkpoint key the classifier's L2 covering check requires;
    /// an unkeyed hand-appended MK would never classify as
    /// covering).
    fn stage_checkpoint(
        medium: &mut dyn CanonicalMedium,
        prefix: &StagedPrefix,
        tick: u64,
    ) -> (Hash, CheckpointMetadata) {
        let sk_record = medium.read_record(&prefix.sk).unwrap();
        let seed = compute_digest_seed(&prefix.founding);
        let mut digest = seed;
        let mut index = Vec::with_capacity(prefix.members.len() + 1);
        for id in &prefix.members {
            let record = medium.read_record(id).unwrap();
            digest = chain_range_digest_step(&digest, &record.canonical_bytes());
            index.push(ReclaimedIndexEntry::new(
                record.idempotency_key.clone(),
                *id,
                record.epoch,
            ));
        }
        index.push(ReclaimedIndexEntry::new(
            sk_record.idempotency_key.clone(),
            prefix.sk,
            sk_record.epoch,
        ));
        let sk_parent = sk_record.parent.expect("the SK is a regular revision");
        let metadata = CheckpointMetadata::new(
            prefix.sk,
            sk_parent,
            vec![SkipEntry::new(Hash::ZERO, prefix.p1)],
            index,
            digest,
            seed,
        );
        // (bp1's F1 structural assertions, restated over this
        // campaign: the digest recomputes over the members
        // EXCLUDING SK.)
        let mut recomputed = seed;
        for id in &prefix.members {
            let bytes = medium.read_record(id).unwrap().canonical_bytes();
            recomputed = chain_range_digest_step(&recomputed, &bytes);
        }
        assert_eq!(metadata.range_digest, recomputed);
        let (mk, _outcome) =
            publish_checkpoint_mk(medium, prefix.sk, sk_record.epoch, &metadata, tick).unwrap();
        (mk, metadata)
    }

    /// (Phase C) The V2 plan publication with the authoritative
    /// read-back (the plan RECORD identity is what the begin and
    /// the markers bind to).
    fn stage_plan(
        medium: &mut dyn CanonicalMedium,
        mk: &Hash,
        journal_root: &std::path::Path,
        ledger_byte_limit: u64,
        tick: u64,
    ) -> (Hash, ReclaimPlan) {
        let (_candidate, _outcome) =
            publish_reclamation_plan(medium, mk, ledger_byte_limit, journal_root, tick).unwrap();
        let (plan_record, plan) = find_installed_plan(medium, mk).unwrap().unwrap();
        (plan_record, plan)
    }

    /// (Phase D) One audit per plan batch — each publication's
    /// returned record identity verified through the authoritative
    /// read-back.
    fn stage_audits(
        medium: &mut dyn CanonicalMedium,
        mk: &Hash,
        plan: &ReclaimPlan,
        tick: u64,
    ) -> Vec<Hash> {
        let mut audits = Vec::with_capacity(plan.batches.len());
        for batch in &plan.batches {
            let (audit, _outcome) =
                publish_reclamation_ledger_v2(medium, mk, batch.index, tick).unwrap();
            let (installed, ledger) = find_installed_v2_audit(medium, mk, batch.index)
                .unwrap()
                .unwrap();
            assert_eq!(
                audit, installed,
                "the audit publication installs exactly the returned record"
            );
            assert_eq!(ledger.batch, batch.index);
            audits.push(audit);
        }
        audits
    }

    /// (Phase E) The V2 begin — the plan-bound admission.
    fn stage_begin(medium: &mut dyn CanonicalMedium, mk: &Hash, tick: u64) -> Hash {
        let outcome = medium.begin_reclamation_v2(mk, tick).unwrap();
        let BeginPublicationOutcome::Accepted { id, .. } = outcome else {
            panic!("the production begin over the genuine campaign must be Accepted")
        };
        id
    }

    /// (Phase F) The B-P1 ancestry witness publication.
    fn stage_witness(medium: &mut dyn CanonicalMedium, mk: &Hash, tick: u64) -> Hash {
        let publication = publish_ancestry_witness(medium, mk, tick).unwrap();
        assert!(
            matches!(
                publication.manifest_outcome,
                WitnessPublicationOutcome::Accepted { .. }
            ),
            "the genuine campaign publishes a fresh witness"
        );
        publication.manifest_record
    }

    /// The C1 marker's classification entries — genuine campaign
    /// identities (SK and the keyed members as records, the
    /// founding's shared content as deferred storage).
    fn campaign_entries(campaign: &StagedCampaign) -> Vec<ClassificationEntry> {
        let mut entries = vec![
            ClassificationEntry {
                identity: campaign.sk,
                role: StorageRole::Record,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            },
            ClassificationEntry {
                identity: campaign.shared_object,
                role: StorageRole::Object,
                classification: EntryClassification::Deferred,
                conditional_on_prior_batch: false,
            },
        ];
        entries.extend(campaign.members.iter().map(|id| ClassificationEntry {
            identity: *id,
            role: StorageRole::Record,
            classification: EntryClassification::Mandatory,
            conditional_on_prior_batch: false,
        }));
        entries
    }

    /// The in-crate receipt installation (the same seam the V1
    /// publication rides): a regular revision under the supplied
    /// deterministic key, carrying the payload as its single
    /// content object, appended above the current head.
    fn install_keyed_object_record(
        medium: &mut dyn CanonicalMedium,
        key: Vec<u8>,
        payload: Vec<u8>,
        tick: u64,
    ) -> Hash {
        let object = StoredObject::new(payload).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let record = RevisionRecord::regular(head, epoch, tick)
            .with_idempotency_key(key)
            .with_content_refs(vec![object.hash()]);
        let id = record.id();
        medium
            .append_admitted(
                AdmittedAppend::generic(&record, std::slice::from_ref(&object)),
                Some(head),
            )
            .unwrap();
        id
    }

    /// The marker installation shared by the honest and the
    /// adversarial forms: the record is ALWAYS keyed by the
    /// (mk, key_batch) deterministic key while the payload commits
    /// the SUPPLIED context (the honest markers pass the matching
    /// context; the lying form binds a mutated one).
    fn install_marker(
        medium: &mut dyn CanonicalMedium,
        campaign: &StagedCampaign,
        context: &BatchCompletionContext,
        key_batch: u64,
        tick: u64,
    ) -> Hash {
        let commitment = commit_classification(context, campaign_entries(campaign)).unwrap();
        let marker = BatchCompletionMarkerV2 {
            mk: context.mk,
            plan: context.plan,
            batch_index: context.batch_index,
            audit: context.audit,
            begin: context.begin,
            commitment: commitment.digest,
            counters: commitment.counters,
        };
        install_keyed_object_record(
            medium,
            reclamation_complete_v2_idempotency_key(&campaign.mk, key_batch),
            marker.canonical_bytes(),
            tick,
        )
    }

    /// (Phase G) One honest C1 batch marker per plan batch — the
    /// genuine context bindings committed over the campaign's
    /// entry set and installed through the in-crate admitted
    /// receipt (the only /2 installation route; no /2 publication
    /// route exists).
    fn stage_marker(
        medium: &mut dyn CanonicalMedium,
        campaign: &StagedCampaign,
        batch: u64,
        tick: u64,
    ) -> Hash {
        let context = BatchCompletionContext {
            mk: campaign.mk,
            plan: campaign.plan_record,
            batch_index: batch,
            audit: campaign.audits[batch as usize],
            begin: campaign.begin,
        };
        install_marker(medium, campaign, &context, batch, tick)
    }

    // -------------------------------------------- the observation helpers

    fn classify_via_medium(medium: &mut dyn CanonicalMedium) -> RecoveryClassification {
        let head = medium.head().unwrap().unwrap();
        let mut reads = MediumReads::new(medium);
        classify_recovery_evidence(&mut reads, &head, &DiscoveryLimits::default_limits()).unwrap()
    }

    fn discover_via_medium(medium: &mut dyn CanonicalMedium) -> RecoveryEvidenceDiscovery {
        let head = medium.head().unwrap().unwrap();
        let mut reads = MediumReads::new(medium);
        discover_recovery_evidence(&mut reads, &head, &DiscoveryLimits::default_limits()).unwrap()
    }

    /// The ledger byte limit that splits the three-unit retirement
    /// range into multiple single-unit batches (the CP2 campaign's
    /// measured value over the identical shape).
    const R1A2_LEDGER_BYTE_LIMIT: u64 = 400;

    /// The complete staged V2 campaign with a classification
    /// checkpoint after EVERY phase — the production publication
    /// ladder observed head-relative at each rung, from
    /// NoCoveringCheckpoint to AllBatchMarkersStructurallyMatched.
    /// Returns the campaign facts (the terminal state: every marker
    /// installed).
    fn staged_production_ladder(
        medium: &mut dyn CanonicalMedium,
        label: &str,
        journal_root: &std::path::Path,
    ) -> StagedCampaign {
        // ---- Phase A: founding → journal → p1 → members → SK ----
        let prefix = stage_pre_checkpoint(medium, label, journal_root);
        let classification = classify_via_medium(medium);
        assert!(
            matches!(classification.state, RecoveryState::NoCoveringCheckpoint),
            "the pre-checkpoint campaign has no covering checkpoint"
        );
        assert!(matches!(
            classification.witness,
            WitnessEvidenceObservation::NoneObserved
        ));
        assert_eq!(
            classification.unresolved_authority.older_checkpoints,
            OlderRangeStatus::NoCoveringCheckpointObserved
        );

        // ---- Phase B: the keyed MK ----
        let (mk, _metadata) = stage_checkpoint(medium, &prefix, 5);
        let classification = classify_via_medium(medium);
        let RecoveryState::NoRetirementEvidenceObserved { covering } = classification.state else {
            panic!("after the MK publication the state must be NoRetirementEvidenceObserved")
        };
        assert_eq!(covering.record, mk);
        assert_eq!(covering.metadata.sk, prefix.sk);
        assert_eq!(
            classification.unresolved_authority.older_checkpoints,
            OlderRangeStatus::UnresolvedBelowCoveringBoundary
        );

        // ---- Phase C: the plan ----
        let (plan_record, plan) = stage_plan(medium, &mk, journal_root, R1A2_LEDGER_BYTE_LIMIT, 6);
        let n = plan.batches.len();
        assert!(
            n >= 2,
            "the ledger byte limit must split the range into at least two batches: {n}"
        );
        let classification = classify_via_medium(medium);
        let RecoveryState::PlanObservedWithoutBegin {
            plan: observed,
            audits_observed,
            audits_not_observed,
            ..
        } = classification.state
        else {
            panic!("after the plan publication the state must be PlanObservedWithoutBegin")
        };
        assert_eq!(observed.record, plan_record);
        assert_eq!(audits_observed, Vec::<u64>::new());
        assert_eq!(audits_not_observed, (0..n as u64).collect::<Vec<u64>>());

        // ---- Phase D: every audit ----
        let audits = stage_audits(medium, &mk, &plan, 7);
        let classification = classify_via_medium(medium);
        let RecoveryState::PlanObservedWithoutBegin {
            audits_observed,
            audits_not_observed,
            ..
        } = classification.state
        else {
            panic!("after every audit the state must remain PlanObservedWithoutBegin")
        };
        assert_eq!(audits_observed, (0..n as u64).collect::<Vec<u64>>());
        assert_eq!(audits_not_observed, Vec::<u64>::new());

        // ---- Phase E: the begin ----
        let begin = stage_begin(medium, &mk, 8);
        let classification = classify_via_medium(medium);
        let RecoveryState::V2BeginObserved {
            begin: observed_begin,
            markers_not_observed,
            ..
        } = classification.state
        else {
            panic!("after the begin the state must be V2BeginObserved")
        };
        assert_eq!(observed_begin.record, begin);
        assert_eq!(observed_begin.begin.source, plan_record);
        assert_eq!(markers_not_observed, (0..n as u64).collect::<Vec<u64>>());

        let campaign = assemble_campaign(prefix, mk, plan_record, plan, audits, begin);

        // ---- Phase F: the witness ----
        let manifest_record = stage_witness(medium, &mk, 9);
        let classification = classify_via_medium(medium);
        assert!(
            matches!(classification.state, RecoveryState::V2BeginObserved { .. }),
            "the witness publication does not move the recovery state"
        );
        let WitnessEvidenceObservation::CoherentlyObserved {
            record,
            manifest,
            shard_records,
        } = classification.witness
        else {
            panic!("the genuine witness publication must classify CoherentlyObserved")
        };
        assert_eq!(record, manifest_record);
        assert_eq!(manifest.member_count, 3);
        assert_eq!(manifest.plan_record, plan_record);
        assert_eq!(manifest.begin_record, begin);
        assert_eq!(shard_records.len(), manifest.shard_count as usize);
        assert!(manifest.shard_count >= 1);

        // ---- Phase G: the markers, ascending batch order ----
        stage_marker(medium, &campaign, 0, 10);
        let classification = classify_via_medium(medium);
        let RecoveryState::SomeBatchMarkersObserved {
            markers_structurally_matched,
            markers_not_observed,
            ..
        } = classification.state
        else {
            panic!("after the first marker the state must be SomeBatchMarkersObserved")
        };
        assert_eq!(markers_structurally_matched, vec![0u64]);
        assert_eq!(markers_not_observed, (1..n as u64).collect::<Vec<u64>>());
        for batch in 1..n as u64 {
            stage_marker(medium, &campaign, batch, 10 + batch);
        }
        let classification = classify_via_medium(medium);
        let RecoveryState::AllBatchMarkersStructurallyMatched {
            markers_structurally_matched,
            ..
        } = classification.state
        else {
            panic!("after every marker the state must be AllBatchMarkersStructurallyMatched")
        };
        assert_eq!(
            markers_structurally_matched,
            (0..n as u64).rev().collect::<Vec<u64>>(),
            "the matched list is in discovery order (newest first = descending batch)"
        );
        assert!(matches!(
            classification.witness,
            WitnessEvidenceObservation::CoherentlyObserved { .. }
        ));
        campaign
    }

    // ======================================================= the tests

    /// (B-R1a2 §1) The staged production publication classifies per
    /// phase under SQLite — the full ladder, every rung asserted.
    #[test]
    fn the_sqlite_staged_production_publication_classifies_per_phase() {
        let (path, mut store) = sqlite_store("t1");
        let journal = temp_path("journal", "t1");
        staged_production_ladder(&mut store, "t1", &journal);
        drop(store);
        cleanup(&[path, journal]);
    }

    /// (B-R1a2 §2) The same staged ladder under POSIX LocalFile —
    /// the provider parity over identical publication APIs.
    #[cfg(unix)]
    #[test]
    fn the_localfile_staged_production_publication_classifies_per_phase() {
        let root = temp_path("store", "t2");
        std::fs::create_dir_all(&root).unwrap();
        let mut medium = crate::LocalFileMedium::open(&root).unwrap();
        let journal = temp_path("journal", "t2");
        staged_production_ladder(&mut medium, "t2", &journal);
        drop(medium);
        cleanup(&[root, journal]);
    }

    /// (B-R1a2 §3) The markers classify when installed over the
    /// WITNESS-LESS terminal shape (the more common production
    /// form): Some → All across the ascending installations, the
    /// witness observation staying None throughout.
    #[test]
    fn the_production_markers_classify_when_installed() {
        let (path, mut store) = sqlite_store("t3");
        let medium: &mut dyn CanonicalMedium = &mut store;
        let journal = temp_path("journal", "t3");
        let prefix = stage_pre_checkpoint(medium, "t3", &journal);
        let (mk, _metadata) = stage_checkpoint(medium, &prefix, 5);
        let (plan_record, plan) = stage_plan(medium, &mk, &journal, R1A2_LEDGER_BYTE_LIMIT, 6);
        let n = plan.batches.len();
        assert!(n >= 2, "the multi-batch plan is required: {n}");
        let audits = stage_audits(medium, &mk, &plan, 7);
        let begin = stage_begin(medium, &mk, 8);
        let campaign = assemble_campaign(prefix, mk, plan_record, plan, audits, begin);
        for batch in 0..n as u64 {
            stage_marker(medium, &campaign, batch, 10 + batch);
            let classification = classify_via_medium(medium);
            if batch + 1 < n as u64 {
                assert!(
                    matches!(
                        classification.state,
                        RecoveryState::SomeBatchMarkersObserved { .. }
                    ),
                    "batch {batch}: the partial marker set is SomeBatchMarkersObserved"
                );
            } else {
                assert!(
                    matches!(
                        classification.state,
                        RecoveryState::AllBatchMarkersStructurallyMatched { .. }
                    ),
                    "batch {batch}: the complete marker set is AllBatchMarkersStructurallyMatched"
                );
            }
            assert!(matches!(
                classification.witness,
                WitnessEvidenceObservation::NoneObserved
            ));
        }
        drop(store);
        cleanup(&[path, journal]);
    }

    /// (B-R1a2 §4) A correctly keyed but context-lying marker (the
    /// payload binding ANOTHER batch's audit) degrades the evidence
    /// with the C2 reader's typed context refusal — while the
    /// witness observation stays coherent (the orthogonality).
    #[test]
    fn the_production_lying_marker_degrades() {
        let (path, mut store) = sqlite_store("t4");
        let medium: &mut dyn CanonicalMedium = &mut store;
        let journal = temp_path("journal", "t4");
        let prefix = stage_pre_checkpoint(medium, "t4", &journal);
        let (mk, _metadata) = stage_checkpoint(medium, &prefix, 5);
        let (plan_record, plan) = stage_plan(medium, &mk, &journal, R1A2_LEDGER_BYTE_LIMIT, 6);
        let n = plan.batches.len();
        assert!(n >= 2, "the lying form needs a second batch's audit: {n}");
        let audits = stage_audits(medium, &mk, &plan, 7);
        let begin = stage_begin(medium, &mk, 8);
        stage_witness(medium, &mk, 9);
        let campaign = assemble_campaign(prefix, mk, plan_record, plan, audits, begin);
        for batch in 1..n as u64 {
            stage_marker(medium, &campaign, batch, 10 + batch);
        }
        // Keyed for batch 0 while the payload binds batch 1's audit.
        let lying = BatchCompletionContext {
            mk: campaign.mk,
            plan: campaign.plan_record,
            batch_index: 0,
            audit: campaign.audits[1],
            begin: campaign.begin,
        };
        let lying_record = install_marker(medium, &campaign, &lying, 0, 20);
        let classification = classify_via_medium(medium);
        assert_eq!(
            classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::MarkerContradicted {
                    record: lying_record,
                    batch: Some(0),
                    rule: String::from("batch-candidate/context-mismatch"),
                }
            }
        );
        assert!(matches!(
            classification.witness,
            WitnessEvidenceObservation::CoherentlyObserved { .. }
        ));
        drop(store);
        cleanup(&[path, journal]);
    }

    /// (B-R1a2 §5) The F8 payload-inspecting foreign-key arm over
    /// production evidence: a shard record keyed for a FOREIGN
    /// checkpoint but carrying a GENUINE published shard payload
    /// (binding the covering MK) degrades the witness observation,
    /// while the retirement-evidence state is untouched.
    #[test]
    fn the_foreign_keyed_covering_bound_shard_degrades_over_production_evidence() {
        let (path, mut store) = sqlite_store("t5");
        let medium: &mut dyn CanonicalMedium = &mut store;
        let journal = temp_path("journal", "t5");
        staged_production_ladder(medium, "t5", &journal);
        let classification = classify_via_medium(medium);
        let WitnessEvidenceObservation::CoherentlyObserved { shard_records, .. } =
            classification.witness
        else {
            panic!("the terminal campaign carries a coherent witness")
        };
        let shard_object = medium.read_record(&shard_records[0]).unwrap().content_refs[0];
        let shard_bytes = medium.read_object(&shard_object).unwrap();
        let foreign_mk = crate::hash::hash_object(b"r1a2-foreign-checkpoint");
        install_keyed_object_record(
            medium,
            crate::ancestry_witness::reclamation_witness_shard_idempotency_key(&foreign_mk, 0),
            shard_bytes,
            30,
        );
        let classification = classify_via_medium(medium);
        let WitnessEvidenceObservation::Degraded {
            reason: WitnessDegradation::ShardKeyNoncanonical { detail },
        } = &classification.witness
        else {
            panic!("the foreign-keyed covering-bound shard must degrade the witness observation")
        };
        assert!(
            detail.contains("foreign-keyed"),
            "the F8 arm fired (not another noncanonical form): {detail}"
        );
        assert!(
            matches!(
                classification.state,
                RecoveryState::AllBatchMarkersStructurallyMatched { .. }
            ),
            "the retirement-evidence state is untouched by the witness degradation"
        );
        drop(store);
        cleanup(&[path, journal]);
    }

    /// (B-R1a2 §6) The witness-degraded shapes over genuine
    /// production evidence under LocalFile: (a) the manifestless
    /// partial (the manifest record's file removed, the HEAD
    /// rewound to the last installed shard — the crash shape)
    /// classifies ManifestAbsentWithShardEvidence and the resumed
    /// publication restores coherence; (b) the manifest-present
    /// corruption (the object bytes rewritten behind the open
    /// medium's back) degrades with the typed unreadability — the
    /// retirement state staying V2BeginObserved throughout (the
    /// orthogonality).
    #[cfg(unix)]
    #[test]
    fn the_production_witness_degradations_classify() {
        let root = temp_path("store", "t6");
        std::fs::create_dir_all(&root).unwrap();
        let mut medium = crate::LocalFileMedium::open(&root).unwrap();
        let journal = temp_path("journal", "t6");
        let prefix = stage_pre_checkpoint(&mut medium, "t6", &journal);
        let (mk, _metadata) = stage_checkpoint(&mut medium, &prefix, 5);
        let (_plan_record, plan) =
            stage_plan(&mut medium, &mk, &journal, R1A2_LEDGER_BYTE_LIMIT, 6);
        stage_audits(&mut medium, &mk, &plan, 7);
        stage_begin(&mut medium, &mk, 8);
        let publication = publish_ancestry_witness(&mut medium, &mk, 9).unwrap();
        // (a) The manifestless partial: remove the manifest record
        // and rewind the HEAD to the last installed shard (bp1's
        // crash-shape construction, the production entry untouched).
        let last_shard = publication
            .shard_outcomes
            .iter()
            .rev()
            .find_map(|outcome| match outcome {
                WitnessPublicationOutcome::Accepted { record, .. }
                | WitnessPublicationOutcome::DuplicateVerified { record } => Some(*record),
                _ => None,
            })
            .expect("at least one installed shard");
        let shard_count = publication.shard_outcomes.len() as u64;
        std::fs::remove_file(medium.object_path_pub(&publication.manifest_record)).unwrap();
        {
            let mut enc = engine_core::Encoder::new();
            enc.write_str(crate::local::HEAD_DOMAIN);
            enc.write_u32(crate::local::HEAD_VERSION);
            enc.write_bytes(last_shard.as_bytes());
            std::fs::write(root.join("HEAD"), enc.into_bytes()).unwrap();
        }
        let classification = classify_via_medium(&mut medium);
        let WitnessEvidenceObservation::ManifestAbsentWithShardEvidence { shard_indices } =
            &classification.witness
        else {
            panic!("the manifestless shard prefix must classify ManifestAbsentWithShardEvidence")
        };
        assert_eq!(*shard_indices, (0..shard_count).collect::<Vec<u64>>());
        assert!(
            matches!(classification.state, RecoveryState::V2BeginObserved { .. }),
            "the retirement state is untouched by the manifestless witness shape"
        );
        // The resumed publication restores coherence (the B-P1
        // reconciliation over the same evidence).
        let resumed = publish_ancestry_witness(&mut medium, &mk, 10).unwrap();
        assert!(matches!(
            resumed.manifest_outcome,
            WitnessPublicationOutcome::Accepted { .. }
        ));
        let classification = classify_via_medium(&mut medium);
        assert!(matches!(
            classification.witness,
            WitnessEvidenceObservation::CoherentlyObserved { .. }
        ));
        // (b) The manifest-present corruption: the manifest object's
        // bytes rewritten behind the open medium's back — the L0
        // discovery walk itself refuses with the typed
        // object-corruption error (the walk hash-verifies every
        // consulted record's first content object, so the corrupted
        // manifest never reaches the witness classification).
        let manifest_object = medium
            .read_record(&resumed.manifest_record)
            .unwrap()
            .content_refs[0];
        std::fs::write(
            medium.object_path_pub(&manifest_object),
            b"corrupted-manifest-bytes",
        )
        .unwrap();
        let head = medium.head().unwrap().unwrap();
        let mut reads = MediumReads::new(&mut medium);
        let error =
            classify_recovery_evidence(&mut reads, &head, &DiscoveryLimits::default_limits())
                .unwrap_err();
        assert!(
            matches!(
                &error,
                ClassificationError::Discovery(DiscoveryError::EvidenceObjectCorrupt {
                    record,
                    object,
                }) if *record == resumed.manifest_record && *object == manifest_object
            ),
            "the corrupted manifest object refuses at the L0 walk: {error:?}"
        );
        drop(medium);
        cleanup(&[root, journal]);
    }

    /// (B-R1a2 §7) The interrupted pre-begin shape: the plan with a
    /// PARTIAL audit set (every batch except the last) stays
    /// PlanObservedWithoutBegin with the exact per-batch split.
    #[test]
    fn the_interrupted_pre_begin_shape_classifies() {
        let (path, mut store) = sqlite_store("t7");
        let medium: &mut dyn CanonicalMedium = &mut store;
        let journal = temp_path("journal", "t7");
        let prefix = stage_pre_checkpoint(medium, "t7", &journal);
        let (mk, _metadata) = stage_checkpoint(medium, &prefix, 5);
        let (_plan_record, plan) = stage_plan(medium, &mk, &journal, R1A2_LEDGER_BYTE_LIMIT, 6);
        let n = plan.batches.len();
        assert!(
            n >= 2,
            "the partial-audit shape needs multiple batches: {n}"
        );
        for batch in &plan.batches {
            if batch.index == n as u64 - 1 {
                continue;
            }
            let (_audit, _outcome) =
                publish_reclamation_ledger_v2(medium, &mk, batch.index, 7).unwrap();
        }
        let classification = classify_via_medium(medium);
        let RecoveryState::PlanObservedWithoutBegin {
            audits_observed,
            audits_not_observed,
            ..
        } = classification.state
        else {
            panic!("the interrupted pre-begin shape must stay PlanObservedWithoutBegin")
        };
        assert_eq!(audits_observed, (0..n as u64 - 1).collect::<Vec<u64>>());
        assert_eq!(audits_not_observed, vec![n as u64 - 1]);
        drop(store);
        cleanup(&[path, journal]);
    }

    /// (B-R1a2 §8) The V1 legacy production lane: a genuine V1
    /// ledger (constructed over the campaign's actual retirement
    /// index, newest-first, with the read-back content references)
    /// plus the V1 begin (bound to the audit) classifies
    /// V1EvidenceObserved with the exact record identities.
    #[test]
    fn the_v1_legacy_production_lane_classifies() {
        let (path, mut store) = sqlite_store("t8");
        let medium: &mut dyn CanonicalMedium = &mut store;
        let journal = temp_path("journal", "t8");
        let prefix = stage_pre_checkpoint(medium, "t8", &journal);
        let (mk, metadata) = stage_checkpoint(medium, &prefix, 5);
        // The V1 ledger over the actual index, newest-first (SK,
        // then the members in reverse construction order).
        let mut units: Vec<ReclaimUnit> = Vec::new();
        let mut keyed = 0u64;
        let (unit, is_keyed) = v1_unit(medium, &prefix.sk);
        units.push(unit);
        keyed += is_keyed as u64;
        for id in prefix.members.iter().rev() {
            let (unit, is_keyed) = v1_unit(medium, id);
            units.push(unit);
            keyed += is_keyed as u64;
        }
        let ledger = ReclamationLedger {
            mk,
            sk: prefix.sk,
            range_digest: metadata.range_digest,
            seed: metadata.seed,
            records: units.len() as u64,
            reclaimed_keys: keyed,
            journal_files: Vec::new(),
            units,
        };
        let (v1_audit, _outcome) = publish_reclamation_ledger(medium, &ledger, 6).unwrap();
        let outcome = medium.begin_reclamation(&v1_audit, &mk, 7).unwrap();
        let BeginPublicationOutcome::Accepted { id: begin, .. } = outcome else {
            panic!("the V1 begin over the genuine ledger must be Accepted")
        };
        let classification = classify_via_medium(medium);
        let RecoveryState::V1EvidenceObserved {
            v1_audit: observed_audit,
            begin: observed_begin,
            ..
        } = classification.state
        else {
            panic!("the V1 legacy lane must classify V1EvidenceObserved")
        };
        assert_eq!(observed_audit, v1_audit);
        let Some(observed) = observed_begin else {
            panic!("the V1 begin must be observed")
        };
        assert_eq!(observed.record, begin);
        assert_eq!(observed.begin.source, v1_audit);
        drop(store);
        cleanup(&[path, journal]);
    }

    /// One V1 reclaim unit read back from the actual record (the
    /// genuine content references), with its keyed-ness.
    fn v1_unit(medium: &mut dyn CanonicalMedium, id: &Hash) -> (ReclaimUnit, bool) {
        let record = medium.read_record(id).unwrap();
        let keyed = record.idempotency_key.is_some();
        (
            ReclaimUnit {
                record: *id,
                objects: record.content_refs.clone(),
            },
            keyed,
        )
    }

    /// (B-R1a2 §9) On Windows the LocalFile begin admission refuses
    /// the platform tier (the typed
    /// `v2-begin-admission/tier-insufficient`) before any begin
    /// exists — the classification truthfully stays
    /// PlanObservedWithoutBegin with every audit observed.
    #[cfg(windows)]
    #[test]
    fn the_windows_localfile_tier_refusal_stands() {
        let root = temp_path("store", "t9");
        std::fs::create_dir_all(&root).unwrap();
        let mut medium = crate::LocalFileMedium::open(&root).unwrap();
        let journal = temp_path("journal", "t9");
        let prefix = stage_pre_checkpoint(&mut medium, "t9", &journal);
        let (mk, _metadata) = stage_checkpoint(&mut medium, &prefix, 5);
        let (_plan_record, plan) =
            stage_plan(&mut medium, &mk, &journal, R1A2_LEDGER_BYTE_LIMIT, 6);
        let _audits = stage_audits(&mut medium, &mk, &plan, 7);
        let error = medium.begin_reclamation_v2(&mk, 8).unwrap_err();
        assert!(
            matches!(
                error,
                crate::medium::MediumError::Chain {
                    rule: "v2-begin-admission/tier-insufficient"
                }
            ),
            "the Windows tier refusal is typed: {error:?}"
        );
        let classification = classify_via_medium(&mut medium);
        let RecoveryState::PlanObservedWithoutBegin {
            audits_observed,
            audits_not_observed,
            ..
        } = classification.state
        else {
            panic!("the refused begin must leave the state PlanObservedWithoutBegin")
        };
        assert_eq!(
            audits_observed,
            (0..plan.batches.len() as u64).collect::<Vec<u64>>()
        );
        assert_eq!(audits_not_observed, Vec::<u64>::new());
        drop(medium);
        cleanup(&[root, journal]);
    }

    /// (B-R1a2 §10) The SQLite twin carries the full staged ladder
    /// on Windows — the platform's positive evidence (the LocalFile
    /// positives cannot run there; SQLite is transactionally
    /// durable).
    #[cfg(windows)]
    #[test]
    fn the_sqlite_twin_carries_windows() {
        let (path, mut store) = sqlite_store("t10");
        let journal = temp_path("journal", "t10");
        staged_production_ladder(&mut store, "t10", &journal);
        drop(store);
        cleanup(&[path, journal]);
    }

    /// (B-R1a2 §11) The read-only proof at the file level: the
    /// terminal campaign classified AND discovered repeatedly
    /// leaves the LocalFile store byte-identical (the HEAD and
    /// every file unchanged) — the observation paths write nothing.
    #[cfg(unix)]
    #[test]
    fn the_production_read_only_proof() {
        let root = temp_path("store", "t11");
        std::fs::create_dir_all(&root).unwrap();
        let mut medium = crate::LocalFileMedium::open(&root).unwrap();
        let journal = temp_path("journal", "t11");
        staged_production_ladder(&mut medium, "t11", &journal);
        let head_before = medium.head().unwrap().unwrap();
        let inventory_before = store_inventory(&root);
        let first = classify_via_medium(&mut medium);
        let discovery = discover_via_medium(&mut medium);
        let second = classify_via_medium(&mut medium);
        assert_eq!(medium.head().unwrap().unwrap(), head_before);
        assert_eq!(store_inventory(&root), inventory_before);
        assert_eq!(first.state, second.state);
        assert_eq!(first.witness, second.witness);
        assert_eq!(first.discovery, discovery);
        drop(medium);
        cleanup(&[root, journal]);
    }

    /// (B-R1a2 §12) The unresolved-authority statement at the
    /// terminal state is TOTAL — every authority field destructured
    /// to NotEstablished, older_checkpoints unresolved below the
    /// boundary — over genuine production evidence.
    #[test]
    fn the_production_unresolved_authority_is_total() {
        let (path, mut store) = sqlite_store("t12");
        let journal = temp_path("journal", "t12");
        staged_production_ladder(&mut store, "t12", &journal);
        let classification = classify_via_medium(&mut store);
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
        drop(store);
        cleanup(&[path, journal]);
    }

    /// (B-R1a2 §13) The discovery layer's own observations over the
    /// terminal campaign: the keyed covering identification, the
    /// newest-first record order with every family present, the
    /// boundary below the covering checkpoint, and the complete
    /// structural-candidate set.
    #[test]
    fn the_discovery_layer_production_observations() {
        let (path, mut store) = sqlite_store("t13");
        let journal = temp_path("journal", "t13");
        let campaign = staged_production_ladder(&mut store, "t13", &journal);
        let discovery = discover_via_medium(&mut store);
        // The covering checkpoint is the keyed MK.
        let CoveringObservation::Identified(covering) = &discovery.covering else {
            panic!("the terminal campaign's covering checkpoint must be identified")
        };
        assert_eq!(covering.record, campaign.mk);
        assert_eq!(covering.metadata.sk, campaign.sk);
        assert_eq!(
            covering.key.as_deref(),
            Some(checkpoint_mk_idempotency_key(&campaign.sk).as_slice())
        );
        let Some(boundary) = &discovery.boundary else {
            panic!("the identified covering checkpoint carries the boundary")
        };
        assert_eq!(boundary.sk, campaign.sk);
        // Every family present, newest-first from the last marker.
        let families: Vec<DiscoveredFamily> = discovery
            .records
            .iter()
            .map(|record| record.family)
            .collect();
        assert_eq!(families[0], DiscoveredFamily::ReclamationCompleteV2);
        let n = campaign.plan.batches.len();
        for family in [
            DiscoveredFamily::ReclamationCompleteV2,
            DiscoveredFamily::AncestryWitnessManifest,
            DiscoveredFamily::AncestryWitnessShard,
            DiscoveredFamily::ReclamationBegin,
            DiscoveredFamily::ReclamationAuditV2,
            DiscoveredFamily::ReclamationPlan,
        ] {
            assert!(
                families.contains(&family),
                "the family is present: {family:?}"
            );
        }
        assert_eq!(
            families
                .iter()
                .filter(|family| **family == DiscoveredFamily::ReclamationAuditV2)
                .count(),
            n
        );
        assert_eq!(
            families
                .iter()
                .filter(|family| **family == DiscoveredFamily::ReclamationCompleteV2)
                .count(),
            n
        );
        // The terminus is counted but not carried in records.
        assert_eq!(
            discovery.records.len() as u64 + 1,
            discovery.records_consulted
        );
        // The marker structural candidates: one per batch, all
        // structural.
        assert_eq!(discovery.completion_candidates.len(), n);
        assert!(
            discovery
                .completion_candidates
                .iter()
                .all(|candidate| matches!(
                    candidate.structural,
                    CompletionStructural::StructuralCandidate(_)
                )),
            "every honest production marker is a structural candidate"
        );
        // The plan and begin payloads decoded and carried.
        assert!(
            discovery
                .records
                .iter()
                .any(|record| matches!(record.payload, DiscoveredPayload::Plan(_)))
        );
        assert!(
            discovery
                .records
                .iter()
                .any(|record| matches!(record.payload, DiscoveredPayload::Begin(_)))
        );
        drop(store);
        cleanup(&[path, journal]);
    }

    /// (B-R1a2 §14) The discovery read-only proof: repeated
    /// head-relative discovery over the terminal campaign returns
    /// identical observations and never moves the head.
    #[test]
    fn the_discovery_layer_read_only_proof() {
        let (path, mut store) = sqlite_store("t14");
        let journal = temp_path("journal", "t14");
        staged_production_ladder(&mut store, "t14", &journal);
        let head = store.head().unwrap().unwrap();
        let first = discover_via_medium(&mut store);
        let second = discover_via_medium(&mut store);
        assert_eq!(first, second);
        assert_eq!(store.head().unwrap().unwrap(), head);
        assert_eq!(first.head, head);
        drop(store);
        cleanup(&[path, journal]);
    }
}

// ===================================================================
// (B-R1a3) The production-path retained-evidence DEGRADATION
// MATRIX — the standing delegation's next recovery-classification
// package: the EvidenceDegradation arms not yet exercised over
// genuine published evidence. B-R1a1 proved the classifier over
// the synthetic reader; B-R1a2 proved the POSITIVE production
// ladder plus the lying-marker, foreign-shard, manifestless, and
// corrupted-object arms; this package drives the REMAINING
// fail-closed arms through the same production adapters and the
// sanctioned in-crate receipt seam (AdmittedAppend::generic — the
// adversarial installs the chain-unique keys would otherwise
// refuse). ZERO production-code changes; every degradation below
// is a TEST-side construction over genuine campaign facts.
// ===================================================================

#[cfg(test)]
mod r1a3_degradation_tests {
    use super::*;
    use crate::checkpoint::{SkipEntry, chain_range_digest_step, compute_digest_seed};
    use crate::medium::{AdmittedAppend, CanonicalMedium, StoredObject};
    use crate::reclaim::{
        MediumReads, PlanBatch, ReclaimPlan, ReclaimUnit, ReclamationLedger, ReclamationLedgerV2,
        create_campaign_bound_journal, journal_root_id, publish_journal_root_binding,
        publish_reclamation_ledger, publish_reclamation_ledger_v2, publish_reclamation_plan,
        reclamation_begin_idempotency_key, reclamation_ledger_idempotency_key,
        reclamation_ledger_v2_idempotency_key, reclamation_plan_idempotency_key,
    };
    use crate::record::{AuthorityMode, RevisionRecord};

    fn temp_path(prefix: &str, label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "s7-r1a3-{prefix}-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn plain_object(payload: &[u8]) -> StoredObject {
        StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(payload);
            enc.into_bytes()
        })
        .unwrap()
    }

    fn sqlite_store(label: &str) -> (std::path::PathBuf, crate::SqliteMedium) {
        let path = temp_path("sqlite", label).with_extension("db");
        let medium = crate::SqliteMedium::open(&path).unwrap();
        (path, medium)
    }

    fn cleanup(paths: &[std::path::PathBuf]) {
        for path in paths {
            let _ = std::fs::remove_dir_all(path);
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
            let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
        }
    }

    /// The pre-MK campaign facts: the founding, the bound journal,
    /// p1, the two keyed members, SK, and the checkpoint metadata
    /// (built over the actual chain) — everything but the MK
    /// publication, which the caller performs (genuinely keyed, or
    /// doctored for the degradation arms).
    struct PreMk {
        members: Vec<Hash>,
        sk: Hash,
        metadata: CheckpointMetadata,
    }

    /// (Phase A of the B-R1a2 staged ladder, compactly rebuilt —
    /// the accepted r1a2 module is left untouched.)
    fn stage_pre_mk(
        medium: &mut dyn CanonicalMedium,
        label: &str,
        journal_root: &std::path::Path,
    ) -> PreMk {
        let shared = plain_object(b"r1a3-shared-payload");
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, label, 0)
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        let founding_id = founding.id();
        let location = crate::reclaim::JournalRootBinding::normalize_location(journal_root);
        let root_id = journal_root_id(&founding_id, &location);
        drop(create_campaign_bound_journal(medium, journal_root, &founding_id).unwrap());
        let (_binding, _outcome) =
            publish_journal_root_binding(medium, &founding_id, &root_id, &location, 1).unwrap();
        let step = |medium: &mut dyn CanonicalMedium, tick: u64| -> Hash {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record =
                RevisionRecord::regular(head, epoch, tick).with_content_refs(vec![shared.hash()]);
            medium
                .append(&record, std::slice::from_ref(&shared), Some(head))
                .unwrap();
            record.id()
        };
        let _p1 = step(medium, 2);
        let mut members = Vec::new();
        for index in 0..2u32 {
            let object = plain_object(format!("r1a3-{label}-payload-{index}").as_bytes());
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = RevisionRecord::regular(head, epoch, 3)
                .with_idempotency_key(format!("r1a3-{label}-member-{index}").into_bytes())
                .with_content_refs(vec![object.hash()]);
            medium
                .append(&record, std::slice::from_ref(&object), Some(head))
                .unwrap();
            members.push(record.id());
        }
        let sk = step(medium, 4);
        let sk_record = medium.read_record(&sk).unwrap();
        let seed = compute_digest_seed(&founding_id);
        let mut digest = seed;
        let mut index = Vec::with_capacity(members.len() + 1);
        for id in &members {
            let record = medium.read_record(id).unwrap();
            digest = chain_range_digest_step(&digest, &record.canonical_bytes());
            index.push(ReclaimedIndexEntry::new(
                record.idempotency_key.clone(),
                *id,
                record.epoch,
            ));
        }
        index.push(ReclaimedIndexEntry::new(
            sk_record.idempotency_key.clone(),
            sk,
            sk_record.epoch,
        ));
        let sk_parent = sk_record.parent.expect("the SK is a regular revision");
        let metadata = CheckpointMetadata::new(
            sk,
            sk_parent,
            vec![SkipEntry::new(Hash::ZERO, _p1)],
            index,
            digest,
            seed,
        );
        PreMk {
            members,
            sk,
            metadata,
        }
    }

    /// The pre-plan campaign facts every degradation arm builds
    /// from: the founding, the bound journal, p1, the two keyed
    /// members, SK, the checkpoint metadata, and the KEYED MK
    /// (publish_checkpoint_mk — the production route; the B-R1a2
    /// finding that the classifier's L2 requires exactly this
    /// key).
    struct Campaign {
        members: Vec<Hash>,
        sk: Hash,
        mk: Hash,
        metadata: CheckpointMetadata,
    }

    /// The full prefix with the genuine keyed MK publication.
    fn stage_prefix(
        medium: &mut dyn CanonicalMedium,
        label: &str,
        journal_root: &std::path::Path,
    ) -> Campaign {
        let pre = stage_pre_mk(medium, label, journal_root);
        let sk_epoch = medium.read_record(&pre.sk).unwrap().epoch;
        let (mk, _outcome) =
            crate::checkpoint::publish_checkpoint_mk(medium, pre.sk, sk_epoch, &pre.metadata, 5)
                .unwrap();
        Campaign {
            members: pre.members,
            sk: pre.sk,
            mk,
            metadata: pre.metadata,
        }
    }

    /// The in-crate receipt installation (the B-R1a2 seam helper):
    /// a regular revision under the supplied deterministic key,
    /// carrying the payload as its single content object, appended
    /// above the current head — the route for every adversarial
    /// install the chain-unique keys or the publication gates
    /// would otherwise refuse.
    fn install_keyed_object_record(
        medium: &mut dyn CanonicalMedium,
        key: Vec<u8>,
        payload: Vec<u8>,
        tick: u64,
    ) -> Hash {
        let object = StoredObject::new(payload).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let record = RevisionRecord::regular(head, epoch, tick)
            .with_idempotency_key(key)
            .with_content_refs(vec![object.hash()]);
        let id = record.id();
        medium
            .append_admitted(
                AdmittedAppend::generic(&record, std::slice::from_ref(&object)),
                Some(head),
            )
            .unwrap();
        id
    }

    fn classify_via_medium(medium: &mut dyn CanonicalMedium) -> RecoveryClassification {
        let head = medium.head().unwrap().unwrap();
        let mut reads = MediumReads::new(medium);
        classify_recovery_evidence(&mut reads, &head, &DiscoveryLimits::default_limits()).unwrap()
    }

    /// The ledger byte limit (the B-R1a2 measured value over the
    /// identical campaign shape — at least two single-unit
    /// batches).
    const R1A3_LEDGER_BYTE_LIMIT: u64 = 400;

    // ======================================================= the tests

    /// (B-R1a3 §1) The unkeyed checkpoint record degrades with the
    /// typed key mismatch — the B-R1a2 finding (a) pinned as a
    /// regression: EVERY production route keys the MK
    /// (checkpoint.rs, ancestry_history, ancestry_accounting all
    /// use checkpoint_mk_idempotency_key), so a metadata record
    /// without the deterministic key is contradictory evidence,
    /// never an unrecognized absence (the bp1 fixture's own
    /// hand-appended unkeyed shape is exactly this arm).
    #[test]
    fn the_unkeyed_checkpoint_record_degrades() {
        let (path, mut store) = sqlite_store("d1");
        let medium: &mut dyn CanonicalMedium = &mut store;
        let journal = temp_path("journal", "d1");
        // The prefix through SK, then the metadata object carried
        // by an UNKEYED regular record (the bp1 fixture's shape).
        let shared = plain_object(b"r1a3-d1-shared");
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "d1", 0)
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let member = RevisionRecord::regular(head, epoch, 1)
            .with_idempotency_key(b"r1a3-d1-member".to_vec())
            .with_content_refs(vec![shared.hash()]);
        medium
            .append(&member, std::slice::from_ref(&shared), Some(head))
            .unwrap();
        let sk_id = {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let sk = RevisionRecord::regular(head, epoch, 2).with_content_refs(vec![shared.hash()]);
            medium
                .append(&sk, std::slice::from_ref(&shared), Some(head))
                .unwrap();
            sk.id()
        };
        let sk_record = medium.read_record(&sk_id).unwrap();
        let seed = compute_digest_seed(&founding.id());
        let metadata = CheckpointMetadata::new(
            sk_id,
            sk_record.parent.expect("the SK is a regular revision"),
            vec![],
            vec![ReclaimedIndexEntry::new(
                sk_record.idempotency_key.clone(),
                sk_id,
                sk_record.epoch,
            )],
            seed,
            seed,
        );
        let metadata_object = StoredObject::new(metadata.canonical_bytes()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let unkeyed =
            RevisionRecord::regular(head, epoch, 3).with_content_refs(vec![metadata_object.hash()]);
        medium
            .append(&unkeyed, std::slice::from_ref(&metadata_object), Some(head))
            .unwrap();
        let classification = classify_via_medium(medium);
        assert_eq!(
            classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::CheckpointKeyMismatch {
                    record: unkeyed.id(),
                }
            }
        );
        drop(store);
        cleanup(&[path, journal]);
    }

    /// (B-R1a3 §2) The invalid index shapes degrade: an EMPTY
    /// retirement index and a duplicate-epoch index (the struct
    /// literal bypassing CheckpointMetadata::new's normalization —
    /// the wire decode accepts both, the classifier's L2 refuses)
    /// — both published through the genuine keyed route as the
    /// campaign's FIRST MK (publish_checkpoint_mk requires head ==
    /// sk exactly, so the doctored metadata must be the only MK).
    #[test]
    fn the_invalid_index_shapes_degrade() {
        // (a) The empty index.
        {
            let (path, mut store) = sqlite_store("d2a");
            let medium: &mut dyn CanonicalMedium = &mut store;
            let journal = temp_path("journal", "d2a");
            let pre = stage_pre_mk(medium, "d2a", &journal);
            let empty = CheckpointMetadata {
                sk: pre.sk,
                sk_parent: pre.metadata.sk_parent,
                skips: Vec::new(),
                index: Vec::new(),
                range_digest: pre.metadata.range_digest,
                seed: pre.metadata.seed,
                range_records: 0,
            };
            let sk_epoch = medium.read_record(&pre.sk).unwrap().epoch;
            let (mk, _outcome) =
                crate::checkpoint::publish_checkpoint_mk(medium, pre.sk, sk_epoch, &empty, 5)
                    .unwrap();
            let classification = classify_via_medium(medium);
            assert_eq!(
                classification.state,
                RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::EmptyRetirementIndex { record: mk }
                }
            );
            drop(store);
            cleanup(&[path, journal]);
        }
        // (b) The duplicate-epoch index.
        {
            let (path, mut store) = sqlite_store("d2b");
            let medium: &mut dyn CanonicalMedium = &mut store;
            let journal = temp_path("journal", "d2b");
            let pre = stage_pre_mk(medium, "d2b", &journal);
            let first_member = medium.read_record(&pre.members[0]).unwrap();
            let second_member = medium.read_record(&pre.members[1]).unwrap();
            let sk_epoch = medium.read_record(&pre.sk).unwrap().epoch;
            // The duplicate-epoch index under the decode's
            // canonical ascending-hash order (the wire accepts any
            // epochs; the classifier's L2 refuses the duplicate).
            let mut duplicate_index = vec![
                ReclaimedIndexEntry::new(
                    first_member.idempotency_key.clone(),
                    pre.members[0],
                    first_member.epoch,
                ),
                ReclaimedIndexEntry::new(
                    second_member.idempotency_key.clone(),
                    pre.members[1],
                    first_member.epoch,
                ),
                ReclaimedIndexEntry::new(None, pre.sk, sk_epoch),
            ];
            duplicate_index.sort_unstable_by_key(|entry| entry.hash);
            let duplicate = CheckpointMetadata {
                sk: pre.sk,
                sk_parent: pre.metadata.sk_parent,
                skips: Vec::new(),
                index: duplicate_index,
                range_digest: pre.metadata.range_digest,
                seed: pre.metadata.seed,
                range_records: 3,
            };
            let (mk, _outcome) =
                crate::checkpoint::publish_checkpoint_mk(medium, pre.sk, sk_epoch, &duplicate, 5)
                    .unwrap();
            let classification = classify_via_medium(medium);
            assert_eq!(
                classification.state,
                RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::IndexShapeInvalid { record: mk }
                }
            );
            drop(store);
            cleanup(&[path, journal]);
        }
    }

    /// A plan payload over the campaign's genuine bindings with
    /// the caller's digest, ledger byte limit, and batch
    /// membership (every doctored form stays wire-decodable; the
    /// codec refuses an empty batch set, so the binding and limit
    /// arms carry the minimal one-batch membership their
    /// earlier-firing checks never reach).
    fn plan_payload(
        campaign: &Campaign,
        range_digest: [u8; 32],
        ledger_byte_limit: u64,
        batches: Vec<PlanBatch>,
    ) -> Vec<u8> {
        ReclaimPlan {
            mk: campaign.mk,
            sk: campaign.sk,
            range_digest,
            seed: campaign.metadata.seed,
            ledger_byte_limit,
            batches,
            journal_files: Vec::new(),
        }
        .canonical_bytes()
    }

    /// The minimal decodable membership (one single-record batch).
    fn minimal_batches(campaign: &Campaign) -> Vec<PlanBatch> {
        vec![PlanBatch {
            index: 0,
            records: vec![campaign.sk],
        }]
    }

    /// (B-R1a3 §3) The doctored plan payloads degrade: a TRUNCATED
    /// payload (MalformedPlanPayload), a binding-incorrect digest
    /// (PlanBindingMismatch{range-digest}), an over-ceiling ledger
    /// byte limit (PlanLedgerByteLimitInvalid), and a reordered
    /// batch membership (PlanMembershipMismatch) — each under the
    /// exact plan key through the in-crate receipt seam (the
    /// publication gates would refuse every doctored form).
    #[test]
    fn the_doctored_plan_payloads_degrade() {
        // (a) The truncated payload.
        {
            let (path, mut store) = sqlite_store("d3a");
            let medium: &mut dyn CanonicalMedium = &mut store;
            let journal = temp_path("journal", "d3a");
            let campaign = stage_prefix(medium, "d3a", &journal);
            let mut payload = plan_payload(
                &campaign,
                campaign.metadata.range_digest,
                R1A3_LEDGER_BYTE_LIMIT,
                vec![],
            );
            payload.truncate(payload.len() - 8);
            let record = install_keyed_object_record(
                medium,
                reclamation_plan_idempotency_key(&campaign.mk),
                payload,
                6,
            );
            let classification = classify_via_medium(medium);
            let RecoveryState::EvidenceDegraded {
                reason:
                    EvidenceDegradation::MalformedPlanPayload {
                        record: observed, ..
                    },
            } = classification.state
            else {
                panic!("the truncated plan payload must degrade as MalformedPlanPayload")
            };
            assert_eq!(observed, record);
            drop(store);
            cleanup(&[path, journal]);
        }
        // (b) The binding-incorrect digest.
        {
            let (path, mut store) = sqlite_store("d3b");
            let medium: &mut dyn CanonicalMedium = &mut store;
            let journal = temp_path("journal", "d3b");
            let campaign = stage_prefix(medium, "d3b", &journal);
            let mut wrong_digest = campaign.metadata.range_digest;
            wrong_digest[0] ^= 0xff;
            let record = install_keyed_object_record(
                medium,
                reclamation_plan_idempotency_key(&campaign.mk),
                plan_payload(
                    &campaign,
                    wrong_digest,
                    R1A3_LEDGER_BYTE_LIMIT,
                    minimal_batches(&campaign),
                ),
                6,
            );
            let classification = classify_via_medium(medium);
            assert_eq!(
                classification.state,
                RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::PlanBindingMismatch {
                        record,
                        rule: "range-digest",
                    }
                }
            );
            drop(store);
            cleanup(&[path, journal]);
        }
        // (c) The over-ceiling ledger byte limit.
        {
            let (path, mut store) = sqlite_store("d3c");
            let medium: &mut dyn CanonicalMedium = &mut store;
            let journal = temp_path("journal", "d3c");
            let campaign = stage_prefix(medium, "d3c", &journal);
            let over_limit = crate::reclaim::MAX_RECLAMATION_LEDGER_BYTES + 1;
            let record = install_keyed_object_record(
                medium,
                reclamation_plan_idempotency_key(&campaign.mk),
                plan_payload(
                    &campaign,
                    campaign.metadata.range_digest,
                    over_limit,
                    minimal_batches(&campaign),
                ),
                6,
            );
            let classification = classify_via_medium(medium);
            assert_eq!(
                classification.state,
                RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::PlanLedgerByteLimitInvalid {
                        record,
                        limit: over_limit,
                    }
                }
            );
            drop(store);
            cleanup(&[path, journal]);
        }
        // (d) The reordered batch membership.
        {
            let (path, mut store) = sqlite_store("d3d");
            let medium: &mut dyn CanonicalMedium = &mut store;
            let journal = temp_path("journal", "d3d");
            let campaign = stage_prefix(medium, "d3d", &journal);
            // The index-derived newest-first order is
            // [sk, members[1], members[0]]; the doctored plan
            // flattens to a different first position.
            let reordered = vec![
                PlanBatch {
                    index: 0,
                    records: vec![campaign.members[0]],
                },
                PlanBatch {
                    index: 1,
                    records: vec![campaign.sk, campaign.members[1]],
                },
            ];
            let record = install_keyed_object_record(
                medium,
                reclamation_plan_idempotency_key(&campaign.mk),
                plan_payload(
                    &campaign,
                    campaign.metadata.range_digest,
                    R1A3_LEDGER_BYTE_LIMIT,
                    reordered,
                ),
                6,
            );
            let classification = classify_via_medium(medium);
            assert_eq!(
                classification.state,
                RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::PlanMembershipMismatch {
                        record,
                        position: 0,
                    }
                }
            );
            drop(store);
            cleanup(&[path, journal]);
        }
    }

    /// (B-R1a3 §4) The doctored begin payloads degrade over a
    /// genuinely published plan: a TRUNCATED payload
    /// (MalformedBeginPayload), a source that is not the observed
    /// plan record (BeginBindingMismatch{source-is-not-the-plan-
    /// record}), and a seed that disagrees with the covering
    /// metadata (BeginBindingMismatch{seed}) — each under the
    /// exact begin key through the in-crate receipt seam (the
    /// admission would refuse every doctored form; the genuine
    /// begin publication is B-R1a2's territory).
    #[test]
    fn the_doctored_begin_payloads_degrade() {
        // (a) The truncated payload.
        {
            let (path, mut store) = sqlite_store("d4a");
            let medium: &mut dyn CanonicalMedium = &mut store;
            let journal = temp_path("journal", "d4a");
            let campaign = stage_prefix(medium, "d4a", &journal);
            publish_reclamation_plan(medium, &campaign.mk, R1A3_LEDGER_BYTE_LIMIT, &journal, 6)
                .unwrap();
            let (plan_record, _plan) = crate::reclaim::find_installed_plan(medium, &campaign.mk)
                .unwrap()
                .unwrap();
            let mut payload = crate::reclaim::ReclamationBegin {
                mk: campaign.mk,
                source: plan_record,
                range_digest: campaign.metadata.range_digest,
                seed: campaign.metadata.seed,
            }
            .canonical_bytes();
            payload.truncate(payload.len() - 4);
            let record = install_keyed_object_record(
                medium,
                reclamation_begin_idempotency_key(&campaign.mk),
                payload,
                7,
            );
            let classification = classify_via_medium(medium);
            let RecoveryState::EvidenceDegraded {
                reason:
                    EvidenceDegradation::MalformedBeginPayload {
                        record: observed, ..
                    },
            } = classification.state
            else {
                panic!("the truncated begin payload must degrade as MalformedBeginPayload")
            };
            assert_eq!(observed, record);
            drop(store);
            cleanup(&[path, journal]);
        }
        // (b) The source that is not the plan record.
        {
            let (path, mut store) = sqlite_store("d4b");
            let medium: &mut dyn CanonicalMedium = &mut store;
            let journal = temp_path("journal", "d4b");
            let campaign = stage_prefix(medium, "d4b", &journal);
            publish_reclamation_plan(medium, &campaign.mk, R1A3_LEDGER_BYTE_LIMIT, &journal, 6)
                .unwrap();
            let payload = crate::reclaim::ReclamationBegin {
                mk: campaign.mk,
                source: campaign.mk,
                range_digest: campaign.metadata.range_digest,
                seed: campaign.metadata.seed,
            }
            .canonical_bytes();
            let record = install_keyed_object_record(
                medium,
                reclamation_begin_idempotency_key(&campaign.mk),
                payload,
                7,
            );
            let classification = classify_via_medium(medium);
            assert_eq!(
                classification.state,
                RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::BeginBindingMismatch {
                        record,
                        rule: "source-is-not-the-plan-record",
                    }
                }
            );
            drop(store);
            cleanup(&[path, journal]);
        }
        // (c) The seed that disagrees with the covering metadata.
        {
            let (path, mut store) = sqlite_store("d4c");
            let medium: &mut dyn CanonicalMedium = &mut store;
            let journal = temp_path("journal", "d4c");
            let campaign = stage_prefix(medium, "d4c", &journal);
            publish_reclamation_plan(medium, &campaign.mk, R1A3_LEDGER_BYTE_LIMIT, &journal, 6)
                .unwrap();
            let (plan_record, _plan) = crate::reclaim::find_installed_plan(medium, &campaign.mk)
                .unwrap()
                .unwrap();
            let mut wrong_seed = campaign.metadata.seed;
            wrong_seed[0] ^= 0xff;
            let payload = crate::reclaim::ReclamationBegin {
                mk: campaign.mk,
                source: plan_record,
                range_digest: campaign.metadata.range_digest,
                seed: wrong_seed,
            }
            .canonical_bytes();
            let record = install_keyed_object_record(
                medium,
                reclamation_begin_idempotency_key(&campaign.mk),
                payload,
                7,
            );
            let classification = classify_via_medium(medium);
            assert_eq!(
                classification.state,
                RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::BeginBindingMismatch {
                        record,
                        rule: "seed",
                    }
                }
            );
            drop(store);
            cleanup(&[path, journal]);
        }
    }

    /// The genuine-shaped begin payload (the V2X install-begin
    /// pattern: the exact bindings the admission would publish).
    fn genuine_begin_payload(campaign: &Campaign, plan_record: Hash) -> Vec<u8> {
        crate::reclaim::ReclamationBegin {
            mk: campaign.mk,
            source: plan_record,
            range_digest: campaign.metadata.range_digest,
            seed: campaign.metadata.seed,
        }
        .canonical_bytes()
    }

    /// (B-R1a3 §5) The begin installed BELOW the audits degrades
    /// with the strict temporal violation: a genuine-shaped begin
    /// under the exact begin key (the in-crate seam) followed by
    /// the GENUINE audit publications above it — the L4 bindings
    /// all hold, the L5 audit-before-begin ordering refuses
    /// (audit-above-begin; the all-audits-first admission is
    /// exactly what this shape circumvents).
    #[test]
    fn the_begin_below_the_audits_degrades() {
        let (path, mut store) = sqlite_store("d5");
        let medium: &mut dyn CanonicalMedium = &mut store;
        let journal = temp_path("journal", "d5");
        let campaign = stage_prefix(medium, "d5", &journal);
        publish_reclamation_plan(medium, &campaign.mk, R1A3_LEDGER_BYTE_LIMIT, &journal, 6)
            .unwrap();
        let (plan_record, plan) = crate::reclaim::find_installed_plan(medium, &campaign.mk)
            .unwrap()
            .unwrap();
        let record = install_keyed_object_record(
            medium,
            reclamation_begin_idempotency_key(&campaign.mk),
            genuine_begin_payload(&campaign, plan_record),
            7,
        );
        for batch in &plan.batches {
            publish_reclamation_ledger_v2(medium, &campaign.mk, batch.index, 8).unwrap();
        }
        let classification = classify_via_medium(medium);
        assert_eq!(
            classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::BeginTemporalViolation {
                    record,
                    rule: "audit-above-begin",
                }
            }
        );
        drop(store);
        cleanup(&[path, journal]);
    }

    /// (B-R1a3 §6) The begin observed with a MISSING plan audit
    /// degrades with the all-audits-first invariant: a
    /// genuine-shaped begin under the exact begin key over a plan
    /// whose LAST batch's audit was never published (the admission
    /// refuses this shape at the gate — the in-crate seam is the
    /// only route, and the classifier independently refuses it).
    #[test]
    fn the_begin_with_a_missing_audit_degrades() {
        let (path, mut store) = sqlite_store("d6");
        let medium: &mut dyn CanonicalMedium = &mut store;
        let journal = temp_path("journal", "d6");
        let campaign = stage_prefix(medium, "d6", &journal);
        publish_reclamation_plan(medium, &campaign.mk, R1A3_LEDGER_BYTE_LIMIT, &journal, 6)
            .unwrap();
        let (plan_record, plan) = crate::reclaim::find_installed_plan(medium, &campaign.mk)
            .unwrap()
            .unwrap();
        let n = plan.batches.len();
        assert!(n >= 2, "the missing-audit arm needs multiple batches: {n}");
        for batch in &plan.batches {
            if batch.index == n as u64 - 1 {
                continue;
            }
            publish_reclamation_ledger_v2(medium, &campaign.mk, batch.index, 7).unwrap();
        }
        install_keyed_object_record(
            medium,
            reclamation_begin_idempotency_key(&campaign.mk),
            genuine_begin_payload(&campaign, plan_record),
            8,
        );
        let classification = classify_via_medium(medium);
        assert_eq!(
            classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::RequiredAuditNotObserved {
                    batch: n as u64 - 1,
                }
            }
        );
        drop(store);
        cleanup(&[path, journal]);
    }

    /// (B-R1a3 §7) A correctly keyed C1 marker over a batch index
    /// BEYOND the plan's membership degrades with the
    /// context-incomplete contradiction: every genuine audit is
    /// published and the begin is observed (the L5 lane passes),
    /// but the discovery's marker pass finds no audit for the
    /// marker's out-of-membership batch and the L6 lane refuses
    /// the marker as contradictory evidence, never a harmless
    /// partial publication — the honest terminal for both this
    /// shape and a missing-audit marker (the standalone
    /// outside-membership arm is unreachable through the
    /// discovery's context construction: a batch without an audit
    /// refuses as context-incomplete first).
    #[test]
    fn the_marker_over_a_missing_audit_degrades() {
        let (path, mut store) = sqlite_store("d7");
        let medium: &mut dyn CanonicalMedium = &mut store;
        let journal = temp_path("journal", "d7");
        let campaign = stage_prefix(medium, "d7", &journal);
        publish_reclamation_plan(medium, &campaign.mk, R1A3_LEDGER_BYTE_LIMIT, &journal, 6)
            .unwrap();
        let (plan_record, plan) = crate::reclaim::find_installed_plan(medium, &campaign.mk)
            .unwrap()
            .unwrap();
        let n = plan.batches.len();
        assert!(n >= 2, "the marker arm needs multiple batches: {n}");
        for batch in &plan.batches {
            publish_reclamation_ledger_v2(medium, &campaign.mk, batch.index, 7).unwrap();
        }
        let begin = install_keyed_object_record(
            medium,
            reclamation_begin_idempotency_key(&campaign.mk),
            genuine_begin_payload(&campaign, plan_record),
            8,
        );
        // The marker keyed for batch n — one beyond the plan's
        // membership — under the exact (mk, n) key, the C1
        // commitment over the campaign's genuine SK entry (the
        // payload is never structurally read: the missing audit
        // for the out-of-membership batch refuses first).
        let context = crate::batch_completion::BatchCompletionContext {
            mk: campaign.mk,
            plan: plan_record,
            batch_index: n as u64,
            audit: campaign.mk,
            begin,
        };
        let commitment = crate::batch_completion::commit_classification(
            &context,
            vec![crate::batch_completion::ClassificationEntry {
                identity: campaign.sk,
                role: crate::ancestry_accounting::StorageRole::Record,
                classification: crate::batch_completion::EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            }],
        )
        .unwrap();
        let marker = crate::batch_completion::BatchCompletionMarkerV2 {
            mk: context.mk,
            plan: context.plan,
            batch_index: context.batch_index,
            audit: context.audit,
            begin: context.begin,
            commitment: commitment.digest,
            counters: commitment.counters,
        };
        let record = install_keyed_object_record(
            medium,
            crate::batch_completion::reclamation_complete_v2_idempotency_key(
                &campaign.mk,
                n as u64,
            ),
            marker.canonical_bytes(),
            9,
        );
        let classification = classify_via_medium(medium);
        assert_eq!(
            classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::MarkerContradicted {
                    record,
                    batch: Some(n as u64),
                    rule: String::from("context-incomplete"),
                }
            }
        );
        drop(store);
        cleanup(&[path, journal]);
    }

    /// The genuine units of a plan batch, read back from the
    /// actual records (the identity, the content references, and
    /// the keyed count — the shape the genuine audit publishes).
    fn genuine_units(
        medium: &mut dyn CanonicalMedium,
        plan: &ReclaimPlan,
        batch_index: u64,
    ) -> (Vec<ReclaimUnit>, u64) {
        let batch = plan
            .batches
            .iter()
            .find(|batch| batch.index == batch_index)
            .expect("the batch index is in the plan");
        let mut units = Vec::with_capacity(batch.records.len());
        let mut keyed = 0u64;
        for record_id in &batch.records {
            let record = medium.read_record(record_id).unwrap();
            if record.idempotency_key.is_some() {
                keyed += 1;
            }
            units.push(ReclaimUnit {
                record: *record_id,
                objects: record.content_refs.clone(),
            });
        }
        (units, keyed)
    }

    /// A V2 audit payload over the campaign's genuine bindings
    /// with the caller's digest, units, and keyed count.
    fn ledger_payload(
        campaign: &Campaign,
        batch: u64,
        range_digest: [u8; 32],
        units: Vec<ReclaimUnit>,
        reclaimed_keys: u64,
    ) -> Vec<u8> {
        ReclamationLedgerV2 {
            mk: campaign.mk,
            sk: campaign.sk,
            batch,
            range_digest,
            seed: campaign.metadata.seed,
            records: units.len() as u64,
            reclaimed_keys,
            units,
        }
        .canonical_bytes()
    }

    /// (B-R1a3 §8) The doctored audit payloads degrade: a
    /// byte-limit-exceeding payload (AuditByteLimitExceeded — the
    /// raw-bytes ceiling check fires before the decode), a
    /// TRUNCATED payload (MalformedAuditPayload), a
    /// binding-incorrect digest (AuditBindingMismatch), reordered
    /// units (AuditUnitsMismatch), and an inflated keyed count
    /// (AuditKeyedCountMismatch) — each under the exact (mk, 0)
    /// audit key through the in-crate receipt seam with the
    /// genuine begin observed (the L5 lane validates every
    /// observed audit in full).
    #[test]
    fn the_doctored_audit_payloads_degrade() {
        // (a) The byte-limit-exceeding payload.
        {
            let (path, mut store) = sqlite_store("d8a");
            let medium: &mut dyn CanonicalMedium = &mut store;
            let journal = temp_path("journal", "d8a");
            let campaign = stage_prefix(medium, "d8a", &journal);
            publish_reclamation_plan(medium, &campaign.mk, R1A3_LEDGER_BYTE_LIMIT, &journal, 6)
                .unwrap();
            let (plan_record, plan) = crate::reclaim::find_installed_plan(medium, &campaign.mk)
                .unwrap()
                .unwrap();
            let n = plan.batches.len();
            assert!(n >= 2, "the audit arms need multiple batches: {n}");
            for batch in &plan.batches {
                if batch.index == 0 {
                    continue;
                }
                publish_reclamation_ledger_v2(medium, &campaign.mk, batch.index, 7).unwrap();
            }
            // A genuine-shaped ledger for batch 0 inflated past
            // the plan's admitted byte limit with repeated units.
            let (units, keyed) = genuine_units(medium, &plan, 0);
            let mut fat_units = units.clone();
            for _ in 0..12 {
                fat_units.extend(units.iter().cloned());
            }
            let payload = ledger_payload(
                &campaign,
                0,
                campaign.metadata.range_digest,
                fat_units,
                keyed,
            );
            let observed = payload.len() as u64;
            assert!(observed > R1A3_LEDGER_BYTE_LIMIT);
            install_keyed_object_record(
                medium,
                reclamation_ledger_v2_idempotency_key(&campaign.mk, 0),
                payload,
                9,
            );
            install_keyed_object_record(
                medium,
                reclamation_begin_idempotency_key(&campaign.mk),
                genuine_begin_payload(&campaign, plan_record),
                8,
            );
            let classification = classify_via_medium(medium);
            assert_eq!(
                classification.state,
                RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::AuditByteLimitExceeded {
                        batch: 0,
                        observed,
                        limit: R1A3_LEDGER_BYTE_LIMIT,
                    }
                }
            );
            drop(store);
            cleanup(&[path, journal]);
        }
        // (b) The truncated payload.
        {
            let (path, mut store) = sqlite_store("d8b");
            let medium: &mut dyn CanonicalMedium = &mut store;
            let journal = temp_path("journal", "d8b");
            let campaign = stage_prefix(medium, "d8b", &journal);
            publish_reclamation_plan(medium, &campaign.mk, R1A3_LEDGER_BYTE_LIMIT, &journal, 6)
                .unwrap();
            let (plan_record, plan) = crate::reclaim::find_installed_plan(medium, &campaign.mk)
                .unwrap()
                .unwrap();
            for batch in &plan.batches {
                if batch.index == 0 {
                    continue;
                }
                publish_reclamation_ledger_v2(medium, &campaign.mk, batch.index, 7).unwrap();
            }
            let (units, keyed) = genuine_units(medium, &plan, 0);
            let mut payload =
                ledger_payload(&campaign, 0, campaign.metadata.range_digest, units, keyed);
            payload.truncate(payload.len() - 6);
            install_keyed_object_record(
                medium,
                reclamation_ledger_v2_idempotency_key(&campaign.mk, 0),
                payload,
                9,
            );
            install_keyed_object_record(
                medium,
                reclamation_begin_idempotency_key(&campaign.mk),
                genuine_begin_payload(&campaign, plan_record),
                8,
            );
            let classification = classify_via_medium(medium);
            let RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::MalformedAuditPayload { batch, .. },
            } = classification.state
            else {
                panic!("the truncated audit payload must degrade as MalformedAuditPayload")
            };
            assert_eq!(batch, 0);
            drop(store);
            cleanup(&[path, journal]);
        }
        // (c) The binding-incorrect digest.
        {
            let (path, mut store) = sqlite_store("d8c");
            let medium: &mut dyn CanonicalMedium = &mut store;
            let journal = temp_path("journal", "d8c");
            let campaign = stage_prefix(medium, "d8c", &journal);
            publish_reclamation_plan(medium, &campaign.mk, R1A3_LEDGER_BYTE_LIMIT, &journal, 6)
                .unwrap();
            let (plan_record, plan) = crate::reclaim::find_installed_plan(medium, &campaign.mk)
                .unwrap()
                .unwrap();
            for batch in &plan.batches {
                if batch.index == 0 {
                    continue;
                }
                publish_reclamation_ledger_v2(medium, &campaign.mk, batch.index, 7).unwrap();
            }
            let (units, keyed) = genuine_units(medium, &plan, 0);
            let mut wrong_digest = campaign.metadata.range_digest;
            wrong_digest[0] ^= 0xff;
            install_keyed_object_record(
                medium,
                reclamation_ledger_v2_idempotency_key(&campaign.mk, 0),
                ledger_payload(&campaign, 0, wrong_digest, units, keyed),
                9,
            );
            install_keyed_object_record(
                medium,
                reclamation_begin_idempotency_key(&campaign.mk),
                genuine_begin_payload(&campaign, plan_record),
                8,
            );
            let classification = classify_via_medium(medium);
            assert_eq!(
                classification.state,
                RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::AuditBindingMismatch {
                        batch: 0,
                        rule: "bindings",
                    }
                }
            );
            drop(store);
            cleanup(&[path, journal]);
        }
        // (d) The reordered units: a WIDE-limit campaign (one
        // multi-record batch, so the reordered payload stays under
        // the plan's own byte ceiling and the decode succeeds),
        // no genuine batch-0 audit published (the doctored form
        // takes the unoccupied key), the units in reverse order
        // against the plan's newest-first membership.
        {
            let (path, mut store) = sqlite_store("d8d");
            let medium: &mut dyn CanonicalMedium = &mut store;
            let journal = temp_path("journal", "d8d");
            let campaign = stage_prefix(medium, "d8d", &journal);
            publish_reclamation_plan(medium, &campaign.mk, 10_000, &journal, 6).unwrap();
            let (plan_record, plan) = crate::reclaim::find_installed_plan(medium, &campaign.mk)
                .unwrap()
                .unwrap();
            let batch0 = plan
                .batches
                .iter()
                .find(|batch| batch.index == 0)
                .expect("batch 0 exists");
            assert!(
                batch0.records.len() >= 2,
                "the wide limit yields one multi-record batch"
            );
            let (units, keyed) = genuine_units(medium, &plan, 0);
            let mut reordered = units.clone();
            reordered.reverse();
            install_keyed_object_record(
                medium,
                reclamation_ledger_v2_idempotency_key(&campaign.mk, 0),
                ledger_payload(
                    &campaign,
                    0,
                    campaign.metadata.range_digest,
                    reordered,
                    keyed,
                ),
                9,
            );
            install_keyed_object_record(
                medium,
                reclamation_begin_idempotency_key(&campaign.mk),
                genuine_begin_payload(&campaign, plan_record),
                8,
            );
            let classification = classify_via_medium(medium);
            assert_eq!(
                classification.state,
                RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::AuditUnitsMismatch {
                        batch: 0,
                        position: 0,
                    }
                }
            );
            drop(store);
            cleanup(&[path, journal]);
        }

        // (e) The inflated keyed count.
        {
            let (path, mut store) = sqlite_store("d8e");
            let medium: &mut dyn CanonicalMedium = &mut store;
            let journal = temp_path("journal", "d8e");
            let campaign = stage_prefix(medium, "d8e", &journal);
            publish_reclamation_plan(medium, &campaign.mk, R1A3_LEDGER_BYTE_LIMIT, &journal, 6)
                .unwrap();
            let (plan_record, plan) = crate::reclaim::find_installed_plan(medium, &campaign.mk)
                .unwrap()
                .unwrap();
            for batch in &plan.batches {
                if batch.index == 0 {
                    continue;
                }
                publish_reclamation_ledger_v2(medium, &campaign.mk, batch.index, 7).unwrap();
            }
            let (units, keyed) = genuine_units(medium, &plan, 0);
            install_keyed_object_record(
                medium,
                reclamation_ledger_v2_idempotency_key(&campaign.mk, 0),
                ledger_payload(
                    &campaign,
                    0,
                    campaign.metadata.range_digest,
                    units,
                    keyed + 1,
                ),
                9,
            );
            install_keyed_object_record(
                medium,
                reclamation_begin_idempotency_key(&campaign.mk),
                genuine_begin_payload(&campaign, plan_record),
                8,
            );
            let classification = classify_via_medium(medium);
            assert_eq!(
                classification.state,
                RecoveryState::EvidenceDegraded {
                    reason: EvidenceDegradation::AuditKeyedCountMismatch {
                        batch: 0,
                        observed: keyed + 1,
                        expected: keyed,
                    }
                }
            );
            drop(store);
            cleanup(&[path, journal]);
        }
    }

    /// (B-R1a3 §9) A begin observed with NO plan and NO V1 audit
    /// to bind its source to degrades with
    /// BeginSourceNotObserved — the begin key occupied over a
    /// bare checkpoint (the publication gates cannot produce this
    /// shape: every begin admission requires its source evidence
    /// first; the in-crate seam is the only route, and the
    /// classifier independently refuses it).
    #[test]
    fn the_begin_without_source_degrades() {
        let (path, mut store) = sqlite_store("d9");
        let medium: &mut dyn CanonicalMedium = &mut store;
        let journal = temp_path("journal", "d9");
        let campaign = stage_prefix(medium, "d9", &journal);
        let record = install_keyed_object_record(
            medium,
            reclamation_begin_idempotency_key(&campaign.mk),
            genuine_begin_payload(&campaign, campaign.mk),
            6,
        );
        let classification = classify_via_medium(medium);
        assert_eq!(
            classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::BeginSourceNotObserved { record }
            }
        );
        drop(store);
        cleanup(&[path, journal]);
    }

    /// (B-R1a3 §10) The mixed V1/V2 evidence degrades: a genuine
    /// V2 plan published plus a genuine-shaped V1 ledger installed
    /// under the exact V1 key (the publication refuses this shape
    /// at its plan-exclusion gate — the in-crate seam is the only
    /// route around it, and the classifier independently refuses
    /// the one-retirement-authority-per-checkpoint invariant).
    #[test]
    fn the_mixed_v1_and_v2_evidence_degrades() {
        let (path, mut store) = sqlite_store("d10");
        let medium: &mut dyn CanonicalMedium = &mut store;
        let journal = temp_path("journal", "d10");
        let campaign = stage_prefix(medium, "d10", &journal);
        publish_reclamation_plan(medium, &campaign.mk, R1A3_LEDGER_BYTE_LIMIT, &journal, 6)
            .unwrap();
        let (plan_record, _plan) = crate::reclaim::find_installed_plan(medium, &campaign.mk)
            .unwrap()
            .unwrap();
        // The V1 ledger over the campaign's actual retirement
        // index, newest-first (SK, then the members reversed),
        // with the read-back content references.
        let mut units: Vec<ReclaimUnit> = Vec::new();
        let mut keyed = 0u64;
        let sk_record = medium.read_record(&campaign.sk).unwrap();
        keyed += sk_record.idempotency_key.is_some() as u64;
        units.push(ReclaimUnit {
            record: campaign.sk,
            objects: sk_record.content_refs.clone(),
        });
        for id in campaign.members.iter().rev() {
            let record = medium.read_record(id).unwrap();
            keyed += record.idempotency_key.is_some() as u64;
            units.push(ReclaimUnit {
                record: *id,
                objects: record.content_refs.clone(),
            });
        }
        let ledger = ReclamationLedger {
            mk: campaign.mk,
            sk: campaign.sk,
            range_digest: campaign.metadata.range_digest,
            seed: campaign.metadata.seed,
            records: units.len() as u64,
            reclaimed_keys: keyed,
            journal_files: Vec::new(),
            units,
        };
        let v1_record = install_keyed_object_record(
            medium,
            reclamation_ledger_idempotency_key(&campaign.mk),
            ledger.canonical_bytes(),
            7,
        );
        let classification = classify_via_medium(medium);
        assert_eq!(
            classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::MixedV1AndV2Evidence {
                    v1_record,
                    v2_record: plan_record,
                }
            }
        );
        drop(store);
        cleanup(&[path, journal]);
    }

    /// (B-R1a3 §11) A witness manifest observed with NO plan and
    /// NO V1 audit to bind to degrades with
    /// OrphanedWitnessManifest — the manifest key occupied over a
    /// bare checkpoint with a genuine-shaped manifest payload
    /// (the publication order makes an orphaned manifest
    /// incoherent retained evidence; the witness observation
    /// degrades independently and orthogonally).
    #[test]
    fn the_orphaned_witness_manifest_degrades() {
        let (path, mut store) = sqlite_store("d11");
        let medium: &mut dyn CanonicalMedium = &mut store;
        let journal = temp_path("journal", "d11");
        let campaign = stage_prefix(medium, "d11", &journal);
        let manifest = crate::ancestry_witness::AncestryWitnessManifest {
            mk: campaign.mk,
            plan_record: campaign.mk,
            begin_record: campaign.mk,
            metadata_object: campaign.mk,
            member_count: 3,
            shard_count: 1,
            coverage_digest: [11u8; 32],
        };
        let record = install_keyed_object_record(
            medium,
            crate::ancestry_witness::reclamation_witness_manifest_idempotency_key(&campaign.mk),
            manifest.canonical_bytes(),
            6,
        );
        let classification = classify_via_medium(medium);
        assert_eq!(
            classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::OrphanedWitnessManifest { record }
            }
        );
        drop(store);
        cleanup(&[path, journal]);
    }

    /// (B-R1a3 §12) A recognized reclamation-family payload
    /// binding the covering MK under an INCORRECT key degrades
    /// with SameCheckpointWrongKeyEvidence — a genuine-shaped plan
    /// payload installed under an arbitrary key over a bare
    /// checkpoint (the same-MK wrong-key scan reads and decodes
    /// the payload solely to test the MK binding; the correctly
    /// keyed lanes never see it).
    #[test]
    fn the_same_checkpoint_wrong_key_evidence_degrades() {
        let (path, mut store) = sqlite_store("d12");
        let medium: &mut dyn CanonicalMedium = &mut store;
        let journal = temp_path("journal", "d12");
        let campaign = stage_prefix(medium, "d12", &journal);
        let record = install_keyed_object_record(
            medium,
            b"r1a3-wrong-plan-key".to_vec(),
            plan_payload(
                &campaign,
                campaign.metadata.range_digest,
                R1A3_LEDGER_BYTE_LIMIT,
                minimal_batches(&campaign),
            ),
            6,
        );
        let classification = classify_via_medium(medium);
        assert_eq!(
            classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::SameCheckpointWrongKeyEvidence {
                    record,
                    family: "plan",
                }
            }
        );
        drop(store);
        cleanup(&[path, journal]);
    }

    /// (B-R1a3 §13) The V1 lane's begin binding: a begin under
    /// the exact begin key over a GENUINELY published V1 ledger
    /// whose source is not the observed V1 audit degrades with
    /// BeginBindingMismatch{v1-source-or-mk} (the genuine V1
    /// begin — the R1a2 package's territory — binds the audit; the
    /// classifier independently refuses the wrong binding).
    #[test]
    fn the_v1_begin_binding_mismatch_degrades() {
        let (path, mut store) = sqlite_store("d13");
        let medium: &mut dyn CanonicalMedium = &mut store;
        let journal = temp_path("journal", "d13");
        let campaign = stage_prefix(medium, "d13", &journal);
        // The genuine V1 ledger over the campaign's actual
        // retirement index, newest-first, published through the
        // genuine route (no V2 plan exists in this campaign).
        let mut units: Vec<ReclaimUnit> = Vec::new();
        let mut keyed = 0u64;
        let sk_record = medium.read_record(&campaign.sk).unwrap();
        keyed += sk_record.idempotency_key.is_some() as u64;
        units.push(ReclaimUnit {
            record: campaign.sk,
            objects: sk_record.content_refs.clone(),
        });
        for id in campaign.members.iter().rev() {
            let record = medium.read_record(id).unwrap();
            keyed += record.idempotency_key.is_some() as u64;
            units.push(ReclaimUnit {
                record: *id,
                objects: record.content_refs.clone(),
            });
        }
        let ledger = ReclamationLedger {
            mk: campaign.mk,
            sk: campaign.sk,
            range_digest: campaign.metadata.range_digest,
            seed: campaign.metadata.seed,
            records: units.len() as u64,
            reclaimed_keys: keyed,
            journal_files: Vec::new(),
            units,
        };
        publish_reclamation_ledger(medium, &ledger, 6).unwrap();
        // The begin under the exact begin key binding the WRONG
        // source (the MK record, not the V1 audit).
        let payload = crate::reclaim::ReclamationBegin {
            mk: campaign.mk,
            source: campaign.mk,
            range_digest: campaign.metadata.range_digest,
            seed: campaign.metadata.seed,
        }
        .canonical_bytes();
        let record = install_keyed_object_record(
            medium,
            reclamation_begin_idempotency_key(&campaign.mk),
            payload,
            7,
        );
        let classification = classify_via_medium(medium);
        assert_eq!(
            classification.state,
            RecoveryState::EvidenceDegraded {
                reason: EvidenceDegradation::BeginBindingMismatch {
                    record,
                    rule: "v1-source-or-mk",
                }
            }
        );
        drop(store);
        cleanup(&[path, journal]);
    }
}
