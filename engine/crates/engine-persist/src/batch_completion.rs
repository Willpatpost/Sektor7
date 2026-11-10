//! The V2 `/2` Batch completion marker wire codec and the pure
//! role-qualified classification commitment (V2X-CP3-B-P2A-C1 —
//! the owner's nondestructive first B-P2A implementation
//! increment, delivered as a STANDALONE codec only).
//!
//! **The C1 boundary (the owner's authorization):** this module
//! implements the versioned V2 Batch completion data structures,
//! the canonical byte encoding, the strict bounded decoding, the
//! canonical role-qualified classification serialization, the
//! deterministic classification commitment, and the pure
//! validation of marker-internal coherence — NOTHING ELSE. There
//! is NO canonical record publication, NO installed-chain reader,
//! NO provider mutation, NO recovery traversal, NO startup
//! behavior, and NO deletion logic here. The decoder certifies
//! STRUCTURAL VALIDITY ONLY — never installed completion, never
//! provider authority, never a durability grade: no field in this
//! marker implies destructive authorization merely through its
//! presence in an otherwise valid encoding. The accepted V1
//! [`crate::reclaim::ReclamationComplete`] representation is
//! untouched and independent — the V2 wire carries its own
//! versioned domain (`engine-persist/reclamation-complete/2`, an
//! exact string distinct from the V1 `/1`) and its own
//! deterministic key namespace
//! (`engine-persist/reclamation-complete-key/2`), so a V1 marker
//! can never decode as V2 and a V2 marker can never decode as V1
//! (both cross-presentations refuse at the domain gate).
//!
//! **What the marker binds (the wire contract):** the checkpoint
//! MK identity, the installed V2 plan revision identity, the
//! selected batch index, the exact V2 audit revision identity, the
//! exact V2 begin revision identity, the role-qualified
//! classification commitment digest, and the explicit bounded
//! classification counters. The `/2` domain is the BATCH
//! completion marker — the range-terminal marker is a future,
//! separately authorized wire whose contract is deliberately NOT
//! represented or finalized here.
//!
//! **The classification commitment:** a domain-separated SHA-256
//! digest over the complete role-qualified classification
//! partition. Each canonical entry is 35 fixed-width bytes —
//! identity (32), storage role (1: 0=Record, 1=Object), the
//! Mandatory/Deferred classification (1: 0=Mandatory,
//! 1=Deferred), and the conditional-on-prior-batch flag (1:
//! 0=false, 1=true) — streamed in canonical (identity ascending,
//! role ascending) order. The digest preimage binds the
//! commitment to its exact Batch context with fixed-width binary
//! framing (never an ambiguous textual concatenation):
//!
//! ```text
//! offset  size  field
//! 0       16    BATCH_CLASSIFICATION_COMMITMENT_MAGIC
//! 16      4     u32-LE BATCH_CLASSIFICATION_COMMITMENT_VERSION
//! 20      32    the checkpoint MK (raw bytes)
//! 52      32    the plan revision (raw bytes)
//! 84      8     u64-LE the batch index
//! 92      32    the audit revision (raw bytes)
//! 124     32    the begin revision (raw bytes)
//! 156     8     u64-LE the declared entry count
//! 164     35*N  the canonical entries, (identity, role) ascending
//! ```
//!
//! The entry count is declared UP FRONT (the engine's count
//! framing discipline) and the streaming builder refuses any
//! stream that does not push exactly the declared count. The
//! counters (mandatory/deferred × record/object) are carried in
//! the marker ALONGSIDE the digest and are deliberately NOT part
//! of the preimage: they are independently checkable per role
//! precisely because a consumer re-derives them from its own
//! authenticated reconstruction and compares. A matching
//! commitment is NOT proof that the input classification is
//! correct — that proof belongs to the future authenticated
//! historical reconstruction (the R5/R6 reader), never to this
//! codec.
//!
//! **The prior-batch premise flag:** the conditional-on-prior-batch
//! flag is the A3 accounting's per-identity premise — a MANDATORY
//! identity whose safe removal presumes a prior batch's
//! retirement executed (the per-identity form of
//! [`crate::ancestry_accounting::PriorBatchPremise`]). A DEFERRED
//! identity is protected by authenticated surviving roots and
//! carries no removal premise, so a Deferred entry with the flag
//! set is an INCONSISTENT flag combination and refuses typed at
//! every surface (the entry codec, the builder, and the
//! canonicalizing wrapper).
//!
//! **The resource bounds:** every count is bounded BEFORE any
//! allocation trusts it. The per-classification ceilings reuse the
//! accepted A3 accounting's materialized-list bound
//! ([`crate::ancestry_accounting::MAX_ACCOUNTING_CLASSIFIED_LIST`],
//! 2^24 — the Mandatory and Deferred partitions each), the total
//! entry ceiling is their sum, and the batch index is bounded by
//! the plan's own batch-count ceiling
//! ([`crate::reclaim::MAX_RECLAMATION_UNITS`]). The streaming
//! builder retains NO entry (pure hashing — one 35-byte frame at a
//! time), so the commitment computation's memory is O(1) in the
//! classification size.

use engine_core::{Decoder, Encoder, SaveError};
use sha2::{Digest, Sha256};

use crate::ancestry_accounting::{MAX_ACCOUNTING_CLASSIFIED_LIST, StorageRole};
use crate::hash::Hash;
use crate::reclaim::MAX_RECLAMATION_UNITS;

/// The error-section name every typed refusal in this module
/// carries.
const SECTION: &str = "batch-completion";

/// The V2 Batch completion marker's versioned domain — the exact
/// string `engine-persist/reclamation-complete/2`, distinct from
/// the accepted V1 `engine-persist/reclamation-complete/1` (both
/// the domain gate and the key namespace differ, so no V1/V2
/// cross-presentation ever decodes).
pub const RECLAMATION_COMPLETE_V2_DOMAIN: &str = "engine-persist/reclamation-complete/2";

/// The V2 Batch completion marker's wire version.
pub const RECLAMATION_COMPLETE_V2_VERSION: u32 = 2;

/// The V2 Batch completion marker's deterministic idempotency-key
/// domain (deterministic in the MK and the plan-defined batch
/// index — one V2 batch marker per batch, ever; the /2 key domain
/// never collides with the V1 /1 key domain).
pub const RECLAMATION_COMPLETE_V2_KEY_DOMAIN: &str = "engine-persist/reclamation-complete-key/2";

/// The completion-marker family prefix — EVERY versioned
/// completion domain (the accepted V1 `/1` and this V2 `/2`)
/// begins with it. Pure string classification only: family
/// membership makes NO wire claim (an unrecognized future variant
/// still refuses at every reader's exact domain gate), and the
/// range-terminal marker's contract is NOT finalized here.
pub const RECLAMATION_COMPLETE_FAMILY: &str = "engine-persist/reclamation-complete/";

/// Whether a domain belongs to the completion-marker family (any
/// version) — the future publication checkpoints' gate input.
pub fn is_reclamation_complete_family(domain: &str) -> bool {
    domain.starts_with(RECLAMATION_COMPLETE_FAMILY)
}

/// The V2 batch marker's deterministic idempotency key: the key
/// domain, the MK's hex, and the batch index (zero-based V2 batch
/// numbering — batch 0 is the plan's first batch, never invalid).
pub fn reclamation_complete_v2_idempotency_key(mk: &Hash, batch_index: u64) -> Vec<u8> {
    format!(
        "{RECLAMATION_COMPLETE_V2_KEY_DOMAIN}:{}:{batch_index}",
        mk.hex()
    )
    .into_bytes()
}

/// The classification commitment's preimage domain separator — a
/// fixed 16-byte magic, distinct from every other engine domain
/// (the marker's own wire domain is a length-prefixed tag on the
/// marker's object bytes; this magic separates the COMMITMENT
/// preimage, a different layer).
pub const BATCH_CLASSIFICATION_COMMITMENT_MAGIC: &[u8; 16] = b"S7V2BATCHCOMMIT1";

