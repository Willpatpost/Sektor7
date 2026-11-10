//! The V2 `/3` Range-terminal completion marker wire codec and its
//! two pure commitments (V2X-CP3-B-P2A-C3 — the owner's ratified
//! C3 GO, delivered as a STANDALONE codec only).
//!
//! **The C3 boundary (the owner's authorization):** this module
//! implements the versioned V2 Range-terminal completion marker
//! data structure, the canonical byte encoding, the strict bounded
//! decoding, the batch-coverage commitment, the journal-obligation
//! commitment, and the pure validation of marker-internal
//! coherence — NOTHING ELSE. There is NO canonical record
//! publication, NO installed-chain reader, NO provider mutation,
//! NO recovery traversal, NO startup behavior, and NO deletion
//! logic here. The decoder certifies STRUCTURAL VALIDITY ONLY —
//! never installed completion, never provider authority, never a
//! durability grade: no field in this marker implies destructive
//! authorization merely through its presence in an otherwise
//! valid encoding. The accepted V1 `/1` kind-gated
//! [`crate::reclaim::ReclamationComplete`] representation and the
//! accepted V2 `/2` Batch marker
//! [`crate::batch_completion::BatchCompletionMarkerV2`] are both
//! untouched and independent — this `/3` wire carries its own
//! versioned domain (`engine-persist/reclamation-complete/3`, an
//! exact string distinct from the V1 `/1` and the V2 `/2`) and its
//! own deterministic key namespace
//! (`engine-persist/reclamation-complete-key/3`), so no V1 or V2
//! marker can ever decode as V3 and no V3 marker can ever decode
//! as V1 or V2 (every cross-presentation refuses at the domain
//! gate), and the existing completion-family publication fence
//! refuses `/3` through the generic append with NO gate change
//! (the family classifier is a pure prefix match that already
//! recognizes every `/N` variant).
//!
//! **What the marker binds (the wire contract):** the checkpoint
//! MK identity, the installed V2 plan revision identity, the V2
//! begin revision identity, the batch-coverage commitment digest,
//! the journal-obligation commitment digest, the declared batch
//! count, the declared journal count, and the four aggregate
//! classification counters — all fixed-width (exactly 297 encoded
//! bytes), no variable-length lists embedded in the marker. The
//! `/3` domain is the RANGE-TERMINAL marker: it certifies the
//! overall checkpoint retirement at the structural level — the
//! complete batch-terminal coverage and the journal-obligation set
//! — never any single batch's own storage obligations (the `/2`
//! Batch marker's contract).
//!
//! **The batch-coverage commitment:** a domain-separated SHA-256
//! digest over the complete plan-ordered batch-terminal coverage.
//! The preimage is the fixed 16-byte separator `S7V2RANGECOVER1!`,
//! the commitment-scheme version 1 (u32-LE, deliberately
//! independent of the marker wire version — a future marker-wire
//! change must not silently redefine existing commitments), the
//! raw 32-byte MK, plan, and begin bindings, the u64-LE declared
//! batch count, and for each batch in canonical plan order the
//! u64-LE batch index and the raw 32-byte Batch completion marker
//! RECORD identity:
//!
//! ```text
//! offset  size  field
//! 0       16    RANGE_COVERAGE_COMMITMENT_MAGIC
//! 16      4     u32-LE RANGE_COVERAGE_COMMITMENT_VERSION
//! 20      32    the checkpoint MK (raw bytes)
//! 52      32    the plan revision (raw bytes)
//! 84      32    the begin revision (raw bytes)
//! 116     8     u64-LE the declared batch count
//! 124     40*N  per batch: u64-LE index, 32-byte marker identity
//! ```
//!
//! The batch indices are CONSECUTIVE from zero (the plan's own
//! deterministic order — the same rule the accepted
//! [`crate::reclaim::ReclaimPlan`] decode enforces), the entry
//! count is declared UP FRONT (the engine's count-framing
//! discipline), and the streaming builder retains NO entry (pure
//! hashing — O(1) memory in the coverage size). The digest binds
//! candidate marker identities only — it does NOT authenticate
//! their canonical installation.
//!
//! **The journal-obligation commitment:** a domain-separated
//! SHA-256 digest over the journal-obligation list bound by the
//! installed plan. The preimage is the fixed 16-byte separator
//! `S7V2JOURNALOBL1!`, the commitment-scheme version 1 (u32-LE),
//! the three context bindings, the u64-LE declared journal count,
//! and each name as its u64-LE UTF-8 byte length followed by its
//! exact UTF-8 bytes. This is a JOURNAL-OBLIGATION commitment —
//! the precise set of obligations the plan defines — NOT proof of
//! completed journal retirement: future coordinated readers and
//! publishers must separately prove the associated physical and
//! ownership obligations. The canonical order is the sorted
//! ascending byte order of the names (the legitimate producer's
//! list is sorted); duplicates, empty names, over-length names,
//! and reordered input refuse typed. Names are committed
//! BYTE-EXACT — never silently reinterpreted or normalized. An
//! empty obligation list is legitimate and produces the
//! deterministic digest over its zero-count preimage (never the
//! placeholder).
//!
//! **The aggregate counters:** the four (role × partition) cells
//! carried in the marker ALONGSIDE the digests and deliberately
//! NOT hashed into either preimage (independent checkability is
//! exactly the property a digest-embedded counter would destroy).
//! They are the CHECKED SUMS of the corresponding counters of the
//! Batch markers covered by the Range terminal — NOT globally
//! deduplicated distinct-identity counts. Each Mandatory or
//! Deferred partition sum is bounded by 2^40 (the accepted 2^24
//! per-batch classified-list ceiling times the 2^16 batch-count
//! ceiling), and the combined aggregate is coherent with a
//! nonempty sequence of nonempty Batch classifications: every
//! covered batch marker's classification is nonempty by the `/2`
//! codec's own refusal of zero-classification, so the aggregate
//! total is at least the batch count.
//!
//! **The resource bounds:** every count is bounded BEFORE any
//! allocation trusts it. The batch count reuses the plan family's
//! own ceiling ([`crate::reclaim::MAX_RECLAMATION_UNITS`], 2^16,
//! and at least one batch — a real retirement is never empty),
//! the journal count reuses the plan family's journal-file
//! ceiling, each journal name is bounded by the plan family's
//! name-byte ceiling, and the aggregate partition sums are
//! bounded by 2^40 with checked arithmetic throughout.
//!
//! **The authority boundary (the owner's C3 GO):** a decoded
//! Range marker or a matching pure commitment establishes ONLY
//! structural coherence with supplied evidence. It MUST NOT
//! establish: installed Range completion; canonical HEAD
//! ancestry; trusted Batch-marker publication; historically
//! correct Mandatory/Deferred classifications; completed
//! physical deletion; discharged journal-retirement obligations;
//! updated journal ownership; any durability grade; recovery
//! convergence; or any permission to reclaim or skip revisions.
//! No capability or result field implies these privileges. The
//! expected context and the supplied observations are
//! CALLER-SUPPLIED and may themselves be untrusted — matching
//! them is a structural comparison, not an authentication of
//! their origin.

use engine_core::{Decoder, Encoder, SaveError};
use sha2::{Digest, Sha256};

use crate::ancestry_accounting::MAX_ACCOUNTING_CLASSIFIED_LIST;
use crate::batch_completion::ClassificationCounters;
use crate::hash::Hash;
use crate::reclaim::{
    MAX_RECLAMATION_JOURNAL_FILES, MAX_RECLAMATION_JOURNAL_NAME, MAX_RECLAMATION_UNITS,
};

/// The error-section name every typed refusal in this module
/// carries.
const SECTION: &str = "range-completion";

/// The V2 Range-terminal completion marker's versioned domain —
/// the exact string `engine-persist/reclamation-complete/3`,
/// distinct from the accepted V1 `/1` and the accepted V2 `/2`
/// (the domain gate, the key namespace, and the wire layout all
/// differ, so no cross-generation presentation ever decodes).
pub const RECLAMATION_COMPLETE_V3_DOMAIN: &str = "engine-persist/reclamation-complete/3";

/// The V2 Range-terminal completion marker's wire version.
pub const RECLAMATION_COMPLETE_V3_VERSION: u32 = 3;

