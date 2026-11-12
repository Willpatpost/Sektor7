//! (V2X-CP3-B-R1a0) The read-only recovery-evidence discovery
//! foundation — a pure, reader-generic, **no-skip** observation
//! walk from a caller-supplied HEAD toward the NEAREST (covering)
//! checkpoint, recognizing the retained V2 reclamation evidence
//! above it. This is a preparatory component for the future
//! authenticated recovery classification (B-R1a and later): it
//! establishes what evidence EXISTS, never what that evidence
//! PROVES.
//!
//! **The head is an observation seed, never an authority.** The
//! caller-supplied HEAD is not independently verified against any
//! provider's canonical state; every positive result of this
//! module is therefore an explicitly **head-relative observation**,
//! not a canonical installation certificate. Nothing here asserts
//! that the supplied head is THE head.
//!
//! **What this discovery MAY establish** (and nothing more):
//! hash-consistent evidence bytes; syntactically recognized
//! evidence families; record parent relationships along the
//! walked segment; candidate identities, deterministic key
//! references, and evidence order relative to the supplied head;
//! and structural metadata suitable for future authentication.
//!
//! **What this discovery may NOT establish — by construction,
//! there is no field, variant, or method that represents any of
//! these claims:** globally safe startup; fully authenticated
//! retirement; trusted HEAD ancestry; V2 begin durability
//! authority; completion of any Batch or Range; correct
//! historical classification; safe deletion; logical-retirement
//! skip authority; or a usable campaign cursor. A structural
//! completion candidate observed here is the accepted C2
//! reader's structural fact and NOTHING else — a present Batch
//! marker is never treated as an authenticated completion
//! receipt, and a begin observed without a completion terminal
//! does not by itself distinguish never-started execution,
//! interrupted execution, or missing terminal evidence. The
//! future authenticated classifier owns every one of those
//! distinctions.
//!
//! **The no-skip discipline.** The walk descends record-by-record
//! through available parent edges only. It never consults the
//! existing presence-based cold walker, never trusts a
//! checkpoint skip-table crossing, and never uses the existing
//! gap-aware authority descents as a substitute for this
//! bootstrap: a missing parent above the covering checkpoint is a
//! typed refusal, never an invented skip. A traversal that
//! reaches a checkpoint boundary returns an explicit
//! unresolved-boundary observation rather than crossing it — the
//! state below the covering checkpoint (its range and every older
//! checkpoint) is NEVER read, classified, or certified here.
//!
//! **Cost shape.** Each consulted record's canonical bytes and
//! its FIRST content object are read in full and hash-verified in
//! this function (the adapter contract re-established in-function,
//! exactly as the accepted C2 reader does); non-first content
//! objects, composite closures, audit payloads, and witness
//! payloads are deliberately NOT read — their verification
//! belongs to the future authenticated traversal. Memory is
//! O(records consulted) with a single record's bytes resident at
//! a time, bounded by [`MAX_DISCOVERY_RECORDS`].

use std::collections::BTreeSet;
use std::fmt;

use crate::ancestry_witness::{
    RECLAMATION_WITNESS_MANIFEST_DOMAIN, RECLAMATION_WITNESS_SHARD_DOMAIN,
    is_reclamation_ancestry_witness_family,
};
use crate::batch_completion::{
    RECLAMATION_COMPLETE_V2_DOMAIN, RECLAMATION_COMPLETE_V2_KEY_DOMAIN, StructuralBatchCandidate,
    is_reclamation_complete_family, read_structural_batch_candidate,
};
use crate::checkpoint::{CHECKPOINT_METADATA_DOMAIN, CheckpointMetadata, peek_object_domain};
use crate::hash::{Hash, hash_object};
use crate::medium::MediumError;
use crate::reclaim::{
    BeginReads, RECLAMATION_BEGIN_DOMAIN, RECLAMATION_COMPLETE_DOMAIN, RECLAMATION_LEDGER_DOMAIN,
    RECLAMATION_LEDGER_V2_DOMAIN, RECLAMATION_PLAN_DOMAIN, RECLAMATION_PLAN_DOMAIN_PREFIX,
    ReclaimPlan, ReclamationBegin, is_reclamation_begin_family, is_reclamation_ledger_family,
    reclamation_begin_idempotency_key, reclamation_ledger_v2_idempotency_key,
    reclamation_plan_idempotency_key,
};
use crate::record::{RecordKind, RevisionRecord};

/// The hard traversal ceiling — the same bound the accepted
/// authority descents use. A caller may configure a smaller
/// limit, never a larger one.
pub const MAX_DISCOVERY_RECORDS: u64 = 1 << 20;

// ============================================================ the limits

/// The caller-configurable traversal bound. The default is the
/// hard ceiling; a caller may tighten it for bounded observation
/// passes, never loosen it past [`MAX_DISCOVERY_RECORDS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryLimits {
    /// The maximum number of records the discovery may consult
    /// (the supplied head included) before refusing with
    /// [`DiscoveryError::TraversalBoundExhausted`].
    pub max_records: u64,
}

impl DiscoveryLimits {
    /// The default limits: the hard ceiling.
    pub fn default_limits() -> Self {
        DiscoveryLimits {
            max_records: MAX_DISCOVERY_RECORDS,
        }
    }

    /// Validates the limits: at least one record and never more
    /// than the hard ceiling.
    pub fn validate(&self) -> Result<(), DiscoveryError> {
        if self.max_records == 0 {
            return Err(DiscoveryError::LimitsInvalid {
                detail: "max-records-zero",
            });
        }
        if self.max_records > MAX_DISCOVERY_RECORDS {
            return Err(DiscoveryError::LimitsInvalid {
                detail: "max-records-above-ceiling",
            });
        }
        Ok(())
    }
}

impl Default for DiscoveryLimits {
    fn default() -> Self {
        Self::default_limits()
    }
}

// ============================================================= the errors

/// The typed, fail-closed refusals of the discovery walk. Every
/// variant preserves the evidence (nothing is mutated — the walk
/// is pure over the reader) and names the exact obstruction.
#[derive(Debug)]
pub enum DiscoveryError {
    /// The supplied head record is absent from the reader — the
    /// observation cannot even start.
    HeadRecordMissing { head: Hash },
    /// A parent edge names a record absent from the reader. The
    /// no-skip discipline: the walk NEVER invents a skip — a gap
    /// above the covering checkpoint is hard corruption.
    ParentMissing { child: Hash, parent: Hash },
    /// A consulted record's bytes do not hash to the identity
    /// they were read under (corruption — fail closed).
    RecordCorrupt { identity: Hash },
    /// A consulted record's canonical bytes refused strict decode.
    RecordUndecodable { identity: Hash },
    /// A consulted record's first content object is absent.
    EvidenceObjectMissing { record: Hash, object: Hash },
    /// A consulted record's first content object does not hash to
    /// the identity it was read under.
    EvidenceObjectCorrupt { record: Hash, object: Hash },
    /// A consulted record's first content object refused the
    /// domain-tag peek (every legitimate object in this store is
    /// domain-framed — an unpeekable object is corruption).
    EvidenceObjectUndecodable { record: Hash, object: Hash },
    /// The strict epoch linkage failed: a regular record's epoch
    /// must be exactly its parent's epoch plus one. There are no
    /// skip jumps on this walk, so no epoch gap is ever
    /// legitimate — this is also where a mutual-cycle adversary
    /// necessarily presents first (a hash-consistent cycle cannot
    /// exist: each record's identity depends on its parent's).
    EpochLinkageInvalid { child: Hash, parent: Hash },
    /// A previously consulted identity reappeared on the parent
    /// path. Defense-in-depth, kept exactly as the accepted
    /// descents keep their cycle guards: a genuine cycle cannot
    /// be constructed with hash-consistent records, but the guard
    /// stands for any future relaxation.
    CycleDetected { revisited: Hash },
    /// Two consulted records carry the same idempotency key —
    /// the chain-wide key-uniqueness invariant, refused exactly
    /// as the validation walks refuse it.
    KeyDuplicate { record: Hash },
    /// The traversal bound was exhausted before a covering
    /// checkpoint or the founding record was reached.
    TraversalBoundExhausted { limit: u64 },
    /// The founding record was reached with a nonzero epoch —
    /// the founding invariant (epoch 0) is part of the chain's
    /// structural validity.
    FoundingEpochInvalid { identity: Hash },
    /// The covering checkpoint's metadata refused strict decode or
    /// its record shape/binding is invalid (the MK must carry
    /// exactly one content object — the metadata — and its parent
    /// must equal the metadata's `sk`).
    CheckpointInvalid { record: Hash, rule: &'static str },
    /// A recognized evidence family carried an unsupported future
    /// version. Interpreting it would affect the discovery's
    /// conclusions, so the discovery refuses rather than guesses.
    UnsupportedEvidenceVariant { record: Hash, domain: String },
    /// A recognized evidence-family record's structural shape is
    /// invalid (every evidence family requires exactly one content
    /// object and its deterministic idempotency key).
    EvidenceShapeInvalid { record: Hash, rule: &'static str },
    /// The configured limits are invalid.
    LimitsInvalid { detail: &'static str },
    /// An unexpected reader failure not mapped to a more precise
    /// variant (fail closed, the original error preserved).
    Reader(MediumError),
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiscoveryError::HeadRecordMissing { head } => {
                write!(f, "recovery-discovery/head-record-missing: {head}")
            }
            DiscoveryError::ParentMissing { child, parent } => write!(
                f,
                "recovery-discovery/parent-missing: child {child} names absent parent {parent} — never an invented skip"
            ),
            DiscoveryError::RecordCorrupt { identity } => {
                write!(f, "recovery-discovery/record-corrupt: {identity}")
            }
            DiscoveryError::RecordUndecodable { identity } => {
                write!(f, "recovery-discovery/record-undecodable: {identity}")
            }
            DiscoveryError::EvidenceObjectMissing { record, object } => write!(
                f,
                "recovery-discovery/evidence-object-missing: record {record} object {object}"
            ),
            DiscoveryError::EvidenceObjectCorrupt { record, object } => write!(
                f,
                "recovery-discovery/evidence-object-corrupt: record {record} object {object}"
            ),
            DiscoveryError::EvidenceObjectUndecodable { record, object } => write!(
                f,
                "recovery-discovery/evidence-object-undecodable: record {record} object {object}"
            ),
            DiscoveryError::EpochLinkageInvalid { child, parent } => write!(
                f,
                "recovery-discovery/epoch-linkage-invalid: child {child} parent {parent}"
            ),
            DiscoveryError::CycleDetected { revisited } => {
                write!(f, "recovery-discovery/cycle-detected: {revisited}")
            }
            DiscoveryError::KeyDuplicate { record } => {
                write!(f, "recovery-discovery/key-duplicate: {record}")
            }
            DiscoveryError::TraversalBoundExhausted { limit } => {
                write!(f, "recovery-discovery/traversal-bound-exhausted: {limit}")
            }
            DiscoveryError::FoundingEpochInvalid { identity } => {
                write!(f, "recovery-discovery/founding-epoch-invalid: {identity}")
            }
            DiscoveryError::CheckpointInvalid { record, rule } => {
                write!(
                    f,
                    "recovery-discovery/checkpoint-invalid: {record} ({rule})"
                )
            }
            DiscoveryError::UnsupportedEvidenceVariant { record, domain } => write!(
                f,
                "recovery-discovery/unsupported-evidence-variant: {record} ({domain})"
            ),
            DiscoveryError::EvidenceShapeInvalid { record, rule } => {
                write!(
                    f,
                    "recovery-discovery/evidence-shape-invalid: {record} ({rule})"
                )
            }
            DiscoveryError::LimitsInvalid { detail } => {
                write!(f, "recovery-discovery/limits-invalid: {detail}")
            }
            DiscoveryError::Reader(error) => write!(f, "recovery-discovery/reader: {error}"),
        }
    }
}

impl std::error::Error for DiscoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DiscoveryError::Reader(error) => Some(error),
            _ => None,
        }
    }
}