/// The classification commitment scheme's version — the FIRST
/// version of the commitment scheme, deliberately INDEPENDENT of
/// the marker wire version (a future marker-wire change must not
/// silently redefine every existing commitment's preimage).
pub const BATCH_CLASSIFICATION_COMMITMENT_VERSION: u32 = 1;

/// The fixed canonical entry width: identity (32) + role (1) +
/// classification (1) + conditional flag (1).
pub const CLASSIFICATION_ENTRY_BYTES: usize = 35;

/// The total classification entry ceiling — the sum of the two
/// accepted A3 per-partition materialized-list bounds (the
/// Mandatory and Deferred lists each 2^24), so every
/// accounting-bounded batch fits and no unbounded stream is ever
/// accepted.
pub const MAX_V2_BATCH_CLASSIFICATION_ENTRIES: u64 = MAX_ACCOUNTING_CLASSIFIED_LIST * 2;

// ------------------------------------------------------ the wire discriminants

const ROLE_RECORD: u8 = 0;
const ROLE_OBJECT: u8 = 1;
const CLASSIFICATION_MANDATORY: u8 = 0;
const CLASSIFICATION_DEFERRED: u8 = 1;
const FLAG_FALSE: u8 = 0;
const FLAG_TRUE: u8 = 1;

fn role_byte(role: StorageRole) -> u8 {
    match role {
        StorageRole::Record => ROLE_RECORD,
        StorageRole::Object => ROLE_OBJECT,
    }
}

fn decode_role(byte: u8) -> Result<StorageRole, SaveError> {
    match byte {
        ROLE_RECORD => Ok(StorageRole::Record),
        ROLE_OBJECT => Ok(StorageRole::Object),
        other => Err(SaveError::Enum {
            section: SECTION,
            discriminant: u64::from(other),
        }),
    }
}

fn classification_byte(classification: EntryClassification) -> u8 {
    match classification {
        EntryClassification::Mandatory => CLASSIFICATION_MANDATORY,
        EntryClassification::Deferred => CLASSIFICATION_DEFERRED,
    }
}

fn decode_classification(byte: u8) -> Result<EntryClassification, SaveError> {
    match byte {
        CLASSIFICATION_MANDATORY => Ok(EntryClassification::Mandatory),
        CLASSIFICATION_DEFERRED => Ok(EntryClassification::Deferred),
        other => Err(SaveError::Enum {
            section: SECTION,
            discriminant: u64::from(other),
        }),
    }
}

fn flag_byte(flag: bool) -> u8 {
    if flag { FLAG_TRUE } else { FLAG_FALSE }
}

fn decode_flag(byte: u8) -> Result<bool, SaveError> {
    match byte {
        FLAG_FALSE => Ok(false),
        FLAG_TRUE => Ok(true),
        other => Err(SaveError::Enum {
            section: SECTION,
            discriminant: u64::from(other),
        }),
    }
}

// ----------------------------------------------------------- the batch context

/// The exact Batch context a classification commitment binds to:
/// the checkpoint MK, the installed V2 plan revision, the selected
/// batch index, the exact V2 audit revision, and the exact V2
/// begin revision — the same five bindings the marker wire
/// carries (mirroring the accepted A3 accounting's
/// `source_mk`/`plan_revision`/`selected_batch`/`audit_revision`/
/// `begin_revision` result fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchCompletionContext {
    /// The retired checkpoint's MK identity.
    pub mk: Hash,
    /// The installed V2 plan revision's identity.
    pub plan: Hash,
    /// The plan-defined batch index (zero-based).
    pub batch_index: u64,
    /// The exact V2 audit revision's identity (the batch's
    /// ledger record).
    pub audit: Hash,
    /// The exact V2 begin revision's identity.
    pub begin: Hash,
}

impl BatchCompletionContext {
    /// The pure context validation: no placeholder identity in
    /// any binding (the all-zero hash is never a valid revision
    /// identity), and the batch index within the plan family's
    /// own batch-count ceiling (a batch index at or above
    /// [`MAX_RECLAMATION_UNITS`] can never be plan-defined).
    pub fn validate(&self) -> Result<(), SaveError> {
        if self.mk == Hash::ZERO
            || self.plan == Hash::ZERO
            || self.audit == Hash::ZERO
            || self.begin == Hash::ZERO
        {
            return Err(SaveError::Integrity("batch-completion/zero-identity"));
        }
        if self.batch_index >= MAX_RECLAMATION_UNITS {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "batch-index",
                value: self.batch_index,
                max: MAX_RECLAMATION_UNITS - 1,
            });
        }
        Ok(())
    }
}

// ------------------------------------------------- the classification entries

/// The Mandatory/Deferred classification of one role-qualified
/// identity — the compact wire projection of the accepted A3
/// accounting's [`crate::ancestry_accounting::Classification`]
/// (which carries the full protector attribution; this projection
/// keeps only the partition side the commitment binds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryClassification {
    /// Audited for retirement with no authenticated surviving
    /// obligation on its bytes (subject to the per-identity
    /// prior-batch premise flag).
    Mandatory,
    /// Protected by authenticated surviving roots (retained).
    Deferred,
}

/// One role-qualified classification entry: the identity, its
/// storage role, its partition side, and the conditional-on-prior-
/// batch premise flag. The derived order (identity, then role,
/// then classification, then flag) is the canonical wire order —
/// within any duplicate-free set the (identity, role) key is
/// total, so the trailing fields never reorder a valid
/// classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClassificationEntry {
    /// The classified identity (a revision record's or an ordinary
    /// content object's — never the all-zero placeholder).
    pub identity: Hash,
    /// The storage role under which the audit retires it (the
    /// same identity may legitimately appear under BOTH roles —
    /// distinct role-qualified entries, never a duplicate).
    pub role: StorageRole,
    /// The partition side.
    pub classification: EntryClassification,
    /// The per-identity prior-batch premise (a Mandatory-only
    /// concept: a deferred identity's protection is authenticated
    /// now and presumes no prior batch — the combination refuses).
    pub conditional_on_prior_batch: bool,
}

impl ClassificationEntry {
    /// The pure entry validation: the identity is not the
    /// placeholder, and the premise flag is consistent with the
    /// classification (the A3 semantics — the premise is a
    /// Mandatory-side removal dependence only).
    pub fn validate(&self) -> Result<(), SaveError> {
        if self.identity == Hash::ZERO {
            return Err(SaveError::Integrity("batch-completion/zero-identity"));
        }
        if self.classification == EntryClassification::Deferred && self.conditional_on_prior_batch {
            return Err(SaveError::Integrity(
                "batch-completion/conditional-on-deferred",
            ));
        }
        Ok(())
    }

    /// The fixed 35-byte canonical form (the commitment preimage's
    /// entry framing — never a standalone wire section).
    pub fn canonical_bytes(&self) -> [u8; CLASSIFICATION_ENTRY_BYTES] {
        let mut bytes = [0u8; CLASSIFICATION_ENTRY_BYTES];
        bytes[..32].copy_from_slice(self.identity.as_bytes());
        bytes[32] = role_byte(self.role);
        bytes[33] = classification_byte(self.classification);
        bytes[34] = flag_byte(self.conditional_on_prior_batch);
        bytes
    }

    /// The strict entry decode: every discriminant must be a
    /// defined value (reserved values refuse), the identity must
    /// not be the placeholder, and the flag/classification
    /// combination must be consistent.
    pub fn from_canonical_bytes(
        bytes: &[u8; CLASSIFICATION_ENTRY_BYTES],
    ) -> Result<Self, SaveError> {
        let identity = Hash::from_bytes(&bytes[..32])
            .ok_or(SaveError::Integrity("batch-completion/hash-width"))?;
        let role = decode_role(bytes[32])?;
        let classification = decode_classification(bytes[33])?;
        let conditional_on_prior_batch = decode_flag(bytes[34])?;
        let entry = ClassificationEntry {
            identity,
            role,
            classification,
            conditional_on_prior_batch,
        };
        entry.validate()?;
        Ok(entry)
    }
}