/// The V2 Range-terminal marker's deterministic idempotency-key
/// domain (deterministic in the MK alone — ONE Range terminal per
/// checkpoint, ever; the /3 key domain never collides with the V1
/// /1 or V2 /2 key domains).
pub const RECLAMATION_COMPLETE_V3_KEY_DOMAIN: &str = "engine-persist/reclamation-complete-key/3";

/// The V3 Range-terminal marker's deterministic idempotency key:
/// the key domain, the MK's hex, and the `range` tag (the V1
/// range-namespace precedent, under a generation-distinct key
/// domain).
pub fn reclamation_complete_v3_range_idempotency_key(mk: &Hash) -> Vec<u8> {
    format!("{RECLAMATION_COMPLETE_V3_KEY_DOMAIN}:{}:range", mk.hex()).into_bytes()
}

/// The batch-coverage commitment's preimage domain separator — a
/// fixed 16-byte magic, distinct from every other engine domain
/// (the marker's own wire domain is a length-prefixed tag on the
/// marker's object bytes; this magic separates the COMMITMENT
/// preimage, a different layer).
pub const RANGE_COVERAGE_COMMITMENT_MAGIC: &[u8; 16] = b"S7V2RANGECOVER1!";

/// The batch-coverage commitment scheme's version — the FIRST
/// version of the coverage commitment scheme, deliberately
/// INDEPENDENT of the marker wire version (a future marker-wire
/// change must not silently redefine every existing commitment's
/// preimage).
pub const RANGE_COVERAGE_COMMITMENT_VERSION: u32 = 1;

/// The journal-obligation commitment's preimage domain separator —
/// a fixed 16-byte magic, distinct from every other engine domain
/// and from the coverage commitment's separator.
pub const RANGE_JOURNAL_OBLIGATION_COMMITMENT_MAGIC: &[u8; 16] = b"S7V2JOURNALOBL1!";

/// The journal-obligation commitment scheme's version — the FIRST
/// version of the journal commitment scheme, deliberately
/// INDEPENDENT of the marker wire version.
pub const RANGE_JOURNAL_OBLIGATION_COMMITMENT_VERSION: u32 = 1;

/// The aggregate partition bound: the accepted per-batch
/// classified-list ceiling (2^24 — the A3 materialized-list bound
/// each `/2` marker's counters obey) times the plan family's
/// batch-count ceiling (2^16) — every Mandatory or Deferred
/// aggregate partition sum over a maximally sized plan is exactly
/// this bound, so any larger declared sum is structurally
/// incoherent.
pub const MAX_RANGE_AGGREGATE_PARTITION: u64 =
    MAX_ACCOUNTING_CLASSIFIED_LIST * MAX_RECLAMATION_UNITS;

// ---------------------------------------------------------- the range context

/// The exact Range-terminal context both commitments bind to: the
/// checkpoint MK, the installed V2 plan revision identity (the
/// RECORD identity the begin binds as its source — the recorded
/// D1 obligation), and the V2 begin revision identity — the same
/// execution-authorization frame every `/2` Batch marker of the
/// range carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeCompletionContext {
    /// The retired checkpoint's MK identity.
    pub mk: Hash,
    /// The installed V2 plan revision's identity.
    pub plan: Hash,
    /// The V2 begin revision's identity.
    pub begin: Hash,
}

impl RangeCompletionContext {
    /// The pure context validation: no placeholder identity in
    /// any binding (the all-zero hash is never a valid revision
    /// identity).
    pub fn validate(&self) -> Result<(), SaveError> {
        if self.mk == Hash::ZERO || self.plan == Hash::ZERO || self.begin == Hash::ZERO {
            return Err(SaveError::Integrity("range-completion/zero-identity"));
        }
        Ok(())
    }
}

// ------------------------------------------------------- the aggregate counters

/// The aggregate classification counters — the four
/// (role × partition) cells, each the CHECKED SUM of the
/// corresponding counter over the Batch markers covered by the
/// Range terminal (never a globally deduplicated distinct-identity
/// count). Carried in the marker ALONGSIDE the digests and
/// deliberately NOT hashed into either preimage: a consumer
/// re-derives them from its own supplied batch observations and
/// compares, so a counters-only tamper refuses on the counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeAggregateCounters {
    /// The summed Mandatory revision-record count.
    pub mandatory_records: u64,
    /// The summed Deferred revision-record count.
    pub deferred_records: u64,
    /// The summed Mandatory ordinary-object count.
    pub mandatory_objects: u64,
    /// The summed Deferred ordinary-object count.
    pub deferred_objects: u64,
}

impl RangeAggregateCounters {
    /// The all-zero counter set (the accumulation's initial
    /// state — never a valid final state: the nonempty-sequence
    /// coherence refuses it).
    pub const EMPTY: RangeAggregateCounters = RangeAggregateCounters {
        mandatory_records: 0,
        deferred_records: 0,
        mandatory_objects: 0,
        deferred_objects: 0,
    };

    /// The checked per-field accumulation of one covered batch
    /// marker's counters (each field independently — a single
    /// overflow refuses the whole accumulation, never wraps).
    pub fn accumulate(&mut self, batch: &ClassificationCounters) -> Result<(), SaveError> {
        self.mandatory_records = self
            .mandatory_records
            .checked_add(batch.mandatory_records)
            .ok_or(SaveError::Overflow { section: SECTION })?;
        self.deferred_records = self
            .deferred_records
            .checked_add(batch.deferred_records)
            .ok_or(SaveError::Overflow { section: SECTION })?;
        self.mandatory_objects = self
            .mandatory_objects
            .checked_add(batch.mandatory_objects)
            .ok_or(SaveError::Overflow { section: SECTION })?;
        self.deferred_objects = self
            .deferred_objects
            .checked_add(batch.deferred_objects)
            .ok_or(SaveError::Overflow { section: SECTION })?;
        Ok(())
    }

    /// The total aggregate entry count (checked — an incoherent
    /// counter set overflows rather than wraps).
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

    /// The pure aggregate coherence for a coverage of exactly
    /// `batch_count` batches: each partition sum representable and
    /// within the 2^40 aggregate bound, and the combined total
    /// coherent with a NONEMPTY sequence of NONEMPTY batch
    /// classifications — every covered `/2` marker's
    /// classification is nonempty by that codec's own refusal of
    /// zero-classification, so the aggregate total is at least the
    /// batch count.
    pub fn validate_for(&self, batch_count: u64) -> Result<(), SaveError> {
        let mandatory = self
            .mandatory_records
            .checked_add(self.mandatory_objects)
            .ok_or(SaveError::Overflow { section: SECTION })?;
        if mandatory > MAX_RANGE_AGGREGATE_PARTITION {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "mandatory-identities",
                value: mandatory,
                max: MAX_RANGE_AGGREGATE_PARTITION,
            });
        }
        let deferred = self
            .deferred_records
            .checked_add(self.deferred_objects)
            .ok_or(SaveError::Overflow { section: SECTION })?;
        if deferred > MAX_RANGE_AGGREGATE_PARTITION {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "deferred-identities",
                value: deferred,
                max: MAX_RANGE_AGGREGATE_PARTITION,
            });
        }
        let total = mandatory
            .checked_add(deferred)
            .ok_or(SaveError::Overflow { section: SECTION })?;
        if total < batch_count {
            return Err(SaveError::Integrity(
                "range-completion/aggregate-incoherent",
            ));
        }
        Ok(())
    }
}

// ------------------------------------------------------ the coverage commitment

/// One supplied Batch observation for the pure coverage
/// computation: the plan-ordered batch index, the batch's
/// `/2` Batch completion marker RECORD identity (the on-chain
/// handle — binding it transitively binds the marker's full
/// hash-addressed content), and that marker's classification
/// counters. CALLER-SUPPLIED evidence — never authenticated
/// historical or installed-chain fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeCoverageObservation {
    /// The plan-defined batch index (consecutive from zero).
    pub batch_index: u64,
    /// The batch's `/2` marker revision identity.
    pub marker_record: Hash,
    /// The batch marker's classification counters.
    pub counters: ClassificationCounters,
}

/// The computed batch-coverage commitment: the domain-separated
/// SHA-256 digest over the canonical coverage stream, the batch
/// count it covers, and the aggregate counters summed from the
/// same observations. A matching commitment is NOT proof that the
/// covered markers are installed, canonical, or historically
/// correct — only that the caller's exact observations and context
/// produce this digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeCoverageCommitment {
    /// The coverage commitment digest (never the placeholder).
    pub digest: Hash,
    /// The covered batch count (the declared preimage count).
    pub batch_count: u64,
    /// The aggregate counters summed over the observations.
    pub aggregate: RangeAggregateCounters,
}