// ======================================================== the observation

/// The evidence family a consulted record's first content object
/// was recognized as — a SYNTACTIC classification by domain tag,
/// never an authenticity claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveredFamily {
    /// The V2 reclamation plan (`engine-persist/reclamation-plan/1`).
    ReclamationPlan,
    /// A V2 per-batch audit ledger (`…/reclamation-ledger/2`).
    ReclamationAuditV2,
    /// A V1 audit ledger (`…/reclamation-ledger/1`).
    ReclamationAuditV1,
    /// The reclamation begin (`…/reclamation-begin/1` — one wire;
    /// the V1/V2 distinction lives in the payload's source
    /// binding, carried uninterpreted for the future classifier).
    ReclamationBegin,
    /// An ancestry-witness manifest object's record.
    AncestryWitnessManifest,
    /// An ancestry-witness shard object's record.
    AncestryWitnessShard,
    /// A V2 Batch completion marker record (`…/reclamation-complete/2`).
    ReclamationCompleteV2,
    /// A V1 completion marker record (`…/reclamation-complete/1`).
    ReclamationCompleteV1,
    /// Any non-reclamation content — observed and carried, never
    /// interpreted (its first-object domain string is preserved
    /// in [`DiscoveredRecord::first_object_domain`]).
    OrdinaryContent,
}

/// What the discovery established about a recognized record's
/// payload — the degrees of observation, never promotion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveredPayload {
    /// The plan payload, strictly decoded and carried (the
    /// structural facts a future authentication consumes).
    Plan(ReclaimPlan),
    /// The begin payload, strictly decoded and carried.
    Begin(ReclamationBegin),
    /// Recognized by domain, key, and shape; the payload is
    /// deliberately NOT decoded or verified here (audits,
    /// witnesses, and completion markers — their verification
    /// belongs to the future authenticated traversal; the V2
    /// markers additionally receive the accepted C2 structural
    /// pass recorded in [`CompletionStructural`]).
    RecognizedNotVerified,
    /// Recognized but the payload refused strict decode — an
    /// observation recorded for the future classifier, never a
    /// trusted promotion.
    MalformedPayload { reason: String },
    /// Non-evidence content — nothing to decode.
    Ordinary,
}

/// One consulted record above the covering checkpoint, in
/// discovery order (newest → oldest), with every fact the future
/// authentication needs: the identity, the parent edge, the
/// epoch, the exact idempotency-key reference, and the first
/// content object's identity and domain tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredRecord {
    /// The record's identity (the hash its canonical bytes were
    /// verified against).
    pub identity: Hash,
    /// The parent the descent would follow next (the terminus
    /// records are carried by [`CoveringObservation`] instead).
    pub parent: Option<Hash>,
    /// The record's chain epoch.
    pub epoch: u64,
    /// The record's exact idempotency key, where present.
    pub key: Option<Vec<u8>>,
    /// The record's first content object's identity (`None` only
    /// for records with no content objects — observed as ordinary
    /// content, carried by their parent edge alone).
    pub first_object: Option<Hash>,
    /// The first content object's leading domain tag, preserved
    /// verbatim for every object-bearing consulted record
    /// (evidence or not).
    pub first_object_domain: Option<String>,
    /// The syntactic family classification.
    pub family: DiscoveredFamily,
    /// The payload observation.
    pub payload: DiscoveredPayload,
}

/// The covering checkpoint — the NEAREST (most recent)
/// checkpoint-metadata record on the head-relative ancestry, with
/// its strictly decoded metadata. Older checkpoints are below the
/// boundary and are never examined here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoveringCheckpoint {
    /// The MK record's identity (hash-verified).
    pub record: Hash,
    /// The MK record's exact idempotency key (the deterministic
    /// checkpoint key), where present.
    pub key: Option<Vec<u8>>,
    /// The metadata object's identity (the MK's single content
    /// reference).
    pub object: Hash,
    /// The strictly decoded checkpoint metadata.
    pub metadata: CheckpointMetadata,
}

/// Whether a covering checkpoint exists on the walked ancestry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoveringObservation {
    /// The nearest checkpoint-metadata record, identified and
    /// bound (its parent equals the metadata's `sk`).
    Identified(Box<CoveringCheckpoint>),
    /// The ancestry reached the founding record with no
    /// checkpoint-metadata record on it — no covering checkpoint
    /// exists head-relative.
    NoneOnAncestry { founding: Hash },
}

/// The explicit unresolved boundary below the covering
/// checkpoint. The identities are facts read ABOVE the boundary
/// (from the metadata); the records themselves are NEVER read,
/// crossed, or certified by this discovery — the state below
/// (the range and every older checkpoint) is the future
/// authenticated classifier's scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryBoundary {
    /// The covering checkpoint's `sk` — MK's parent, the range's
    /// top record (identified from the metadata, never read).
    pub sk: Hash,
    /// The covering checkpoint's `sk_parent` — the self-skip
    /// target carried by the metadata (never read).
    pub sk_parent: Hash,
}

/// The structural observation for a discovered V2 completion
/// marker — the accepted C2 reader's fact and NOTHING else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionStructural {
    /// The C2 structural read succeeded against the discovered
    /// context. STRUCTURAL ONLY: hash-verified, context-matching
    /// bytes — never an authenticated completion receipt, never
    /// a terminal-count fact, never a permission of any kind.
    StructuralCandidate(Box<StructuralBatchCandidate>),
    /// The C2 structural read refused (the typed rule carried).
    StructuralRefusal { rule: String },
    /// The discovered context is incomplete — the plan, the
    /// begin, or the batch's audit was not found above the
    /// covering checkpoint — so the structural read was not
    /// attempted. Missing terminal evidence is an OBSERVATION
    /// here; only the future authenticated classifier may
    /// interpret it.
    ContextIncomplete { missing: Vec<&'static str> },
    /// A V1 `/1` marker — recognized and carried; the existing
    /// V1 completion verifiers own it. No V2 structural read
    /// applies.
    V1Marker,
    /// The record's key does not bind it to the covering
    /// checkpoint (a foreign MK or an unparseable batch index) —
    /// not attempted against the covering context.
    NotBoundToCoveringCheckpoint,
}