// ---------------------------------------------------------- the counters

/// The explicit bounded classification counters — the four
/// (role × partition) cells, each independently checkable against
/// a consumer's own re-derived classification. The counters are
/// carried in the marker ALONGSIDE the commitment digest and are
/// deliberately NOT hashed into the preimage: independent
/// checkability is exactly the property a digest-embedded counter
/// would destroy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassificationCounters {
    /// Mandatory revision records.
    pub mandatory_records: u64,
    /// Deferred revision records.
    pub deferred_records: u64,
    /// Mandatory ordinary objects.
    pub mandatory_objects: u64,
    /// Deferred ordinary objects.
    pub deferred_objects: u64,
}

impl ClassificationCounters {
    /// The all-zero counter set (the builder's initial state).
    pub const EMPTY: ClassificationCounters = ClassificationCounters {
        mandatory_records: 0,
        deferred_records: 0,
        mandatory_objects: 0,
        deferred_objects: 0,
    };

    /// The total entry count (checked — an incoherent counter set
    /// overflows rather than wrapping).
    pub fn total(&self) -> Result<u64, SaveError> {
        let mandatory = self
            .mandatory_records
            .checked_add(self.mandatory_objects)
            .ok_or(SaveError::Overflow { section: SECTION })?;
        let deferred = self
            .deferred_records
            .checked_add(self.deferred_objects)
            .ok_or(SaveError::Overflow { section: SECTION })?;
        mandatory
            .checked_add(deferred)
            .ok_or(SaveError::Overflow { section: SECTION })
    }

    /// The pure counter coherence: each partition side within the
    /// accepted A3 materialized-list bound (2^24 — the Mandatory
    /// and Deferred lists each), the sums representable, and the
    /// classification non-empty (a batch retirement always
    /// involves at least its retiring members).
    pub fn validate(&self) -> Result<(), SaveError> {
        self.validate_with(
            MAX_ACCOUNTING_CLASSIFIED_LIST,
            MAX_ACCOUNTING_CLASSIFIED_LIST,
        )
    }

    /// The parameterized validation core (the C1-F2 corrective):
    /// the SAME production counter-coherence logic with the
    /// partition ceilings as parameters — the production
    /// [`Self::validate`] pins the accepted A3 materialized-list
    /// bound, and the builder's reduced-ceiling test seam drives
    /// this same core deterministically without full-scale
    /// hashing. The ceilings are never a caller authority in
    /// production: only the pinned constants and the test seam
    /// (bounded by the same constants) reach this function.
    fn validate_with(&self, mandatory_max: u64, deferred_max: u64) -> Result<(), SaveError> {
        let mandatory = self
            .mandatory_records
            .checked_add(self.mandatory_objects)
            .ok_or(SaveError::Overflow { section: SECTION })?;
        if mandatory > mandatory_max {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "mandatory-identities",
                value: mandatory,
                max: mandatory_max,
            });
        }
        let deferred = self
            .deferred_records
            .checked_add(self.deferred_objects)
            .ok_or(SaveError::Overflow { section: SECTION })?;
        if deferred > deferred_max {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "deferred-identities",
                value: deferred,
                max: deferred_max,
            });
        }
        if mandatory
            .checked_add(deferred)
            .ok_or(SaveError::Overflow { section: SECTION })?
            == 0
        {
            return Err(SaveError::Integrity("batch-completion/zero-classification"));
        }
        Ok(())
    }
}

// ------------------------------------------------------- the commitment result

/// The computed classification commitment: the domain-separated
/// SHA-256 digest over the canonical entry stream (with the full
/// batch context bound in the preimage) and the per-role counters
/// derived from the same stream. A matching commitment is NOT
/// proof that the classified partition is correct — only that the
/// caller's exact entries and context produce this digest; the
/// correctness proof belongs to the future authenticated
/// historical reconstruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchClassificationCommitment {
    /// The commitment digest (SHA-256 over the specified
    /// preimage; never the all-zero placeholder).
    pub digest: Hash,
    /// The per-role classification counters.
    pub counters: ClassificationCounters,
}

// ---------------------------------------------------- the streaming builder

/// The streaming classification commitment builder: feeds the
/// domain-separated preimage one canonical entry at a time,
/// retaining NO entry (pure hashing — O(1) memory in the
/// classification size). The entry count is declared at
/// construction (the engine's up-front count-framing discipline)
/// and the stream must push EXACTLY that count in strictly
/// ascending (identity, role) order — the production reader's
/// natural input (the accepted A3 result lists are already sorted
/// by identity and role). Disorder, duplicates, inconsistent
/// flags, placeholder identities, and ceiling crossings refuse
/// typed mid-stream, never at completion only.
pub struct ClassificationCommitmentBuilder {
    hasher: Sha256,
    prev: Option<(Hash, StorageRole)>,
    pushed: u64,
    declared: u64,
    counters: ClassificationCounters,
    /// The Mandatory partition's streaming ceiling (the production
    /// constructor pins the accepted A3 bound; the reduced-ceiling
    /// test seam drives the SAME validation core with a small
    /// deterministic bound).
    mandatory_ceiling: u64,
    /// The Deferred partition's streaming ceiling.
    deferred_ceiling: u64,
}

impl ClassificationCommitmentBuilder {
    /// Starts the commitment for the exact batch context over the
    /// declared entry count: the context is validated, the count
    /// is bounded (and non-zero) BEFORE any hashing state is
    /// built, and the fixed-width preimage header (magic, scheme
    /// version, the five context bindings, the count) is fed. The
    /// partition ceilings are the PRODUCTION constants (the
    /// accepted A3 materialized-list bound).
    pub fn new(context: &BatchCompletionContext, entry_count: u64) -> Result<Self, SaveError> {
        Self::new_inner(
            context,
            entry_count,
            MAX_ACCOUNTING_CLASSIFIED_LIST,
            MAX_ACCOUNTING_CLASSIFIED_LIST,
        )
    }

