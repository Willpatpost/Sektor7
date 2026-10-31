//! The reclamation ledger (PP-B3, D-PP-4's verify-before-reclaim
//! retention protocol — the audit half): the versioned
//! `engine-persist/reclamation-ledger/1` object referenced by the
//! reclamation's audit revision on the canonical chain. The audit is
//! appended BEFORE any destructive action (Phase A — the eligibility
//! verified against the still-present bytes, the digest recomputed,
//! the retention window checked) and read back verified; the deletes
//! (Phase B) are idempotent and restartable — a re-run re-derives
//! the eligibility, recognizes the installed audit by its
//! deterministic idempotency key bound to the reclaimed checkpoint,
//! and deletes only what remains.
//!
//! **The binding, never the duplication:** the ledger binds to the
//! reclaimed checkpoint's MK — the PERMANENT metadata object (never
//! GC'd) already carrying the exact identities, keys, epochs, skips,
//! and digests. The audit records the event's bindings (the MK and
//! SK identities, the pre-verified range digest and its seed, the
//! counts, the journal files removed alongside) — the exact
//! identity list lives in the permanent metadata, never
//! reserialized here.

use engine_core::{Decoder, Encoder, SaveError};

use crate::hash::Hash;
use crate::medium::{AppendOutcome, CanonicalMedium, MediumError, StoredObject};
use crate::record::RevisionRecord;

/// The reclamation-ledger object's versioned domain (the audit
/// revision's single content reference).
pub const RECLAMATION_LEDGER_DOMAIN: &str = "engine-persist/reclamation-ledger/1";
pub const RECLAMATION_LEDGER_VERSION: u32 = 1;

/// The audit revision's idempotency-key domain (deterministically
/// derived from the reclaimed MK — a retried audit over the same
/// checkpoint carries the same key, so the chain-wide uniqueness
/// makes the retry idempotent and the restart recognizes the
/// installed audit).
pub const RECLAMATION_LEDGER_KEY_DOMAIN: &str = "engine-persist/reclamation-ledger-key/1";

/// Documented maxima (bounded decode).
pub const MAX_RECLAMATION_JOURNAL_FILES: u64 = 1 << 16;
pub const MAX_RECLAMATION_JOURNAL_NAME: u64 = 1024;
pub const MAX_RECLAMATION_UNITS: u64 = 1 << 16;
pub const MAX_RECLAMATION_UNIT_OBJECTS: u64 = 1 << 16;

/// One audited deletion unit (the review corrective F4): the record
/// identity and its referenced content objects — the COMPLETE
/// manifest collected while the range bytes were still present
/// (Phase A's digest verification reads every range record). The
/// manifest makes an interrupted Phase B resumable: the re-run
/// deletes from the AUDIT, never a re-read of a possibly-absent
/// record, so a process death between a record deletion and its
/// object deletions converges on the retry instead of orphaning
/// content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimUnit {
    /// The reclaimed revision's identity (its record object).
    pub record: Hash,
    /// The revision's referenced content objects (from its
    /// content_refs, read before any deletion).
    pub objects: Vec<Hash>,
}

/// The reclamation audit's bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclamationLedger {
    /// The checkpoint whose range is reclaimed (MK — the permanent
    /// metadata carrying the exact identities, keys, epochs, skips,
    /// and digests; this audit binds to it, never duplicates it).
    pub mk: Hash,
    /// The retired snapshot revision (SK — MK's parent; SK is
    /// reclaimed together with its range, the whole-revision unit).
    pub sk: Hash,
    /// The pre-verified range digest — RECOMPUTED against the
    /// still-present range bytes BEFORE this audit was appended (the
    /// eligibility proof; after the deletes it is a
    /// previously-verified commitment).
    pub range_digest: [u8; 32],
    /// The digest chain's seed (carried from the checkpoint metadata
    /// for the self-contained binding).
    pub seed: [u8; 32],
    /// The reclaimed record count (the checkpoint's index length —
    /// the range publications plus SK's own retirement entry).
    pub records: u64,
    /// The idempotency keys among the reclaimed records (the
    /// membership universe preserves them — observational).
    pub reclaimed_keys: u64,
    /// The journal file NAMES removed alongside the range (only
    /// after the rotation's verified active journal and survival set
    /// stand; empty when no rotation preceded).
    pub journal_files: Vec<String>,
    /// The COMPLETE deletion manifest (the review corrective F4):
    /// one unit per indexed record — the record identity and its
    /// referenced content objects — collected during Phase A's
    /// digest verification while the bytes were present. Phase B
    /// deletes from this manifest, so an interrupted deletion
    /// converges on the retry.
    pub units: Vec<ReclaimUnit>,
}

impl ReclamationLedger {
    /// Canonical bytes (the audit revision's single content object).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_str(RECLAMATION_LEDGER_DOMAIN);
        enc.write_u32(RECLAMATION_LEDGER_VERSION);
        enc.write_bytes(self.mk.as_bytes());
        enc.write_bytes(self.sk.as_bytes());
        enc.write_bytes(&self.range_digest);
        enc.write_bytes(&self.seed);
        enc.write_u64(self.records);
        enc.write_u64(self.reclaimed_keys);
        enc.write_count(self.units.len());
        for unit in &self.units {
            enc.write_bytes(unit.record.as_bytes());
            enc.write_count(unit.objects.len());
            for object in &unit.objects {
                enc.write_bytes(object.as_bytes());
            }
        }
        enc.write_count(self.journal_files.len());
        for name in &self.journal_files {
            enc.write_str(name);
        }
        enc.into_bytes()
    }

    /// Strict decode: the domain and version gates, truncation and
    /// trailing bytes, bounded counts and names, real identities,
    /// and the keys-within-records coherence.
    pub fn decode(bytes: &[u8]) -> Result<Self, SaveError> {
        const SECTION: &str = "reclamation-ledger";
        let mut dec = Decoder::new(bytes);
        dec.read_domain(RECLAMATION_LEDGER_DOMAIN)?;
        let version = dec.read_u32()?;
        if version != RECLAMATION_LEDGER_VERSION {
            return Err(SaveError::Version {
                observed: version,
                supported: RECLAMATION_LEDGER_VERSION,
            });
        }
        let mk = read_hash(&mut dec, SECTION)?;
        let sk = read_hash(&mut dec, SECTION)?;
        if mk == Hash::ZERO || sk == Hash::ZERO {
            return Err(SaveError::Integrity("reclamation-ledger/zero-identity"));
        }
        let range_digest = read_digest(&mut dec, SECTION)?;
        let seed = read_digest(&mut dec, SECTION)?;
        let records = dec.read_u64()?;
        let reclaimed_keys = dec.read_u64()?;
        if reclaimed_keys > records {
            return Err(SaveError::Integrity(
                "reclamation-ledger/keys-exceed-records",
            ));
        }
        let unit_count = dec.read_count(MAX_RECLAMATION_UNITS, SECTION, "unit-count")?;
        let mut units = Vec::with_capacity(unit_count as usize);
        for _ in 0..unit_count {
            let record = read_hash(&mut dec, SECTION)?;
            if record == Hash::ZERO {
                return Err(SaveError::Integrity("reclamation-ledger/zero-identity"));
            }
            let object_count =
                dec.read_count(MAX_RECLAMATION_UNIT_OBJECTS, SECTION, "unit-object-count")?;
            let mut objects = Vec::with_capacity(object_count as usize);
            for _ in 0..object_count {
                objects.push(read_hash(&mut dec, SECTION)?);
            }
            units.push(ReclaimUnit { record, objects });
        }
        if records != units.len() as u64 {
            return Err(SaveError::Integrity("reclamation-ledger/unit-count"));
        }
        let count = dec.read_count(MAX_RECLAMATION_JOURNAL_FILES, SECTION, "journal-count")?;
        let mut journal_files = Vec::with_capacity(count as usize);
        for _ in 0..count {
            journal_files.push(
                dec.read_str(MAX_RECLAMATION_JOURNAL_NAME, SECTION)?
                    .to_owned(),
            );
        }
        dec.finish()?;
        Ok(ReclamationLedger {
            mk,
            sk,
            range_digest,
            seed,
            records,
            reclaimed_keys,
            journal_files,
            units,
        })
    }
}