/// The streaming batch-coverage commitment builder: feeds the
/// domain-separated preimage one batch entry at a time, retaining
/// NO entry (pure hashing — O(1) memory in the coverage size).
/// The batch count is declared at construction (the engine's
/// up-front count-framing discipline), and the stream must push
/// EXACTLY that count with CONSECUTIVE batch indices beginning at
/// zero (the plan's own deterministic order). A skipped, repeated,
/// or out-of-order index, a placeholder identity, and a short or
/// long stream all refuse typed mid-stream, never at completion
/// only.
pub struct RangeCoverageCommitmentBuilder {
    hasher: Sha256,
    pushed: u64,
    declared: u64,
}

impl RangeCoverageCommitmentBuilder {
    /// Starts the coverage commitment for the exact Range context
    /// over the declared batch count: the context is validated,
    /// the count is bounded (and non-zero) BEFORE any hashing
    /// state is built, and the fixed-width preimage header (the
    /// magic, the scheme version, the three context bindings, the
    /// declared count) is fed.
    pub fn new(context: &RangeCompletionContext, batch_count: u64) -> Result<Self, SaveError> {
        context.validate()?;
        if batch_count == 0 {
            return Err(SaveError::Integrity("range-completion/empty-coverage"));
        }
        if batch_count > MAX_RECLAMATION_UNITS {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "batch-count",
                value: batch_count,
                max: MAX_RECLAMATION_UNITS,
            });
        }
        let mut hasher = Sha256::new();
        hasher.update(RANGE_COVERAGE_COMMITMENT_MAGIC);
        hasher.update(RANGE_COVERAGE_COMMITMENT_VERSION.to_le_bytes());
        hasher.update(context.mk.as_bytes());
        hasher.update(context.plan.as_bytes());
        hasher.update(context.begin.as_bytes());
        hasher.update(batch_count.to_le_bytes());
        Ok(RangeCoverageCommitmentBuilder {
            hasher,
            pushed: 0,
            declared: batch_count,
        })
    }

    /// Streams one batch's terminal entry in canonical plan
    /// order: the index must be EXACTLY the next consecutive
    /// position (zero-based — a repeated, skipped, or out-of-order
    /// index refuses here), and the marker record identity must
    /// not be the placeholder.
    pub fn push(&mut self, batch_index: u64, marker_record: Hash) -> Result<(), SaveError> {
        if marker_record == Hash::ZERO {
            return Err(SaveError::Integrity("range-completion/zero-identity"));
        }
        if self.pushed >= self.declared {
            return Err(SaveError::Integrity("range-completion/count-exceeded"));
        }
        if batch_index != self.pushed {
            return Err(SaveError::Integrity("range-completion/batch-index-order"));
        }
        self.hasher.update(batch_index.to_le_bytes());
        self.hasher.update(marker_record.as_bytes());
        self.pushed += 1;
        Ok(())
    }

    /// Completes the commitment: the pushed count must equal the
    /// declared count exactly (a short or long stream is counter
    /// incoherence, never a partial commitment).
    pub fn finish(self) -> Result<Hash, SaveError> {
        if self.pushed != self.declared {
            return Err(SaveError::Integrity("range-completion/count-mismatch"));
        }
        Ok(Hash::from_bytes(&self.hasher.finalize()).expect("the SHA-256 output width"))
    }
}

/// The pure coverage computation over explicitly supplied batch
/// observations: validates the context and every observation's
/// counters (each a coherent NONEMPTY `/2` classification — the
/// codec's own refusal discipline), streams the coverage entries
/// through the builder in the supplied plan order, and sums the
/// aggregate counters under checked arithmetic. The result binds
/// the caller's exact observations — caller-supplied observations
/// are NOT authenticated historical or installed-chain evidence.
pub fn commit_range_coverage(
    context: &RangeCompletionContext,
    observations: &[RangeCoverageObservation],
) -> Result<RangeCoverageCommitment, SaveError> {
    context.validate()?;
    let batch_count = u64::try_from(observations.len()).expect("usize exceeds the u64 wire width");
    if batch_count == 0 {
        return Err(SaveError::Integrity("range-completion/empty-coverage"));
    }
    if batch_count > MAX_RECLAMATION_UNITS {
        return Err(SaveError::Limit {
            section: SECTION,
            kind: "batch-count",
            value: batch_count,
            max: MAX_RECLAMATION_UNITS,
        });
    }
    let mut builder = RangeCoverageCommitmentBuilder::new(context, batch_count)?;
    let mut aggregate = RangeAggregateCounters::EMPTY;
    for observation in observations {
        observation.counters.validate()?;
        builder.push(observation.batch_index, observation.marker_record)?;
        aggregate.accumulate(&observation.counters)?;
    }
    let digest = builder.finish()?;
    aggregate.validate_for(batch_count)?;
    Ok(RangeCoverageCommitment {
        digest,
        batch_count,
        aggregate,
    })
}

// ------------------------------------------------- the journal-obligation commitment

/// The pure journal-obligation commitment: a domain-separated
/// SHA-256 over the exact obligation list bound by the installed
/// plan — the names supplied BYTE-EXACT (never reinterpreted or
/// normalized), in the sorted ascending canonical order (the
/// legitimate producer's list is sorted; reordered input refuses
/// typed, never silently reordered), each as its u64-LE byte
/// length followed by its exact UTF-8 bytes, under the up-front
/// declared count. An empty list is legitimate and produces the
/// deterministic digest over its zero-count preimage.
///
/// This is a JOURNAL-OBLIGATION commitment — the precise set of
/// obligations the plan defines — NOT proof of completed journal
/// retirement: future coordinated readers and publishers must
/// separately prove the associated physical and ownership
/// obligations.
pub fn commit_journal_obligations(
    context: &RangeCompletionContext,
    journal_names: &[String],
) -> Result<Hash, SaveError> {
    context.validate()?;
    let count = u64::try_from(journal_names.len()).expect("usize exceeds the u64 wire width");
    if count > MAX_RECLAMATION_JOURNAL_FILES {
        return Err(SaveError::Limit {
            section: SECTION,
            kind: "journal-count",
            value: count,
            max: MAX_RECLAMATION_JOURNAL_FILES,
        });
    }
    let mut hasher = Sha256::new();
    hasher.update(RANGE_JOURNAL_OBLIGATION_COMMITMENT_MAGIC);
    hasher.update(RANGE_JOURNAL_OBLIGATION_COMMITMENT_VERSION.to_le_bytes());
    hasher.update(context.mk.as_bytes());
    hasher.update(context.plan.as_bytes());
    hasher.update(context.begin.as_bytes());
    hasher.update(count.to_le_bytes());
    let mut previous: Option<&str> = None;
    for (position, name) in journal_names.iter().enumerate() {
        let bytes = name.as_bytes();
        if bytes.is_empty() {
            return Err(SaveError::Integrity("range-completion/journal-name-empty"));
        }
        let name_len = u64::try_from(bytes.len()).expect("usize exceeds the u64 wire width");
        if name_len > MAX_RECLAMATION_JOURNAL_NAME {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "journal-name-bytes",
                value: name_len,
                max: MAX_RECLAMATION_JOURNAL_NAME,
            });
        }
        if let Some(previous_name) = previous {
            if bytes == previous_name.as_bytes() {
                return Err(SaveError::Duplicate {
                    section: SECTION,
                    // The duplicate's 0-based position.
                    seq: position as u64,
                });
            }
            if bytes < previous_name.as_bytes() {
                return Err(SaveError::Order { section: SECTION });
            }
        }
        hasher.update(name_len.to_le_bytes());
        hasher.update(bytes);
        previous = Some(name.as_str());
    }
    Ok(Hash::from_bytes(&hasher.finalize()).expect("the SHA-256 output width"))
}

// ------------------------------------------------------------- the marker