    /// The reduced-ceiling test seam (the C1-F2 corrective,
    /// cfg(test) ONLY): the SAME construction and the SAME
    /// production counter-validation path with small deterministic
    /// partition ceilings, so the boundary discipline (acceptance
    /// AT the ceiling, the typed refusal one past it, with the
    /// exact error category and bound) is exercised without
    /// full-scale hashing. The production numerical ceiling stays
    /// the hard maximum: a seam ceiling above it (or zero)
    /// refuses, exactly as the production bound would.
    #[cfg(test)]
    fn new_with_ceilings(
        context: &BatchCompletionContext,
        entry_count: u64,
        mandatory_ceiling: u64,
        deferred_ceiling: u64,
    ) -> Result<Self, SaveError> {
        if mandatory_ceiling == 0
            || mandatory_ceiling > MAX_ACCOUNTING_CLASSIFIED_LIST
            || deferred_ceiling == 0
            || deferred_ceiling > MAX_ACCOUNTING_CLASSIFIED_LIST
        {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "partition-ceiling",
                value: mandatory_ceiling.max(deferred_ceiling),
                max: MAX_ACCOUNTING_CLASSIFIED_LIST,
            });
        }
        Self::new_inner(context, entry_count, mandatory_ceiling, deferred_ceiling)
    }

    /// The shared construction: the context and count gates, the
    /// preimage header, and the ceiling fields the streaming
    /// counter validation consumes.
    fn new_inner(
        context: &BatchCompletionContext,
        entry_count: u64,
        mandatory_ceiling: u64,
        deferred_ceiling: u64,
    ) -> Result<Self, SaveError> {
        context.validate()?;
        if entry_count == 0 {
            return Err(SaveError::Integrity("batch-completion/zero-classification"));
        }
        if entry_count > MAX_V2_BATCH_CLASSIFICATION_ENTRIES {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "classification-entries",
                value: entry_count,
                max: MAX_V2_BATCH_CLASSIFICATION_ENTRIES,
            });
        }
        let mut hasher = Sha256::new();
        hasher.update(BATCH_CLASSIFICATION_COMMITMENT_MAGIC);
        hasher.update(BATCH_CLASSIFICATION_COMMITMENT_VERSION.to_le_bytes());
        hasher.update(context.mk.as_bytes());
        hasher.update(context.plan.as_bytes());
        hasher.update(context.batch_index.to_le_bytes());
        hasher.update(context.audit.as_bytes());
        hasher.update(context.begin.as_bytes());
        hasher.update(entry_count.to_le_bytes());
        Ok(ClassificationCommitmentBuilder {
            hasher,
            prev: None,
            pushed: 0,
            declared: entry_count,
            counters: ClassificationCounters::EMPTY,
            mandatory_ceiling,
            deferred_ceiling,
        })
    }

    /// Streams one canonical entry: validates it, enforces the
    /// strictly ascending (identity, role) order (an equal key is
    /// a duplicate role-qualified identity; a lesser key is an
    /// unsorted stream), updates the per-role counters under the
    /// partition ceilings, and feeds the fixed 35-byte form to
    /// the commitment hash.
    pub fn push_entry(&mut self, entry: ClassificationEntry) -> Result<(), SaveError> {
        entry.validate()?;
        if self.pushed >= self.declared {
            return Err(SaveError::Integrity("batch-completion/count-exceeded"));
        }
        let key = (entry.identity, entry.role);
        if let Some(prev) = self.prev {
            if key == prev {
                return Err(SaveError::Duplicate {
                    section: SECTION,
                    seq: self.pushed,
                });
            }
            if key < prev {
                return Err(SaveError::Order { section: SECTION });
            }
        }
        match (entry.role, entry.classification) {
            (StorageRole::Record, EntryClassification::Mandatory) => {
                self.counters.mandatory_records = self
                    .counters
                    .mandatory_records
                    .checked_add(1)
                    .ok_or(SaveError::Overflow { section: SECTION })?;
            }
            (StorageRole::Record, EntryClassification::Deferred) => {
                self.counters.deferred_records = self
                    .counters
                    .deferred_records
                    .checked_add(1)
                    .ok_or(SaveError::Overflow { section: SECTION })?;
            }
            (StorageRole::Object, EntryClassification::Mandatory) => {
                self.counters.mandatory_objects = self
                    .counters
                    .mandatory_objects
                    .checked_add(1)
                    .ok_or(SaveError::Overflow { section: SECTION })?;
            }
            (StorageRole::Object, EntryClassification::Deferred) => {
                self.counters.deferred_objects = self
                    .counters
                    .deferred_objects
                    .checked_add(1)
                    .ok_or(SaveError::Overflow { section: SECTION })?;
            }
        }
        self.counters
            .validate_with(self.mandatory_ceiling, self.deferred_ceiling)?;
        let bytes = entry.canonical_bytes();
        self.hasher.update(bytes);
        self.prev = Some(key);
        self.pushed += 1;
        Ok(())
    }

    /// Completes the commitment: the pushed count must equal the
    /// declared count exactly (a short or long stream is counter
    /// incoherence, never a partial commitment), and the result
    /// carries the digest with the stream-derived counters.
    pub fn finish(self) -> Result<BatchClassificationCommitment, SaveError> {
        if self.pushed != self.declared {
            return Err(SaveError::Integrity("batch-completion/count-mismatch"));
        }
        let digest = Hash::from_bytes(&self.hasher.finalize()).expect("the SHA-256 output width");
        Ok(BatchClassificationCommitment {
            digest,
            counters: self.counters,
        })
    }
}

/// The canonicalizing commitment computation: given a
/// classification as an unordered set of entries (any iteration
/// order), validates and bounds it, canonicalizes (sorts by the
/// derived entry order — (identity, role) total on a
/// duplicate-free set), rejects duplicate role-qualified
/// identities, and drives the streaming builder. Equivalent input
/// sets in different iteration orders yield IDENTICAL
/// commitments; different partitions of the same identities never
/// do.
pub fn commit_classification(
    context: &BatchCompletionContext,
    entries: Vec<ClassificationEntry>,
) -> Result<BatchClassificationCommitment, SaveError> {
    let count = u64::try_from(entries.len()).expect("usize exceeds the u64 wire width");
    if count > MAX_V2_BATCH_CLASSIFICATION_ENTRIES {
        return Err(SaveError::Limit {
            section: SECTION,
            kind: "classification-entries",
            value: count,
            max: MAX_V2_BATCH_CLASSIFICATION_ENTRIES,
        });
    }
    let mut sorted = entries;
    sorted.sort_unstable();
    for index in 1..sorted.len() {
        if (sorted[index - 1].identity, sorted[index - 1].role)
            == (sorted[index].identity, sorted[index].role)
        {
            return Err(SaveError::Duplicate {
                section: SECTION,
                // The first duplicate's 1-based position.
                seq: index as u64,
            });
        }
    }
    let mut builder = ClassificationCommitmentBuilder::new(context, count)?;
    for entry in sorted {
        builder.push_entry(entry)?;
    }
    builder.finish()
}

// ------------------------------------------------------------- the marker

/// The V2 `/2` Batch completion marker: the exact batch context,
/// the role-qualified classification commitment digest, and the
/// explicit bounded classification counters. STRUCTURAL EVIDENCE
/// ONLY — decoding this marker certifies its internal coherence,
/// never that the batch's storage obligations were discharged,
/// never that the marker is installed on a chain, and never any
/// durability grade or destructive authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchCompletionMarkerV2 {
    /// The retired checkpoint's MK identity.
    pub mk: Hash,
    /// The installed V2 plan revision's identity.
    pub plan: Hash,
    /// The plan-defined batch index (zero-based).
    pub batch_index: u64,
    /// The exact V2 audit revision's identity.
    pub audit: Hash,
    /// The exact V2 begin revision's identity.
    pub begin: Hash,
    /// The role-qualified classification commitment digest.
    pub commitment: Hash,
    /// The explicit bounded classification counters.
    pub counters: ClassificationCounters,
}

impl BatchCompletionMarkerV2 {
    /// The marker's own batch context (the commitment's binding
    /// frame — a consumer re-derives the commitment under exactly
    /// these five bindings).
    pub fn context(&self) -> BatchCompletionContext {
        BatchCompletionContext {
            mk: self.mk,
            plan: self.plan,
            batch_index: self.batch_index,
            audit: self.audit,
            begin: self.begin,
        }
    }