fn read_hash(dec: &mut Decoder, section: &'static str) -> Result<Hash, SaveError> {
    let bytes = dec.read_bytes(Hash::WIDTH, section)?;
    Hash::from_bytes(bytes).ok_or(SaveError::Integrity("reclamation-ledger/hash-width"))
}

fn read_digest(dec: &mut Decoder, section: &'static str) -> Result<[u8; 32], SaveError> {
    let bytes = dec.read_bytes(32, section)?;
    let mut digest = [0u8; 32];
    digest.copy_from_slice(bytes);
    Ok(digest)
}

/// The audit revision's deterministic idempotency key (derived from
/// the reclaimed MK — one audit per checkpoint, ever).
pub fn reclamation_ledger_idempotency_key(mk: &Hash) -> Vec<u8> {
    format!("{RECLAMATION_LEDGER_KEY_DOMAIN}:{}", mk.hex()).into_bytes()
}

/// Appends the audit revision: a regular revision whose single
/// content object is the ledger, keyed deterministically by the
/// reclaimed MK, parented on the current head. Returns the candidate
/// (for the caller's Unknown reconciliation) and the outcome. A
/// duplicate key means the audit is already installed — the caller's
/// restart path.
pub fn publish_reclamation_ledger(
    medium: &mut dyn CanonicalMedium,
    ledger: &ReclamationLedger,
    tick: u64,
) -> Result<(Hash, AppendOutcome), MediumError> {
    if ledger.records == 0 {
        return Err(MediumError::Chain {
            rule: "reclamation-ledger/empty",
        });
    }
    let bytes = ledger.canonical_bytes();
    let object = StoredObject::new(bytes)?;
    let head = medium
        .head()?
        .ok_or(MediumError::Chain { rule: "unfounded" })?;
    let parent_epoch = medium.read_record(&head)?.epoch;
    let record = RevisionRecord::regular(head, parent_epoch, tick)
        .with_idempotency_key(reclamation_ledger_idempotency_key(&ledger.mk))
        .with_content_refs(vec![object.hash()]);
    let candidate = record.id();
    let outcome = medium.append(&record, std::slice::from_ref(&object), Some(head))?;
    Ok((candidate, outcome))
}

// --------------------------------------- the K2-C2B ratified wire formats
//
// The owner-ratified closure-expanded reclamation protocol's three
// persisted objects (P1 of the implementation ladder: the canonical
// codecs ONLY — no writer, no execution path, no crossing arm; a
// production chain cannot yet carry any of these domains, so this
// entire section is unreachable from any existing production
// behavior). The ratified semantics these codecs implement:
// per-unit provenance with the sharing rules (a duplicate unit
// RECORD is invalid; a repeated OBJECT identity ACROSS units is
// legitimate sharing; a duplicate object WITHIN one unit is
// invalid), the plan's complete-membership partition in the
// deterministic epoch order, and the two-level completion evidence
// (batch markers certify only their own storage obligations; the
// range-terminal marker certifies the whole retirement including
// the journal obligations).

/// The v2 reclamation ledger's versioned domain: the closure-
/// expanded, batch-scoped audit (one ledger per plan batch).
pub const RECLAMATION_LEDGER_V2_DOMAIN: &str = "engine-persist/reclamation-ledger/2";
pub const RECLAMATION_LEDGER_V2_VERSION: u32 = 2;

/// The v2 audit revision's idempotency-key domain (deterministic in
/// the MK and the plan-defined batch index — one v2 audit per batch,
/// ever; the restart recognizes the installed audit by the key).
pub const RECLAMATION_LEDGER_V2_KEY_DOMAIN: &str = "engine-persist/reclamation-ledger-key/2";

/// The canonical batch plan's versioned domain: the complete
/// retirement membership partition for one MK, published before any
/// destructive work (the all-audits-first protocol's authority
/// record).
pub const RECLAMATION_PLAN_DOMAIN: &str = "engine-persist/reclamation-plan/1";
pub const RECLAMATION_PLAN_VERSION: u32 = 1;

/// The plan revision's idempotency-key domain (deterministic in the
/// MK — one plan per checkpoint, ever).
pub const RECLAMATION_PLAN_KEY_DOMAIN: &str = "engine-persist/reclamation-plan-key/1";

/// The two-level completion marker's versioned domain: the batch
/// marker (this batch's storage obligations) and the range-terminal
/// marker (the complete retirement, journals included).
pub const RECLAMATION_COMPLETE_DOMAIN: &str = "engine-persist/reclamation-complete/1";
pub const RECLAMATION_COMPLETE_VERSION: u32 = 1;

/// The completion markers' idempotency-key domain (deterministic in
/// the MK and the batch index, or the MK and the `range` tag for
/// the terminal marker).
pub const RECLAMATION_COMPLETE_KEY_DOMAIN: &str = "engine-persist/reclamation-complete-key/1";

/// The v2 unit's object-count ceiling — COMPOSED from the enforced
/// schema bounds rather than inherited from v1: one record's direct
/// references (`MAX_HASH_LIST_LEN`) plus its verified composite
/// closure (`MAX_MANIFEST_CHUNKS`). The closure-expanded
/// representation's honest worst case.
pub const MAX_RECLAMATION_UNIT_OBJECTS_V2: u64 =
    crate::record::MAX_HASH_LIST_LEN + crate::composite_manifest::MAX_MANIFEST_CHUNKS;