/// The V2 `/3` Range-terminal completion marker: the exact Range
/// context, the batch-coverage commitment digest, the
/// journal-obligation commitment digest, the declared counts, and
/// the aggregate classification counters. STRUCTURAL EVIDENCE
/// ONLY — decoding this marker certifies its internal coherence,
/// never that the range's storage obligations were discharged,
/// never that the marker is installed on a chain, and never any
/// durability grade or destructive authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeCompletionMarkerV3 {
    /// The retired checkpoint's MK identity.
    pub mk: Hash,
    /// The installed V2 plan revision's identity.
    pub plan: Hash,
    /// The V2 begin revision's identity.
    pub begin: Hash,
    /// The batch-coverage commitment digest.
    pub coverage: Hash,
    /// The journal-obligation commitment digest.
    pub journal: Hash,
    /// The declared plan batch count (the coverage's entry count).
    pub batch_count: u64,
    /// The declared journal-obligation count.
    pub journal_count: u64,
    /// The aggregate classification counters.
    pub counters: RangeAggregateCounters,
}

impl RangeCompletionMarkerV3 {
    /// The marker's own Range context (both commitments' binding
    /// frame — a consumer re-derives them under exactly these
    /// three bindings).
    pub fn context(&self) -> RangeCompletionContext {
        RangeCompletionContext {
            mk: self.mk,
            plan: self.plan,
            begin: self.begin,
        }
    }

    /// Canonical bytes (the marker revision's single content
    /// object): the length-prefixed `/3` domain tag, the wire
    /// version, the three context bindings, the two commitment
    /// digests, the two declared counts, and the four aggregate
    /// counters — all fixed-width, no variable-length sections,
    /// exactly 297 encoded bytes.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_str(RECLAMATION_COMPLETE_V3_DOMAIN);
        enc.write_u32(RECLAMATION_COMPLETE_V3_VERSION);
        enc.write_bytes(self.mk.as_bytes());
        enc.write_bytes(self.plan.as_bytes());
        enc.write_bytes(self.begin.as_bytes());
        enc.write_bytes(self.coverage.as_bytes());
        enc.write_bytes(self.journal.as_bytes());
        enc.write_u64(self.batch_count);
        enc.write_u64(self.journal_count);
        enc.write_u64(self.counters.mandatory_records);
        enc.write_u64(self.counters.deferred_records);
        enc.write_u64(self.counters.mandatory_objects);
        enc.write_u64(self.counters.deferred_objects);
        enc.into_bytes()
    }

    /// Strict decode: the domain gate (a V1 or V2 marker refuses
    /// here — the exact `/3` string never matches `/1` or `/2`),
    /// the version gate, the placeholder-identity refusals on
    /// every hash binding and digest, the batch-count bounds
    /// (non-zero, within the plan family's ceiling), the
    /// journal-count ceiling (zero is legitimate — the empty
    /// obligation list), the aggregate-counter bounds and
    /// nonempty-sequence coherence, and the trailing-data
    /// refusal. Structural validity only.
    pub fn decode(bytes: &[u8]) -> Result<Self, SaveError> {
        let mut dec = Decoder::new(bytes);
        dec.read_domain(RECLAMATION_COMPLETE_V3_DOMAIN)?;
        let version = dec.read_u32()?;
        if version != RECLAMATION_COMPLETE_V3_VERSION {
            return Err(SaveError::Version {
                observed: version,
                supported: RECLAMATION_COMPLETE_V3_VERSION,
            });
        }
        let read_binding = |dec: &mut Decoder| -> Result<Hash, SaveError> {
            let bytes = dec.read_bytes(Hash::WIDTH, SECTION)?;
            Hash::from_bytes(bytes).ok_or(SaveError::Integrity("range-completion/hash-width"))
        };
        let mk = read_binding(&mut dec)?;
        let plan = read_binding(&mut dec)?;
        let begin = read_binding(&mut dec)?;
        let coverage = read_binding(&mut dec)?;
        let journal = read_binding(&mut dec)?;
        let context = RangeCompletionContext { mk, plan, begin };
        context.validate()?;
        if coverage == Hash::ZERO || journal == Hash::ZERO {
            return Err(SaveError::Integrity("range-completion/zero-identity"));
        }
        let batch_count = dec.read_u64()?;
        if batch_count == 0 {
            return Err(SaveError::Integrity("range-completion/empty-coverage"));
        }
        if batch_count > MAX_RECLAMATION_UNITS {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "batch-count",
                value: batch_count,
                max: MAX_RECLAMATION_UNITS,
            });
        }
        let journal_count = dec.read_u64()?;
        if journal_count > MAX_RECLAMATION_JOURNAL_FILES {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "journal-count",
                value: journal_count,
                max: MAX_RECLAMATION_JOURNAL_FILES,
            });
        }
        let counters = RangeAggregateCounters {
            mandatory_records: dec.read_u64()?,
            deferred_records: dec.read_u64()?,
            mandatory_objects: dec.read_u64()?,
            deferred_objects: dec.read_u64()?,
        };
        counters.validate_for(batch_count)?;
        dec.finish()?;
        Ok(RangeCompletionMarkerV3 {
            mk: context.mk,
            plan: context.plan,
            begin: context.begin,
            coverage,
            journal,
            batch_count,
            journal_count,
            counters,
        })
    }

    /// The pure re-derivation check over explicitly supplied batch
    /// observations (the future authenticated reader's core step,
    /// delivered codec-side): re-computes the coverage commitment
    /// and the aggregate counters under this marker's own context
    /// and compares BOTH the digest and the counters (a
    /// counters-only tamper refuses on the counters). A match
    /// certifies only that the supplied observations and context
    /// produce this marker's bindings — never that the supplied
    /// coverage is the installed, historically correct one.
    pub fn verify_coverage(
        &self,
        observations: &[RangeCoverageObservation],
    ) -> Result<(), SaveError> {
        let derived = commit_range_coverage(&self.context(), observations)?;
        if derived.digest != self.coverage {
            return Err(SaveError::Integrity("range-completion/coverage-mismatch"));
        }
        if derived.batch_count != self.batch_count {
            return Err(SaveError::Integrity(
                "range-completion/batch-count-mismatch",
            ));
        }
        if derived.aggregate != self.counters {
            return Err(SaveError::Integrity("range-completion/counter-mismatch"));
        }
        Ok(())
    }

    /// The pure re-derivation check over the supplied
    /// journal-obligation names: re-computes the
    /// journal-obligation commitment under this marker's own
    /// context and compares the digest and the declared count. A
    /// match certifies only that the supplied names and context
    /// produce this marker's journal binding — never that the
    /// journals were physically retired.
    pub fn verify_journal_obligations(&self, journal_names: &[String]) -> Result<(), SaveError> {
        let derived = commit_journal_obligations(&self.context(), journal_names)?;
        if derived != self.journal {
            return Err(SaveError::Integrity("range-completion/journal-mismatch"));
        }
        let count = u64::try_from(journal_names.len()).expect("usize exceeds the u64 wire width");
        if count != self.journal_count {
            return Err(SaveError::Integrity(
                "range-completion/journal-count-mismatch",
            ));
        }
        Ok(())
    }
}

// ============================================================== the C3 tests
//
// The owner's required adversarial matrix — pure codec tests (no
// filesystem, no providers, no chain state, no publication, no
// destructive execution of any kind), plus the two integration
// refusals the GO names: the genuine /3 marker through the real
// generic append, and the accepted C2 structural reader over a
// /3 marker object.

#[cfg(test)]
mod c3_tests {
    use super::*;
    use crate::ancestry_accounting::StorageRole;
    use crate::batch_completion::{
        BatchCompletionContext, BatchCompletionMarkerV2, ClassificationEntry, EntryClassification,
        commit_classification, read_structural_batch_candidate,
        reclamation_complete_v2_idempotency_key,
    };
    use crate::hash::hash_object;

    // The full canonical layout chain (the C1 lesson: hash fields
    // sit behind their own 8-byte length prefixes — every mutation
    // offset is computed from the complete framing):
    //   [0..8)      the domain tag's u64-LE length prefix
    //   [8..45)     the 37-byte /3 domain string
    //   [45..49)    the u32-LE wire version
    //   [49..57)    mk length prefix;  [57..89)   mk bytes
    //   [89..97)    plan length prefix; [97..129) plan bytes
    //   [129..137)  begin length prefix; [137..169) begin bytes
    //   [169..177)  coverage length prefix; [177..209) coverage bytes
    //   [209..217)  journal length prefix; [217..249) journal bytes
    //   [249..257)  batch count;  [257..265) journal count
    //   [265..297)  the four aggregate counters
    const VERSION_AT: usize = 8 + RECLAMATION_COMPLETE_V3_DOMAIN.len();
    const MK_AT: usize = VERSION_AT + 4 + 8;
    const PLAN_AT: usize = MK_AT + 8 + 32;
    const BEGIN_AT: usize = PLAN_AT + 8 + 32;
    const COVERAGE_AT: usize = BEGIN_AT + 8 + 32;
    const JOURNAL_AT: usize = COVERAGE_AT + 8 + 32;
    const BATCH_COUNT_AT: usize = JOURNAL_AT + 32;
    const JOURNAL_COUNT_AT: usize = BATCH_COUNT_AT + 8;
    const COUNTERS_AT: usize = JOURNAL_COUNT_AT + 8;
    const MARKER_BYTES: usize = COUNTERS_AT + 4 * 8;