    /// Canonical bytes (the marker revision's single content
    /// object): the length-prefixed `/2` domain tag, the wire
    /// version, the five context bindings, the commitment digest,
    /// and the four counters — all fixed-width, no variable-length
    /// sections.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_str(RECLAMATION_COMPLETE_V2_DOMAIN);
        enc.write_u32(RECLAMATION_COMPLETE_V2_VERSION);
        enc.write_bytes(self.mk.as_bytes());
        enc.write_bytes(self.plan.as_bytes());
        enc.write_u64(self.batch_index);
        enc.write_bytes(self.audit.as_bytes());
        enc.write_bytes(self.begin.as_bytes());
        enc.write_bytes(self.commitment.as_bytes());
        enc.write_u64(self.counters.mandatory_records);
        enc.write_u64(self.counters.deferred_records);
        enc.write_u64(self.counters.mandatory_objects);
        enc.write_u64(self.counters.deferred_objects);
        enc.into_bytes()
    }

    /// Strict decode: the domain gate (a V1 marker refuses here —
    /// the exact `/2` string never matches `/1`), the version
    /// gate, the placeholder-identity refusals on every hash
    /// binding, the batch-index ceiling, the exact digest width,
    /// the counter ceilings and coherence (partition sums within
    /// the A3 bounds, checked arithmetic, non-empty), and the
    /// trailing-data refusal. Structural validity only.
    pub fn decode(bytes: &[u8]) -> Result<Self, SaveError> {
        let mut dec = Decoder::new(bytes);
        dec.read_domain(RECLAMATION_COMPLETE_V2_DOMAIN)?;
        let version = dec.read_u32()?;
        if version != RECLAMATION_COMPLETE_V2_VERSION {
            return Err(SaveError::Version {
                observed: version,
                supported: RECLAMATION_COMPLETE_V2_VERSION,
            });
        }
        let read_binding = |dec: &mut Decoder| -> Result<Hash, SaveError> {
            let bytes = dec.read_bytes(Hash::WIDTH, SECTION)?;
            Hash::from_bytes(bytes).ok_or(SaveError::Integrity("batch-completion/hash-width"))
        };
        let mk = read_binding(&mut dec)?;
        let plan = read_binding(&mut dec)?;
        let batch_index = dec.read_u64()?;
        let audit = read_binding(&mut dec)?;
        let begin = read_binding(&mut dec)?;
        let commitment = read_binding(&mut dec)?;
        let context = BatchCompletionContext {
            mk,
            plan,
            batch_index,
            audit,
            begin,
        };
        context.validate()?;
        if commitment == Hash::ZERO {
            return Err(SaveError::Integrity("batch-completion/zero-identity"));
        }
        let counters = ClassificationCounters {
            mandatory_records: dec.read_u64()?,
            deferred_records: dec.read_u64()?,
            mandatory_objects: dec.read_u64()?,
            deferred_objects: dec.read_u64()?,
        };
        counters.validate()?;
        dec.finish()?;
        Ok(BatchCompletionMarkerV2 {
            mk: context.mk,
            plan: context.plan,
            batch_index: context.batch_index,
            audit: context.audit,
            begin: context.begin,
            commitment,
            counters,
        })
    }

    /// The pure re-derivation check (the future authenticated
    /// reader's core step, delivered codec-side): re-computes the
    /// commitment over the caller's classification under this
    /// marker's own context and compares BOTH the digest and the
    /// per-role counters. A match certifies only that the supplied
    /// entries and context produce this marker's bindings — never
    /// that the supplied classification is the historically
    /// correct one.
    pub fn verify_classification(
        &self,
        entries: Vec<ClassificationEntry>,
    ) -> Result<(), SaveError> {
        let derived = commit_classification(&self.context(), entries)?;
        if derived.digest != self.commitment {
            return Err(SaveError::Integrity("batch-completion/commitment-mismatch"));
        }
        if derived.counters != self.counters {
            return Err(SaveError::Integrity("batch-completion/counter-mismatch"));
        }
        Ok(())
    }
}

// ============================================================== the C1 tests
//
// The owner's required adversarial matrix (A-G) plus the canonical
// round-trips — pure codec tests, no filesystem, no providers, no
// destructive execution of any kind.

#[cfg(test)]
mod c1_tests {
    use super::*;
    use crate::hash::hash_object;

    fn fixture_context() -> BatchCompletionContext {
        BatchCompletionContext {
            mk: hash_object(b"c1-fixture-mk"),
            plan: hash_object(b"c1-fixture-plan"),
            batch_index: 0,
            audit: hash_object(b"c1-fixture-audit"),
            begin: hash_object(b"c1-fixture-begin"),
        }
    }

    fn sk0() -> Hash {
        hash_object(b"c1-fixture-SK0")
    }
    fn m0a() -> Hash {
        hash_object(b"c1-fixture-m0a")
    }
    fn o0a() -> Hash {
        hash_object(b"c1-fixture-o0a")
    }
    fn os() -> Hash {
        hash_object(b"c1-fixture-os")
    }

    /// The directive's test-A fixture: the CORRECT partition —
    /// Mandatory: SK0, m0a, o0a; Deferred: os (SK0 and m0a are
    /// records; o0a and os are objects; batch 0 is the plan's
    /// first, so no entry carries the prior-batch premise).
    fn correct_partition() -> Vec<ClassificationEntry> {
        vec![
            ClassificationEntry {
                identity: sk0(),
                role: StorageRole::Record,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            },
            ClassificationEntry {
                identity: m0a(),
                role: StorageRole::Record,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            },
            ClassificationEntry {
                identity: o0a(),
                role: StorageRole::Object,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            },
            ClassificationEntry {
                identity: os(),
                role: StorageRole::Object,
                classification: EntryClassification::Deferred,
                conditional_on_prior_batch: false,
            },
        ]
    }

    /// The directive's test-A adversary: the INCORRECT partition
    /// over the SAME four identities with o0a and os swapped —
    /// IDENTICAL per-role counters, a different commitment.
    fn incorrect_partition() -> Vec<ClassificationEntry> {
        vec![
            ClassificationEntry {
                identity: sk0(),
                role: StorageRole::Record,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            },
            ClassificationEntry {
                identity: m0a(),
                role: StorageRole::Record,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            },
            ClassificationEntry {
                identity: os(),
                role: StorageRole::Object,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            },
            ClassificationEntry {
                identity: o0a(),
                role: StorageRole::Object,
                classification: EntryClassification::Deferred,
                conditional_on_prior_batch: false,
            },
        ]
    }