/// The observation for one discovered V1 or V2 completion marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionCandidateObservation {
    /// The marker record's identity.
    pub record: Hash,
    /// The batch index parsed from the record's deterministic key
    /// relative to the covering MK (`None` when the key does not
    /// bind — see [`CompletionStructural::NotBoundToCoveringCheckpoint`]).
    pub batch_index: Option<u64>,
    /// The structural observation.
    pub structural: CompletionStructural,
}

/// The discovery result — EXACTLY what was established,
/// head-relative, and nothing more. There is deliberately no
/// verdict, grade, cursor, or completion field of any kind: the
/// destructuring of this type names everything it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryEvidenceDiscovery {
    /// The caller-supplied head — THE OBSERVATION SEED, never an
    /// authority. Every fact below is relative to it.
    pub head: Hash,
    /// The number of records consulted (the supplied head and the
    /// terminus included).
    pub records_consulted: u64,
    /// The covering checkpoint observation.
    pub covering: CoveringObservation,
    /// Every consulted record ABOVE the terminus, in discovery
    /// order (newest → oldest). The terminus itself (the covering
    /// MK, or the founding record when no checkpoint exists) is
    /// carried by [`RecoveryEvidenceDiscovery::covering`].
    pub records: Vec<DiscoveredRecord>,
    /// Present only when a covering checkpoint was identified: the
    /// explicit unresolved boundary below it.
    pub boundary: Option<DiscoveryBoundary>,
    /// The per-marker completion observations (built only when a
    /// covering checkpoint was identified). A plan or audit
    /// without a begin is NOT classified as an intact range here,
    /// and a begin without terminals is NOT classified as
    /// interrupted — the observations are carried for the future
    /// authenticated classifier, which owns those distinctions.
    pub completion_candidates: Vec<CompletionCandidateObservation>,
}

// ============================================================== the walk

/// The read-only recovery-evidence discovery: descends from the
/// supplied HEAD record-by-record through available parent edges
/// — hash-verifying and strictly decoding every consulted record
/// and its first content object — until the NEAREST
/// checkpoint-metadata record (the covering checkpoint) or the
/// founding record, recognizing the candidate V2 plan, audit,
/// begin, witness, and completion-family records above it.
///
/// **The no-skip discipline:** a missing parent above the
/// covering checkpoint refuses typed
/// ([`DiscoveryError::ParentMissing`]) — the walk never consults
/// the presence-based cold walker, never trusts a skip-table
/// crossing, and never invents a skip. The state below the
/// covering checkpoint is never read: the result carries the
/// explicit unresolved [`DiscoveryBoundary`] instead.
///
/// **The head is an observation seed, never an authority:** every
/// positive result is an explicitly head-relative observation,
/// not a canonical installation certificate (see the module
/// documentation for the full trust separation).
///
/// After the descent, each discovered V2 completion marker
/// receives the accepted C2 structural read against the
/// discovered context (the plan/begin/audit identities bound to
/// the covering MK) — the structural layer only, never an
/// authenticated receipt.
pub fn discover_recovery_evidence<R: BeginReads>(
    reads: &mut R,
    head: &Hash,
    limits: &DiscoveryLimits,
) -> Result<RecoveryEvidenceDiscovery, DiscoveryError> {
    limits.validate()?;
    let mut seen_ids: BTreeSet<Hash> = BTreeSet::new();
    let mut seen_keys: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut records: Vec<DiscoveredRecord> = Vec::new();
    let mut current = *head;
    // The record whose parent edge is being followed (None only
    // for the supplied head itself).
    let mut child: Option<(Hash, u64)> = None;
    let mut consulted: u64 = 0;
    let terminus: CoveringObservation;
    loop {
        if !seen_ids.insert(current) {
            return Err(DiscoveryError::CycleDetected { revisited: current });
        }
        if consulted >= limits.max_records {
            return Err(DiscoveryError::TraversalBoundExhausted {
                limit: limits.max_records,
            });
        }
        // The record read — hash-verified in-function (the C2
        // reader's adapter-contract discipline: no implementation
        // subtlety can smuggle unverified bytes past this walk).
        let bytes = match reads.read_record_bytes(&current) {
            Ok(bytes) => bytes,
            Err(MediumError::MissingObject { .. }) => {
                return Err(match child {
                    None => DiscoveryError::HeadRecordMissing { head: current },
                    Some((child_id, _)) => DiscoveryError::ParentMissing {
                        child: child_id,
                        parent: current,
                    },
                });
            }
            Err(MediumError::HashMismatch { .. } | MediumError::ReferencedObjectCorrupt { .. }) => {
                return Err(DiscoveryError::RecordCorrupt { identity: current });
            }
            Err(error) => return Err(DiscoveryError::Reader(error)),
        };
        if hash_object(&bytes) != current {
            return Err(DiscoveryError::RecordCorrupt { identity: current });
        }
        let record = match RevisionRecord::decode(&bytes) {
            Ok(record) => record,
            Err(_) => return Err(DiscoveryError::RecordUndecodable { identity: current }),
        };
        consulted += 1;
        // The strict epoch linkage: a regular record's epoch is
        // exactly its parent's epoch plus one. There are no skip
        // jumps on this walk, so no epoch gap is ever legitimate.
        if let Some((child_id, child_epoch)) = child
            && record.epoch.checked_add(1) != Some(child_epoch)
        {
            return Err(DiscoveryError::EpochLinkageInvalid {
                child: child_id,
                parent: current,
            });
        }
        // The chain-wide idempotency-key uniqueness, refused
        // exactly as the validation walks refuse it.
        if let Some(key) = record.idempotency_key.as_ref()
            && !seen_keys.insert(key.clone())
        {
            return Err(DiscoveryError::KeyDuplicate { record: current });
        }

        // The founding terminus: no checkpoint exists on the
        // walked ancestry (a regular record with no parent cannot
        // decode — the kind-parent coherence — so this arm is the
        // only parentless terminus).
        if matches!(record.kind, RecordKind::Founding { .. }) {
            if record.epoch != 0 {
                return Err(DiscoveryError::FoundingEpochInvalid { identity: current });
            }
            terminus = CoveringObservation::NoneOnAncestry { founding: current };
            break;
        }

        // Every consulted record's first content object: read,
        // hash-verified, domain-peeked (the established walk
        // discipline — every legitimate object in this store is
        // domain-framed; an unpeekable object is corruption).
        // A record with no content objects carries no domain
        // classification — observed as ordinary content and
        // carried by its parent edge alone.
        let Some(first_object) = record.content_refs.first().copied() else {
            records.push(DiscoveredRecord {
                identity: current,
                parent: record.parent,
                epoch: record.epoch,
                key: record.idempotency_key.clone(),
                first_object: None,
                first_object_domain: None,
                family: DiscoveredFamily::OrdinaryContent,
                payload: DiscoveredPayload::Ordinary,
            });
            child = Some((current, record.epoch));
            current = record.parent.expect("regular records decode with a parent");
            continue;
        };
        let object_bytes = match reads.read_object_bytes(&first_object) {
            Ok(bytes) => bytes,
            Err(MediumError::MissingObject { .. }) => {
                return Err(DiscoveryError::EvidenceObjectMissing {
                    record: current,
                    object: first_object,
                });
            }
            Err(MediumError::HashMismatch { .. } | MediumError::ReferencedObjectCorrupt { .. }) => {
                return Err(DiscoveryError::EvidenceObjectCorrupt {
                    record: current,
                    object: first_object,
                });
            }
            Err(error) => return Err(DiscoveryError::Reader(error)),
        };
        if hash_object(&object_bytes) != first_object {
            return Err(DiscoveryError::EvidenceObjectCorrupt {
                record: current,
                object: first_object,
            });
        }
        let domain = match peek_object_domain(&object_bytes) {
            Ok(domain) => domain.to_string(),
            Err(_) => {
                return Err(DiscoveryError::EvidenceObjectUndecodable {
                    record: current,
                    object: first_object,
                });
            }
        };

        // The covering checkpoint: the nearest checkpoint-metadata
        // record ends the walk HERE — nothing below it is read.
        if domain == CHECKPOINT_METADATA_DOMAIN {
            if record.content_refs.len() != 1 {
                return Err(DiscoveryError::CheckpointInvalid {
                    record: current,
                    rule: "checkpoint-metadata/content-refs",
                });
            }
            let metadata = match CheckpointMetadata::decode(&object_bytes) {
                Ok(metadata) => metadata,
                Err(_) => {
                    return Err(DiscoveryError::CheckpointInvalid {
                        record: current,
                        rule: "checkpoint-metadata/undecodable",
                    });
                }
            };
            if record.parent != Some(metadata.sk) {
                return Err(DiscoveryError::CheckpointInvalid {
                    record: current,
                    rule: "checkpoint-metadata/sk-mismatch",
                });
            }
            terminus = CoveringObservation::Identified(Box::new(CoveringCheckpoint {
                record: current,
                key: record.idempotency_key.clone(),
                object: first_object,
                metadata,
            }));
            break;
        }

        // The syntactic family classification (never an
        // authenticity claim). A known family with an unsupported
        // future version refuses closed — interpreting it would
        // affect the discovery's conclusions. Unknown families are
        // ordinary content: observed, never interpreted.
        let FamilyClassification::Public(family) = classify_family(&domain) else {
            return Err(DiscoveryError::UnsupportedEvidenceVariant {
                record: current,
                domain,
            });
        };
        // Every evidence family requires exactly one content
        // object and its deterministic idempotency key.
        if family != DiscoveredFamily::OrdinaryContent {
            if record.content_refs.len() != 1 {
                return Err(DiscoveryError::EvidenceShapeInvalid {
                    record: current,
                    rule: "evidence-family/content-refs",
                });
            }
            if record.idempotency_key.is_none() {
                return Err(DiscoveryError::EvidenceShapeInvalid {
                    record: current,
                    rule: "evidence-family/keyless",
                });
            }
        }

        // The payload observation: the plan and begin payloads are
        // strictly decoded and carried (the structural facts the
        // future authentication consumes); audits, witnesses, and
        // markers are recognized, never verified here.
        let payload = match family {
            DiscoveredFamily::ReclamationPlan => match ReclaimPlan::decode(&object_bytes) {
                Ok(plan) => DiscoveredPayload::Plan(plan),
                Err(error) => DiscoveredPayload::MalformedPayload {
                    reason: format!("{error:?}"),
                },
            },
            DiscoveredFamily::ReclamationBegin => match ReclamationBegin::decode(&object_bytes) {
                Ok(begin) => DiscoveredPayload::Begin(begin),
                Err(error) => DiscoveredPayload::MalformedPayload {
                    reason: format!("{error:?}"),
                },
            },
            DiscoveredFamily::OrdinaryContent => DiscoveredPayload::Ordinary,
            _ => DiscoveredPayload::RecognizedNotVerified,
        };
        records.push(DiscoveredRecord {
            identity: current,
            parent: record.parent,
            epoch: record.epoch,
            key: record.idempotency_key.clone(),
            first_object: Some(first_object),
            first_object_domain: Some(domain),
            family,
            payload,
        });
        child = Some((current, record.epoch));
        current = record.parent.expect("regular records decode with a parent");
    }

    // The post-descent assembly: the boundary and the per-marker
    // structural observations, built only against a covering
    // checkpoint.
    let mut boundary = None;
    let mut completion_candidates: Vec<CompletionCandidateObservation> = Vec::new();
    if let CoveringObservation::Identified(covering) = &terminus {
        boundary = Some(DiscoveryBoundary {
            sk: covering.metadata.sk,
            sk_parent: covering.metadata.sk_parent,
        });
        let mk = covering.record;
        let plan_key = reclamation_plan_idempotency_key(&mk);
        let begin_key = reclamation_begin_idempotency_key(&mk);
        let plan_record = records
            .iter()
            .find(|record| {
                record.family == DiscoveredFamily::ReclamationPlan
                    && record.key.as_deref() == Some(plan_key.as_slice())
            })
            .map(|record| record.identity);
        let begin_record = records
            .iter()
            .find(|record| {
                record.family == DiscoveredFamily::ReclamationBegin
                    && record.key.as_deref() == Some(begin_key.as_slice())
            })
            .map(|record| record.identity);
        // The covering-bound key prefix for the V2 marker keys:
        // "{KEY_DOMAIN}:{mk.hex}:" with the zero-based batch index
        // as the trailing decimal.
        let marker_prefix = format!("{RECLAMATION_COMPLETE_V2_KEY_DOMAIN}:{}:", mk.hex());
        for record in &records {
            match record.family {
                DiscoveredFamily::ReclamationCompleteV1 => {
                    // Recognized, carried for the existing V1
                    // verifiers, never structurally read here.
                    completion_candidates.push(CompletionCandidateObservation {
                        record: record.identity,
                        batch_index: None,
                        structural: CompletionStructural::V1Marker,
                    });
                }
                DiscoveredFamily::ReclamationCompleteV2 => {
                    let Some(key) = record.key.as_deref() else {
                        continue;
                    };
                    let key_str = String::from_utf8_lossy(key);
                    let batch_index = key_str
                        .strip_prefix(marker_prefix.as_str())
                        .and_then(|tail| tail.parse::<u64>().ok());
                    let structural = match (batch_index, plan_record, begin_record) {
                        (Some(batch), Some(plan), Some(begin)) => {
                            let audit_key = reclamation_ledger_v2_idempotency_key(&mk, batch);
                            let audit_record = records
                                .iter()
                                .find(|candidate| {
                                    candidate.family == DiscoveredFamily::ReclamationAuditV2
                                        && candidate.key.as_deref() == Some(audit_key.as_slice())
                                })
                                .map(|candidate| candidate.identity);
                            match audit_record {
                                Some(audit) => {
                                    let expected =
                                        crate::batch_completion::BatchCompletionContext {
                                            mk,
                                            plan,
                                            batch_index: batch,
                                            audit,
                                            begin,
                                        };
                                    match read_structural_batch_candidate(
                                        reads,
                                        &record.identity,
                                        &expected,
                                    ) {
                                        Ok(candidate) => CompletionStructural::StructuralCandidate(
                                            Box::new(candidate),
                                        ),
                                        Err(error) => CompletionStructural::StructuralRefusal {
                                            rule: structural_rule(&error),
                                        },
                                    }
                                }
                                None => CompletionStructural::ContextIncomplete {
                                    missing: vec!["audit"],
                                },
                            }
                        }
                        (Some(_), None, _) => CompletionStructural::ContextIncomplete {
                            missing: vec!["plan"],
                        },
                        (Some(_), Some(_), None) => CompletionStructural::ContextIncomplete {
                            missing: vec!["begin"],
                        },
                        (None, _, _) => CompletionStructural::NotBoundToCoveringCheckpoint,
                    };
                    completion_candidates.push(CompletionCandidateObservation {
                        record: record.identity,
                        batch_index,
                        structural,
                    });
                }
                _ => {}
            }
        }
    }

    Ok(RecoveryEvidenceDiscovery {
        head: *head,
        records_consulted: consulted,
        covering: terminus,
        records,
        boundary,
        completion_candidates,
    })
}