    fn fixture_context() -> RangeCompletionContext {
        RangeCompletionContext {
            mk: hash_object(b"c3-fixture-mk"),
            plan: hash_object(b"c3-fixture-plan"),
            begin: hash_object(b"c3-fixture-begin"),
        }
    }

    fn marker_records() -> Vec<Hash> {
        (0..3)
            .map(|index| hash_object(format!("c3-fixture-marker-{index}").as_bytes()))
            .collect()
    }

    fn batch_counters(
        mandatory_records: u64,
        deferred_records: u64,
        mandatory_objects: u64,
        deferred_objects: u64,
    ) -> ClassificationCounters {
        ClassificationCounters {
            mandatory_records,
            deferred_records,
            mandatory_objects,
            deferred_objects,
        }
    }

    fn observation(
        batch_index: u64,
        marker_record: Hash,
        counters: ClassificationCounters,
    ) -> RangeCoverageObservation {
        RangeCoverageObservation {
            batch_index,
            marker_record,
            counters,
        }
    }

    /// Three consecutive batches with distinct marker identities
    /// and coherent nonempty classifications.
    fn fixture_observations() -> Vec<RangeCoverageObservation> {
        let records = marker_records();
        vec![
            observation(0, records[0], batch_counters(2, 0, 1, 1)),
            observation(1, records[1], batch_counters(1, 1, 0, 0)),
            observation(2, records[2], batch_counters(0, 0, 3, 0)),
        ]
    }

    fn fixture_journal_names() -> Vec<String> {
        vec!["journal.t1.0".to_owned(), "journal.t1.2".to_owned()]
    }

    fn committed_marker() -> RangeCompletionMarkerV3 {
        let context = fixture_context();
        let coverage = commit_range_coverage(&context, &fixture_observations()).unwrap();
        let journal = commit_journal_obligations(&context, &fixture_journal_names()).unwrap();
        RangeCompletionMarkerV3 {
            mk: context.mk,
            plan: context.plan,
            begin: context.begin,
            coverage: coverage.digest,
            journal,
            batch_count: coverage.batch_count,
            journal_count: fixture_journal_names().len() as u64,
            counters: coverage.aggregate,
        }
    }

    /// The independently hand-assembled coverage preimage (the
    /// known-answer construction: the bytes follow the documented
    /// layout, assembled without the codec under test).
    fn hand_coverage_preimage(
        context: &RangeCompletionContext,
        entries: &[(u64, Hash)],
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(RANGE_COVERAGE_COMMITMENT_MAGIC);
        bytes.extend_from_slice(&RANGE_COVERAGE_COMMITMENT_VERSION.to_le_bytes());
        bytes.extend_from_slice(context.mk.as_bytes());
        bytes.extend_from_slice(context.plan.as_bytes());
        bytes.extend_from_slice(context.begin.as_bytes());
        bytes.extend_from_slice(&(entries.len() as u64).to_le_bytes());
        for (index, identity) in entries {
            bytes.extend_from_slice(&index.to_le_bytes());
            bytes.extend_from_slice(identity.as_bytes());
        }
        bytes
    }

    /// The independently hand-assembled journal-obligation
    /// preimage.
    fn hand_journal_preimage(context: &RangeCompletionContext, names: &[&str]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(RANGE_JOURNAL_OBLIGATION_COMMITMENT_MAGIC);
        bytes.extend_from_slice(&RANGE_JOURNAL_OBLIGATION_COMMITMENT_VERSION.to_le_bytes());
        bytes.extend_from_slice(context.mk.as_bytes());
        bytes.extend_from_slice(context.plan.as_bytes());
        bytes.extend_from_slice(context.begin.as_bytes());
        bytes.extend_from_slice(&(names.len() as u64).to_le_bytes());
        for name in names {
            let name_bytes = name.as_bytes();
            bytes.extend_from_slice(&(name_bytes.len() as u64).to_le_bytes());
            bytes.extend_from_slice(name_bytes);
        }
        bytes
    }

    #[test]
    fn the_marker_round_trips_and_reencodes_deterministically() {
        let marker = committed_marker();
        let bytes = marker.canonical_bytes();
        assert_eq!(bytes.len(), MARKER_BYTES);
        assert_eq!(MARKER_BYTES, 297);
        let restored = RangeCompletionMarkerV3::decode(&bytes).unwrap();
        assert_eq!(restored, marker);
        assert_eq!(restored.canonical_bytes(), bytes);
        // A fresh construction over the same inputs is
        // byte-identical (deterministic encoding).
        assert_eq!(committed_marker().canonical_bytes(), bytes);
    }

    #[test]
    fn the_commitments_match_the_specified_preimages() {
        // The coverage known-answer vector: the codec's digest
        // equals the SHA-256 of the independently assembled
        // documented preimage.
        let context = fixture_context();
        let observations = fixture_observations();
        let entries: Vec<(u64, Hash)> = observations
            .iter()
            .map(|obs| (obs.batch_index, obs.marker_record))
            .collect();
        let coverage = commit_range_coverage(&context, &observations).unwrap();
        assert_eq!(
            coverage.digest,
            hash_object(&hand_coverage_preimage(&context, &entries))
        );
        // Determinism: a fresh computation yields the identical
        // digest.
        assert_eq!(
            commit_range_coverage(&context, &observations).unwrap(),
            coverage
        );

        // The journal known-answer vector, including the empty
        // list's zero-count preimage.
        let names = fixture_journal_names();
        let as_strs: Vec<&str> = names.iter().map(String::as_str).collect();
        let journal = commit_journal_obligations(&context, &names).unwrap();
        assert_eq!(
            journal,
            hash_object(&hand_journal_preimage(&context, &as_strs))
        );
        assert_eq!(
            journal,
            commit_journal_obligations(&context, &names).unwrap()
        );
        let empty = commit_journal_obligations(&context, &[]).unwrap();
        assert_eq!(empty, hash_object(&hand_journal_preimage(&context, &[])));
        assert_ne!(empty, Hash::ZERO);
        assert_ne!(empty, journal);
    }

    #[test]
    fn each_context_binding_affects_both_commitments() {
        let context = fixture_context();
        let observations = fixture_observations();
        let names = fixture_journal_names();
        let coverage = commit_range_coverage(&context, &observations).unwrap();
        let journal = commit_journal_obligations(&context, &names).unwrap();
        for mutated in [
            RangeCompletionContext {
                mk: hash_object(b"c3-fixture-mk-2"),
                plan: context.plan,
                begin: context.begin,
            },
            RangeCompletionContext {
                mk: context.mk,
                plan: hash_object(b"c3-fixture-plan-2"),
                begin: context.begin,
            },
            RangeCompletionContext {
                mk: context.mk,
                plan: context.plan,
                begin: hash_object(b"c3-fixture-begin-2"),
            },
        ] {
            let mutated_coverage = commit_range_coverage(&mutated, &observations).unwrap();
            let mutated_journal = commit_journal_obligations(&mutated, &names).unwrap();
            assert_ne!(mutated_coverage.digest, coverage.digest);
            assert_ne!(mutated_journal, journal);
        }
    }