    fn committed_marker() -> BatchCompletionMarkerV2 {
        let context = fixture_context();
        let commitment = commit_classification(&context, correct_partition()).unwrap();
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

    #[test]
    fn the_marker_round_trips_and_reencodes_deterministically() {
        let marker = committed_marker();
        let bytes = marker.canonical_bytes();
        let restored = BatchCompletionMarkerV2::decode(&bytes).unwrap();
        assert_eq!(restored, marker);
        // Deterministic re-encoding: the exact bytes, twice.
        assert_eq!(restored.canonical_bytes(), bytes);
        assert_eq!(marker.canonical_bytes(), bytes);
        // The context accessor reproduces the commitment frame.
        assert_eq!(restored.context(), fixture_context());
    }

    #[test]
    fn the_commitment_binds_the_full_batch_context() {
        let context = fixture_context();
        let base = commit_classification(&context, correct_partition()).unwrap();
        // Determinism: the same inputs commit to the same digest.
        let again = commit_classification(&context, correct_partition()).unwrap();
        assert_eq!(base.digest, again.digest);
        // Each context binding participates in the preimage.
        for mutated in [
            BatchCompletionContext {
                mk: hash_object(b"c1-other-mk"),
                ..context
            },
            BatchCompletionContext {
                plan: hash_object(b"c1-other-plan"),
                ..context
            },
            BatchCompletionContext {
                batch_index: 1,
                ..context
            },
            BatchCompletionContext {
                audit: hash_object(b"c1-other-audit"),
                ..context
            },
            BatchCompletionContext {
                begin: hash_object(b"c1-other-begin"),
                ..context
            },
        ] {
            let other = commit_classification(&mutated, correct_partition()).unwrap();
            assert_ne!(base.digest, other.digest);
        }
    }

    #[test]
    fn the_same_count_repartition_differs() {
        let context = fixture_context();
        let correct = commit_classification(&context, correct_partition()).unwrap();
        let incorrect = commit_classification(&context, incorrect_partition()).unwrap();
        // The adversary's whole point: identical per-role counters
        // under a different identity partition — the counters
        // alone can never distinguish them.
        assert_eq!(correct.counters, incorrect.counters);
        assert_eq!(correct.counters.mandatory_records, 2);
        assert_eq!(correct.counters.mandatory_objects, 1);
        assert_eq!(correct.counters.deferred_records, 0);
        assert_eq!(correct.counters.deferred_objects, 1);
        // The commitment does.
        assert_ne!(correct.digest, incorrect.digest);
        // And the marker-side verification refuses the adversary.
        let marker = committed_marker();
        assert!(matches!(
            marker.verify_classification(incorrect_partition()),
            Err(SaveError::Integrity("batch-completion/commitment-mismatch"))
        ));
        assert!(marker.verify_classification(correct_partition()).is_ok());
    }

    #[test]
    fn the_role_collision_is_represented_unambiguously() {
        let context = fixture_context();
        // The SAME identity under both storage roles: two distinct
        // role-qualified entries, never a duplicate — the
        // record/object namespace distinction the exact accounting
        // preserves.
        let both_roles = vec![
            ClassificationEntry {
                identity: sk0(),
                role: StorageRole::Record,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            },
            ClassificationEntry {
                identity: sk0(),
                role: StorageRole::Object,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            },
        ];
        let commitment = commit_classification(&context, both_roles.clone()).unwrap();
        assert_eq!(commitment.counters.mandatory_records, 1);
        assert_eq!(commitment.counters.mandatory_objects, 1);
        // Deterministic and reproducible regardless of the input
        // iteration order (the reversed set commits identically).
        let mut reversed = both_roles.clone();
        reversed.reverse();
        let reordered = commit_classification(&context, reversed).unwrap();
        assert_eq!(commitment.digest, reordered.digest);
        // The single-role forms are DIFFERENT commitments — the
        // role qualifier is bound, never collapsed.
        let record_only = commit_classification(
            &context,
            vec![ClassificationEntry {
                identity: sk0(),
                role: StorageRole::Record,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            }],
        )
        .unwrap();
        let object_only = commit_classification(
            &context,
            vec![ClassificationEntry {
                identity: sk0(),
                role: StorageRole::Object,
                classification: EntryClassification::Mandatory,
                conditional_on_prior_batch: false,
            }],
        )
        .unwrap();
        assert_ne!(commitment.digest, record_only.digest);
        assert_ne!(commitment.digest, object_only.digest);
        assert_ne!(record_only.digest, object_only.digest);
    }

    #[test]
    fn the_conditional_flag_changes_the_commitment() {
        let context = fixture_context();
        let unconditional = commit_classification(&context, correct_partition()).unwrap();
        // The same partition with SK0's removal premise flagged
        // (a prior batch's audit also covers it): a different
        // commitment for identical counters.
        let mut flagged = correct_partition();
        for entry in &mut flagged {
            if entry.identity == sk0() && entry.role == StorageRole::Record {
                entry.conditional_on_prior_batch = true;
            }
        }
        let conditional = commit_classification(&context, flagged).unwrap();
        assert_eq!(unconditional.counters, conditional.counters);
        assert_ne!(unconditional.digest, conditional.digest);
        // The premise is a Mandatory-side concept only: a Deferred
        // entry carrying it refuses at every surface.
        let mut inconsistent = correct_partition();
        for entry in &mut inconsistent {
            if entry.identity == os() {
                entry.conditional_on_prior_batch = true;
            }
        }
        assert!(matches!(
            commit_classification(&context, inconsistent),
            Err(SaveError::Integrity(
                "batch-completion/conditional-on-deferred"
            ))
        ));
    }

    #[test]
    fn the_canonical_form_is_order_independent() {
        let context = fixture_context();
        let base = commit_classification(&context, correct_partition()).unwrap();
        // Equivalent input sets in different iteration orders
        // (every rotation of the four-entry fixture, plus the
        // reverse) yield the identical canonical commitment.
        let entries = correct_partition();
        for shift in 0..entries.len() {
            let mut rotated = entries.clone();
            rotated.rotate_left(shift);
            assert_eq!(
                commit_classification(&context, rotated).unwrap().digest,
                base.digest
            );
        }
        let mut reversed = entries;
        reversed.reverse();
        assert_eq!(
            commit_classification(&context, reversed).unwrap().digest,
            base.digest
        );
        // A duplicate role-qualified identity refuses (the same
        // identity AND role twice — the classification flip does
        // not save it: the (identity, role) key is the identity).
        let mut duplicated = correct_partition();
        let last = duplicated[3];
        duplicated.push(ClassificationEntry {
            identity: last.identity,
            role: last.role,
            classification: EntryClassification::Mandatory,
            conditional_on_prior_batch: false,
        });
        assert!(matches!(
            commit_classification(&context, duplicated),
            Err(SaveError::Duplicate { .. })
        ));
        // A placeholder identity refuses.
        let mut zeroed = correct_partition();
        zeroed[0].identity = Hash::ZERO;
        assert!(matches!(
            commit_classification(&context, zeroed),
            Err(SaveError::Integrity("batch-completion/zero-identity"))
        ));
    }

    #[test]
    fn the_v1_and_v2_wires_never_cross_decode() {
        // A genuine V1 batch marker presented to the V2 decoder.
        let context = fixture_context();
        let v1 = crate::reclaim::ReclamationComplete {
            kind: crate::reclaim::CompletionKind::Batch,
            mk: context.mk,
            batch: 0,
            source: hash_object(b"c1-v1-source"),
            mandatory_count: 3,
            deferred: vec![hash_object(b"c1-v1-deferred")],
            journal_files: Vec::new(),
        };
        let v1_bytes = v1.canonical_bytes();
        assert!(matches!(
            BatchCompletionMarkerV2::decode(&v1_bytes),
            Err(SaveError::Domain { .. })
        ));
        // The V2 marker presented to the V1 decoder.
        let v2_bytes = committed_marker().canonical_bytes();
        assert!(matches!(
            crate::reclaim::ReclamationComplete::decode(&v2_bytes),
            Err(SaveError::Domain { .. })
        ));
        // The V1 codec's own fixtures remain unchanged and valid.
        let restored = crate::reclaim::ReclamationComplete::decode(&v1_bytes).unwrap();
        assert_eq!(restored, v1);
        assert_eq!(restored.canonical_bytes(), v1_bytes);
        // The deterministic key namespaces never collide.
        let v1_key = crate::reclaim::reclamation_complete_idempotency_key(&context.mk, Some(0));
        let v2_key = reclamation_complete_v2_idempotency_key(&context.mk, 0);
        assert_ne!(v1_key, v2_key);
        // The family classifier recognizes both generations.
        assert!(is_reclamation_complete_family(
            crate::reclaim::RECLAMATION_COMPLETE_DOMAIN
        ));
        assert!(is_reclamation_complete_family(
            RECLAMATION_COMPLETE_V2_DOMAIN
        ));
        assert!(!is_reclamation_complete_family(
            crate::reclaim::RECLAMATION_PLAN_DOMAIN
        ));
    }

    #[test]
    fn the_marker_decoder_refuses_malformed_forms() {
        let bytes = committed_marker().canonical_bytes();
        // Truncation at every cut, and trailing data.
        for cut in 0..bytes.len() {
            assert!(BatchCompletionMarkerV2::decode(&bytes[..cut]).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            BatchCompletionMarkerV2::decode(&trailing),
            Err(SaveError::Trailing { .. })
        ));
        // A corrupted domain tag (the V1 exact string also refuses
        // — the cross-generation test covers the genuine form).
        let mut bad_domain = bytes.clone();
        bad_domain[10] = b'x';
        assert!(matches!(
            BatchCompletionMarkerV2::decode(&bad_domain),
            Err(SaveError::Domain { .. })
        ));
        // The wrong version.
        let mut bad_version = bytes.clone();
        bad_version[8 + RECLAMATION_COMPLETE_V2_DOMAIN.len()..][..4]
            .copy_from_slice(&3u32.to_le_bytes());
        assert!(matches!(
            BatchCompletionMarkerV2::decode(&bad_version),
            Err(SaveError::Version { .. })
        ));
        // A placeholder MK (the binding's 32 bytes zeroed).
        let mk_at = 8 + RECLAMATION_COMPLETE_V2_DOMAIN.len() + 4 + 8;
        let mut zero_mk = bytes.clone();
        zero_mk[mk_at..mk_at + 32].fill(0);
        assert!(matches!(
            BatchCompletionMarkerV2::decode(&zero_mk),
            Err(SaveError::Integrity("batch-completion/zero-identity"))
        ));
        // An out-of-ceiling batch index (the plan family's own
        // batch-count bound — no plan can define it).
        let mut bad_batch = committed_marker();
        bad_batch.batch_index = MAX_RECLAMATION_UNITS;
        assert!(matches!(
            BatchCompletionMarkerV2::decode(&bad_batch.canonical_bytes()),
            Err(SaveError::Limit { .. })
        ));
        // A malformed commitment length (the digest field's
        // length prefix rewritten — a shorter declared width is a
        // typed failure, never a panic).
        let digest_len_at = 8 // the domain tag's length prefix
            + RECLAMATION_COMPLETE_V2_DOMAIN.len() // the domain tag
            + 4 // the wire version
            + 8 + 32 // the MK binding
            + 8 + 32 // the plan binding
            + 8 // the batch index
            + 8 + 32 // the audit binding
            + 8 + 32; // the begin binding
        let mut bad_width = bytes.clone();
        bad_width[digest_len_at..digest_len_at + 8].copy_from_slice(&5u64.to_le_bytes());
        assert!(matches!(
            BatchCompletionMarkerV2::decode(&bad_width),
            Err(SaveError::Integrity("batch-completion/hash-width"))
        ));
        // Counter incoherence: an over-ceiling partition sum.
        let mut bad_counters = committed_marker();
        bad_counters.counters.mandatory_records = MAX_ACCOUNTING_CLASSIFIED_LIST + 1;
        assert!(matches!(
            BatchCompletionMarkerV2::decode(&bad_counters.canonical_bytes()),
            Err(SaveError::Limit { .. })
        ));
        // Counter overflow: unrepresentable sums.
        let mut overflow = committed_marker();
        overflow.counters.mandatory_records = u64::MAX;
        assert!(matches!(
            BatchCompletionMarkerV2::decode(&overflow.canonical_bytes()),
            Err(SaveError::Overflow { .. })
        ));
        // The empty classification: all four counters zero.
        let mut empty = committed_marker();
        empty.counters = ClassificationCounters::EMPTY;
        assert!(matches!(
            BatchCompletionMarkerV2::decode(&empty.canonical_bytes()),
            Err(SaveError::Integrity("batch-completion/zero-classification"))
        ));
        // A placeholder commitment digest.
        let mut zero_digest = committed_marker();
        zero_digest.commitment = Hash::ZERO;
        assert!(matches!(
            BatchCompletionMarkerV2::decode(&zero_digest.canonical_bytes()),
            Err(SaveError::Integrity("batch-completion/zero-identity"))
        ));
    }

    #[test]
    fn the_entry_codec_is_strict() {
        let entry = ClassificationEntry {
            identity: sk0(),
            role: StorageRole::Record,
            classification: EntryClassification::Mandatory,
            conditional_on_prior_batch: true,
        };
        let bytes = entry.canonical_bytes();
        assert_eq!(size_of_val(&bytes), CLASSIFICATION_ENTRY_BYTES);
        assert_eq!(
            ClassificationEntry::from_canonical_bytes(&bytes).unwrap(),
            entry
        );
        // Reserved role discriminant.
        let mut bad_role = bytes;
        bad_role[32] = 2;
        assert!(matches!(
            ClassificationEntry::from_canonical_bytes(&bad_role),
            Err(SaveError::Enum { .. })
        ));
        // Reserved classification discriminant.
        let mut bad_class = bytes;
        bad_class[33] = 2;
        assert!(matches!(
            ClassificationEntry::from_canonical_bytes(&bad_class),
            Err(SaveError::Enum { .. })
        ));
        // A non-boolean flag byte.
        let mut bad_flag = bytes;
        bad_flag[34] = 2;
        assert!(matches!(
            ClassificationEntry::from_canonical_bytes(&bad_flag),
            Err(SaveError::Enum { .. })
        ));
        // A placeholder identity.
        let mut zero_identity = bytes;
        zero_identity[..32].fill(0);
        assert!(matches!(
            ClassificationEntry::from_canonical_bytes(&zero_identity),
            Err(SaveError::Integrity("batch-completion/zero-identity"))
        ));
        // The inconsistent premise combination (Deferred carrying
        // the conditional flag) refuses at the byte surface too.
        let mut inconsistent = bytes;
        inconsistent[33] = CLASSIFICATION_DEFERRED;
        inconsistent[34] = FLAG_TRUE;
        assert!(matches!(
            ClassificationEntry::from_canonical_bytes(&inconsistent),
            Err(SaveError::Integrity(
                "batch-completion/conditional-on-deferred"
            ))
        ));
    }

    #[test]
    fn the_streaming_builder_enforces_order_duplicates_and_counts() {
        let context = fixture_context();
        // The canonical (sorted) stream for the fixture partition.
        let mut sorted = correct_partition();
        sorted.sort_unstable();
        // An unsorted stream refuses (the order gate).
        let mut builder = ClassificationCommitmentBuilder::new(&context, 4).unwrap();
        let unsorted = {
            let mut entries = sorted.clone();
            entries.reverse();
            entries
        };
        let mut refused_order = false;
        for entry in unsorted {
            if builder.push_entry(entry).is_err() {
                refused_order = true;
            }
        }
        assert!(refused_order);
        // A duplicate role-qualified identity refuses: the
        // immediately-repeated (identity, role) key fires the
        // duplicate gate (a non-adjacent repeat re-presents as an
        // unsorted stream — the ascending-order gate — and refuses
        // there; every duplicate refuses somewhere).
        let mut builder = ClassificationCommitmentBuilder::new(&context, 4).unwrap();
        for entry in &sorted[..3] {
            builder.push_entry(*entry).unwrap();
        }
        assert!(matches!(
            builder.push_entry(sorted[2]),
            Err(SaveError::Duplicate { .. })
        ));
        // A stream that stops short of the declared count refuses
        // at finish (counter incoherence, never a partial
        // commitment).
        let mut builder = ClassificationCommitmentBuilder::new(&context, 4).unwrap();
        for entry in &sorted[..3] {
            builder.push_entry(*entry).unwrap();
        }
        assert!(matches!(
            builder.finish(),
            Err(SaveError::Integrity("batch-completion/count-mismatch"))
        ));
        // A stream that would exceed the declared count refuses
        // mid-stream.
        let mut builder = ClassificationCommitmentBuilder::new(&context, 3).unwrap();
        for entry in &sorted[..3] {
            builder.push_entry(*entry).unwrap();
        }
        assert!(matches!(
            builder.push_entry(sorted[3]),
            Err(SaveError::Integrity("batch-completion/count-exceeded"))
        ));
        // The exact declared count commits, and matches the
        // canonicalizing wrapper over the same set.
        let mut builder = ClassificationCommitmentBuilder::new(&context, 4).unwrap();
        for entry in sorted {
            builder.push_entry(entry).unwrap();
        }
        let streamed = builder.finish().unwrap();
        let wrapped = commit_classification(&context, correct_partition()).unwrap();
        assert_eq!(streamed.digest, wrapped.digest);
        assert_eq!(streamed.counters, wrapped.counters);
        // A zero declared count refuses (the empty classification).
        assert!(matches!(
            ClassificationCommitmentBuilder::new(&context, 0),
            Err(SaveError::Integrity("batch-completion/zero-classification"))
        ));
        // An over-ceiling declared count refuses before any state
        // is built.
        assert!(matches!(
            ClassificationCommitmentBuilder::new(&context, MAX_V2_BATCH_CLASSIFICATION_ENTRIES + 1),
            Err(SaveError::Limit { .. })
        ));
        // The mid-stream partition-ceiling crossing is proven by
        // the dedicated boundary test (the reduced-ceiling seam —
        // the C1-F2 corrective replaced this block's broken
        // full-scale form, whose first generated identity was the
        // all-zero placeholder and whose assertion therefore
        // passed on the WRONG refusal).
    }

    #[test]
    fn the_partition_ceilings_refuse_at_the_boundary() {
        // (The C1-F2 corrective) The partition-ceiling discipline
        // over the SAME production counter-validation logic
        // (validate_with), driven by the deterministic
        // reduced-ceiling test seam so the boundary is reached
        // without full-scale hashing. The production numerical
        // ceiling is preserved as the hard maximum: the seam
        // refuses any ceiling above it, and the pure-counter
        // boundary AT the production constants is proven directly.
        let context = fixture_context();
        // Strictly increasing nonzero identities: the sequence
        // number's big-endian form in the first 8 bytes, starting
        // at 1 (never the all-zero placeholder — the C1 review's
        // finding).
        let entry = |seq: u64, classification: EntryClassification| {
            let mut identity = [0u8; 32];
            identity[..8].copy_from_slice(&seq.to_be_bytes());
            ClassificationEntry {
                identity: Hash::from_bytes(&identity).unwrap(),
                role: StorageRole::Object,
                classification,
                conditional_on_prior_batch: false,
            }
        };
        const SEAM_CEILING: u64 = 8;
        // ACCEPTANCE at the boundary: exactly SEAM_CEILING
        // mandatory entries stream and finish cleanly.
        let mut builder = ClassificationCommitmentBuilder::new_with_ceilings(
            &context,
            SEAM_CEILING,
            SEAM_CEILING,
            SEAM_CEILING,
        )
        .unwrap();
        for seq in 1..=SEAM_CEILING {
            builder
                .push_entry(entry(seq, EntryClassification::Mandatory))
                .unwrap();
        }
        assert!(builder.finish().is_ok());
        // REFUSAL one past the boundary: the next mandatory entry
        // crosses the partition ceiling with the EXACT error
        // category and bound (never a bare is_err).
        let mut builder = ClassificationCommitmentBuilder::new_with_ceilings(
            &context,
            SEAM_CEILING + 1,
            SEAM_CEILING,
            SEAM_CEILING,
        )
        .unwrap();
        for seq in 1..=SEAM_CEILING {
            builder
                .push_entry(entry(seq, EntryClassification::Mandatory))
                .unwrap();
        }
        match builder.push_entry(entry(SEAM_CEILING + 1, EntryClassification::Mandatory)) {
            Err(SaveError::Limit {
                kind: "mandatory-identities",
                value,
                max,
                ..
            }) => {
                assert_eq!(value, SEAM_CEILING + 1);
                assert_eq!(max, SEAM_CEILING);
            }
            other => panic!("the ceiling crossing must refuse with the exact bound: {other:?}"),
        }
        // The DEFERRED ceiling independently (the same validation
        // core's other parameter).
        let mut builder = ClassificationCommitmentBuilder::new_with_ceilings(
            &context,
            SEAM_CEILING + 1,
            SEAM_CEILING,
            SEAM_CEILING,
        )
        .unwrap();
        for seq in 1..=SEAM_CEILING {
            builder
                .push_entry(entry(seq, EntryClassification::Deferred))
                .unwrap();
        }
        match builder.push_entry(entry(SEAM_CEILING + 1, EntryClassification::Deferred)) {
            Err(SaveError::Limit {
                kind: "deferred-identities",
                value,
                max,
                ..
            }) => {
                assert_eq!(value, SEAM_CEILING + 1);
                assert_eq!(max, SEAM_CEILING);
            }
            other => panic!("the deferred crossing must refuse with the exact bound: {other:?}"),
        }
        // The seam preserves the production numerical ceiling as
        // the hard maximum: a ceiling above it (or zero) refuses.
        assert!(matches!(
            ClassificationCommitmentBuilder::new_with_ceilings(
                &context,
                1,
                MAX_ACCOUNTING_CLASSIFIED_LIST + 1,
                MAX_ACCOUNTING_CLASSIFIED_LIST
            ),
            Err(SaveError::Limit { .. })
        ));
        assert!(matches!(
            ClassificationCommitmentBuilder::new_with_ceilings(
                &context,
                1,
                MAX_ACCOUNTING_CLASSIFIED_LIST,
                0
            ),
            Err(SaveError::Limit { .. })
        ));
        // The PRODUCTION boundary, proven purely (no hashing): the
        // counters AT the accepted A3 materialized-list bound
        // validate, and one past refuses with the exact bound.
        let at_bound = ClassificationCounters {
            mandatory_records: MAX_ACCOUNTING_CLASSIFIED_LIST,
            mandatory_objects: 0,
            deferred_records: 0,
            deferred_objects: 1,
        };
        assert!(at_bound.validate().is_ok());
        let past_bound = ClassificationCounters {
            mandatory_records: MAX_ACCOUNTING_CLASSIFIED_LIST + 1,
            ..at_bound
        };
        match past_bound.validate() {
            Err(SaveError::Limit {
                kind: "mandatory-identities",
                value,
                max,
                ..
            }) => {
                assert_eq!(value, MAX_ACCOUNTING_CLASSIFIED_LIST + 1);
                assert_eq!(max, MAX_ACCOUNTING_CLASSIFIED_LIST);
            }
            other => panic!("the production bound must refuse one past it: {other:?}"),
        }
        let deferred_past = ClassificationCounters {
            deferred_objects: MAX_ACCOUNTING_CLASSIFIED_LIST + 1,
            mandatory_records: 1,
            ..at_bound
        };
        match deferred_past.validate() {
            Err(SaveError::Limit {
                kind: "deferred-identities",
                value,
                max,
                ..
            }) => {
                assert_eq!(value, MAX_ACCOUNTING_CLASSIFIED_LIST + 1);
                assert_eq!(max, MAX_ACCOUNTING_CLASSIFIED_LIST);
            }
            other => panic!("the deferred production bound must refuse: {other:?}"),
        }
    }

    #[test]
    fn the_marker_verifies_rederived_classifications() {
        let marker = committed_marker();
        // The correct partition verifies (digest and counters).
        assert!(marker.verify_classification(correct_partition()).is_ok());
        // The same-count repartition refuses on the commitment.
        assert!(matches!(
            marker.verify_classification(incorrect_partition()),
            Err(SaveError::Integrity("batch-completion/commitment-mismatch"))
        ));
        // A counters-only tamper (the digest untouched, one
        // counter field rewritten) refuses on the counters — the
        // independent per-role check the digest cannot provide.
        let mut tampered = committed_marker();
        tampered.counters.mandatory_records += 1;
        assert!(matches!(
            tampered.verify_classification(correct_partition()),
            Err(SaveError::Integrity("batch-completion/counter-mismatch"))
        ));
    }

    #[test]
    fn the_context_validation_refuses_placeholders_and_indices() {
        let context = fixture_context();
        // Every placeholder binding refuses.
        for mutated in [
            BatchCompletionContext {
                mk: Hash::ZERO,
                ..context
            },
            BatchCompletionContext {
                plan: Hash::ZERO,
                ..context
            },
            BatchCompletionContext {
                audit: Hash::ZERO,
                ..context
            },
            BatchCompletionContext {
                begin: Hash::ZERO,
                ..context
            },
        ] {
            assert!(matches!(
                mutated.validate(),
                Err(SaveError::Integrity("batch-completion/zero-identity"))
            ));
            assert!(matches!(
                commit_classification(&mutated, correct_partition()),
                Err(SaveError::Integrity("batch-completion/zero-identity"))
            ));
        }
        // The batch-index ceiling (zero-based: index 0 is valid,
        // the plan family's count bound and above are not).
        assert!(fixture_context().validate().is_ok());
        let mut high = context;
        high.batch_index = MAX_RECLAMATION_UNITS - 1;
        assert!(high.validate().is_ok());
        high.batch_index = MAX_RECLAMATION_UNITS;
        assert!(matches!(
            commit_classification(&high, correct_partition()),
            Err(SaveError::Limit { .. })
        ));
        // The deterministic keys are distinct per batch.
        let key_zero = reclamation_complete_v2_idempotency_key(&context.mk, 0);
        let key_one = reclamation_complete_v2_idempotency_key(&context.mk, 1);
        assert_ne!(key_zero, key_one);
        assert_eq!(
            key_zero,
            format!(
                "{RECLAMATION_COMPLETE_V2_KEY_DOMAIN}:{}:0",
                context.mk.hex()
            )
            .into_bytes()
        );
    }
}