// ==================================================== the family tri-state

/// The internal classification tri-state: the public
/// [`DiscoveredFamily`] plus the unsupported-variant marker that
/// drives the fail-closed refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FamilyClassification {
    Public(DiscoveredFamily),
    Unsupported,
}

/// The syntactic domain classification. The family prefixes are
/// pairwise disjoint; a known family with an unrecognized version
/// classifies Unsupported (refused closed by the caller — its
/// interpretation would affect the discovery's conclusions);
/// unknown families are ordinary content (observed, never
/// interpreted).
fn classify_family(domain: &str) -> FamilyClassification {
    if is_reclamation_complete_family(domain) {
        if domain == RECLAMATION_COMPLETE_V2_DOMAIN {
            return FamilyClassification::Public(DiscoveredFamily::ReclamationCompleteV2);
        }
        if domain == RECLAMATION_COMPLETE_DOMAIN {
            return FamilyClassification::Public(DiscoveredFamily::ReclamationCompleteV1);
        }
        return FamilyClassification::Unsupported;
    }
    if is_reclamation_begin_family(domain) {
        if domain == RECLAMATION_BEGIN_DOMAIN {
            return FamilyClassification::Public(DiscoveredFamily::ReclamationBegin);
        }
        return FamilyClassification::Unsupported;
    }
    if domain.starts_with(RECLAMATION_PLAN_DOMAIN_PREFIX) {
        if domain == RECLAMATION_PLAN_DOMAIN {
            return FamilyClassification::Public(DiscoveredFamily::ReclamationPlan);
        }
        return FamilyClassification::Unsupported;
    }
    if is_reclamation_ledger_family(domain) {
        if domain == RECLAMATION_LEDGER_V2_DOMAIN {
            return FamilyClassification::Public(DiscoveredFamily::ReclamationAuditV2);
        }
        if domain == RECLAMATION_LEDGER_DOMAIN {
            return FamilyClassification::Public(DiscoveredFamily::ReclamationAuditV1);
        }
        return FamilyClassification::Unsupported;
    }
    if is_reclamation_ancestry_witness_family(domain) {
        if domain == RECLAMATION_WITNESS_MANIFEST_DOMAIN {
            return FamilyClassification::Public(DiscoveredFamily::AncestryWitnessManifest);
        }
        if domain == RECLAMATION_WITNESS_SHARD_DOMAIN {
            return FamilyClassification::Public(DiscoveredFamily::AncestryWitnessShard);
        }
        return FamilyClassification::Unsupported;
    }
    FamilyClassification::Public(DiscoveredFamily::OrdinaryContent)
}

/// Extracts the typed rule string from the C2 reader's
/// `MediumError` for the structural-refusal observation.
fn structural_rule(error: &MediumError) -> String {
    match error {
        MediumError::Chain { rule } => (*rule).to_string(),
        MediumError::Format(_) => "format".to_string(),
        _ => "reader".to_string(),
    }
}