/// The ratified maximum ledger byte ceiling (the admission's L) —
/// the codec-visible form of the owner-ratified bound.
pub const MAX_RECLAMATION_LEDGER_BYTES: u64 = 512 * 1024 * 1024;

/// The maximum deferred-identity cardinality a batch marker may
/// certify — derived from the LEDGER byte ceiling, not the
/// single-unit object bound (a batch spans many units, so its
/// combined distinct deferred set may legitimately exceed
/// `MAX_RECLAMATION_UNIT_OBJECTS_V2`): every deferred identity
/// appears in its batch's ledger as a unit object costing at least
/// `Hash::WIDTH` bytes plus per-unit count framing, so a ledger
/// within the byte ceiling cannot declare more distinct objects
/// than this. The bound is therefore always sufficient — a batch
/// can never become unfinishable because its valid completion
/// evidence exceeded the decoder limit.
pub const MAX_RECLAMATION_DEFERRED_IDENTITIES: u64 =
    MAX_RECLAMATION_LEDGER_BYTES / (Hash::WIDTH + 8);

/// The reclamation-ledger family prefix — every versioned audit
/// domain begins with it. The family gate's classification input:
/// a domain in the family with no matching exact arm refuses typed
/// as recognized-but-unsupported, never silently walked past.
pub const RECLAMATION_LEDGER_FAMILY: &str = "engine-persist/reclamation-ledger/";

/// Whether a domain belongs to the recognized reclamation-ledger
/// family (any version).
pub fn is_reclamation_ledger_family(domain: &str) -> bool {
    domain.starts_with(RECLAMATION_LEDGER_FAMILY)
}

/// The v2 reclamation ledger: the ratified closure-expanded,
/// batch-scoped audit. The units carry each retiring record's
/// direct references PLUS its verified composite closure (collected
/// and envelope-verified at Phase A, while the bytes were present —
/// the durable provenance the manifest-driven resume and the
/// post-deletion authentication rely on). The journal files are
/// RANGE-level obligations and live in the plan, not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclamationLedgerV2 {
    /// The checkpoint whose range is reclaimed (the permanent
    /// metadata binding).
    pub mk: Hash,
    /// The retired snapshot revision (MK's parent).
    pub sk: Hash,
    /// The plan-defined batch index this ledger covers.
    pub batch: u64,
    /// The pre-verified range digest (recomputed at Phase A against
    /// the still-present bytes).
    pub range_digest: [u8; 32],
    /// The digest chain's seed (carried from the checkpoint
    /// metadata).
    pub seed: [u8; 32],
    /// The reclaimed record count (this batch's units).
    pub records: u64,
    /// The idempotency keys among this batch's records
    /// (observational).
    pub reclaimed_keys: u64,
    /// The complete deletion manifest for this batch: one unit per
    /// retiring record, its objects closure-expanded.
    pub units: Vec<ReclaimUnit>,
}

impl ReclamationLedgerV2 {
    /// Canonical bytes (the batch audit revision's single content
    /// object) — deterministic in the given order.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_str(RECLAMATION_LEDGER_V2_DOMAIN);
        enc.write_u32(RECLAMATION_LEDGER_V2_VERSION);
        enc.write_bytes(self.mk.as_bytes());
        enc.write_bytes(self.sk.as_bytes());
        enc.write_u64(self.batch);
        enc.write_bytes(&self.range_digest);
        enc.write_bytes(&self.seed);
        enc.write_u64(self.records);
        enc.write_u64(self.reclaimed_keys);
        enc.write_count(self.units.len());
        for unit in &self.units {
            enc.write_bytes(unit.record.as_bytes());
            enc.write_count(unit.objects.len());
            for object in &unit.objects {
                enc.write_bytes(object.as_bytes());
            }
        }
        enc.into_bytes()
    }

    /// Strict decode: the domain and version gates, the bounded
    /// counts, the zero-identity refusals, the count coherence, and
    /// THE SHARING RULES — a duplicate unit record (the index
    /// members are unique) or a duplicate object WITHIN one unit (a
    /// record's references and closure are sets) refuses typed;
    /// repeated object identities ACROSS units are legitimate
    /// sharing and pass.
    pub fn decode(bytes: &[u8]) -> Result<Self, SaveError> {
        const SECTION: &str = "reclamation-ledger-v2";
        let mut dec = Decoder::new(bytes);
        dec.read_domain(RECLAMATION_LEDGER_V2_DOMAIN)?;
        let version = dec.read_u32()?;
        if version != RECLAMATION_LEDGER_V2_VERSION {
            return Err(SaveError::Version {
                observed: version,
                supported: RECLAMATION_LEDGER_V2_VERSION,
            });
        }
        let mk = read_hash(&mut dec, SECTION)?;
        let sk = read_hash(&mut dec, SECTION)?;
        if mk == Hash::ZERO || sk == Hash::ZERO {
            return Err(SaveError::Integrity("reclamation-ledger-v2/zero-identity"));
        }
        let batch = dec.read_u64()?;
        let range_digest = read_digest(&mut dec, SECTION)?;
        let seed = read_digest(&mut dec, SECTION)?;
        let records = dec.read_u64()?;
        let reclaimed_keys = dec.read_u64()?;
        if reclaimed_keys > records {
            return Err(SaveError::Integrity(
                "reclamation-ledger-v2/keys-exceed-records",
            ));
        }
        let unit_count = dec.read_count(MAX_RECLAMATION_UNITS, SECTION, "unit-count")?;
        let mut units = Vec::with_capacity(unit_count as usize);
        let mut seen_records = std::collections::BTreeSet::new();
        for _ in 0..unit_count {
            let record = read_hash(&mut dec, SECTION)?;
            if record == Hash::ZERO {
                return Err(SaveError::Integrity("reclamation-ledger-v2/zero-identity"));
            }
            if !seen_records.insert(record) {
                return Err(SaveError::Integrity(
                    "reclamation-ledger-v2/duplicate-unit-record",
                ));
            }
            let object_count = dec.read_count(
                MAX_RECLAMATION_UNIT_OBJECTS_V2,
                SECTION,
                "unit-object-count",
            )?;
            let mut objects = Vec::with_capacity(object_count as usize);
            let mut seen_objects = std::collections::BTreeSet::new();
            for _ in 0..object_count {
                let object = read_hash(&mut dec, SECTION)?;
                if object == Hash::ZERO {
                    return Err(SaveError::Integrity("reclamation-ledger-v2/zero-identity"));
                }
                if !seen_objects.insert(object) {
                    return Err(SaveError::Integrity(
                        "reclamation-ledger-v2/duplicate-unit-object",
                    ));
                }
                objects.push(object);
            }
            units.push(ReclaimUnit { record, objects });
        }
        if records != units.len() as u64 {
            return Err(SaveError::Integrity("reclamation-ledger-v2/unit-count"));
        }
        dec.finish()?;
        Ok(ReclamationLedgerV2 {
            mk,
            sk,
            batch,
            range_digest,
            seed,
            records,
            reclaimed_keys,
            units,
        })
    }
}