    #[test]
    fn the_same_count_different_marker_identities_differ() {
        let context = fixture_context();
        let records = marker_records();
        let observations = fixture_observations();
        // The repartition: batches 0 and 1 swap their marker
        // identities under IDENTICAL counters and consecutive
        // indices — a different coverage commitment, the same
        // aggregate.
        let swapped = vec![
            observation(0, records[1], batch_counters(2, 0, 1, 1)),
            observation(1, records[0], batch_counters(1, 1, 0, 0)),
            observation(2, records[2], batch_counters(0, 0, 3, 0)),
        ];
        let original = commit_range_coverage(&context, &observations).unwrap();
        let repartition = commit_range_coverage(&context, &swapped).unwrap();
        assert_ne!(repartition.digest, original.digest);
        assert_eq!(repartition.aggregate, original.aggregate);
        assert_eq!(repartition.batch_count, original.batch_count);
        // And the marker-side verification refuses the
        // repartition on the coverage digest.
        let marker = committed_marker();
        assert!(matches!(
            marker.verify_coverage(&swapped),
            Err(SaveError::Integrity("range-completion/coverage-mismatch"))
        ));
    }

    #[test]
    fn the_coverage_builder_refuses_index_and_count_violations() {
        let context = fixture_context();
        let records = marker_records();
        // A zero declaration refuses (a real retirement is never
        // empty).
        assert!(matches!(
            RangeCoverageCommitmentBuilder::new(&context, 0),
            Err(SaveError::Integrity("range-completion/empty-coverage"))
        ));
        // An over-ceiling declaration refuses at the count (the
        // bounded read, before any hashing state is built).
        assert!(matches!(
            RangeCoverageCommitmentBuilder::new(&context, MAX_RECLAMATION_UNITS + 1),
            Err(SaveError::Limit {
                kind: "batch-count",
                ..
            })
        ));
        // The index discipline: the stream must begin at zero...
        {
            let mut builder = RangeCoverageCommitmentBuilder::new(&context, 1).unwrap();
            assert!(matches!(
                builder.push(1, records[0]),
                Err(SaveError::Integrity("range-completion/batch-index-order"))
            ));
        }
        // ...continue consecutively (a repeated index refuses)...
        {
            let mut builder = RangeCoverageCommitmentBuilder::new(&context, 2).unwrap();
            builder.push(0, records[0]).unwrap();
            assert!(matches!(
                builder.push(0, records[1]),
                Err(SaveError::Integrity("range-completion/batch-index-order"))
            ));
        }
        // ...with no omission (a skipped index refuses)...
        {
            let mut builder = RangeCoverageCommitmentBuilder::new(&context, 2).unwrap();
            builder.push(0, records[0]).unwrap();
            assert!(matches!(
                builder.push(2, records[2]),
                Err(SaveError::Integrity("range-completion/batch-index-order"))
            ));
        }
        // ...never past the declared count...
        {
            let mut builder = RangeCoverageCommitmentBuilder::new(&context, 1).unwrap();
            builder.push(0, records[0]).unwrap();
            assert!(matches!(
                builder.push(1, records[1]),
                Err(SaveError::Integrity("range-completion/count-exceeded"))
            ));
        }
        // ...and never with the placeholder identity.
        {
            let mut builder = RangeCoverageCommitmentBuilder::new(&context, 1).unwrap();
            assert!(matches!(
                builder.push(0, Hash::ZERO),
                Err(SaveError::Integrity("range-completion/zero-identity"))
            ));
        }
        // A short stream refuses at completion (never a partial
        // commitment).
        {
            let mut builder = RangeCoverageCommitmentBuilder::new(&context, 3).unwrap();
            builder.push(0, records[0]).unwrap();
            builder.push(1, records[1]).unwrap();
            assert!(matches!(
                builder.finish(),
                Err(SaveError::Integrity("range-completion/count-mismatch"))
            ));
        }
        // The wrapper enforces the same discipline over supplied
        // observations with wrong indices.
        let misindexed = vec![
            observation(1, records[1], batch_counters(2, 0, 1, 1)),
            observation(0, records[0], batch_counters(1, 1, 0, 0)),
        ];
        assert!(matches!(
            commit_range_coverage(&context, &misindexed),
            Err(SaveError::Integrity("range-completion/batch-index-order"))
        ));
        // The empty observation set refuses.
        assert!(matches!(
            commit_range_coverage(&context, &[]),
            Err(SaveError::Integrity("range-completion/empty-coverage"))
        ));
    }

    #[test]
    fn the_journal_commitment_validates_names_and_order() {
        let context = fixture_context();
        // A name mutation changes the digest (the binding).
        let names = fixture_journal_names();
        let journal = commit_journal_obligations(&context, &names).unwrap();
        let mutated = vec!["journal.t1.0".to_owned(), "journal.t1.3".to_owned()];
        assert_ne!(
            commit_journal_obligations(&context, &mutated).unwrap(),
            journal
        );
        // Reordered input refuses (the canonical order is sorted
        // ascending — never silently reordered).
        let reordered = vec!["journal.t1.2".to_owned(), "journal.t1.0".to_owned()];
        assert!(matches!(
            commit_journal_obligations(&context, &reordered),
            Err(SaveError::Order { .. })
        ));
        // An adjacent duplicate refuses as the duplicate...
        let adjacent = vec!["journal.t1.0".to_owned(), "journal.t1.0".to_owned()];
        assert!(matches!(
            commit_journal_obligations(&context, &adjacent),
            Err(SaveError::Duplicate { .. })
        ));
        // ...and a non-adjacent repeat presents as the order
        // refusal (the ascending-order gate).
        let non_adjacent = vec![
            "journal.t1.0".to_owned(),
            "journal.t1.2".to_owned(),
            "journal.t1.0".to_owned(),
        ];
        assert!(matches!(
            commit_journal_obligations(&context, &non_adjacent),
            Err(SaveError::Order { .. })
        ));
        // An empty name refuses.
        let empty_name = vec![String::new()];
        assert!(matches!(
            commit_journal_obligations(&context, &empty_name),
            Err(SaveError::Integrity("range-completion/journal-name-empty"))
        ));
        // The name-byte boundary: exactly the plan family's
        // ceiling commits, one past refuses at the count.
        let at_bound = vec!["x".repeat(1024)];
        assert!(commit_journal_obligations(&context, &at_bound).is_ok());
        let over_bound = vec!["x".repeat(1025)];
        assert!(matches!(
            commit_journal_obligations(&context, &over_bound),
            Err(SaveError::Limit {
                kind: "journal-name-bytes",
                ..
            })
        ));
        // The declared-count ceiling: one past the plan family's
        // journal-file bound refuses at the count, before any
        // name is consumed.
        let over_count: Vec<String> = (0..=MAX_RECLAMATION_JOURNAL_FILES)
            .map(|index| format!("n{index}"))
            .collect();
        assert!(matches!(
            commit_journal_obligations(&context, &over_count),
            Err(SaveError::Limit {
                kind: "journal-count",
                ..
            })
        ));
    }

    #[test]
    fn the_aggregate_counters_enforce_bounds_and_coherence() {
        // A partition sum above the 2^40 aggregate bound refuses
        // with the exact category and bound.
        let over = RangeAggregateCounters {
            mandatory_records: MAX_RANGE_AGGREGATE_PARTITION + 1,
            ..RangeAggregateCounters::EMPTY
        };
        assert!(matches!(
            over.validate_for(1),
            Err(SaveError::Limit {
                kind: "mandatory-identities",
                ..
            })
        ));
        let over_deferred = RangeAggregateCounters {
            deferred_objects: MAX_RANGE_AGGREGATE_PARTITION + 1,
            ..RangeAggregateCounters::EMPTY
        };
        assert!(matches!(
            over_deferred.validate_for(1),
            Err(SaveError::Limit {
                kind: "deferred-identities",
                ..
            })
        ));
        // A representability overflow refuses (never wraps).
        let overflowing = RangeAggregateCounters {
            mandatory_records: u64::MAX,
            mandatory_objects: 1,
            ..RangeAggregateCounters::EMPTY
        };
        assert!(matches!(
            overflowing.validate_for(1),
            Err(SaveError::Overflow { .. })
        ));
        // The nonempty-sequence coherence: an aggregate total
        // below the batch count refuses (every covered batch's
        // classification is nonempty by the /2 codec's own
        // refusal).
        let incoherent = RangeAggregateCounters {
            mandatory_records: 1,
            ..RangeAggregateCounters::EMPTY
        };
        assert!(matches!(
            incoherent.validate_for(3),
            Err(SaveError::Integrity(
                "range-completion/aggregate-incoherent"
            ))
        ));
        assert!(incoherent.validate_for(1).is_ok());
        // The checked accumulation refuses a single overflow
        // rather than wrapping.
        let mut aggregate = RangeAggregateCounters {
            mandatory_records: u64::MAX,
            ..RangeAggregateCounters::EMPTY
        };
        assert!(matches!(
            aggregate.accumulate(&batch_counters(1, 0, 0, 0)),
            Err(SaveError::Overflow { .. })
        ));
    }