// ============================================================== the tests
//
// The owner's B-R1a0 required adversarial matrix (1-11) — pure
// in-memory synthetic-reader tests, no filesystem, no providers,
// no publication, and no destructive execution of any kind. The
// fixtures build genuine canonical records with the real codecs
// (CheckpointMetadata, ReclaimPlan, ReclamationBegin, and the C1
// marker via commit_classification) so every hash verification
// and strict decode runs over honest bytes.

#[cfg(test)]
mod discovery_tests {
    use super::*;
    use crate::ancestry_accounting::StorageRole;
    use crate::ancestry_witness::{
        reclamation_witness_manifest_idempotency_key, reclamation_witness_shard_idempotency_key,
    };
    use crate::batch_completion::{
        BatchCompletionContext, BatchCompletionMarkerV2, ClassificationEntry, EntryClassification,
        commit_classification, reclamation_complete_v2_idempotency_key,
    };
    use crate::checkpoint::{
        ReclaimedIndexEntry, SkipEntry, chain_range_digest, checkpoint_mk_idempotency_key,
        compute_digest_seed,
    };
    use crate::reclaim::{
        PlanBatch, reclamation_complete_idempotency_key, reclamation_ledger_idempotency_key,
    };
    use crate::record::AuthorityMode;
    use engine_core::Encoder;
    use std::collections::BTreeMap;

    const ORDINARY_DOMAIN: &str = "sektor7/test/ordinary";
    const SNAPSHOT_DOMAIN: &str = "engine-slice/session-snapshot/2";

    /// The in-memory synthetic reader (the accepted C2 test
    /// pattern): two byte maps and NOTHING else — no HEAD, no
    /// chain, no founding, no durability state of any kind.
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

        /// Installs a record under its TRUE content identity.
        fn install_record(&mut self, record: &RevisionRecord) -> Hash {
            let bytes = record.canonical_bytes();
            let id = hash_object(&bytes);
            self.records.insert(id, bytes);
            id
        }

        /// Installs a record's bytes under a SUPPLIED identity
        /// (the hash-mismatch adversary — the stored bytes do not
        /// hash to the identity they are stored under).
        fn install_record_under(&mut self, id: Hash, record: &RevisionRecord) {
            self.records.insert(id, record.canonical_bytes());
        }