/// One batch of the canonical plan: the plan-defined index and the
/// retiring-record membership (a contiguous slice of the
/// deterministic epoch order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanBatch {
    /// The plan-defined batch index (0-based, strictly increasing —
    /// the deterministic identity component).
    pub index: u64,
    /// This batch's retiring-record identities, in the plan's epoch
    /// order.
    pub records: Vec<Hash>,
}

/// The canonical batch plan: the complete, admission-validated
/// retirement membership partition for one MK — the durable
/// authority record that defines the expected batch set, the
/// execution order, and the range-level journal obligations.
/// Published before any destructive work; the restart reads the
/// plan, never re-derives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimPlan {
    /// The checkpoint whose range is retired.
    pub mk: Hash,
    /// The retired snapshot revision (MK's parent).
    pub sk: Hash,
    /// The pre-verified range digest and its seed (the checkpoint
    /// bindings).
    pub range_digest: [u8; 32],
    pub seed: [u8; 32],
    /// The ledger byte bound the partitioning used (recorded for
    /// cross-checking the batches' ledgers).
    pub ledger_byte_limit: u64,
    /// The batches, in the deterministic order (index 0 first — the
    /// newest epoch slice).
    pub batches: Vec<PlanBatch>,
    /// The range-level journal retirement obligations (the stale
    /// generations removable after every batch completes — the
    /// rotation's survival-evidence-derived names).
    pub journal_files: Vec<String>,
}

impl ReclaimPlan {
    /// The total retiring-record count (the partition's union).
    pub fn total_units(&self) -> u64 {
        self.batches.iter().map(|b| b.records.len() as u64).sum()
    }

    /// Canonical bytes (the plan revision's single content object).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_str(RECLAMATION_PLAN_DOMAIN);
        enc.write_u32(RECLAMATION_PLAN_VERSION);
        enc.write_bytes(self.mk.as_bytes());
        enc.write_bytes(self.sk.as_bytes());
        enc.write_bytes(&self.range_digest);
        enc.write_bytes(&self.seed);
        enc.write_u64(self.ledger_byte_limit);
        enc.write_count(self.batches.len());
        for batch in &self.batches {
            enc.write_u64(batch.index);
            enc.write_count(batch.records.len());
            for record in &batch.records {
                enc.write_bytes(record.as_bytes());
            }
        }
        enc.write_count(self.journal_files.len());
        for name in &self.journal_files {
            enc.write_str(name);
        }
        enc.into_bytes()
    }

    /// Strict decode: the gates, the bounded counts, the
    /// partition rules — the batch indices strictly increasing from
    /// zero (the deterministic order), no empty batch, no duplicate
    /// record across ANY batch (the complete-membership partition:
    /// no overlap), and the journal-name bounds.
    pub fn decode(bytes: &[u8]) -> Result<Self, SaveError> {
        const SECTION: &str = "reclamation-plan";
        let mut dec = Decoder::new(bytes);
        dec.read_domain(RECLAMATION_PLAN_DOMAIN)?;
        let version = dec.read_u32()?;
        if version != RECLAMATION_PLAN_VERSION {
            return Err(SaveError::Version {
                observed: version,
                supported: RECLAMATION_PLAN_VERSION,
            });
        }
        let mk = read_hash(&mut dec, SECTION)?;
        let sk = read_hash(&mut dec, SECTION)?;
        if mk == Hash::ZERO || sk == Hash::ZERO {
            return Err(SaveError::Integrity("reclamation-plan/zero-identity"));
        }
        let range_digest = read_digest(&mut dec, SECTION)?;
        let seed = read_digest(&mut dec, SECTION)?;
        let ledger_byte_limit = dec.read_u64()?;
        if ledger_byte_limit == 0 {
            return Err(SaveError::Integrity("reclamation-plan/zero-byte-limit"));
        }
        let batch_count = dec.read_count(MAX_RECLAMATION_UNITS, SECTION, "batch-count")?;
        if batch_count == 0 {
            // A real retirement has at least one record — a
            // zero-batch plan is not a legitimate ratified state.
            return Err(SaveError::Integrity("reclamation-plan/empty-plan"));
        }
        let mut batches = Vec::with_capacity(batch_count as usize);
        let mut seen_records = std::collections::BTreeSet::new();
        let mut aggregate_records: u64 = 0;
        for expected_index in 0..batch_count {
            let index = dec.read_u64()?;
            if index != expected_index {
                return Err(SaveError::Integrity("reclamation-plan/batch-index-order"));
            }
            let record_count =
                dec.read_count(MAX_RECLAMATION_UNITS, SECTION, "batch-record-count")?;
            if record_count == 0 {
                return Err(SaveError::Integrity("reclamation-plan/empty-batch"));
            }
            // The aggregate membership ceiling (checked
            // arithmetic): the plan partitions ONE MK retirement
            // range, whose membership is bounded by the checkpoint
            // index — the same bound the v1 ledger's unit count
            // uses.
            aggregate_records =
                aggregate_records
                    .checked_add(record_count)
                    .ok_or(SaveError::Integrity(
                        "reclamation-plan/aggregate-membership",
                    ))?;
            if aggregate_records > MAX_RECLAMATION_UNITS {
                return Err(SaveError::Integrity(
                    "reclamation-plan/aggregate-membership",
                ));
            }
            let mut records = Vec::with_capacity(record_count as usize);
            for _ in 0..record_count {
                let record = read_hash(&mut dec, SECTION)?;
                if record == Hash::ZERO {
                    return Err(SaveError::Integrity("reclamation-plan/zero-identity"));
                }
                if !seen_records.insert(record) {
                    return Err(SaveError::Integrity("reclamation-plan/duplicate-record"));
                }
                records.push(record);
            }
            batches.push(PlanBatch { index, records });
        }
        let journal_count =
            dec.read_count(MAX_RECLAMATION_JOURNAL_FILES, SECTION, "journal-count")?;
        let mut journal_files = Vec::with_capacity(journal_count as usize);
        for _ in 0..journal_count {
            journal_files.push(
                dec.read_str(MAX_RECLAMATION_JOURNAL_NAME, SECTION)?
                    .to_owned(),
            );
        }
        dec.finish()?;
        Ok(ReclaimPlan {
            mk,
            sk,
            range_digest,
            seed,
            ledger_byte_limit,
            batches,
            journal_files,
        })
    }
}

/// Which completion level a marker certifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    /// This batch's own provider-storage obligations (its mandatory
    /// object set, across normal and quarantine forms) — never the
    /// journals, never another batch.
    Batch,
    /// The complete MK retirement: every planned batch's marker
    /// installed and verified, the journal retirement and ownership
    /// updates applied.
    Range,
}