    #[test]
    fn the_marker_decoder_refuses_malformed_forms() {
        let bytes = committed_marker().canonical_bytes();
        // Truncation at every byte boundary.
        for cut in 0..bytes.len() {
            assert!(
                RangeCompletionMarkerV3::decode(&bytes[..cut]).is_err(),
                "truncation at {cut} must refuse"
            );
        }
        // Trailing data.
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            RangeCompletionMarkerV3::decode(&trailing),
            Err(SaveError::Trailing { .. })
        ));
        // A corrupted domain (a valid-ASCII replacement of the
        // same width — the corrupted byte lands inside the domain
        // string, never inside its UTF-8 framing).
        let mut bad_domain = bytes.clone();
        bad_domain[VERSION_AT - 1] = b'9';
        assert!(matches!(
            RangeCompletionMarkerV3::decode(&bad_domain),
            Err(SaveError::Domain { .. })
        ));
        // The wrong version.
        let mut bad_version = bytes.clone();
        bad_version[VERSION_AT..VERSION_AT + 4].copy_from_slice(&4u32.to_le_bytes());
        assert!(matches!(
            RangeCompletionMarkerV3::decode(&bad_version),
            Err(SaveError::Version { .. })
        ));
        // The zeroed context bindings.
        let mut zero_mk = bytes.clone();
        zero_mk[MK_AT..MK_AT + 32].fill(0);
        assert!(matches!(
            RangeCompletionMarkerV3::decode(&zero_mk),
            Err(SaveError::Integrity("range-completion/zero-identity"))
        ));
        // The placeholder digests.
        let mut zero_coverage = bytes.clone();
        zero_coverage[COVERAGE_AT..COVERAGE_AT + 32].fill(0);
        assert!(matches!(
            RangeCompletionMarkerV3::decode(&zero_coverage),
            Err(SaveError::Integrity("range-completion/zero-identity"))
        ));
        // The zero batch count.
        let mut zero_batches = bytes.clone();
        zero_batches[BATCH_COUNT_AT..BATCH_COUNT_AT + 8].fill(0);
        assert!(matches!(
            RangeCompletionMarkerV3::decode(&zero_batches),
            Err(SaveError::Integrity("range-completion/empty-coverage"))
        ));
        // The over-ceiling batch count.
        let mut over_batches = bytes.clone();
        over_batches[BATCH_COUNT_AT..BATCH_COUNT_AT + 8]
            .copy_from_slice(&(MAX_RECLAMATION_UNITS + 1).to_le_bytes());
        assert!(matches!(
            RangeCompletionMarkerV3::decode(&over_batches),
            Err(SaveError::Limit {
                kind: "batch-count",
                ..
            })
        ));
        // The over-ceiling journal count.
        let mut over_journals = bytes.clone();
        over_journals[JOURNAL_COUNT_AT..JOURNAL_COUNT_AT + 8]
            .copy_from_slice(&(MAX_RECLAMATION_JOURNAL_FILES + 1).to_le_bytes());
        assert!(matches!(
            RangeCompletionMarkerV3::decode(&over_journals),
            Err(SaveError::Limit {
                kind: "journal-count",
                ..
            })
        ));
        // The incoherent aggregate: the total below the batch
        // count (all-zero counters).
        let mut incoherent = bytes.clone();
        incoherent[COUNTERS_AT..MARKER_BYTES].fill(0);
        assert!(matches!(
            RangeCompletionMarkerV3::decode(&incoherent),
            Err(SaveError::Integrity(
                "range-completion/aggregate-incoherent"
            ))
        ));
        // The overflowing aggregate partition (mandatory records +
        // mandatory objects overflows the u64 width).
        let mut overflowing = bytes.clone();
        overflowing[COUNTERS_AT..COUNTERS_AT + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        overflowing[COUNTERS_AT + 16..COUNTERS_AT + 24].copy_from_slice(&1u64.to_le_bytes());
        assert!(matches!(
            RangeCompletionMarkerV3::decode(&overflowing),
            Err(SaveError::Overflow { .. })
        ));
    }

    #[test]
    fn the_v1_v2_v3_wires_never_cross_decode() {
        use crate::reclaim::{CompletionKind, ReclamationComplete};
        // The genuine V1 Batch marker (a real encoding by the
        // accepted V1 codec, never a fabricated domain string).
        let v1_batch = ReclamationComplete {
            kind: CompletionKind::Batch,
            mk: hash_object(b"c3-fixture-mk"),
            batch: 0,
            source: hash_object(b"c3-fixture-ledger"),
            mandatory_count: 1,
            deferred: vec![hash_object(b"c3-fixture-deferred")],
            journal_files: Vec::new(),
        }
        .canonical_bytes();
        // The genuine V1 Range marker.
        let v1_range = ReclamationComplete {
            kind: CompletionKind::Range,
            mk: hash_object(b"c3-fixture-mk"),
            batch: 0,
            source: hash_object(b"c3-fixture-plan"),
            mandatory_count: 0,
            deferred: Vec::new(),
            journal_files: vec!["journal.t1.2".to_owned()],
        }
        .canonical_bytes();
        // The genuine V2 Batch marker (a real committed
        // classification fixture).
        let v2_context = BatchCompletionContext {
            mk: hash_object(b"c3-fixture-mk"),
            plan: hash_object(b"c3-fixture-plan"),
            batch_index: 0,
            audit: hash_object(b"c3-fixture-audit"),
            begin: hash_object(b"c3-fixture-begin"),
        };
        let v2_commitment = commit_classification(
            &v2_context,
            vec![
                ClassificationEntry {
                    identity: hash_object(b"c3-fixture-sk0"),
                    role: StorageRole::Record,
                    classification: EntryClassification::Mandatory,
                    conditional_on_prior_batch: false,
                },
                ClassificationEntry {
                    identity: hash_object(b"c3-fixture-o0a"),
                    role: StorageRole::Object,
                    classification: EntryClassification::Deferred,
                    conditional_on_prior_batch: false,
                },
            ],
        )
        .unwrap();
        let v2_batch = BatchCompletionMarkerV2 {
            mk: v2_context.mk,
            plan: v2_context.plan,
            batch_index: v2_context.batch_index,
            audit: v2_context.audit,
            begin: v2_context.begin,
            commitment: v2_commitment.digest,
            counters: v2_commitment.counters,
        }
        .canonical_bytes();
        // Every V1/V2 presentation refuses at the /3 domain gate.
        for earlier in [&v1_batch, &v1_range, &v2_batch] {
            assert!(RangeCompletionMarkerV3::decode(earlier).is_err());
        }
        // And the /3 marker refuses at both earlier codecs' own
        // domain gates.
        let v3 = committed_marker().canonical_bytes();
        assert!(ReclamationComplete::decode(&v3).is_err());
        assert!(BatchCompletionMarkerV2::decode(&v3).is_err());
        // Each generation's own fixture still round-trips
        // byte-identically (the accepted codecs are untouched).
        assert_eq!(
            ReclamationComplete::decode(&v1_batch)
                .unwrap()
                .canonical_bytes(),
            v1_batch
        );
        assert_eq!(
            ReclamationComplete::decode(&v1_range)
                .unwrap()
                .canonical_bytes(),
            v1_range
        );
        assert_eq!(
            BatchCompletionMarkerV2::decode(&v2_batch)
                .unwrap()
                .canonical_bytes(),
            v2_batch
        );
    }

    #[test]
    fn the_deterministic_keys_are_distinct() {
        let mk = hash_object(b"c3-key-mk");
        let v3_range = reclamation_complete_v3_range_idempotency_key(&mk);
        // Deterministic.
        assert_eq!(v3_range, reclamation_complete_v3_range_idempotency_key(&mk));
        // Distinct across every accepted namespace: the V1 batch
        // key, the V1 range key, and the V2 batch key.
        let v1_batch = crate::reclaim::reclamation_complete_idempotency_key(&mk, Some(0));
        let v1_range = crate::reclaim::reclamation_complete_idempotency_key(&mk, None);
        let v2_batch = reclamation_complete_v2_idempotency_key(&mk, 0);
        assert_ne!(v3_range, v1_batch);
        assert_ne!(v3_range, v1_range);
        assert_ne!(v3_range, v2_batch);
        assert_ne!(v1_batch, v1_range);
        assert_ne!(v1_batch, v2_batch);
        assert_ne!(v1_range, v2_batch);
        // A different MK yields a different /3 key.
        assert_ne!(
            v3_range,
            reclamation_complete_v3_range_idempotency_key(&hash_object(b"c3-key-mk-2"))
        );
    }

    #[test]
    fn the_generic_append_refuses_the_genuine_v3_marker() {
        use crate::local::LocalFileMedium;
        use crate::medium::{AppendOutcome, CanonicalMedium, MediumError, StoredObject};
        use crate::record::{AuthorityMode, RevisionRecord};

        let root = std::env::temp_dir().join(format!(
            "s7-c3-gate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut medium = LocalFileMedium::open(&root).unwrap();
        // A founded store with one plain unrelated object.
        let plain = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "c3gate", 0)
            .with_content_refs(vec![plain.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&plain), None)
            .unwrap();
        let head = founding.id();

        // The family classifier recognizes the /3 domain (the
        // pure boundary — no gate change was needed for it).
        assert!(crate::batch_completion::is_reclamation_complete_family(
            RECLAMATION_COMPLETE_V3_DOMAIN
        ));

        // The GENUINE /3 marker — valid by this codec's own round
        // trip, never a fabricated domain string — refuses through
        // the real generic append with the preserved typed rule.
        let marker = committed_marker();
        let marker_object = StoredObject::new(marker.canonical_bytes()).unwrap();
        let key = reclamation_complete_v3_range_idempotency_key(&marker.mk);
        let record = RevisionRecord::regular(head, 0, 1)
            .with_idempotency_key(key)
            .with_content_refs(vec![marker_object.hash()]);
        let error = medium
            .append(&record, std::slice::from_ref(&marker_object), Some(head))
            .unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "reclamation-complete/generic-append-refused"
                }
            ),
            "the completion gate remains enforced: {error:?}"
        );

        // An unrelated content domain still appends Accepted after
        // the refusal (the CAS form, the current head expected).
        let plain2 = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"payload-2");
            enc.into_bytes()
        })
        .unwrap();
        let record2 = RevisionRecord::regular(head, 0, 2).with_content_refs(vec![plain2.hash()]);
        let outcome = medium
            .append(&record2, std::slice::from_ref(&plain2), Some(head))
            .unwrap();
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));

        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_c2_structural_reader_refuses_the_v3_marker() {
        use crate::medium::MediumError;
        use crate::record::RevisionRecord;

        struct SyntheticReads {
            records: std::collections::BTreeMap<Hash, Vec<u8>>,
            objects: std::collections::BTreeMap<Hash, Vec<u8>>,
        }
        impl crate::reclaim::BeginReads for SyntheticReads {
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

        // A record keyed EXACTLY as a V2 batch-0 candidate would
        // be, whose referenced content object is the GENUINE /3
        // Range-terminal marker — the reader must refuse at its
        // own /2 domain gate, never reinterpret the Range marker
        // as a Batch candidate.
        let marker = committed_marker();
        let object_bytes = marker.canonical_bytes();
        let object_hash = hash_object(&object_bytes);
        let key = reclamation_complete_v2_idempotency_key(&marker.mk, 0);
        let record = RevisionRecord::regular(hash_object(b"c3-fixture-parent"), 0, 0)
            .with_idempotency_key(key)
            .with_content_refs(vec![object_hash]);
        let record_id = record.id();
        let mut reads = SyntheticReads {
            records: [(record_id, record.canonical_bytes())]
                .into_iter()
                .collect(),
            objects: [(object_hash, object_bytes)].into_iter().collect(),
        };
        let expected = BatchCompletionContext {
            mk: marker.mk,
            plan: marker.plan,
            batch_index: 0,
            audit: hash_object(b"c3-fixture-audit"),
            begin: marker.begin,
        };
        let result = read_structural_batch_candidate(&mut reads, &record_id, &expected);
        assert!(
            matches!(result, Err(MediumError::Format(_))),
            "the C2 reader must refuse the /3 marker: {result:?}"
        );
    }

    #[test]
    fn the_off_chain_marker_carries_no_authority() {
        // A fabricated, fully self-consistent Range marker —
        // constructed from pure fabricated observations, never
        // touching any medium, chain, provider, or publication —
        // decodes clean and passes the pure structural
        // verification. The destructure names EVERY field the
        // type holds: three context bindings, two commitment
        // digests, two declared counts, and the aggregate
        // counters — no installation, durability, or permission
        // field of any kind exists, and no boolean or verdict
        // field exists.
        let marker = committed_marker();
        // The pure verification passes over the same fabricated
        // observations — structural coherence with supplied
        // evidence, and NOTHING else.
        marker.verify_coverage(&fixture_observations()).unwrap();
        marker
            .verify_journal_obligations(&fixture_journal_names())
            .unwrap();
        let RangeCompletionMarkerV3 {
            mk,
            plan,
            begin,
            coverage: _,
            journal: _,
            batch_count,
            journal_count: _,
            counters: _,
        } = marker.clone();
        assert_eq!(mk, fixture_context().mk);
        assert_eq!(plan, fixture_context().plan);
        assert_eq!(begin, fixture_context().begin);
        assert_eq!(batch_count, 3);
        // The identical inputs under a fresh construction yield
        // the identical bytes (installation-independent by
        // construction — no medium was ever consulted).
        assert_eq!(
            committed_marker().canonical_bytes(),
            marker.canonical_bytes()
        );
    }

    #[test]
    fn the_marker_verifies_rederived_evidence() {
        let marker = committed_marker();
        let records = marker_records();
        // The correct observations verify.
        marker.verify_coverage(&fixture_observations()).unwrap();
        marker
            .verify_journal_obligations(&fixture_journal_names())
            .unwrap();

        // The identity repartition refuses on the coverage digest
        // (same counters, different coverage).
        let swapped = vec![
            observation(0, records[1], batch_counters(2, 0, 1, 1)),
            observation(1, records[0], batch_counters(1, 1, 0, 0)),
            observation(2, records[2], batch_counters(0, 0, 3, 0)),
        ];
        assert!(matches!(
            marker.verify_coverage(&swapped),
            Err(SaveError::Integrity("range-completion/coverage-mismatch"))
        ));

        // The counters-only tamper: the SAME marker identities
        // (the digest is unchanged) with one batch's counters
        // mutated — the comparison refuses on the counters (the
        // compare-BOTH discipline).
        let counter_tampered = vec![
            observation(0, records[0], batch_counters(3, 0, 1, 1)),
            observation(1, records[1], batch_counters(1, 1, 0, 0)),
            observation(2, records[2], batch_counters(0, 0, 3, 0)),
        ];
        assert!(matches!(
            marker.verify_coverage(&counter_tampered),
            Err(SaveError::Integrity("range-completion/counter-mismatch"))
        ));

        // A wrong-count observation set refuses.
        let short = vec![fixture_observations()[0].clone()];
        assert!(marker.verify_coverage(&short).is_err());

        // The journal evidence: a mutated name list refuses on
        // the journal digest.
        let mutated_names = vec!["journal.t1.0".to_owned(), "journal.t1.3".to_owned()];
        assert!(matches!(
            marker.verify_journal_obligations(&mutated_names),
            Err(SaveError::Integrity("range-completion/journal-mismatch"))
        ));
        // The empty obligation list: a marker built over it
        // verifies against the empty list (the legitimate
        // zero-count state).
        let context = fixture_context();
        let empty_journal = commit_journal_obligations(&context, &[]).unwrap();
        let empty_marker = RangeCompletionMarkerV3 {
            mk: context.mk,
            plan: context.plan,
            begin: context.begin,
            coverage: commit_range_coverage(&context, &fixture_observations())
                .unwrap()
                .digest,
            journal: empty_journal,
            batch_count: 3,
            journal_count: 0,
            counters: commit_range_coverage(&context, &fixture_observations())
                .unwrap()
                .aggregate,
        };
        let empty_bytes = empty_marker.canonical_bytes();
        assert_eq!(
            RangeCompletionMarkerV3::decode(&empty_bytes).unwrap(),
            empty_marker
        );
        empty_marker.verify_journal_obligations(&[]).unwrap();
    }
}