        /// Installs an object under its TRUE content identity.
        fn install_object(&mut self, bytes: &[u8]) -> Hash {
            let id = hash_object(bytes);
            self.objects.insert(id, bytes.to_vec());
            id
        }
    }

    impl BeginReads for SyntheticReads {
        fn read_record_bytes(&mut self, id: &Hash) -> Result<Vec<u8>, MediumError> {
            self.records
                .get(id)
                .cloned()
                .ok_or(MediumError::MissingObject { hash: *id })
        }
        fn read_object_bytes(&mut self, hash: &Hash) -> Result<Vec<u8>, MediumError> {
            self.objects
                .get(hash)
                .cloned()
                .ok_or(MediumError::MissingObject { hash: *hash })
        }
    }

    /// A domain-framed object with a distinguishing suffix: the
    /// leading tag peek classifies it, the suffix keeps every
    /// fixture object's identity distinct.
    fn domain_object(domain: &str, tag: &[u8]) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.write_str(domain);
        let mut bytes = encoder.into_bytes();
        bytes.extend_from_slice(tag);
        bytes
    }

    /// The sequential chain builder: a founded root, then regular
    /// records pushed one parent at a time.
    struct ChainBuilder {
        reads: SyntheticReads,
        tip: Hash,
        tip_epoch: u64,
    }

    impl ChainBuilder {
        fn founded() -> Self {
            let record = RevisionRecord::founding(AuthorityMode::Standalone, "b-r1a0-discovery", 0);
            let mut reads = SyntheticReads::new();
            let founding = reads.install_record(&record);
            ChainBuilder {
                reads,
                tip: founding,
                tip_epoch: 0,
            }
        }

        fn push(&mut self, key: Option<Vec<u8>>, object_bytes: &[u8]) -> Hash {
            let object = self.reads.install_object(object_bytes);
            let mut record = RevisionRecord::regular(self.tip, self.tip_epoch, 1)
                .with_content_refs(vec![object]);
            if let Some(key) = key {
                record = record.with_idempotency_key(key);
            }
            let identity = self.reads.install_record(&record);
            self.tip = identity;
            self.tip_epoch += 1;
            identity
        }

        fn push_domain(&mut self, domain: &str, tag: &[u8]) -> Hash {
            self.push(None, &domain_object(domain, tag))
        }
    }

    /// The shared covering-chain prefix: founding → anchor → SK →
    /// MK (with genuine metadata over two absent range members).
    struct CoveringChain {
        chain: ChainBuilder,
        anchor: Hash,
        sk: Hash,
        mk: Hash,
        metadata: CheckpointMetadata,
        range_digest: [u8; 32],
        seed: [u8; 32],
    }

    fn covering_chain() -> CoveringChain {
        let mut chain = ChainBuilder::founded();
        let founding = chain.tip;
        let anchor = chain.push_domain(ORDINARY_DOMAIN, b"anchor");
        let sk = chain.push_domain(SNAPSHOT_DOMAIN, b"sk");
        let m0 = hash_object(b"b-r1a0-m0");
        let m1 = hash_object(b"b-r1a0-m1");
        let seed = compute_digest_seed(&founding);
        let range_digest = chain_range_digest(&seed, &[b"m0-bytes", b"m1-bytes"]);
        let metadata = CheckpointMetadata::new(
            sk,
            anchor,
            vec![SkipEntry::self_skip(anchor)],
            vec![
                ReclaimedIndexEntry::new(None, m0, 1),
                ReclaimedIndexEntry::new(None, m1, 2),
            ],
            range_digest,
            seed,
        );
        let mk = chain.push(
            Some(checkpoint_mk_idempotency_key(&sk)),
            &metadata.canonical_bytes(),
        );
        CoveringChain {
            chain,
            anchor,
            sk,
            mk,
            metadata,
            range_digest,
            seed,
        }
    }

    /// The full V2 evidence stack above the covering checkpoint.
    struct Fixture {
        reads: SyntheticReads,
        head: Hash,
        head_epoch: u64,
        anchor: Hash,
        sk: Hash,
        mk: Hash,
        plan: Hash,
        audit0: Hash,
        audit1: Hash,
        begin: Hash,
        manifest: Hash,
        shard: Hash,
        marker0: Hash,
        metadata: CheckpointMetadata,
        plan_payload: ReclaimPlan,
        begin_payload: ReclamationBegin,
        marker_context: BatchCompletionContext,
    }

    fn classification_entries() -> Vec<ClassificationEntry> {
        vec![
            ClassificationEntry {
                identity: hash_object(b"b-r1a0-SK0"),
                role: StorageRole::Record,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            },
            ClassificationEntry {
                identity: hash_object(b"b-r1a0-m0a"),
                role: StorageRole::Record,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            },
            ClassificationEntry {
                identity: hash_object(b"b-r1a0-o0a"),
                role: StorageRole::Object,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            },
            ClassificationEntry {
                identity: hash_object(b"b-r1a0-os"),
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

    fn build_fixture() -> Fixture {
        let mut covering = covering_chain();
        let mk = covering.mk;
        let sk = covering.sk;
        let plan_payload = ReclaimPlan {
            mk,
            sk,
            range_digest: covering.range_digest,
            seed: covering.seed,
            ledger_byte_limit: 64 * 1024 * 1024,
            batches: vec![
                PlanBatch {
                    index: 0,
                    records: vec![sk, hash_object(b"b-r1a0-m0")],
                },
                PlanBatch {
                    index: 1,
                    records: vec![hash_object(b"b-r1a0-m1")],
                },
            ],
            journal_files: Vec::new(),
        };
        let plan = covering.chain.push(
            Some(reclamation_plan_idempotency_key(&mk)),
            &plan_payload.canonical_bytes(),
        );
        let audit0 = covering.chain.push(
            Some(reclamation_ledger_v2_idempotency_key(&mk, 0)),
            &domain_object(RECLAMATION_LEDGER_V2_DOMAIN, b"audit-0"),
        );
        let audit1 = covering.chain.push(
            Some(reclamation_ledger_v2_idempotency_key(&mk, 1)),
            &domain_object(RECLAMATION_LEDGER_V2_DOMAIN, b"audit-1"),
        );
        let begin_payload = ReclamationBegin {
            mk,
            source: plan,
            range_digest: covering.range_digest,
            seed: covering.seed,
        };
        let begin = covering.chain.push(
            Some(reclamation_begin_idempotency_key(&mk)),
            &begin_payload.canonical_bytes(),
        );
        let manifest = covering.chain.push(
            Some(reclamation_witness_manifest_idempotency_key(&mk)),
            &domain_object(RECLAMATION_WITNESS_MANIFEST_DOMAIN, b"manifest"),
        );
        let shard = covering.chain.push(
            Some(reclamation_witness_shard_idempotency_key(&mk, 0)),
            &domain_object(RECLAMATION_WITNESS_SHARD_DOMAIN, b"shard-0"),
        );
        let marker_context = BatchCompletionContext {
            mk,
            plan,
            batch_index: 0,
            audit: audit0,
            begin,
        };
        let marker0 = covering.chain.push(
            Some(reclamation_complete_v2_idempotency_key(&mk, 0)),
            &marker_for(&marker_context).canonical_bytes(),
        );
        let head = covering.chain.push_domain(ORDINARY_DOMAIN, b"head");
        Fixture {
            head_epoch: covering.chain.tip_epoch,
            reads: covering.chain.reads,
            head,
            anchor: covering.anchor,
            sk: covering.sk,
            mk,
            plan,
            audit0,
            audit1,
            begin,
            manifest,
            shard,
            marker0,
            metadata: covering.metadata,
            plan_payload,
            begin_payload,
            marker_context,
        }
    }

    fn default_limits() -> DiscoveryLimits {
        DiscoveryLimits::default_limits()
    }

    // (1) Valid retained evidence above an MK, discovered in
    // exact descending order.
    #[test]
    fn the_valid_retained_evidence_above_the_covering_checkpoint_is_discovered_in_exact_descending_order()
     {
        let mut fixture = build_fixture();
        let discovery =
            discover_recovery_evidence(&mut fixture.reads, &fixture.head, &default_limits())
                .unwrap();
        // The result names everything it holds — there is no
        // verdict, grade, or cursor field to read.
        let RecoveryEvidenceDiscovery {
            head,
            records_consulted,
            covering,
            records,
            boundary,
            completion_candidates,
        } = discovery;
        assert_eq!(head, fixture.head);
        assert_eq!(records_consulted, 9);
        // The exact descending order (newest → oldest), terminus
        // excluded; the founding/anchor/SK segment below the MK is
        // never consulted.
        let order: Vec<Hash> = records.iter().map(|record| record.identity).collect();
        assert_eq!(
            order,
            vec![
                fixture.head,
                fixture.marker0,
                fixture.shard,
                fixture.manifest,
                fixture.begin,
                fixture.audit1,
                fixture.audit0,
                fixture.plan,
            ]
        );
        let expected = [
            (
                DiscoveredFamily::OrdinaryContent,
                None,
                DiscoveredPayload::Ordinary,
            ),
            (
                DiscoveredFamily::ReclamationCompleteV2,
                Some(reclamation_complete_v2_idempotency_key(&fixture.mk, 0)),
                DiscoveredPayload::RecognizedNotVerified,
            ),
            (
                DiscoveredFamily::AncestryWitnessShard,
                Some(reclamation_witness_shard_idempotency_key(&fixture.mk, 0)),
                DiscoveredPayload::RecognizedNotVerified,
            ),
            (
                DiscoveredFamily::AncestryWitnessManifest,
                Some(reclamation_witness_manifest_idempotency_key(&fixture.mk)),
                DiscoveredPayload::RecognizedNotVerified,
            ),
            (
                DiscoveredFamily::ReclamationBegin,
                Some(reclamation_begin_idempotency_key(&fixture.mk)),
                DiscoveredPayload::Begin(fixture.begin_payload.clone()),
            ),
            (
                DiscoveredFamily::ReclamationAuditV2,
                Some(reclamation_ledger_v2_idempotency_key(&fixture.mk, 1)),
                DiscoveredPayload::RecognizedNotVerified,
            ),
            (
                DiscoveredFamily::ReclamationAuditV2,
                Some(reclamation_ledger_v2_idempotency_key(&fixture.mk, 0)),
                DiscoveredPayload::RecognizedNotVerified,
            ),
            (
                DiscoveredFamily::ReclamationPlan,
                Some(reclamation_plan_idempotency_key(&fixture.mk)),
                DiscoveredPayload::Plan(fixture.plan_payload.clone()),
            ),
        ];
        for (record, (family, key, payload)) in records.iter().zip(expected) {
            assert_eq!(record.family, family, "family of {record:?}");
            assert_eq!(record.key, key, "key of {record:?}");
            assert_eq!(record.payload, payload, "payload of {record:?}");
            assert!(record.parent.is_some());
            assert!(record.first_object.is_some());
            assert!(record.first_object_domain.is_some());
        }
        assert_eq!(
            records[1].first_object_domain.as_deref(),
            Some(RECLAMATION_COMPLETE_V2_DOMAIN)
        );
        // The covering checkpoint with its metadata binding.
        let CoveringObservation::Identified(covering) = covering else {
            panic!("the covering checkpoint was not identified")
        };
        assert_eq!(covering.record, fixture.mk);
        assert_eq!(
            covering.key,
            Some(checkpoint_mk_idempotency_key(&fixture.sk))
        );
        assert_eq!(
            covering.object,
            hash_object(&fixture.metadata.canonical_bytes())
        );
        assert_eq!(covering.metadata, fixture.metadata);
        // The explicit unresolved boundary, read from the metadata
        // above it — SK itself is never consulted.
        let boundary = boundary.expect("the boundary is present");
        assert_eq!(boundary.sk, fixture.sk);
        assert_eq!(boundary.sk_parent, fixture.anchor);
        assert_eq!(completion_candidates.len(), 1);
    }

    // (2) A record missing above MK — typed refusal, never an
    // invented skip.
    #[test]
    fn a_record_missing_above_the_checkpoint_refuses_typed_never_an_invented_skip() {
        let mut fixture = build_fixture();
        fixture.reads.records.remove(&fixture.audit1);
        let error =
            discover_recovery_evidence(&mut fixture.reads, &fixture.head, &default_limits())
                .unwrap_err();
        assert!(matches!(
            &error,
            DiscoveryError::ParentMissing { child, parent }
                if *child == fixture.begin && *parent == fixture.audit1
        ));
        // The supplied head itself missing.
        let mut fixture = build_fixture();
        fixture.reads.records.remove(&fixture.head);
        let error =
            discover_recovery_evidence(&mut fixture.reads, &fixture.head, &default_limits())
                .unwrap_err();
        assert!(matches!(
            &error,
            DiscoveryError::HeadRecordMissing { head } if *head == fixture.head
        ));
    }

    // (3) Corrupted bytes under an expected identity — typed
    // refusal.
    #[test]
    fn corrupted_bytes_under_an_expected_identity_refuse_typed() {
        // Record corruption: impostor bytes stored under the
        // plan record's identity.
        let mut fixture = build_fixture();
        fixture
            .reads
            .records
            .insert(fixture.plan, domain_object(ORDINARY_DOMAIN, b"impostor"));
        let error =
            discover_recovery_evidence(&mut fixture.reads, &fixture.head, &default_limits())
                .unwrap_err();
        assert!(matches!(
            &error,
            DiscoveryError::RecordCorrupt { identity } if *identity == fixture.plan
        ));
        // Object corruption: impostor bytes stored under the
        // marker object's identity.
        let mut fixture = build_fixture();
        let marker_object =
            discover_recovery_evidence(&mut fixture.reads, &fixture.head, &default_limits())
                .unwrap()
                .records[1]
                .first_object
                .expect("the marker object is present");
        fixture
            .reads
            .objects
            .insert(marker_object, domain_object(ORDINARY_DOMAIN, b"impostor"));
        let error =
            discover_recovery_evidence(&mut fixture.reads, &fixture.head, &default_limits())
                .unwrap_err();
        assert!(matches!(
            &error,
            DiscoveryError::EvidenceObjectCorrupt { record, object }
                if *record == fixture.marker0 && *object == marker_object
        ));
    }

    // (4) Missing or malformed evidence payload — no trusted
    // promotion.
    #[test]
    fn a_malformed_or_missing_evidence_payload_never_receives_a_trusted_promotion() {
        // (a) A hash-CONSISTENT begin object whose payload refuses
        // strict decode: recognized, observed malformed, never
        // decoded-and-trusted.
        let mut covering = covering_chain();
        let mk = covering.mk;
        covering.chain.push(
            Some(reclamation_plan_idempotency_key(&mk)),
            &ReclaimPlan {
                mk,
                sk: covering.sk,
                range_digest: covering.range_digest,
                seed: covering.seed,
                ledger_byte_limit: 64 * 1024 * 1024,
                batches: vec![PlanBatch {
                    index: 0,
                    records: vec![covering.sk, hash_object(b"b-r1a0-m0")],
                }],
                journal_files: Vec::new(),
            }
            .canonical_bytes(),
        );
        let begin = covering.chain.push(
            Some(reclamation_begin_idempotency_key(&mk)),
            &domain_object(RECLAMATION_BEGIN_DOMAIN, b"garbage-payload"),
        );
        let head = covering.chain.push_domain(ORDINARY_DOMAIN, b"head");
        let discovery =
            discover_recovery_evidence(&mut covering.chain.reads, &head, &default_limits())
                .unwrap();
        let begin_record = discovery
            .records
            .iter()
            .find(|record| record.identity == begin)
            .expect("the begin record was consulted");
        assert_eq!(begin_record.family, DiscoveredFamily::ReclamationBegin);
        assert!(matches!(
            begin_record.payload,
            DiscoveredPayload::MalformedPayload { .. }
        ));
        // (b) The begin record's object missing entirely: the
        // walk fails closed.
        let mut fixture = build_fixture();
        let begin_object =
            discover_recovery_evidence(&mut fixture.reads, &fixture.head, &default_limits())
                .unwrap()
                .records[4]
                .first_object
                .expect("the begin object is present");
        fixture.reads.objects.remove(&begin_object);
        let error =
            discover_recovery_evidence(&mut fixture.reads, &fixture.head, &default_limits())
                .unwrap_err();
        assert!(matches!(
            &error,
            DiscoveryError::EvidenceObjectMissing { record, object }
                if *record == fixture.begin && *object == begin_object
        ));
    }

    // (5) A self-referencing or cyclic ancestry adversary —
    // bounded refusal. (A hash-consistent cycle is unconstructible
    // — each record's identity depends on its parent's — so the
    // constructible adversary is the mismatched-identity
    // self-reference, refused by the in-function hash check; the
    // cycle guard stands as defense-in-depth exactly as the
    // accepted descents keep theirs.)
    #[test]
    fn a_self_referencing_or_cyclic_ancestry_adversary_refuses_within_the_bound() {
        let mut reads = SyntheticReads::new();
        let phantom = hash_object(b"b-r1a0-self-cycle");
        let record = RevisionRecord::regular(phantom, 5, 1);
        reads.install_record_under(phantom, &record);
        let error =
            discover_recovery_evidence(&mut reads, &phantom, &default_limits()).unwrap_err();
        assert!(matches!(
            &error,
            DiscoveryError::RecordCorrupt { identity } if *identity == phantom
        ));
        // The traversal bound.
        let mut fixture = build_fixture();
        let error = discover_recovery_evidence(
            &mut fixture.reads,
            &fixture.head,
            &DiscoveryLimits { max_records: 4 },
        )
        .unwrap_err();
        assert!(matches!(
            &error,
            DiscoveryError::TraversalBoundExhausted { limit } if *limit == 4
        ));
        // Invalid limits.
        assert!(matches!(
            DiscoveryLimits { max_records: 0 }.validate(),
            Err(DiscoveryError::LimitsInvalid { .. })
        ));
        assert!(matches!(
            DiscoveryLimits {
                max_records: MAX_DISCOVERY_RECORDS + 1
            }
            .validate(),
            Err(DiscoveryError::LimitsInvalid { .. })
        ));
        // The founding-epoch adversary.
        let mut reads = SyntheticReads::new();
        let record = RevisionRecord {
            kind: RecordKind::Founding {
                authority: AuthorityMode::Standalone,
                campaign_id: String::from("b-r1a0-bad-founding"),
            },
            epoch: 7,
            parent: None,
            idempotency_key: None,
            tick: 0,
            content_refs: Vec::new(),
            depends_on: Vec::new(),
        };
        let founding = reads.install_record(&record);
        let error =
            discover_recovery_evidence(&mut reads, &founding, &default_limits()).unwrap_err();
        assert!(matches!(
            &error,
            DiscoveryError::FoundingEpochInvalid { identity } if *identity == founding
        ));
    }

    /// The stacked two-checkpoint fixture: an older checkpoint
    /// fully below a newer one.
    struct StackedFixture {
        reads: SyntheticReads,
        head: Hash,
        mk1: Hash,
        sk1: Hash,
        anchor1: Hash,
    }

    fn stacked_fixture() -> StackedFixture {
        let mut chain = ChainBuilder::founded();
        let _anchor0 = chain.push_domain(ORDINARY_DOMAIN, b"anchor-0");
        let sk0 = chain.push_domain(SNAPSHOT_DOMAIN, b"sk-0");
        let metadata0 = CheckpointMetadata::new(
            sk0,
            hash_object(b"b-r1a0-anchor-0"),
            vec![SkipEntry::self_skip(hash_object(b"b-r1a0-anchor-0"))],
            vec![ReclaimedIndexEntry::new(
                None,
                hash_object(b"b-r1a0-m00"),
                1,
            )],
            [1u8; 32],
            [2u8; 32],
        );
        let _mk0 = chain.push(
            Some(checkpoint_mk_idempotency_key(&sk0)),
            &metadata0.canonical_bytes(),
        );
        let anchor1 = chain.push_domain(ORDINARY_DOMAIN, b"anchor-1");
        let sk1 = chain.push_domain(SNAPSHOT_DOMAIN, b"sk-1");
        let metadata1 = CheckpointMetadata::new(
            sk1,
            anchor1,
            vec![SkipEntry::self_skip(anchor1)],
            vec![ReclaimedIndexEntry::new(
                None,
                hash_object(b"b-r1a0-m10"),
                1,
            )],
            [3u8; 32],
            [4u8; 32],
        );
        let mk1 = chain.push(
            Some(checkpoint_mk_idempotency_key(&sk1)),
            &metadata1.canonical_bytes(),
        );
        let plan = chain.push(
            Some(reclamation_plan_idempotency_key(&mk1)),
            &ReclaimPlan {
                mk: mk1,
                sk: sk1,
                range_digest: [3u8; 32],
                seed: [4u8; 32],
                ledger_byte_limit: 64 * 1024 * 1024,
                batches: vec![PlanBatch {
                    index: 0,
                    records: vec![sk1],
                }],
                journal_files: Vec::new(),
            }
            .canonical_bytes(),
        );
        let _begin = chain.push(
            Some(reclamation_begin_idempotency_key(&mk1)),
            &ReclamationBegin {
                mk: mk1,
                source: plan,
                range_digest: [3u8; 32],
                seed: [4u8; 32],
            }
            .canonical_bytes(),
        );
        let head = chain.push_domain(ORDINARY_DOMAIN, b"head");
        StackedFixture {
            reads: chain.reads,
            head,
            mk1,
            sk1,
            anchor1,
        }
    }

    // (6) Multiple checkpoints — the nearest identified without
    // silently certifying older checkpoint state.
    #[test]
    fn multiple_checkpoints_identify_the_nearest_without_certifying_older_state() {
        let fixture = stacked_fixture();
        let mut reads = fixture.reads;
        let discovery =
            discover_recovery_evidence(&mut reads, &fixture.head, &default_limits()).unwrap();
        let CoveringObservation::Identified(covering) = &discovery.covering else {
            panic!("the covering checkpoint was not identified")
        };
        assert_eq!(covering.record, fixture.mk1);
        let boundary = discovery.boundary.expect("the boundary is present");
        assert_eq!(boundary.sk, fixture.sk1);
        assert_eq!(boundary.sk_parent, fixture.anchor1);
        // The older segment (SK1's parent, MK0, SK0, the founding)
        // is present in the reader but NEVER consulted: the count
        // proves it. Above MK1 there are exactly three records
        // (plan, begin, head) plus MK1 itself.
        assert_eq!(discovery.records_consulted, 4);
        assert_eq!(discovery.records.len(), 3);
    }

    // (7) An unresolved older range — no fabricated global
    // completion.
    #[test]
    fn an_unresolved_older_range_is_neither_traversed_nor_globally_completed() {
        // The older checkpoint's range members are entirely absent
        // from the reader (an unresolved partial retirement below
        // the older MK): the discovery succeeds over the COVERING
        // checkpoint without touching the older range, and the
        // result offers no global completion claim — the total
        // destructuring names every field it holds.
        let fixture = stacked_fixture();
        let mut reads = fixture.reads;
        let RecoveryEvidenceDiscovery {
            head,
            records_consulted,
            covering,
            records,
            boundary,
            completion_candidates,
        } = discover_recovery_evidence(&mut reads, &fixture.head, &default_limits()).unwrap();
        assert_eq!(head, fixture.head);
        assert_eq!(records_consulted, 4);
        let CoveringObservation::Identified(covering) = covering else {
            panic!("the covering checkpoint was not identified")
        };
        assert_eq!(covering.record, fixture.mk1);
        let boundary = boundary.expect("the boundary is present");
        assert_eq!(boundary.sk, fixture.sk1);
        assert_eq!(records.len(), 3);
        assert!(completion_candidates.is_empty());
    }

    // (8) A valid structural completion candidate that lacks
    // historical authority — remains structural only.
    #[test]
    fn a_valid_structural_completion_candidate_remains_structural_only() {
        let mut fixture = build_fixture();
        let discovery =
            discover_recovery_evidence(&mut fixture.reads, &fixture.head, &default_limits())
                .unwrap();
        let [candidate] = discovery.completion_candidates.as_slice() else {
            panic!("exactly one completion candidate was expected")
        };
        assert_eq!(candidate.record, fixture.marker0);
        assert_eq!(candidate.batch_index, Some(0));
        let CompletionStructural::StructuralCandidate(structural) = &candidate.structural else {
            panic!("the genuine marker did not read structurally")
        };
        // The C2 authority boundary: the two structural facts and
        // NOTHING else.
        let StructuralBatchCandidate { revision, marker } = &**structural;
        assert_eq!(*revision, fixture.marker0);
        assert_eq!(marker.context(), fixture.marker_context);
        // The lying-marker adversary: keyed for batch 1 while
        // binding batch 0's context — the structural read refuses
        // at the context comparison.
        let marker_object = fixture
            .reads
            .install_object(&marker_for(&fixture.marker_context).canonical_bytes());
        let lying = fixture.reads.install_record(
            &RevisionRecord::regular(fixture.head, fixture.head_epoch, 1)
                .with_idempotency_key(reclamation_complete_v2_idempotency_key(&fixture.mk, 1))
                .with_content_refs(vec![marker_object]),
        );
        let discovery =
            discover_recovery_evidence(&mut fixture.reads, &lying, &default_limits()).unwrap();
        let lying_candidate = discovery
            .completion_candidates
            .iter()
            .find(|candidate| candidate.record == lying)
            .expect("the lying marker was discovered");
        assert_eq!(lying_candidate.batch_index, Some(1));
        assert!(matches!(
            &lying_candidate.structural,
            CompletionStructural::StructuralRefusal { rule }
                if rule == "batch-candidate/context-mismatch"
        ));
    }

    // (9) A prepared but unexecuted V2 plan — evidence presence,
    // not execution.
    #[test]
    fn a_prepared_but_unexecuted_plan_is_evidence_presence_not_execution() {
        let mut covering = covering_chain();
        let mk = covering.mk;
        let plan = covering.chain.push(
            Some(reclamation_plan_idempotency_key(&mk)),
            &ReclaimPlan {
                mk,
                sk: covering.sk,
                range_digest: covering.range_digest,
                seed: covering.seed,
                ledger_byte_limit: 64 * 1024 * 1024,
                batches: vec![
                    PlanBatch {
                        index: 0,
                        records: vec![covering.sk, hash_object(b"b-r1a0-m0")],
                    },
                    PlanBatch {
                        index: 1,
                        records: vec![hash_object(b"b-r1a0-m1")],
                    },
                ],
                journal_files: Vec::new(),
            }
            .canonical_bytes(),
        );
        let audit0 = covering.chain.push(
            Some(reclamation_ledger_v2_idempotency_key(&mk, 0)),
            &domain_object(RECLAMATION_LEDGER_V2_DOMAIN, b"audit-0"),
        );
        // A marker whose begin binding names a record that is NOT
        // installed: the structural read is not attempted.
        let absent_begin = hash_object(b"b-r1a0-absent-begin");
        let marker_context = BatchCompletionContext {
            mk,
            plan,
            batch_index: 0,
            audit: audit0,
            begin: absent_begin,
        };
        let marker = covering.chain.push(
            Some(reclamation_complete_v2_idempotency_key(&mk, 0)),
            &marker_for(&marker_context).canonical_bytes(),
        );
        let head = covering.chain.push_domain(ORDINARY_DOMAIN, b"head");
        let discovery =
            discover_recovery_evidence(&mut covering.chain.reads, &head, &default_limits())
                .unwrap();
        // The plan is present and decoded; the begin is absent —
        // an observation, never an error, and never an
        // execution-state verdict.
        assert!(
            discovery
                .records
                .iter()
                .any(|record| record.family == DiscoveredFamily::ReclamationPlan)
        );
        assert!(
            !discovery
                .records
                .iter()
                .any(|record| record.family == DiscoveredFamily::ReclamationBegin)
        );
        let [candidate] = discovery.completion_candidates.as_slice() else {
            panic!("exactly one completion candidate was expected")
        };
        assert_eq!(candidate.record, marker);
        assert_eq!(candidate.batch_index, Some(0));
        assert!(matches!(
            &candidate.structural,
            CompletionStructural::ContextIncomplete { missing }
                if missing.as_slice() == ["begin"]
        ));
    }

    // (10) V1-only and no-retirement examples — no invented V2
    // obligations.
    #[test]
    fn v1_only_and_no_retirement_ancestries_invent_no_v2_obligations() {
        // (a) V1-only: a V1 audit, a V1 begin (source = the V1
        // audit revision), and a V1 completion marker — recognized
        // and carried, never measured against V2 obligations.
        let mut covering = covering_chain();
        let mk = covering.mk;
        let v1_audit = covering.chain.push(
            Some(reclamation_ledger_idempotency_key(&mk)),
            &domain_object(RECLAMATION_LEDGER_DOMAIN, b"v1-audit"),
        );
        let v1_begin_payload = ReclamationBegin {
            mk,
            source: v1_audit,
            range_digest: covering.range_digest,
            seed: covering.seed,
        };
        let v1_begin = covering.chain.push(
            Some(reclamation_begin_idempotency_key(&mk)),
            &v1_begin_payload.canonical_bytes(),
        );
        let v1_marker = covering.chain.push(
            Some(reclamation_complete_idempotency_key(&mk, Some(0))),
            &domain_object(RECLAMATION_COMPLETE_DOMAIN, b"v1-marker"),
        );
        let head = covering.chain.push_domain(ORDINARY_DOMAIN, b"head");
        let discovery =
            discover_recovery_evidence(&mut covering.chain.reads, &head, &default_limits())
                .unwrap();
        let begin_record = discovery
            .records
            .iter()
            .find(|record| record.identity == v1_begin)
            .expect("the V1 begin was consulted");
        assert_eq!(
            begin_record.payload,
            DiscoveredPayload::Begin(v1_begin_payload)
        );
        assert!(
            discovery
                .records
                .iter()
                .any(|record| record.family == DiscoveredFamily::ReclamationAuditV1)
        );
        let [candidate] = discovery.completion_candidates.as_slice() else {
            panic!("exactly one completion candidate was expected")
        };
        assert_eq!(candidate.record, v1_marker);
        assert!(matches!(
            candidate.structural,
            CompletionStructural::V1Marker
        ));
        // (b) No retirement at all: ordinary records above the MK.
        let mut covering = covering_chain();
        let head = covering.chain.push_domain(ORDINARY_DOMAIN, b"head");
        let discovery =
            discover_recovery_evidence(&mut covering.chain.reads, &head, &default_limits())
                .unwrap();
        assert!(
            discovery
                .records
                .iter()
                .all(|record| record.family == DiscoveredFamily::OrdinaryContent)
        );
        assert_eq!(discovery.records.len(), 1);
        assert!(discovery.completion_candidates.is_empty());
        // (c) A checkpoint-less ancestry reaches the founding.
        let mut chain = ChainBuilder::founded();
        let founding = chain.tip;
        let head = chain.push_domain(ORDINARY_DOMAIN, b"head");
        let discovery =
            discover_recovery_evidence(&mut chain.reads, &head, &default_limits()).unwrap();
        assert_eq!(
            discovery.covering,
            CoveringObservation::NoneOnAncestry { founding }
        );
        assert_eq!(discovery.records_consulted, 2);
        assert!(discovery.boundary.is_none());
        assert!(discovery.completion_candidates.is_empty());
    }

    // (11) Unsupported future evidence variants — fail closed
    // where interpretation would affect the discovery's
    // conclusions.
    #[test]
    fn unsupported_future_evidence_variants_fail_closed_where_interpretation_matters() {
        // A future completion version refuses.
        let mut covering = covering_chain();
        let head = covering.chain.push(
            Some(reclamation_complete_v2_idempotency_key(&covering.mk, 0)),
            &domain_object("engine-persist/reclamation-complete/3", b"future"),
        );
        let error = discover_recovery_evidence(&mut covering.chain.reads, &head, &default_limits())
            .unwrap_err();
        assert!(matches!(
            &error,
            DiscoveryError::UnsupportedEvidenceVariant { domain, .. }
                if domain == "engine-persist/reclamation-complete/3"
        ));
        // A future begin version refuses.
        let mut covering = covering_chain();
        let head = covering
            .chain
            .push_domain("engine-persist/reclamation-begin/2", b"future");
        let error = discover_recovery_evidence(&mut covering.chain.reads, &head, &default_limits())
            .unwrap_err();
        assert!(matches!(
            &error,
            DiscoveryError::UnsupportedEvidenceVariant { domain, .. }
                if domain == "engine-persist/reclamation-begin/2"
        ));
        // A future witness-manifest version refuses.
        let mut covering = covering_chain();
        let head = covering
            .chain
            .push_domain("engine-persist/reclamation-ancestry-witness/2", b"future");
        let error = discover_recovery_evidence(&mut covering.chain.reads, &head, &default_limits())
            .unwrap_err();
        assert!(matches!(
            &error,
            DiscoveryError::UnsupportedEvidenceVariant { domain, .. }
                if domain == "engine-persist/reclamation-ancestry-witness/2"
        ));
        // An unrelated unknown domain is ordinary content —
        // observed, never interpreted, never a refusal.
        let mut covering = covering_chain();
        let head = covering
            .chain
            .push_domain("sektor7/test/future-unrelated", b"content");
        let discovery =
            discover_recovery_evidence(&mut covering.chain.reads, &head, &default_limits())
                .unwrap();
        assert_eq!(
            discovery.records[0].family,
            DiscoveredFamily::OrdinaryContent
        );
        assert_eq!(
            discovery.records[0].first_object_domain.as_deref(),
            Some("sektor7/test/future-unrelated")
        );
    }
}