/// The two-level completion marker: the persisted terminal evidence.
/// A batch marker binds its audited ledger's revision identity and
/// records the deferred (shared, still-live) identities it
/// authorized to preserve; the range marker binds the plan and the
/// retired journal names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclamationComplete {
    /// Which level this marker certifies.
    pub kind: CompletionKind,
    /// The retired checkpoint.
    pub mk: Hash,
    /// The plan-defined batch index (batch markers only).
    pub batch: u64,
    /// The audited source's revision identity: the batch ledger's
    /// record for a batch marker; the plan's record for the range
    /// marker.
    pub source: Hash,
    /// The batch's mandatory (deleted) identity count (batch
    /// markers only — cross-checkable against the ledger and the
    /// partition).
    pub mandatory_count: u64,
    /// The deferred identities this batch authorized to preserve
    /// (shared with surviving roots — the visible debt the later
    /// audit claims).
    pub deferred: Vec<Hash>,
    /// The retired journal-file names (range markers only — the
    /// completed obligations the marker certifies).
    pub journal_files: Vec<String>,
}

impl ReclamationComplete {
    /// Canonical bytes (the marker revision's single content
    /// object) — kind-gated: a batch marker encodes its batch,
    /// source, mandatory count, and deferred set; a range marker
    /// encodes its source and the retired journal names.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_str(RECLAMATION_COMPLETE_DOMAIN);
        enc.write_u32(RECLAMATION_COMPLETE_VERSION);
        enc.write_u8(match self.kind {
            CompletionKind::Batch => 0,
            CompletionKind::Range => 1,
        });
        enc.write_bytes(self.mk.as_bytes());
        match self.kind {
            CompletionKind::Batch => {
                enc.write_u64(self.batch);
                enc.write_bytes(self.source.as_bytes());
                enc.write_u64(self.mandatory_count);
                enc.write_count(self.deferred.len());
                for identity in &self.deferred {
                    enc.write_bytes(identity.as_bytes());
                }
            }
            CompletionKind::Range => {
                enc.write_bytes(self.source.as_bytes());
                enc.write_count(self.journal_files.len());
                for name in &self.journal_files {
                    enc.write_str(name);
                }
            }
        }
        enc.into_bytes()
    }

    /// Strict decode: the gates, the kind discriminant, the
    /// kind-coherence (a batch marker carries no journal names; a
    /// range marker carries no batch, deferred set, or mandatory
    /// count), the zero-identity refusals, and the deferred set's
    /// duplicate rejection.
    pub fn decode(bytes: &[u8]) -> Result<Self, SaveError> {
        const SECTION: &str = "reclamation-complete";
        let mut dec = Decoder::new(bytes);
        dec.read_domain(RECLAMATION_COMPLETE_DOMAIN)?;
        let version = dec.read_u32()?;
        if version != RECLAMATION_COMPLETE_VERSION {
            return Err(SaveError::Version {
                observed: version,
                supported: RECLAMATION_COMPLETE_VERSION,
            });
        }
        let kind = match dec.read_u8()? {
            0 => CompletionKind::Batch,
            1 => CompletionKind::Range,
            other => {
                return Err(SaveError::Enum {
                    section: SECTION,
                    discriminant: u64::from(other),
                });
            }
        };
        let mk = read_hash(&mut dec, SECTION)?;
        if mk == Hash::ZERO {
            return Err(SaveError::Integrity("reclamation-complete/zero-identity"));
        }
        match kind {
            CompletionKind::Batch => {
                let batch = dec.read_u64()?;
                let source = read_hash(&mut dec, SECTION)?;
                if source == Hash::ZERO {
                    return Err(SaveError::Integrity("reclamation-complete/zero-identity"));
                }
                let mandatory_count = dec.read_u64()?;
                let deferred_count = dec.read_count(
                    MAX_RECLAMATION_DEFERRED_IDENTITIES,
                    SECTION,
                    "deferred-count",
                )?;
                let mut deferred = Vec::with_capacity(deferred_count as usize);
                let mut seen = std::collections::BTreeSet::new();
                for _ in 0..deferred_count {
                    let identity = read_hash(&mut dec, SECTION)?;
                    if identity == Hash::ZERO {
                        return Err(SaveError::Integrity("reclamation-complete/zero-identity"));
                    }
                    if !seen.insert(identity) {
                        return Err(SaveError::Integrity(
                            "reclamation-complete/duplicate-deferred",
                        ));
                    }
                    deferred.push(identity);
                }
                dec.finish()?;
                Ok(ReclamationComplete {
                    kind,
                    mk,
                    batch,
                    source,
                    mandatory_count,
                    deferred,
                    journal_files: Vec::new(),
                })
            }
            CompletionKind::Range => {
                let source = read_hash(&mut dec, SECTION)?;
                if source == Hash::ZERO {
                    return Err(SaveError::Integrity("reclamation-complete/zero-identity"));
                }
                let journal_count =
                    dec.read_count(MAX_RECLAMATION_JOURNAL_FILES, SECTION, "journal-count")?;
                let mut journal_files = Vec::with_capacity(journal_count as usize);
                for _ in 0..journal_count {
                    journal_files.push(
                        dec.read_str(MAX_RECLAMATION_JOURNAL_NAME, SECTION)?
                            .to_owned(),
                    );
                }
                dec.finish()?;
                Ok(ReclamationComplete {
                    kind,
                    mk,
                    batch: 0,
                    source,
                    mandatory_count: 0,
                    deferred: Vec::new(),
                    journal_files,
                })
            }
        }
    }
}

/// The v2 audit revision's deterministic idempotency key (the MK
/// and the plan-defined batch index — one v2 audit per batch, ever;
/// the W2 exclusion and the restart recognition both key on this).
pub fn reclamation_ledger_v2_idempotency_key(mk: &Hash, batch: u64) -> Vec<u8> {
    format!("{RECLAMATION_LEDGER_V2_KEY_DOMAIN}:{}:{batch}", mk.hex()).into_bytes()
}

/// The plan revision's deterministic idempotency key (the MK — one
/// plan per checkpoint, ever).
pub fn reclamation_plan_idempotency_key(mk: &Hash) -> Vec<u8> {
    format!("{RECLAMATION_PLAN_KEY_DOMAIN}:{}", mk.hex()).into_bytes()
}

/// The completion markers' deterministic idempotency key: the batch
/// marker's (the MK and the batch index) or the range-terminal
/// marker's (the MK and the `range` tag).
pub fn reclamation_complete_idempotency_key(mk: &Hash, batch: Option<u64>) -> Vec<u8> {
    match batch {
        Some(batch) => {
            format!("{RECLAMATION_COMPLETE_KEY_DOMAIN}:{}:{batch}", mk.hex()).into_bytes()
        }
        None => format!("{RECLAMATION_COMPLETE_KEY_DOMAIN}:{}:range", mk.hex()).into_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash_object;

    fn sample_ledger() -> ReclamationLedger {
        ReclamationLedger {
            mk: hash_object(b"sample-mk"),
            sk: hash_object(b"sample-sk"),
            range_digest: [7; 32],
            seed: [9; 32],
            records: 3,
            reclaimed_keys: 2,
            journal_files: vec!["journal.t1".to_owned()],
            units: vec![
                ReclaimUnit {
                    record: hash_object(b"sample-record-a"),
                    objects: vec![hash_object(b"sample-object-a")],
                },
                ReclaimUnit {
                    record: hash_object(b"sample-record-b"),
                    objects: Vec::new(),
                },
                ReclaimUnit {
                    record: hash_object(b"sample-record-c"),
                    objects: vec![
                        hash_object(b"sample-object-c1"),
                        hash_object(b"sample-object-c2"),
                    ],
                },
            ],
        }
    }

    #[test]
    fn the_round_trip_is_byte_deterministic() {
        let ledger = sample_ledger();
        let bytes = ledger.canonical_bytes();
        let restored = ReclamationLedger::decode(&bytes).unwrap();
        assert_eq!(restored, ledger);
        assert_eq!(restored.canonical_bytes(), bytes);
    }

    #[test]
    fn the_domain_and_version_are_refused() {
        let bytes = sample_ledger().canonical_bytes();
        let mut wrong_domain = Vec::new();
        let mut enc = Encoder::new();
        enc.write_str("sektor7/other/1");
        wrong_domain.extend_from_slice(&enc.into_bytes());
        wrong_domain.extend_from_slice(&bytes[8 + RECLAMATION_LEDGER_DOMAIN.len()..]);
        assert!(matches!(
            ReclamationLedger::decode(&wrong_domain),
            Err(SaveError::Domain { .. })
        ));

        let mut wrong_version = bytes.clone();
        let version_at = 8 + RECLAMATION_LEDGER_DOMAIN.len();
        wrong_version[version_at..version_at + 4].copy_from_slice(&2u32.to_le_bytes());
        assert!(matches!(
            ReclamationLedger::decode(&wrong_version),
            Err(SaveError::Version { observed: 2, .. })
        ));
    }

    #[test]
    fn truncation_and_trailing_bytes_are_rejected() {
        let bytes = sample_ledger().canonical_bytes();
        for cut in 0..bytes.len() {
            assert!(
                ReclamationLedger::decode(&bytes[..cut]).is_err(),
                "a {cut}-byte prefix must not decode"
            );
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            ReclamationLedger::decode(&trailing),
            Err(SaveError::Trailing { .. })
        ));
    }

    #[test]
    fn the_coherence_rules_are_enforced() {
        // The keys-within-records bound.
        let mut ledger = sample_ledger();
        ledger.reclaimed_keys = ledger.records + 1;
        assert!(matches!(
            ReclamationLedger::decode(&ledger.canonical_bytes()),
            Err(SaveError::Integrity(
                "reclamation-ledger/keys-exceed-records"
            ))
        ));
        // The zero identities (the struct form — the wire's
        // length-prefixed fields make byte patching fragile; a
        // zero-identity mk encodes and must refuse at decode).
        let mut ledger = sample_ledger();
        ledger.mk = Hash::ZERO;
        assert!(matches!(
            ReclamationLedger::decode(&ledger.canonical_bytes()),
            Err(SaveError::Integrity("reclamation-ledger/zero-identity"))
        ));
        // The manifest-count coherence (the audited record count must
        // equal the unit count).
        let mut ledger = sample_ledger();
        ledger.records = ledger.units.len() as u64 + 1;
        assert!(matches!(
            ReclamationLedger::decode(&ledger.canonical_bytes()),
            Err(SaveError::Integrity("reclamation-ledger/unit-count"))
        ));
    }

    #[test]
    fn the_key_is_deterministic_in_mk() {
        let mk = hash_object(b"sample-mk");
        assert_eq!(
            reclamation_ledger_idempotency_key(&mk),
            reclamation_ledger_idempotency_key(&mk)
        );
        assert_ne!(
            reclamation_ledger_idempotency_key(&mk),
            reclamation_ledger_idempotency_key(&hash_object(b"other-mk"))
        );
    }

    // ------------------------- the K2-C2B ratified wire formats (P1)

    fn sample_v2_ledger() -> ReclamationLedgerV2 {
        // The SHARED object appears in BOTH units — the legitimate
        // cross-unit repetition the sharing rules permit.
        let shared = hash_object(b"v2-shared-chunk");
        ReclamationLedgerV2 {
            mk: hash_object(b"v2-mk"),
            sk: hash_object(b"v2-sk"),
            batch: 0,
            range_digest: [3; 32],
            seed: [5; 32],
            records: 2,
            reclaimed_keys: 1,
            units: vec![
                ReclaimUnit {
                    record: hash_object(b"v2-record-a"),
                    objects: vec![hash_object(b"v2-object-a"), shared],
                },
                ReclaimUnit {
                    record: hash_object(b"v2-record-b"),
                    objects: vec![shared, hash_object(b"v2-object-b")],
                },
            ],
        }
    }

    #[test]
    fn the_v2_ledger_round_trip_permits_cross_unit_sharing() {
        let ledger = sample_v2_ledger();
        let bytes = ledger.canonical_bytes();
        let restored = ReclamationLedgerV2::decode(&bytes).unwrap();
        assert_eq!(restored, ledger);
        assert_eq!(restored.canonical_bytes(), bytes);
    }

    #[test]
    fn the_v2_ledger_refuses_the_domain_and_version() {
        let bytes = sample_v2_ledger().canonical_bytes();
        // The v1 domain is a different string — the domain gate
        // refuses it before any field decoding.
        let mut wrong = Vec::new();
        let mut enc = Encoder::new();
        enc.write_str(RECLAMATION_LEDGER_DOMAIN);
        wrong.extend_from_slice(&enc.into_bytes());
        wrong.extend_from_slice(&bytes[8 + RECLAMATION_LEDGER_V2_DOMAIN.len()..]);
        assert!(matches!(
            ReclamationLedgerV2::decode(&wrong),
            Err(SaveError::Domain { .. })
        ));
        let mut version = bytes.clone();
        let at = 8 + RECLAMATION_LEDGER_V2_DOMAIN.len();
        version[at..at + 4].copy_from_slice(&3u32.to_le_bytes());
        assert!(matches!(
            ReclamationLedgerV2::decode(&version),
            Err(SaveError::Version { observed: 3, .. })
        ));
    }

    #[test]
    fn the_v2_ledger_refuses_truncation_and_trailing() {
        let bytes = sample_v2_ledger().canonical_bytes();
        for cut in 0..bytes.len() {
            assert!(
                ReclamationLedgerV2::decode(&bytes[..cut]).is_err(),
                "a {cut}-byte prefix must not decode"
            );
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            ReclamationLedgerV2::decode(&trailing),
            Err(SaveError::Trailing { .. })
        ));
    }

    #[test]
    fn the_v2_ledger_enforces_the_sharing_rules() {
        // A duplicate unit RECORD across units is invalid.
        let mut ledger = sample_v2_ledger();
        ledger.units[1].record = ledger.units[0].record;
        assert!(matches!(
            ReclamationLedgerV2::decode(&ledger.canonical_bytes()),
            Err(SaveError::Integrity(
                "reclamation-ledger-v2/duplicate-unit-record"
            ))
        ));
        // A duplicate object WITHIN one unit is invalid (a record's
        // references and closure are sets).
        let mut ledger = sample_v2_ledger();
        let first = ledger.units[0].objects[0];
        ledger.units[0].objects.push(first);
        ledger.records = ledger.units.len() as u64;
        assert!(matches!(
            ReclamationLedgerV2::decode(&ledger.canonical_bytes()),
            Err(SaveError::Integrity(
                "reclamation-ledger-v2/duplicate-unit-object"
            ))
        ));
        // The count coherence and the keys bound.
        let mut ledger = sample_v2_ledger();
        ledger.records += 1;
        assert!(matches!(
            ReclamationLedgerV2::decode(&ledger.canonical_bytes()),
            Err(SaveError::Integrity("reclamation-ledger-v2/unit-count"))
        ));
        let mut ledger = sample_v2_ledger();
        ledger.reclaimed_keys = ledger.records + 1;
        assert!(matches!(
            ReclamationLedgerV2::decode(&ledger.canonical_bytes()),
            Err(SaveError::Integrity(
                "reclamation-ledger-v2/keys-exceed-records"
            ))
        ));
    }

    fn sample_plan() -> ReclaimPlan {
        ReclaimPlan {
            mk: hash_object(b"plan-mk"),
            sk: hash_object(b"plan-sk"),
            range_digest: [7; 32],
            seed: [9; 32],
            ledger_byte_limit: 512 * 1024 * 1024,
            batches: vec![
                PlanBatch {
                    index: 0,
                    records: vec![hash_object(b"plan-r1"), hash_object(b"plan-r2")],
                },
                PlanBatch {
                    index: 1,
                    records: vec![hash_object(b"plan-r3")],
                },
            ],
            journal_files: vec!["journal.t1.2".to_owned()],
        }
    }

    #[test]
    fn the_plan_round_trip_and_total_units() {
        let plan = sample_plan();
        let bytes = plan.canonical_bytes();
        let restored = ReclaimPlan::decode(&bytes).unwrap();
        assert_eq!(restored, plan);
        assert_eq!(restored.canonical_bytes(), bytes);
        assert_eq!(restored.total_units(), 3);
    }

    #[test]
    fn the_plan_enforces_the_partition_rules() {
        // A duplicate record ACROSS batches breaks the partition.
        let mut plan = sample_plan();
        let duplicated = plan.batches[0].records[0];
        plan.batches[1].records.push(duplicated);
        assert!(matches!(
            ReclaimPlan::decode(&plan.canonical_bytes()),
            Err(SaveError::Integrity("reclamation-plan/duplicate-record"))
        ));
        // The batch indices are strictly increasing from zero.
        let mut plan = sample_plan();
        plan.batches[0].index = 1;
        assert!(matches!(
            ReclaimPlan::decode(&plan.canonical_bytes()),
            Err(SaveError::Integrity("reclamation-plan/batch-index-order"))
        ));
        // No empty batch.
        let mut plan = sample_plan();
        plan.batches[1].records.clear();
        assert!(matches!(
            ReclaimPlan::decode(&plan.canonical_bytes()),
            Err(SaveError::Integrity("reclamation-plan/empty-batch"))
        ));
        // Truncation and trailing.
        let bytes = sample_plan().canonical_bytes();
        for cut in 0..bytes.len() {
            assert!(ReclaimPlan::decode(&bytes[..cut]).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            ReclaimPlan::decode(&trailing),
            Err(SaveError::Trailing { .. })
        ));
    }

    fn sample_batch_marker() -> ReclamationComplete {
        ReclamationComplete {
            kind: CompletionKind::Batch,
            mk: hash_object(b"mark-mk"),
            batch: 1,
            source: hash_object(b"mark-ledger-record"),
            mandatory_count: 5,
            deferred: vec![hash_object(b"mark-deferred")],
            journal_files: Vec::new(),
        }
    }

    fn sample_range_marker() -> ReclamationComplete {
        ReclamationComplete {
            kind: CompletionKind::Range,
            mk: hash_object(b"mark-mk"),
            batch: 0,
            source: hash_object(b"mark-plan-record"),
            mandatory_count: 0,
            deferred: Vec::new(),
            journal_files: vec!["journal.t1.2".to_owned()],
        }
    }

    #[test]
    fn the_marker_round_trips_both_kinds() {
        let batch = sample_batch_marker();
        let bytes = batch.canonical_bytes();
        let restored = ReclamationComplete::decode(&bytes).unwrap();
        assert_eq!(restored, batch);
        assert_eq!(restored.canonical_bytes(), bytes);
        let range = sample_range_marker();
        let bytes = range.canonical_bytes();
        let restored = ReclamationComplete::decode(&bytes).unwrap();
        assert_eq!(restored, range);
        assert_eq!(restored.canonical_bytes(), bytes);
    }

    #[test]
    fn the_marker_refuses_malformed_forms() {
        // The bad kind discriminant.
        let bytes = sample_batch_marker().canonical_bytes();
        let mut bad_kind = bytes.clone();
        let at = 8 + RECLAMATION_COMPLETE_DOMAIN.len() + 4;
        bad_kind[at] = 7;
        assert!(matches!(
            ReclamationComplete::decode(&bad_kind),
            Err(SaveError::Enum { .. })
        ));
        // The duplicate deferred identity.
        let mut marker = sample_batch_marker();
        let first = marker.deferred[0];
        marker.deferred.push(first);
        assert!(matches!(
            ReclamationComplete::decode(&marker.canonical_bytes()),
            Err(SaveError::Integrity(
                "reclamation-complete/duplicate-deferred"
            ))
        ));
        // Truncation and trailing (both kinds).
        for bytes in [
            sample_batch_marker().canonical_bytes(),
            sample_range_marker().canonical_bytes(),
        ] {
            for cut in 0..bytes.len() {
                assert!(ReclamationComplete::decode(&bytes[..cut]).is_err());
            }
            let mut trailing = bytes.clone();
            trailing.push(0);
            assert!(matches!(
                ReclamationComplete::decode(&trailing),
                Err(SaveError::Trailing { .. })
            ));
        }
        // A range marker's wire decoded with the batch path's
        // expectations fails (the kind gates the field layout).
        let range_bytes = sample_range_marker().canonical_bytes();
        let mut as_batch = range_bytes.clone();
        let at = 8 + RECLAMATION_COMPLETE_DOMAIN.len() + 4;
        as_batch[at] = 0;
        assert!(ReclamationComplete::decode(&as_batch).is_err());
    }

    #[test]
    fn the_ratified_keys_are_deterministic_and_distinct() {
        let mk = hash_object(b"key-mk");
        assert_eq!(
            reclamation_ledger_v2_idempotency_key(&mk, 0),
            reclamation_ledger_v2_idempotency_key(&mk, 0)
        );
        assert_ne!(
            reclamation_ledger_v2_idempotency_key(&mk, 0),
            reclamation_ledger_v2_idempotency_key(&mk, 1)
        );
        assert_ne!(
            reclamation_ledger_v2_idempotency_key(&mk, 0),
            reclamation_ledger_idempotency_key(&mk)
        );
        assert_eq!(
            reclamation_plan_idempotency_key(&mk),
            reclamation_plan_idempotency_key(&mk)
        );
        assert_ne!(
            reclamation_plan_idempotency_key(&mk),
            reclamation_ledger_v2_idempotency_key(&mk, 0)
        );
        assert_eq!(
            reclamation_complete_idempotency_key(&mk, Some(2)),
            reclamation_complete_idempotency_key(&mk, Some(2))
        );
        assert_ne!(
            reclamation_complete_idempotency_key(&mk, Some(2)),
            reclamation_complete_idempotency_key(&mk, None)
        );
    }

    #[test]
    fn the_ledger_family_recognition_covers_every_version() {
        assert!(is_reclamation_ledger_family(RECLAMATION_LEDGER_DOMAIN));
        assert!(is_reclamation_ledger_family(RECLAMATION_LEDGER_V2_DOMAIN));
        assert!(is_reclamation_ledger_family(
            "engine-persist/reclamation-ledger/3"
        ));
        assert!(!is_reclamation_ledger_family(
            "engine-persist/reclamation-plan/1"
        ));
        assert!(!is_reclamation_ledger_family(
            "engine-slice/session-snapshot/2"
        ));
    }

    // ------------------ the P1-F1/F2/F3 review corrective

    #[test]
    fn the_marker_deferred_set_exceeding_the_single_unit_bound_decodes() {
        // P1-F1: a batch's combined distinct deferred set may
        // legitimately exceed the SINGLE-UNIT object bound
        // (69,632) while its ledger stays within the ratified byte
        // ceiling — the marker's decoder bound is derived from the
        // LEDGER ceiling, so this valid terminal evidence decodes.
        let deferred: Vec<Hash> = (0..69_700)
            .map(|index| hash_object(format!("deferred-{index}").as_bytes()))
            .collect();
        let marker = ReclamationComplete {
            kind: CompletionKind::Batch,
            mk: hash_object(b"mark-mk"),
            batch: 0,
            source: hash_object(b"mark-ledger-record"),
            mandatory_count: 1,
            deferred,
            journal_files: Vec::new(),
        };
        let bytes = marker.canonical_bytes();
        let restored = ReclamationComplete::decode(&bytes).unwrap();
        assert_eq!(restored.deferred.len(), 69_700);
        assert_eq!(restored.canonical_bytes(), bytes);
    }

    #[test]
    fn the_marker_deferred_count_bound_refuses_the_derived_ceiling() {
        // The deferred-count bound itself: a declared count above
        // the ledger-derived ceiling refuses at the count (the
        // bounded read), before any identity is consumed.
        let mk = hash_object(b"mark-mk");
        let source = hash_object(b"mark-ledger-record");
        let mut enc = Encoder::new();
        enc.write_str(RECLAMATION_COMPLETE_DOMAIN);
        enc.write_u32(RECLAMATION_COMPLETE_VERSION);
        enc.write_u8(0);
        enc.write_bytes(mk.as_bytes());
        enc.write_u64(0);
        enc.write_bytes(source.as_bytes());
        enc.write_u64(0);
        enc.write_count((MAX_RECLAMATION_DEFERRED_IDENTITIES + 1) as usize);
        assert!(matches!(
            ReclamationComplete::decode(&enc.into_bytes()),
            Err(SaveError::Limit { .. })
        ));
    }

    #[test]
    fn the_zero_identity_refusals_cover_objects_and_deferred() {
        // P1-F2: the v2 deletion-object identities and the marker's
        // deferred identities refuse the all-zero identity (the v1
        // decoder is unchanged).
        let mut ledger = sample_v2_ledger();
        ledger.units[0].objects[0] = Hash::ZERO;
        assert!(matches!(
            ReclamationLedgerV2::decode(&ledger.canonical_bytes()),
            Err(SaveError::Integrity("reclamation-ledger-v2/zero-identity"))
        ));
        let mut marker = sample_batch_marker();
        marker.deferred[0] = Hash::ZERO;
        assert!(matches!(
            ReclamationComplete::decode(&marker.canonical_bytes()),
            Err(SaveError::Integrity("reclamation-complete/zero-identity"))
        ));
    }

    #[test]
    fn the_plan_aggregate_membership_is_bounded() {
        // P1-F3: the aggregate record count across ALL batches is
        // bounded by the checkpoint-index membership ceiling —
        // 65,538 records across two batches refuses.
        let over: Vec<Hash> = (0..32_769)
            .map(|index| hash_object(format!("over-{index}").as_bytes()))
            .collect();
        let plan = ReclaimPlan {
            mk: hash_object(b"plan-mk"),
            sk: hash_object(b"plan-sk"),
            range_digest: [1; 32],
            seed: [2; 32],
            ledger_byte_limit: 512 * 1024 * 1024,
            batches: vec![
                PlanBatch {
                    index: 0,
                    records: over.clone(),
                },
                PlanBatch {
                    index: 1,
                    records: over,
                },
            ],
            journal_files: Vec::new(),
        };
        assert!(matches!(
            ReclaimPlan::decode(&plan.canonical_bytes()),
            Err(SaveError::Integrity(
                "reclamation-plan/aggregate-membership"
            ))
        ));
    }

    #[test]
    fn the_empty_plan_refuses() {
        // P1-F3: a zero-batch plan is not a legitimate ratified
        // state — a real retirement has at least one record.
        let mk = hash_object(b"plan-mk");
        let sk = hash_object(b"plan-sk");
        let mut enc = Encoder::new();
        enc.write_str(RECLAMATION_PLAN_DOMAIN);
        enc.write_u32(RECLAMATION_PLAN_VERSION);
        enc.write_bytes(mk.as_bytes());
        enc.write_bytes(sk.as_bytes());
        enc.write_bytes(&[0u8; 32]);
        enc.write_bytes(&[0u8; 32]);
        enc.write_u64(512 * 1024 * 1024);
        enc.write_count(0);
        enc.write_count(0);
        assert!(matches!(
            ReclaimPlan::decode(&enc.into_bytes()),
            Err(SaveError::Integrity("reclamation-plan/empty-plan"))
        ));
    }
}
