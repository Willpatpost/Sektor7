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
    // The exact-width requirement (the OC-C1-E corrective):
    // read_bytes treats its argument as a MAXIMUM, so a shorter
    // declared length reaches the copy and would panic — a malformed
    // digest is a typed failure, never a panic.
    if bytes.len() != 32 {
        return Err(SaveError::Integrity("reclamation/digest-width"));
    }
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

/// The completion-marker domain gate for the GENERIC append (the
/// P2-F4 correction): a completion-marker object may enter the
/// chain ONLY through the admission-controlled
/// [`publish_reclamation_complete`] boundary — a syntactically
/// valid marker appended through the unrestricted generic path
/// refuses typed, so no caller can install trusted terminal
/// evidence merely by encoding correct bytes.
pub fn enforce_completion_marker_gate(objects: &[StoredObject]) -> Result<(), MediumError> {
    for object in objects {
        if object.domain() == RECLAMATION_COMPLETE_DOMAIN {
            return Err(MediumError::Chain {
                rule: "reclamation-complete/generic-append-refused",
            });
        }
    }
    Ok(())
}

// ------------------------------------- the OC-C1 ratified begin-record codec
//
// The owner-ratified durable execution-authorization evidence
// (engine-persist/reclamation-begin/1, the 206-byte canonical form).
// OC-C1 delivers the codec, the publication gate, and the reader
// support ONLY — the trusted admission, the execution-boundary
// enforcement, and the completion integration belong to the later
// separately authorized checkpoints.

/// The ratified begin-record domain (34 UTF-8 bytes).
pub const RECLAMATION_BEGIN_DOMAIN: &str = "engine-persist/reclamation-begin/1";

/// The ratified begin-record version.
pub const RECLAMATION_BEGIN_VERSION: u32 = 1;

/// The begin-family prefix: EVERY version in this family is refused
/// by the generic publication path (an unsupported future variant
/// can never ride it).
pub const RECLAMATION_BEGIN_FAMILY: &str = "engine-persist/reclamation-begin/";

/// The begin-record's deterministic key domain (one begin per MK,
/// ever — mirroring the audit and completion-marker disciplines).
pub const RECLAMATION_BEGIN_KEY_DOMAIN: &str = "engine-persist/reclamation-begin-key/1";

/// Whether a domain belongs to the reclamation-begin family (any
/// version).
pub fn is_reclamation_begin_family(domain: &str) -> bool {
    domain.starts_with(RECLAMATION_BEGIN_FAMILY)
}

/// The begin revision's deterministic idempotency key (derived from
/// the reclaimed MK).
pub fn reclamation_begin_idempotency_key(mk: &Hash) -> Vec<u8> {
    format!("{RECLAMATION_BEGIN_KEY_DOMAIN}:{}", mk.hex()).into_bytes()
}

/// The ratified durable execution-authorization evidence: the exact
/// MK, the installed source-audit identity (transitively the whole
/// hash-verified ledger), the verified range digest, and the
/// founding-derived seed — the bindings a future trusted admission
/// constructs from verified values and the recovery reader
/// re-establishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclamationBegin {
    /// The retired checkpoint's key-record identity.
    pub mk: Hash,
    /// The installed v1 audit's revision identity.
    pub source: Hash,
    /// The recomputed-and-matched range digest.
    pub range_digest: [u8; 32],
    /// The founding-derived seed.
    pub seed: [u8; 32],
}

impl ReclamationBegin {
    /// The canonical 206-byte encoding (the ratified layout: the
    /// u64-length-prefixed 34-byte domain, the u32-LE version, and
    /// four u64-length-prefixed 32-byte fields).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_str(RECLAMATION_BEGIN_DOMAIN);
        enc.write_u32(RECLAMATION_BEGIN_VERSION);
        enc.write_bytes(self.mk.as_bytes());
        enc.write_bytes(self.source.as_bytes());
        enc.write_bytes(&self.range_digest);
        enc.write_bytes(&self.seed);
        enc.into_bytes()
    }

    /// The strict decode: the exact domain and version, the exact
    /// hash and digest widths, the zero-identity refusal, and the
    /// trailing-data refusal — every malformed input is a typed
    /// failure, never a panic.
    pub fn decode(bytes: &[u8]) -> Result<Self, SaveError> {
        const SECTION: &str = "reclamation-begin";
        let mut dec = Decoder::new(bytes);
        dec.read_domain(RECLAMATION_BEGIN_DOMAIN)?;
        let version = dec.read_u32()?;
        if version != RECLAMATION_BEGIN_VERSION {
            return Err(SaveError::Version {
                observed: version,
                supported: RECLAMATION_BEGIN_VERSION,
            });
        }
        let read_begin_hash = |dec: &mut Decoder| -> Result<Hash, SaveError> {
            let bytes = dec.read_bytes(Hash::WIDTH, SECTION)?;
            Hash::from_bytes(bytes).ok_or(SaveError::Integrity("reclamation-begin/hash-width"))
        };
        let mk = read_begin_hash(&mut dec)?;
        let source = read_begin_hash(&mut dec)?;
        if mk == Hash::ZERO || source == Hash::ZERO {
            return Err(SaveError::Integrity("reclamation-begin/zero-identity"));
        }
        let range_digest = read_digest(&mut dec, SECTION)?;
        let seed = read_digest(&mut dec, SECTION)?;
        dec.finish()?;
        Ok(Self {
            mk,
            source,
            range_digest,
            seed,
        })
    }
}

/// The begin-family publication gate: NO version of
/// engine-persist/reclamation-begin/ may enter the chain through the
/// unrestricted generic append — the begin is trusted
/// execution-authorization evidence, installable only through the
/// future OC-C2 in-crate admission after the full first-execution
/// verification. A valid codec alone never grants publication
/// authority.
pub fn enforce_reclamation_begin_gate(objects: &[StoredObject]) -> Result<(), MediumError> {
    for object in objects {
        if is_reclamation_begin_family(object.domain()) {
            return Err(MediumError::Chain {
                rule: "reclamation-begin/generic-append-refused",
            });
        }
    }
    Ok(())
}

/// The TEST-ONLY begin-record install (the OC-C1 reader-support
/// regressions): publishes a caller-constructed begin record through
/// the in-crate admitted append, BYPASSING the future admission's
/// verification — exactly the corrupted-in-crate-path threat the
/// OC-C3/OC-C4 authority checks must catch. COMPILE-TIME ISOLATED:
/// exists only under the composite-test-fixture feature (never in a
/// production build; the CI's feature-disabled artifact check
/// enforces the absence).
#[cfg(feature = "composite-test-fixture")]
#[doc(hidden)]
pub fn test_only_install_begin_record(
    medium: &mut dyn CanonicalMedium,
    begin: &ReclamationBegin,
    tick: u64,
) -> Result<Hash, MediumError> {
    let head = medium
        .head()?
        .ok_or(MediumError::Chain { rule: "unfounded" })?;
    let parent_epoch = medium.read_record(&head)?.epoch;
    let key = reclamation_begin_idempotency_key(&begin.mk);
    let object = StoredObject::new(begin.canonical_bytes())?;
    let record = RevisionRecord::regular(head, parent_epoch, tick)
        .with_idempotency_key(key)
        .with_content_refs(vec![object.hash()]);
    let admitted = crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(&object));
    medium.append_admitted(admitted, Some(head))?;
    Ok(record.id())
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

/// The authenticated reclamation directive (K2-C2B P2): the audit
/// revision's identity ON THE AUTHORITATIVE CHAIN plus the caller's
/// candidate manifest. The provider re-reads and strictly decodes
/// the ledger UNDER its coordination boundary, cross-checks the
/// supplied units against the decoded manifest (any mismatch
/// refuses typed before any deletion), derives the virtual-exclusion
/// set from the decoded unit RECORDS only, verifies the audit's
/// chain membership and MK-bound idempotency key, and executes the
/// two-pass schedule against the survivor partition. A caller-
/// supplied unit list is never independent authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimDirective {
    /// The audit revision's identity on the authoritative chain.
    pub audit_record: Hash,
    /// The caller's candidate deletion manifest.
    pub units: Vec<ReclaimUnit>,
}

/// The partitioned reclamation outcome: the mandatory identities
/// deleted (the unit records plus the objects no surviving root
/// requires), the deferred identities authorized to survive
/// (shared with surviving roots — the visible debt the completion
/// marker records), and the raw removal count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimOutcome {
    pub removed: u64,
    pub mandatory: Vec<Hash>,
    pub deferred: Vec<Hash>,
}

/// The survivor-aware Mandatory/Deferred partition (the trusted
/// directive's decision core): given the verified audit's units and
/// the SURVIVOR SET — the exclusion walk's reachable identities,
/// everything that must remain reachable after the authorized
/// retirement — classify every audited identity. Identity-set
/// semantics throughout: a shared object appearing in multiple
/// units classifies exactly once (deleted at most once), and an
/// object referenced by ANY surviving root is DEFERRED regardless
/// of how many retiring units name it. The retiring records
/// themselves are mandatory (the audit's exact membership
/// authorizes their removal; the exclusion walk never reaches
/// them). The result is reproducible from verified canonical
/// evidence after an interruption (the same units and the same
/// walk derive the same partition).
pub fn partition_reclaim(
    units: &[ReclaimUnit],
    survivors: &std::collections::BTreeSet<Hash>,
) -> Result<ReclaimOutcome, MediumError> {
    // The defensive invariant (the provider-execution review): a
    // retiring-record identity that ALSO appears in the survivor set
    // is an internal inconsistency — the authenticated exclusion
    // never reaches the audited records, so an overlap means the
    // inputs disagree. Refuse before deletion; never silently
    // override reachability.
    for unit in units {
        if survivors.contains(&unit.record) {
            return Err(MediumError::Chain {
                rule: "reclaim-partition/record-survivor-overlap",
            });
        }
    }
    let mut mandatory: Vec<Hash> = Vec::new();
    let mut deferred: Vec<Hash> = Vec::new();
    let mut seen: std::collections::BTreeSet<Hash> = std::collections::BTreeSet::new();
    for unit in units {
        if seen.insert(unit.record) {
            mandatory.push(unit.record);
        }
        for object in &unit.objects {
            if survivors.contains(object) {
                if seen.insert(*object) {
                    deferred.push(*object);
                }
            } else if seen.insert(*object) {
                mandatory.push(*object);
            }
        }
    }
    Ok(ReclaimOutcome {
        removed: 0,
        mandatory,
        deferred,
    })
}

/// The E2 retirement-order verification: the persisted unit
/// sequence must be strictly DESCENDING by indexed epoch (the
/// newest-first retirement order — the crossing's Phase A
/// collection order). The provider never trusts the caller's
/// construction: an oldest-first or unsorted ledger refuses before
/// any mutation (the interrupted-prefix recovery property depends
/// on the order).
pub fn verify_newest_first_order(
    units: &[ReclaimUnit],
    metadata: &crate::checkpoint::CheckpointMetadata,
) -> Result<(), MediumError> {
    let mut previous: Option<u64> = None;
    for unit in units {
        let Some(entry) = metadata.index.iter().find(|e| e.hash == unit.record) else {
            return Err(MediumError::Chain {
                rule: "reclaim/unit-not-indexed",
            });
        };
        if let Some(previous_epoch) = previous
            && entry.epoch >= previous_epoch
        {
            return Err(MediumError::Chain {
                rule: "reclaim/unit-order-not-newest-first",
            });
        }
        previous = Some(entry.epoch);
    }
    Ok(())
}

/// The E1 founding-derived seed verification: the checkpoint's
/// stored seed must equal `compute_digest_seed(founding)` — the
/// campaign's permanent founding identity (never reclaimed; the
/// check is computable on BOTH the first execution and the resume).
/// A fabricated seed value refuses.
pub fn verify_founding_seed(
    metadata: &crate::checkpoint::CheckpointMetadata,
    founding: &Hash,
) -> Result<(), MediumError> {
    let expected = crate::checkpoint::compute_digest_seed(founding);
    if metadata.seed != expected {
        return Err(MediumError::Chain {
            rule: "reclaim/seed-not-founding-derived",
        });
    }
    Ok(())
}

/// Publishes the completion marker revision: a regular revision
/// whose single content object is the marker, keyed
/// deterministically by the marker's KIND (the batch form by the MK
/// and batch index — P2-F3: the range-terminal form by the MK and
/// the `range` tag; the two namespaces never collide), and CAS-bound
/// to the EXACT expected parent (P2-F2: the HEAD under which the
/// completion obligations were verified — an advancement between
/// validation and publication surfaces `RejectedStaleParent`, never
/// a silently rebased marker; the caller rederives the completion
/// conditions against the new HEAD before retrying). Returns the
/// candidate (for the caller's Unknown reconciliation) and the
/// outcome — the same shape as the audit's own publication. A
/// duplicate-key result is never alone proof of completion: the
/// caller must discover and verify the existing marker's contents
/// and underlying obligations.
pub fn publish_reclamation_complete(
    medium: &mut dyn CanonicalMedium,
    marker: &ReclamationComplete,
    expected_parent: Hash,
    tick: u64,
) -> Result<(Hash, AppendOutcome), MediumError> {
    // THE ADMISSION (P2-F4): a completion marker may become trusted
    // retirement evidence ONLY through this boundary — the generic
    // append refuses the marker domain typed, and this admission
    // verifies the marker's semantic authorization BEFORE the
    // receipt: the source audit must be an installed, keyed,
    // MK-bound ledger revision. A syntactically valid marker with a
    // nonexistent, orphaned, mismatched, or unkeyed source refuses
    // typed with nothing mutated.
    if marker.kind == CompletionKind::Range {
        // The range-terminal protocol belongs to the separately
        // authorized later package — it cannot be used to claim
        // completion under the v1 protocol.
        return Err(MediumError::Chain {
            rule: "reclamation-complete/range-unauthorized",
        });
    }
    // P2-F4C: the v1-only execution boundary — the admitted source
    // is a v1 audit, the batch index is zero (the v1 single-batch
    // interpretation), and the v2 form is refused until the
    // separately authorized v2 protocol exists.
    if marker.batch != 0 {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/batch-nonzero",
        });
    }
    // P2-F4B: the CANONICAL AUDIT AUTHORITY — not merely an
    // audit-shaped record: the source must be an installed v1 audit
    // ON THE AUTHORITATIVE CHAIN whose MK is a real checkpoint,
    // whose bindings agree with the permanent metadata, and whose
    // units exactly match the checkpoint's retirement index —
    // verified in the RECOVERY-SAFE form (the completed
    // retirement's records are absent by construction).
    let (ledger, _metadata) = verify_installed_v1_audit(medium, &marker.source, &marker.mk)?;
    // P2-F4: the verified-completion RECEIPT — the marker's
    // certified obligations are checked against the ACTUAL
    // post-execution state, never the caller's claim: every
    // retiring record absent, the partition recomputed from the
    // surviving chain, every mandatory identity absent, every
    // deferred identity present and hash-verified.
    let (mandatory_count, deferred) = verify_completion_state(medium, &ledger)?;
    if marker.mandatory_count != mandatory_count {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/mandatory-count-mismatch",
        });
    }
    if marker
        .deferred
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<Hash>>()
        != deferred
    {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/deferred-mismatch",
        });
    }
    // The provider-internal residue (the quarantine forms): the
    // mandatory-empty postcondition across every authenticated
    // storage form — the advisory probe's observation, bound into
    // the admission.
    let directive = ReclaimDirective {
        audit_record: marker.source,
        units: ledger.units.clone(),
    };
    if medium.reclaim_pending(&directive)? {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/mandatory-residue",
        });
    }
    // The CAS-bound publication: the receipt is constructed
    // IN-CRATE (the generic gate does not apply — this admission IS
    // the authority), the append's expected parent binding to the
    // exact verified HEAD.
    let bytes = marker.canonical_bytes();
    let object = StoredObject::new(bytes)?;
    let parent_epoch = medium.read_record(&expected_parent)?.epoch;
    let key = reclamation_complete_idempotency_key(&marker.mk, Some(marker.batch));
    let record = RevisionRecord::regular(expected_parent, parent_epoch, tick)
        .with_idempotency_key(key)
        .with_content_refs(vec![object.hash()]);
    let candidate = record.id();
    let admitted = crate::medium::AdmittedAppend::generic(&record, &[object]);
    let outcome = medium.append_admitted(admitted, Some(expected_parent))?;
    Ok((candidate, outcome))
}

/// The canonical v1 audit authority verification (P2 Increment A,
/// corrected per A1-A4): a gap-aware authoritative descent proving
/// (A1) the EXACT supplied audit revision identity is encountered on
/// the chain — its computed identity, its first content object's
/// strict v1-ledger decode, its MK, and its deterministic MK-bound
/// key all checked AT that record (an unrelated or fabricated
/// source refuses even when a valid audit for the MK exists);
/// (A2) the requested MK is bound BY REVISION IDENTITY — the record
/// whose identity equals the MK, whose first reference strictly
/// decodes as checkpoint metadata whose SK is that record's parent
/// (a newer checkpoint's metadata never substitutes); (A3) the
/// descent crosses legitimately reclaimed gaps ONLY under the most
/// recent authenticated checkpoint's skip table (the established
/// discovery discipline — uncovered gaps refuse fail-closed; the
/// descent bound is a documented safety valve, not a schema limit);
/// and (A4) the pre-deletion manifest authority — every unit's
/// record is hash-verified by identity and its object list equals
/// the record's actual content references (the v1 collection
/// contract; omitted, extraneous, or mismatched deletion identities
/// refuse). Post-interruption resume verification — where retiring
/// records may already be deleted — is the manifest-driven
/// recovery's own path, never this pre-deletion boundary.
pub fn verify_canonical_v1_audit(
    medium: &mut dyn CanonicalMedium,
    audit_record: &Hash,
    mk: &Hash,
) -> Result<(ReclamationLedger, crate::checkpoint::CheckpointMetadata), MediumError> {
    // The safety-valve descent bound: each iteration consumes one
    // present record or one authenticated skip jump; a campaign
    // exceeding this refuses typed (the documented operational
    // limit — the same policy class as the survivor-walk bound,
    // pending the owner's numerical ratification).
    const MAX_DESCENT: u64 = 1 << 20;
    let mut current = medium
        .head()?
        .ok_or(MediumError::Chain { rule: "unfounded" })?;
    let mut ledger: Option<ReclamationLedger> = None;
    let mut metadata: Option<crate::checkpoint::CheckpointMetadata> = None;
    // (A3) The traversal skip table: the most recent checkpoint
    // metadata seen on the descent (ANY checkpoint — the gap-cover
    // authority), distinct from the requested MK's own metadata.
    let mut skip_table: Option<crate::checkpoint::CheckpointMetadata> = None;
    for _ in 0..MAX_DESCENT {
        let record = medium.read_record(&current)?;
        let parent = record.parent;
        if let Some(object) = record.content_refs.first().copied() {
            let bytes = medium.read_object(&object)?;
            let domain = StoredObject::new(bytes.clone())?.domain().to_owned();
            if domain == crate::checkpoint::CHECKPOINT_METADATA_DOMAIN {
                let decoded = crate::checkpoint::CheckpointMetadata::decode(&bytes)
                    .map_err(MediumError::Format)?;
                skip_table = Some(decoded.clone());
                // (A2) The requested MK, bound by revision identity:
                // this record IS the MK, its metadata's SK is its
                // own parent.
                if current == *mk {
                    if decoded.sk != parent.unwrap_or(Hash::ZERO) {
                        return Err(MediumError::Chain {
                            rule: "reclamation-complete/mk-binding-mismatch",
                        });
                    }
                    metadata = Some(decoded);
                }
            } else if domain == RECLAMATION_LEDGER_DOMAIN && current == *audit_record {
                // (A1) The EXACT requested audit identity.
                let decoded = ReclamationLedger::decode(&bytes).map_err(MediumError::Format)?;
                if decoded.mk != *mk {
                    return Err(MediumError::Chain {
                        rule: "reclamation-complete/mk-mismatch",
                    });
                }
                let expected = reclamation_ledger_idempotency_key(mk);
                if record.idempotency_key != Some(expected) {
                    return Err(MediumError::Chain {
                        rule: "reclamation-complete/audit-key-mismatch",
                    });
                }
                ledger = Some(decoded);
            }
        }
        if ledger.is_some() && metadata.is_some() {
            break;
        }
        let Some(parent) = parent else {
            break;
        };
        // (A3) The gap-aware advance: a present parent follows
        // normally; an absent parent crosses ONLY under the most
        // recent authenticated skip table (the owner/self-skip
        // distinction preserved); an uncovered gap refuses.
        match medium.read_record(&parent) {
            Ok(_) => current = parent,
            Err(MediumError::MissingObject { .. }) => {
                let Some(table) = skip_table.as_ref() else {
                    return Err(MediumError::MissingObject { hash: parent });
                };
                let target = if table.sk == parent {
                    // The skip owner's own parent (the SK) is
                    // absent: the self-skip names the owner's
                    // nearest retained ancestor.
                    table
                        .self_skip()
                        .map(|entry| entry.nearest_retained_ancestor)
                } else {
                    table
                        .skip_for(&current)
                        .map(|entry| entry.nearest_retained_ancestor)
                };
                let Some(target) = target else {
                    return Err(MediumError::MissingObject { hash: parent });
                };
                current = target;
            }
            Err(other) => return Err(other),
        }
    }
    let Some(ledger) = ledger else {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/audit-not-canonical",
        });
    };
    let Some(metadata) = metadata else {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/mk-not-a-checkpoint",
        });
    };
    // The bindings: the ledger's SK, digest, and seed agree with the
    // requested MK's permanent metadata.
    if ledger.sk != metadata.sk
        || ledger.range_digest != metadata.range_digest
        || ledger.seed != metadata.seed
    {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/binding-mismatch",
        });
    }
    // The units exactly match the retirement index (the record
    // identities, as a set — no omission, no extraneous entry).
    let index: std::collections::BTreeSet<Hash> =
        metadata.index.iter().map(|entry| entry.hash).collect();
    let units: std::collections::BTreeSet<Hash> =
        ledger.units.iter().map(|unit| unit.record).collect();
    if index.len() != metadata.index.len() || units.len() != ledger.units.len() {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/index-incoherent",
        });
    }
    if index != units {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/units-index-mismatch",
        });
    }
    // (A4) The pre-deletion manifest authority: every unit's record
    // is hash-verified by identity and its object list equals the
    // record's actual content references (the v1 collection
    // contract — a deletion manifest that omits or adds objects
    // refuses).
    for unit in &ledger.units {
        // (B1) The provider's RECORD-specific hash-verified read —
        // the cross-provider interface (SQLite stores revision
        // records in their own table; read_object is the
        // content-object namespace and would refuse a valid SQLite
        // audit whose records are absent from the objects table).
        // Both providers' read_record implementations load and
        // hash-verify the canonical record bytes.
        let record = medium.read_record(&unit.record)?;
        if unit.objects != record.content_refs {
            return Err(MediumError::Chain {
                rule: "reclamation-complete/units-objects-mismatch",
            });
        }
    }
    Ok((ledger, metadata))
}

// ------------------------------------------ the trusted completion (P2-F4)
//
// The verified completion receipt: the marker installed ONLY
// through [`publish_reclamation_complete`] — the in-crate admission
// (the generic append refuses the completion domain; the
// `AdmittedAppend` receipt is `pub(crate)`) — whose verification
// establishes the ACTUAL retirement state, never the caller's
// claim. The trusted consumer re-establishes the structural
// authority for readers; the admission-time state verification is
// the gate a marker-shaped object cannot bypass.

/// The founding revision's identity: the gap-aware descent from the
/// authoritative HEAD to the parentless record (the completion
/// verification's seed derivation — the founding is permanent, so
/// this is always computable).
fn founding_of(medium: &mut dyn CanonicalMedium) -> Result<Hash, MediumError> {
    const MAX_DESCENT: u64 = 1 << 20;
    let mut current = medium
        .head()?
        .ok_or(MediumError::Chain { rule: "unfounded" })?;
    let mut skip_table: Option<crate::checkpoint::CheckpointMetadata> = None;
    for _ in 0..MAX_DESCENT {
        let record = medium.read_record(&current)?;
        if let Some(object) = record.content_refs.first().copied() {
            let bytes = medium.read_object(&object)?;
            if StoredObject::new(bytes.clone())?.domain()
                == crate::checkpoint::CHECKPOINT_METADATA_DOMAIN
            {
                skip_table = Some(
                    crate::checkpoint::CheckpointMetadata::decode(&bytes)
                        .map_err(MediumError::Format)?,
                );
            }
        }
        let Some(parent) = record.parent else {
            return Ok(current);
        };
        match medium.read_record(&parent) {
            Ok(_) => current = parent,
            Err(MediumError::MissingObject { .. }) => {
                let Some(table) = skip_table.as_ref() else {
                    return Err(MediumError::MissingObject { hash: parent });
                };
                let target = if table.sk == parent {
                    table
                        .self_skip()
                        .map(|entry| entry.nearest_retained_ancestor)
                } else {
                    table
                        .skip_for(&current)
                        .map(|entry| entry.nearest_retained_ancestor)
                };
                let Some(target) = target else {
                    return Err(MediumError::MissingObject { hash: parent });
                };
                current = target;
            }
            Err(other) => return Err(other),
        }
    }
    Err(MediumError::Chain {
        rule: "reclamation-complete/descent-bound",
    })
}

/// The recovery-safe audit authentication: the same descent,
/// bindings, and units-vs-index membership as
/// [`verify_canonical_v1_audit`] — but WITHOUT the pre-deletion
/// requirement that every unit's record is present. The completed
/// retirement's records are absent BY CONSTRUCTION; the absent set
/// must hold the (E1) deletion-prefix shape (an absent unit after a
/// present one is missing storage, never proof of a prior
/// authorized deletion), and the range digest is recomputed
/// whenever every digest member is present. A present unit's
/// manifest is still verified against its actual content
/// references.
pub fn verify_installed_v1_audit(
    medium: &mut dyn CanonicalMedium,
    audit_record: &Hash,
    mk: &Hash,
) -> Result<(ReclamationLedger, crate::checkpoint::CheckpointMetadata), MediumError> {
    const MAX_DESCENT: u64 = 1 << 20;
    let mut current = medium
        .head()?
        .ok_or(MediumError::Chain { rule: "unfounded" })?;
    let mut ledger: Option<ReclamationLedger> = None;
    let mut metadata: Option<crate::checkpoint::CheckpointMetadata> = None;
    let mut skip_table: Option<crate::checkpoint::CheckpointMetadata> = None;
    for _ in 0..MAX_DESCENT {
        let record = medium.read_record(&current)?;
        let parent = record.parent;
        if let Some(object) = record.content_refs.first().copied() {
            let bytes = medium.read_object(&object)?;
            let domain = StoredObject::new(bytes.clone())?.domain().to_owned();
            if domain == crate::checkpoint::CHECKPOINT_METADATA_DOMAIN {
                let decoded = crate::checkpoint::CheckpointMetadata::decode(&bytes)
                    .map_err(MediumError::Format)?;
                skip_table = Some(decoded.clone());
                if current == *mk {
                    if decoded.sk != parent.unwrap_or(Hash::ZERO) {
                        return Err(MediumError::Chain {
                            rule: "reclamation-complete/mk-binding-mismatch",
                        });
                    }
                    metadata = Some(decoded);
                }
            } else if domain == RECLAMATION_LEDGER_DOMAIN && current == *audit_record {
                let decoded = ReclamationLedger::decode(&bytes).map_err(MediumError::Format)?;
                if decoded.mk != *mk {
                    return Err(MediumError::Chain {
                        rule: "reclamation-complete/mk-mismatch",
                    });
                }
                let expected = reclamation_ledger_idempotency_key(mk);
                if record.idempotency_key != Some(expected) {
                    return Err(MediumError::Chain {
                        rule: "reclamation-complete/audit-key-mismatch",
                    });
                }
                ledger = Some(decoded);
            }
        }
        if ledger.is_some() && metadata.is_some() {
            break;
        }
        let Some(parent) = parent else {
            break;
        };
        match medium.read_record(&parent) {
            Ok(_) => current = parent,
            Err(MediumError::MissingObject { .. }) => {
                let Some(table) = skip_table.as_ref() else {
                    return Err(MediumError::MissingObject { hash: parent });
                };
                let target = if table.sk == parent {
                    table
                        .self_skip()
                        .map(|entry| entry.nearest_retained_ancestor)
                } else {
                    table
                        .skip_for(&current)
                        .map(|entry| entry.nearest_retained_ancestor)
                };
                let Some(target) = target else {
                    return Err(MediumError::MissingObject { hash: parent });
                };
                current = target;
            }
            Err(other) => return Err(other),
        }
    }
    let Some(ledger) = ledger else {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/audit-not-canonical",
        });
    };
    let Some(metadata) = metadata else {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/mk-not-a-checkpoint",
        });
    };
    if ledger.sk != metadata.sk
        || ledger.range_digest != metadata.range_digest
        || ledger.seed != metadata.seed
    {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/binding-mismatch",
        });
    }
    let index: std::collections::BTreeSet<Hash> =
        metadata.index.iter().map(|entry| entry.hash).collect();
    let units: std::collections::BTreeSet<Hash> =
        ledger.units.iter().map(|unit| unit.record).collect();
    if index.len() != metadata.index.len() || units.len() != ledger.units.len() {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/index-incoherent",
        });
    }
    if index != units {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/units-index-mismatch",
        });
    }
    // (E2) The newest-first order — the prefix property below
    // depends on it.
    verify_newest_first_order(&ledger.units, &metadata)?;
    // The founding-derived seed: ALWAYS verifiable (the founding is
    // permanent — the descent crosses the reclaimed gaps under the
    // authenticated skip tables).
    verify_founding_seed(&metadata, &founding_of(medium)?)?;
    // The (E1) recovery boundary + the digest recomputation: the
    // present units verify their manifests; the absent set must be
    // a deletion prefix; every digest member present triggers the
    // recomputation.
    let mut present_seen = false;
    let mut all_present = true;
    for unit in &ledger.units {
        match medium.read_record(&unit.record) {
            Ok(record) => {
                present_seen = true;
                if unit.objects != record.content_refs {
                    return Err(MediumError::Chain {
                        rule: "reclamation-complete/units-objects-mismatch",
                    });
                }
            }
            Err(MediumError::MissingObject { .. }) => {
                if present_seen {
                    return Err(MediumError::Chain {
                        rule: "reclaim/absence-not-deletion-prefix",
                    });
                }
                if unit.record != ledger.sk {
                    all_present = false;
                }
            }
            Err(other) => return Err(other),
        }
    }
    if all_present {
        let mut digest_members: Vec<(u64, Hash)> = ledger
            .units
            .iter()
            .filter(|unit| unit.record != ledger.sk)
            .filter_map(|unit| {
                metadata
                    .index
                    .iter()
                    .find(|e| e.hash == unit.record)
                    .map(|e| (e.epoch, unit.record))
            })
            .collect();
        digest_members.sort_unstable_by_key(|(epoch, _)| *epoch);
        let mut range_bytes = Vec::with_capacity(digest_members.len());
        for (_, record) in digest_members {
            range_bytes.push(medium.read_record(&record)?.canonical_bytes());
        }
        let recomputed = crate::checkpoint::chain_range_digest(&metadata.seed, &range_bytes);
        if recomputed != ledger.range_digest {
            return Err(MediumError::Chain {
                rule: "reclaim/range-digest-mismatch",
            });
        }
    }
    Ok((ledger, metadata))
}

/// The trait-level survivor object set: the gap-aware cold walk
/// from the authoritative HEAD to the founding, EXCLUDING the
/// retiring records — the partition input for the completion
/// verification. The closure is PARITY-EXACT with the providers'
/// authenticated exclusion walks (P2-R3): every walked record's
/// identity, every content object it references, AND the VERIFIED
/// transitive composite closure — a surviving /2 snapshot root's
/// chunks are reachable through its manifest (each dependency
/// hash-verified and envelope-verified against its manifest entry
/// before it joins the set; a missing, corrupt, or malformed chunk
/// fails the walk closed).
pub fn survivor_objects(
    medium: &mut dyn CanonicalMedium,
    exclusion: &std::collections::BTreeSet<Hash>,
) -> Result<std::collections::BTreeSet<Hash>, MediumError> {
    const MAX_DESCENT: u64 = 1 << 20;
    let mut current = medium
        .head()?
        .ok_or(MediumError::Chain { rule: "unfounded" })?;
    let mut reachable = std::collections::BTreeSet::new();
    let mut skip_table: Option<crate::checkpoint::CheckpointMetadata> = None;
    for _ in 0..MAX_DESCENT {
        let record = medium.read_record(&current)?;
        if !exclusion.contains(&current) {
            reachable.insert(current);
            for reference in &record.content_refs {
                let bytes = medium.read_object(reference)?;
                reachable.insert(*reference);
                // The composite-closure expansion: a record whose
                // first reference is a /2 snapshot manifest carries
                // its chunks transitively — the same verified
                // semantics as the providers' walks.
                if reference == record.content_refs.first().unwrap()
                    && StoredObject::new(bytes.clone())?.domain()
                        == crate::composite_manifest::SESSION_SNAPSHOT_V2_DOMAIN
                {
                    let manifest =
                        crate::composite_manifest::decode(&bytes).map_err(MediumError::Format)?;
                    for entry in &manifest.entries {
                        let chunk_bytes = medium.read_object(&entry.identity)?;
                        crate::composite_append::verify_chunk_dependency(
                            &chunk_bytes,
                            &entry.identity,
                            &manifest,
                        )
                        .map_err(|_error| MediumError::Chain {
                            rule: "composite-closure-verification",
                        })?;
                        reachable.insert(entry.identity);
                    }
                }
            }
            if let Some(object) = record.content_refs.first().copied() {
                let bytes = medium.read_object(&object)?;
                if StoredObject::new(bytes.clone())?.domain()
                    == crate::checkpoint::CHECKPOINT_METADATA_DOMAIN
                {
                    skip_table = Some(
                        crate::checkpoint::CheckpointMetadata::decode(&bytes)
                            .map_err(MediumError::Format)?,
                    );
                }
            }
        }
        let Some(parent) = record.parent else {
            return Ok(reachable);
        };
        match medium.read_record(&parent) {
            Ok(_) => current = parent,
            Err(MediumError::MissingObject { .. }) => {
                let Some(table) = skip_table.as_ref() else {
                    return Err(MediumError::MissingObject { hash: parent });
                };
                let target = if table.sk == parent {
                    table
                        .self_skip()
                        .map(|entry| entry.nearest_retained_ancestor)
                } else {
                    table
                        .skip_for(&current)
                        .map(|entry| entry.nearest_retained_ancestor)
                };
                let Some(target) = target else {
                    return Err(MediumError::MissingObject { hash: parent });
                };
                current = target;
            }
            Err(other) => return Err(other),
        }
    }
    Err(MediumError::Chain {
        rule: "reclamation-complete/descent-bound",
    })
}

/// The completion postconditions over the live medium: every
/// retiring record absent, the partition recomputed from the
/// surviving chain, every mandatory identity absent, and every
/// deferred identity present and hash-verified. Returns the
/// verified (mandatory count, deferred set) the marker must carry.
pub fn verify_completion_state(
    medium: &mut dyn CanonicalMedium,
    ledger: &ReclamationLedger,
) -> Result<(u64, std::collections::BTreeSet<Hash>), MediumError> {
    for unit in &ledger.units {
        match medium.read_record(&unit.record) {
            Err(MediumError::MissingObject { .. }) => {}
            Ok(_) => {
                return Err(MediumError::Chain {
                    rule: "reclamation-complete/record-still-present",
                });
            }
            Err(other) => return Err(other),
        }
    }
    let exclusion: std::collections::BTreeSet<Hash> =
        ledger.units.iter().map(|unit| unit.record).collect();
    let survivors = survivor_objects(medium, &exclusion)?;
    let partition = partition_reclaim(&ledger.units, &survivors)?;
    for identity in &partition.mandatory {
        match medium.read_object(identity) {
            Err(MediumError::MissingObject { .. }) => {}
            Ok(_) => {
                return Err(MediumError::Chain {
                    rule: "reclamation-complete/mandatory-residue",
                });
            }
            Err(other) => return Err(other),
        }
    }
    for identity in &partition.deferred {
        // A deferred identity is a required surviving object: its
        // absence is a postcondition violation (the shared evidence
        // the marker certifies as preserved).
        medium.read_object(identity)?;
    }
    Ok((
        partition.mandatory.len() as u64,
        partition.deferred.iter().copied().collect(),
    ))
}

/// The trusted completion-receipt path: verifies the recovery-safe
/// audit authority and the ACTUAL postconditions, constructs the
/// marker FROM the verified state, and installs it through the
/// CAS-bound admission with the full reconciliation (the stale
/// parent revalidated against the fresh HEAD; the Unknown outcome
/// reconciled; the duplicate deterministic key resolved by
/// discovering and EXACTLY verifying the installed marker — never
/// inferring success from the key alone).
///
/// (P2-R4) The completion-publication coordination: the marker's
/// LIVE certified claims — every retiring record absent, every
/// mandatory identity absent — are STABLE under every legal
/// concurrent storage operation (the only mutations are deletions
/// by authenticated reclamation; a deletion never falsifies an
/// absence claim), so the multi-read verification sequence plus the
/// CAS append cannot publish a marker whose live claims are false
/// at installation, even when another maintenance operation runs
/// without HEAD advancement. The deferred list is the batch's
/// AUTHORIZATION record (what this audit classified as shared and
/// authorized to preserve): its admission-time presence check
/// either passes on a consistent observation or fails closed
/// (retryable — the retry recomputes against the then-current
/// state); a concurrent later authorized retirement that consumes a
/// deferred object does not falsify the authorization, and the
/// consumer's later-evidence relaxation recognizes exactly that
/// case.
pub fn complete_reclamation(
    medium: &mut dyn CanonicalMedium,
    source: &Hash,
    mk: &Hash,
) -> Result<(Hash, ReclamationComplete), MediumError> {
    let (ledger, _metadata) = verify_installed_v1_audit(medium, source, mk)?;
    let (mandatory_count, deferred) = verify_completion_state(medium, &ledger)?;
    // The provider-internal residue (the quarantine forms) — the
    // mandatory-empty postcondition across every authenticated
    // storage form.
    let directive = ReclaimDirective {
        audit_record: *source,
        units: ledger.units.clone(),
    };
    if medium.reclaim_pending(&directive)? {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/mandatory-residue",
        });
    }
    let marker = ReclamationComplete {
        kind: CompletionKind::Batch,
        mk: *mk,
        batch: 0,
        source: *source,
        mandatory_count,
        deferred: deferred.into_iter().collect(),
        journal_files: Vec::new(),
    };
    for attempt in 0..2 {
        let head = medium
            .head()?
            .ok_or(MediumError::Chain { rule: "unfounded" })?;
        let tick = medium.read_record(&head)?.tick.saturating_add(1);
        let outcome = publish_reclamation_complete(medium, &marker, head, tick);
        match outcome {
            Ok((id, crate::medium::AppendOutcome::Accepted { .. })) => return Ok((id, marker)),
            Ok((_, crate::medium::AppendOutcome::RejectedStaleParent { .. })) => {
                if attempt == 1 {
                    return Err(MediumError::Chain {
                        rule: "reclamation-complete/stale-unresolved",
                    });
                }
                // The fresh revalidation: the next iteration re-reads
                // the authoritative HEAD and the admission re-verifies
                // the entire postcondition set against it.
                continue;
            }
            Ok((candidate, crate::medium::AppendOutcome::Unknown { .. })) => {
                match medium.reconcile_unknown(&candidate)? {
                    crate::medium::Reconciliation::Installed { id } => return Ok((id, marker)),
                    crate::medium::Reconciliation::NotInstalled { .. } => {
                        if attempt == 1 {
                            return Err(MediumError::Chain {
                                rule: "reclamation-complete/unknown-unresolved",
                            });
                        }
                        continue;
                    }
                    crate::medium::Reconciliation::Unverifiable { .. } => {
                        return Err(MediumError::Chain {
                            rule: "reclamation-complete/unknown-unverifiable",
                        });
                    }
                }
            }
            Err(MediumError::Chain {
                rule: "duplicate-idempotency-key",
            }) => {
                // The deterministic key is taken: discover and
                // verify the installed marker's EXACT content — the
                // certified obligations must match the verified
                // state, never the key's mere existence.
                let Some((id, installed)) = find_installed_completion(medium, mk)? else {
                    return Err(MediumError::Chain {
                        rule: "reclamation-complete/duplicate-unresolved",
                    });
                };
                if installed != marker {
                    return Err(MediumError::Chain {
                        rule: "reclamation-complete/duplicate-content-mismatch",
                    });
                }
                return Ok((id, installed));
            }
            Err(other) => return Err(other),
        }
    }
    unreachable!("the retry loop returns on every path")
}

/// The keyed completion-marker discovery: the gap-aware descent
/// from the authoritative HEAD, returning the first record carrying
/// THIS mk's deterministic completion key with its strictly decoded
/// marker (the mk binding checked; everything else verified by
/// [`verify_reclamation_complete`]).
pub fn find_installed_completion(
    medium: &mut dyn CanonicalMedium,
    mk: &Hash,
) -> Result<Option<(Hash, ReclamationComplete)>, MediumError> {
    const MAX_DESCENT: u64 = 1 << 20;
    let key = reclamation_complete_idempotency_key(mk, Some(0));
    let mut current = medium
        .head()?
        .ok_or(MediumError::Chain { rule: "unfounded" })?;
    let mut skip_table: Option<crate::checkpoint::CheckpointMetadata> = None;
    for _ in 0..MAX_DESCENT {
        let record = medium.read_record(&current)?;
        if record.idempotency_key.as_deref() == Some(key.as_slice()) {
            let Some(object) = record.content_refs.first().copied() else {
                return Err(MediumError::Chain {
                    rule: "reclamation-complete/marker-malformed",
                });
            };
            let bytes = medium.read_object(&object)?;
            let marker = ReclamationComplete::decode(&bytes).map_err(MediumError::Format)?;
            if marker.mk != *mk {
                return Err(MediumError::Chain {
                    rule: "reclamation-complete/marker-mk-mismatch",
                });
            }
            return Ok(Some((current, marker)));
        }
        if let Some(object) = record.content_refs.first().copied() {
            let bytes = medium.read_object(&object)?;
            if StoredObject::new(bytes.clone())?.domain()
                == crate::checkpoint::CHECKPOINT_METADATA_DOMAIN
            {
                skip_table = Some(
                    crate::checkpoint::CheckpointMetadata::decode(&bytes)
                        .map_err(MediumError::Format)?,
                );
            }
        }
        let Some(parent) = record.parent else {
            return Ok(None);
        };
        match medium.read_record(&parent) {
            Ok(_) => current = parent,
            Err(MediumError::MissingObject { .. }) => {
                let Some(table) = skip_table.as_ref() else {
                    return Err(MediumError::MissingObject { hash: parent });
                };
                let target = if table.sk == parent {
                    table
                        .self_skip()
                        .map(|entry| entry.nearest_retained_ancestor)
                } else {
                    table
                        .skip_for(&current)
                        .map(|entry| entry.nearest_retained_ancestor)
                };
                let Some(target) = target else {
                    return Err(MediumError::MissingObject { hash: parent });
                };
                current = target;
            }
            Err(other) => return Err(other),
        }
    }
    Ok(None)
}

/// The trusted completion consumer: the read-side authority for
/// maintenance and discovery decisions. (P2-R2) The verification is
/// INDEPENDENT of the writer's honesty: (1) CANONICAL MEMBERSHIP —
/// the marker revision must be ENCOUNTERED on the authoritative
/// HEAD ancestry (a hash-valid, keyed, correctly-sourced marker
/// that is not installed on the chain is not terminal evidence);
/// the same descent observes whether any LATER reclamation evidence
/// (an audit or a completion marker) stands above this marker —
/// the only mechanism that can shrink the survivor closure; (2) the
/// structural authority — the record's deterministic key, the
/// strict decode, the v1 boundary gates; (3) the recovery-safe
/// source-audit authentication; (4) the SEMANTIC RECHECK against
/// the live state — the retiring records absent (stable under every
/// legal concurrent operation) and, when this marker is the NEWEST
/// reclamation evidence, the FULL obligation equality (the
/// recomputed partition, the mandatory-count, the deferred set,
/// every mandatory identity absent, every deferred identity
/// present); when later COMPLETED retirements stand above (their
/// markers installed — the only mechanism that can have deleted
/// objects this batch deferred), each divergent claim is accounted
/// per-object against the later completed retirement's own audited
/// manifest: a later audit that never executed deletes nothing and
/// relaxes nothing. The precise provable claims are documented on
/// the semantic recheck below.
pub fn verify_reclamation_complete(
    medium: &mut dyn CanonicalMedium,
    marker_record: &Hash,
    mk: &Hash,
) -> Result<ReclamationComplete, MediumError> {
    // (1) The membership descent: walk the authoritative ancestry
    // until the marker record is encountered; collect the LATER
    // COMPLETION MARKERS above it (a completed retirement's
    // certificate — never a mere audit, which may never execute).
    const MAX_DESCENT: u64 = 1 << 20;
    let mut current = medium
        .head()?
        .ok_or(MediumError::Chain { rule: "unfounded" })?;
    let mut skip_table: Option<crate::checkpoint::CheckpointMetadata> = None;
    let mut later_markers: Vec<(Hash, ReclamationComplete)> = Vec::new();
    let mut installed = false;
    for _ in 0..MAX_DESCENT {
        if current == *marker_record {
            installed = true;
            break;
        }
        let record = medium.read_record(&current)?;
        if let Some(object) = record.content_refs.first().copied() {
            let bytes = medium.read_object(&object)?;
            let domain = StoredObject::new(bytes.clone())?.domain().to_owned();
            if domain == RECLAMATION_COMPLETE_DOMAIN {
                // A completion marker ABOVE the requested one: a
                // candidate completed later retirement. It is NOT
                // trusted yet — the accounting below verifies each
                // one (the key, the gates, the source audit, the
                // stable claims, and its own obligation equality)
                // BEFORE its manifest becomes attribution evidence.
                let decoded = ReclamationComplete::decode(&bytes).map_err(MediumError::Format)?;
                let expected =
                    reclamation_complete_idempotency_key(&decoded.mk, Some(decoded.batch));
                if record.idempotency_key != Some(expected) {
                    return Err(MediumError::Chain {
                        rule: "reclamation-complete/marker-key-mismatch",
                    });
                }
                later_markers.push((current, decoded));
            } else if domain == crate::checkpoint::CHECKPOINT_METADATA_DOMAIN {
                skip_table = Some(
                    crate::checkpoint::CheckpointMetadata::decode(&bytes)
                        .map_err(MediumError::Format)?,
                );
            }
        }
        let Some(parent) = record.parent else {
            break;
        };
        match medium.read_record(&parent) {
            Ok(_) => current = parent,
            Err(MediumError::MissingObject { .. }) => {
                let Some(table) = skip_table.as_ref() else {
                    return Err(MediumError::MissingObject { hash: parent });
                };
                let target = if table.sk == parent {
                    table
                        .self_skip()
                        .map(|entry| entry.nearest_retained_ancestor)
                } else {
                    table
                        .skip_for(&current)
                        .map(|entry| entry.nearest_retained_ancestor)
                };
                let Some(target) = target else {
                    return Err(MediumError::MissingObject { hash: parent });
                };
                current = target;
            }
            Err(other) => return Err(other),
        }
    }
    if !installed {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/marker-not-installed",
        });
    }
    // (2) The structural authority.
    let record = medium.read_record(marker_record)?;
    let Some(object) = record.content_refs.first().copied() else {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/marker-malformed",
        });
    };
    let bytes = medium.read_object(&object)?;
    let marker = ReclamationComplete::decode(&bytes).map_err(MediumError::Format)?;
    if marker.mk != *mk {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/marker-mk-mismatch",
        });
    }
    if marker.kind == CompletionKind::Range {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/range-unauthorized",
        });
    }
    if marker.batch != 0 {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/batch-nonzero",
        });
    }
    let expected = reclamation_complete_idempotency_key(mk, Some(marker.batch));
    if record.idempotency_key != Some(expected) {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/marker-key-mismatch",
        });
    }
    // (3) The source audit's authority in the recovery-safe form:
    // the completed retirement's records are absent by construction.
    let (ledger, _metadata) = verify_installed_v1_audit(medium, &marker.source, &marker.mk)?;
    // (4) The semantic recheck. The retiring records absent (the
    // records-table read — the cross-provider interface) is the
    // stable live claim; the full obligation equality applies when
    // this marker is the newest reclamation evidence; the superset
    // deletion-side check applies when later evidence stands above.
    for unit in &ledger.units {
        match medium.read_record(&unit.record) {
            Err(MediumError::MissingObject { .. }) => {}
            Ok(_) => {
                return Err(MediumError::Chain {
                    rule: "reclamation-complete/record-still-present",
                });
            }
            Err(other) => return Err(other),
        }
    }
    let exclusion: std::collections::BTreeSet<Hash> =
        ledger.units.iter().map(|unit| unit.record).collect();
    let survivors = survivor_objects(medium, &exclusion)?;
    let partition = partition_reclaim(&ledger.units, &survivors)?;
    for identity in &partition.mandatory {
        match medium.read_object(identity) {
            Err(MediumError::MissingObject { .. }) => {}
            Ok(_) => {
                return Err(MediumError::Chain {
                    rule: "reclamation-complete/mandatory-residue",
                });
            }
            Err(other) => return Err(other),
        }
    }
    // (P2-R2-H1) THE AUTHENTICATED CASCADE: each later marker is
    // verified — newest first, its own stable claims (the key and
    // gates checked at collection; the source audit; every retiring
    // record absent; every mandatory identity absent) plus its own
    // obligation equality against the coverage of the ALREADY-
    // VERIFIED newer markers — BEFORE its source-ledger manifest
    // becomes attribution evidence. A fabricated or malformed later
    // marker therefore refuses the whole verification (fail closed)
    // rather than relaxing an earlier marker's certification. No
    // circularity: each marker's accounting uses only the markers
    // ABOVE it.
    let mut later_covered: std::collections::BTreeSet<Hash> = std::collections::BTreeSet::new();
    for (_record_id, later_marker) in &later_markers {
        if later_marker.kind == CompletionKind::Range || later_marker.batch != 0 {
            return Err(MediumError::Chain {
                rule: "reclamation-complete/later-marker-unauthorized",
            });
        }
        let (later_ledger, _later_metadata) =
            verify_installed_v1_audit(medium, &later_marker.source, &later_marker.mk)?;
        for unit in &later_ledger.units {
            match medium.read_record(&unit.record) {
                Err(MediumError::MissingObject { .. }) => {}
                Ok(_) => {
                    return Err(MediumError::Chain {
                        rule: "reclamation-complete/later-record-still-present",
                    });
                }
                Err(other) => return Err(other),
            }
        }
        let later_exclusion: std::collections::BTreeSet<Hash> =
            later_ledger.units.iter().map(|unit| unit.record).collect();
        let later_survivors = survivor_objects(medium, &later_exclusion)?;
        let later_partition = partition_reclaim(&later_ledger.units, &later_survivors)?;
        for identity in &later_partition.mandatory {
            match medium.read_object(identity) {
                Err(MediumError::MissingObject { .. }) => {}
                Ok(_) => {
                    return Err(MediumError::Chain {
                        rule: "reclamation-complete/later-mandatory-residue",
                    });
                }
                Err(other) => return Err(other),
            }
        }
        // The later marker's own obligation equality, against the
        // coverage accumulated from the newer verified markers.
        let mut later_accounted: std::collections::BTreeSet<Hash> =
            std::collections::BTreeSet::new();
        for unit in &later_ledger.units {
            for identity in unit.objects.iter().copied() {
                // The defensive set-membership invariant: only an
                // identity the recomputation actually classified
                // MANDATORY may be accounted (an identity outside the
                // reclassified set is attribution-incoherent).
                if later_covered.contains(&identity)
                    && later_partition.mandatory.contains(&identity)
                {
                    match medium.read_object(&identity) {
                        Err(MediumError::MissingObject { .. }) => {
                            later_accounted.insert(identity);
                        }
                        Ok(_) => {}
                        Err(other) => return Err(other),
                    }
                }
            }
        }
        let later_reconstructed_mandatory = later_partition
            .mandatory
            .len()
            .checked_sub(later_accounted.len())
            .ok_or(MediumError::Chain {
                rule: "reclamation-complete/attribution-incoherent",
            })?;
        if later_marker.mandatory_count != later_reconstructed_mandatory as u64
            || later_marker
                .deferred
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<Hash>>()
                != later_partition
                    .deferred
                    .iter()
                    .copied()
                    .chain(later_accounted.iter().copied())
                    .collect::<std::collections::BTreeSet<Hash>>()
        {
            return Err(MediumError::Chain {
                rule: "reclamation-complete/later-marker-incoherent",
            });
        }
        for identity in &later_partition.deferred {
            medium.read_object(identity)?;
        }
        // Verified: its manifest joins the attribution coverage.
        for unit in &later_ledger.units {
            later_covered.insert(unit.record);
            for object in &unit.objects {
                later_covered.insert(*object);
            }
        }
    }
    // (P2-R2-H2) THE TYPED ABSENCE + the correct namespace: the
    // accounted set contains only OBJECT identities (the content
    // namespace; the retiring records are covered by the stable
    // record-absent check and remain in the mandatory count), each
    // absence established ONLY by the authenticated MissingObject
    // result — corruption and I/O failures propagate, never
    // masquerading as deletion — and the arithmetic is CHECKED (an
    // accounted identity outside the recomputed mandatory set is an
    // inconsistency, never silently saturated).
    let mut accounted: std::collections::BTreeSet<Hash> = std::collections::BTreeSet::new();
    for unit in &ledger.units {
        for identity in unit.objects.iter().copied() {
            if later_covered.contains(&identity)
                && !accounted.contains(&identity)
                && partition.mandatory.contains(&identity)
            {
                match medium.read_object(&identity) {
                    Err(MediumError::MissingObject { .. }) => {
                        accounted.insert(identity);
                    }
                    Ok(_) => {}
                    Err(other) => return Err(other),
                }
            }
        }
    }
    let reconstructed_deferred: std::collections::BTreeSet<Hash> = partition
        .deferred
        .iter()
        .copied()
        .chain(accounted.iter().copied())
        .collect();
    let reconstructed_mandatory = partition
        .mandatory
        .len()
        .checked_sub(accounted.len())
        .ok_or(MediumError::Chain {
            rule: "reclamation-complete/attribution-incoherent",
        })?;
    if marker.mandatory_count != reconstructed_mandatory as u64 {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/mandatory-count-mismatch",
        });
    }
    if marker
        .deferred
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<Hash>>()
        != reconstructed_deferred
    {
        return Err(MediumError::Chain {
            rule: "reclamation-complete/deferred-mismatch",
        });
    }
    // The still-deferred identities remain present and hash-verified;
    // the accounted ones are absent BY the later verified
    // retirement's certified deletion.
    for identity in &partition.deferred {
        medium.read_object(identity)?;
    }
    Ok(marker)
}

/// The TEST-ONLY adversarial completion-marker install (the R4-L2/
/// R4-L3 regressions): publishes a caller-constructed marker through
/// the in-crate admitted append, BYPASSING the admission's state
/// verification — exactly the corrupted-in-crate-path threat the
/// consumer's independent verification must catch. COMPILE-TIME
/// ISOLATED: exists only under the composite-test-fixture feature
/// (never in a production build).
#[cfg(feature = "composite-test-fixture")]
#[doc(hidden)]
pub fn test_only_install_completion_marker(
    medium: &mut dyn CanonicalMedium,
    marker: &ReclamationComplete,
    tick: u64,
) -> Result<Hash, MediumError> {
    let head = medium
        .head()?
        .ok_or(MediumError::Chain { rule: "unfounded" })?;
    let parent_epoch = medium.read_record(&head)?.epoch;
    let key = reclamation_complete_idempotency_key(&marker.mk, Some(marker.batch));
    let object = StoredObject::new(marker.canonical_bytes())?;
    let record = RevisionRecord::regular(head, parent_epoch, tick)
        .with_idempotency_key(key)
        .with_content_refs(vec![object.hash()]);
    let admitted = crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(&object));
    medium.append_admitted(admitted, Some(head))?;
    Ok(record.id())
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

    // --------------------------- the OC-C1 codec, gate, and
    // digest-corrective regressions

    fn sample_begin() -> ReclamationBegin {
        ReclamationBegin {
            mk: Hash::from_bytes(&[0x01; 32]).unwrap(),
            source: Hash::from_bytes(&[0x02; 32]).unwrap(),
            range_digest: [0x03; 32],
            seed: [0x04; 32],
        }
    }

    #[test]
    fn the_begin_round_trip_is_byte_deterministic() {
        let begin = sample_begin();
        let bytes = begin.canonical_bytes();
        assert_eq!(bytes.len(), 206, "the ratified canonical length");
        let restored = ReclamationBegin::decode(&bytes).unwrap();
        assert_eq!(restored, begin);
        assert_eq!(restored.canonical_bytes(), bytes);
        // The golden vector: the exact content hash and the initial
        // byte layout (the 34-length prefix, the domain, the version).
        assert_eq!(
            hash_object(&bytes).hex(),
            "2a4348f3e1ec4f8aebb94ee751b92700e7a4bc57b697829f22d917d206ad9010"
        );
        assert_eq!(&bytes[..8], &[34, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            &bytes[8..42],
            b"engine-persist/reclamation-begin/1".as_slice()
        );
        assert_eq!(&bytes[42..46], &[1, 0, 0, 0]);
        assert_eq!(&bytes[46..54], &[32, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(bytes[54], 0x01);
    }

    #[test]
    fn the_begin_deterministic_key_is_mk_bound() {
        let a = Hash::from_bytes(&[0x01; 32]).unwrap();
        let b = Hash::from_bytes(&[0x02; 32]).unwrap();
        assert_eq!(
            reclamation_begin_idempotency_key(&a),
            reclamation_begin_idempotency_key(&a)
        );
        assert_ne!(
            reclamation_begin_idempotency_key(&a),
            reclamation_begin_idempotency_key(&b)
        );
        assert_eq!(
            reclamation_begin_idempotency_key(&a),
            format!("engine-persist/reclamation-begin-key/1:{}", a.hex()).into_bytes()
        );
    }

    #[test]
    fn the_begin_malformed_matrix_refuses_typed() {
        let begin = sample_begin();
        let bytes = begin.canonical_bytes();
        // The incorrect domain.
        let mut wrong = bytes.clone();
        wrong[9] = b'X';
        assert!(matches!(
            ReclamationBegin::decode(&wrong),
            Err(SaveError::Domain { .. })
        ));
        // The unsupported version.
        let mut wrong = bytes.clone();
        wrong[42] = 2;
        assert!(matches!(
            ReclamationBegin::decode(&wrong),
            Err(SaveError::Version { observed: 2, .. })
        ));
        // The MK undersized (31 declared): the length prefix shrinks
        // and the field's bytes shift — construct surgically.
        let undersized_mk = {
            let mut enc = Encoder::new();
            enc.write_str(RECLAMATION_BEGIN_DOMAIN);
            enc.write_u32(RECLAMATION_BEGIN_VERSION);
            enc.write_bytes(&begin.mk.as_bytes()[..31]);
            enc.write_bytes(begin.source.as_bytes());
            enc.write_bytes(&begin.range_digest);
            enc.write_bytes(&begin.seed);
            enc.into_bytes()
        };
        assert!(matches!(
            ReclamationBegin::decode(&undersized_mk),
            Err(SaveError::Integrity("reclamation-begin/hash-width"))
        ));
        // The MK oversized (33 declared): the limit check refuses.
        let oversized_mk = {
            let mut extended = begin.mk.as_bytes().to_vec();
            extended.push(0x01);
            let mut enc = Encoder::new();
            enc.write_str(RECLAMATION_BEGIN_DOMAIN);
            enc.write_u32(RECLAMATION_BEGIN_VERSION);
            enc.write_bytes(&extended);
            enc.write_bytes(begin.source.as_bytes());
            enc.write_bytes(&begin.range_digest);
            enc.write_bytes(&begin.seed);
            enc.into_bytes()
        };
        assert!(matches!(
            ReclamationBegin::decode(&oversized_mk),
            Err(SaveError::Limit { .. })
        ));
        // The source undersized/oversized: the same pair.
        let undersized_source = {
            let mut enc = Encoder::new();
            enc.write_str(RECLAMATION_BEGIN_DOMAIN);
            enc.write_u32(RECLAMATION_BEGIN_VERSION);
            enc.write_bytes(begin.mk.as_bytes());
            enc.write_bytes(&begin.source.as_bytes()[..31]);
            enc.write_bytes(&begin.range_digest);
            enc.write_bytes(&begin.seed);
            enc.into_bytes()
        };
        assert!(matches!(
            ReclamationBegin::decode(&undersized_source),
            Err(SaveError::Integrity("reclamation-begin/hash-width"))
        ));
        let oversized_source = {
            let mut extended = begin.source.as_bytes().to_vec();
            extended.push(0x02);
            let mut enc = Encoder::new();
            enc.write_str(RECLAMATION_BEGIN_DOMAIN);
            enc.write_u32(RECLAMATION_BEGIN_VERSION);
            enc.write_bytes(begin.mk.as_bytes());
            enc.write_bytes(&extended);
            enc.write_bytes(&begin.range_digest);
            enc.write_bytes(&begin.seed);
            enc.into_bytes()
        };
        assert!(matches!(
            ReclamationBegin::decode(&oversized_source),
            Err(SaveError::Limit { .. })
        ));
        // The digest undersized (31): the typed exact-width refusal —
        // never a panic (the OC-C1-E corrective).
        let undersized_digest = {
            let mut enc = Encoder::new();
            enc.write_str(RECLAMATION_BEGIN_DOMAIN);
            enc.write_u32(RECLAMATION_BEGIN_VERSION);
            enc.write_bytes(begin.mk.as_bytes());
            enc.write_bytes(begin.source.as_bytes());
            enc.write_bytes(&begin.range_digest[..31]);
            enc.write_bytes(&begin.seed);
            enc.into_bytes()
        };
        assert!(matches!(
            ReclamationBegin::decode(&undersized_digest),
            Err(SaveError::Integrity("reclamation/digest-width"))
        ));
        // The digest oversized (33): the limit check.
        let oversized_digest = {
            let mut extended = begin.range_digest.to_vec();
            extended.push(0x03);
            let mut enc = Encoder::new();
            enc.write_str(RECLAMATION_BEGIN_DOMAIN);
            enc.write_u32(RECLAMATION_BEGIN_VERSION);
            enc.write_bytes(begin.mk.as_bytes());
            enc.write_bytes(begin.source.as_bytes());
            enc.write_bytes(&extended);
            enc.write_bytes(&begin.seed);
            enc.into_bytes()
        };
        assert!(matches!(
            ReclamationBegin::decode(&oversized_digest),
            Err(SaveError::Limit { .. })
        ));
        // The seed undersized: the typed width refusal.
        let undersized_seed = {
            let mut enc = Encoder::new();
            enc.write_str(RECLAMATION_BEGIN_DOMAIN);
            enc.write_u32(RECLAMATION_BEGIN_VERSION);
            enc.write_bytes(begin.mk.as_bytes());
            enc.write_bytes(begin.source.as_bytes());
            enc.write_bytes(&begin.range_digest);
            enc.write_bytes(&begin.seed[..31]);
            enc.into_bytes()
        };
        assert!(matches!(
            ReclamationBegin::decode(&undersized_seed),
            Err(SaveError::Integrity("reclamation/digest-width"))
        ));
        // The zero identities.
        let zero_mk = ReclamationBegin {
            mk: Hash::ZERO,
            ..sample_begin()
        };
        assert!(matches!(
            ReclamationBegin::decode(&zero_mk.canonical_bytes()),
            Err(SaveError::Integrity("reclamation-begin/zero-identity"))
        ));
        let zero_source = ReclamationBegin {
            source: Hash::ZERO,
            ..sample_begin()
        };
        assert!(matches!(
            ReclamationBegin::decode(&zero_source.canonical_bytes()),
            Err(SaveError::Integrity("reclamation-begin/zero-identity"))
        ));
        // The truncated length prefix (the input ends mid-prefix):
        // the typed Truncated failure.
        assert!(matches!(
            ReclamationBegin::decode(&bytes[..50]),
            Err(SaveError::Truncated)
        ));
        // The truncated field content (the seed's content cut short
        // after a complete 32-length prefix): read_count's
        // remaining-bounds Limit failure — never a panic.
        assert!(matches!(
            ReclamationBegin::decode(&bytes[..205]),
            Err(SaveError::Limit { .. })
        ));
        // The single and multiple trailing bytes.
        let mut trailing_one = bytes.clone();
        trailing_one.push(0xAA);
        assert!(matches!(
            ReclamationBegin::decode(&trailing_one),
            Err(SaveError::Trailing { leftover: 1 })
        ));
        let mut trailing_many = bytes.clone();
        trailing_many.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        assert!(matches!(
            ReclamationBegin::decode(&trailing_many),
            Err(SaveError::Trailing { leftover: 3 })
        ));
    }

    #[test]
    fn the_persisted_ledger_digest_width_refuses_typed() {
        // (OC-C1-E) The preexisting read_digest defect: a ledger
        // whose declared digest length is undersized previously
        // REACHED copy_from_slice and panicked; the corrective
        // returns the typed Integrity failure. The oversized
        // declaration refuses at the limit check.
        let ledger = sample_ledger();
        let undersized = {
            let mut enc = Encoder::new();
            enc.write_str(RECLAMATION_LEDGER_DOMAIN);
            enc.write_u32(RECLAMATION_LEDGER_VERSION);
            enc.write_bytes(ledger.mk.as_bytes());
            enc.write_bytes(ledger.sk.as_bytes());
            enc.write_bytes(&ledger.range_digest[..31]);
            enc.write_bytes(&ledger.seed);
            enc.write_u64(ledger.records);
            enc.write_u64(ledger.reclaimed_keys);
            enc.write_count(ledger.journal_files.len());
            for name in &ledger.journal_files {
                enc.write_str(name);
            }
            enc.write_count(ledger.units.len());
            for unit in &ledger.units {
                enc.write_bytes(unit.record.as_bytes());
                enc.write_count(unit.objects.len());
                for object in &unit.objects {
                    enc.write_bytes(object.as_bytes());
                }
            }
            enc.into_bytes()
        };
        assert!(matches!(
            ReclamationLedger::decode(&undersized),
            Err(SaveError::Integrity("reclamation/digest-width"))
        ));
        let oversized = {
            let mut extended = ledger.range_digest.to_vec();
            extended.push(0x07);
            let mut enc = Encoder::new();
            enc.write_str(RECLAMATION_LEDGER_DOMAIN);
            enc.write_u32(RECLAMATION_LEDGER_VERSION);
            enc.write_bytes(ledger.mk.as_bytes());
            enc.write_bytes(ledger.sk.as_bytes());
            enc.write_bytes(&extended);
            enc.write_bytes(&ledger.seed);
            enc.write_u64(ledger.records);
            enc.write_u64(ledger.reclaimed_keys);
            enc.write_count(ledger.journal_files.len());
            for name in &ledger.journal_files {
                enc.write_str(name);
            }
            enc.write_count(ledger.units.len());
            for unit in &ledger.units {
                enc.write_bytes(unit.record.as_bytes());
                enc.write_count(unit.objects.len());
                for object in &unit.objects {
                    enc.write_bytes(object.as_bytes());
                }
            }
            enc.into_bytes()
        };
        assert!(matches!(
            ReclamationLedger::decode(&oversized),
            Err(SaveError::Limit { .. })
        ));
        // The valid ledger still round-trips (the compatibility
        // regression).
        let valid = ledger.canonical_bytes();
        assert_eq!(ReclamationLedger::decode(&valid).unwrap(), ledger);
    }

    #[test]
    fn the_generic_append_refuses_the_begin_family() {
        use crate::StoredObject;
        use crate::local::LocalFileMedium;
        use crate::medium::CanonicalMedium;
        use crate::record::{AuthorityMode, RevisionRecord};
        let root = std::env::temp_dir().join(format!(
            "s7-occ1-gate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut medium = LocalFileMedium::open(&root).unwrap();
        let plain = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(b"payload");
            enc.into_bytes()
        })
        .unwrap();
        let founding = RevisionRecord::founding(AuthorityMode::Standalone, "occ1", 0)
            .with_content_refs(vec![plain.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&plain), None)
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        // The ratified /1 begin object: the generic append refuses.
        let begin_object = StoredObject::new(sample_begin().canonical_bytes()).unwrap();
        let record =
            RevisionRecord::regular(head, epoch, 9).with_content_refs(vec![begin_object.hash()]);
        let error = medium
            .append(&record, std::slice::from_ref(&begin_object), Some(head))
            .unwrap_err();
        assert!(
            matches!(
                error,
                crate::medium::MediumError::Chain {
                    rule: "reclamation-begin/generic-append-refused"
                }
            ),
            "the ratified begin refuses the generic append: {error:?}"
        );
        // An unsupported family variant (/2): equally refused.
        let unsupported = StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("engine-persist/reclamation-begin/2");
            enc.write_u32(1);
            enc.write_bytes(&[0x01; 32]);
            enc.write_bytes(&[0x02; 32]);
            enc.write_bytes(&[0x03; 32]);
            enc.write_bytes(&[0x04; 32]);
            enc.into_bytes()
        })
        .unwrap();
        let record =
            RevisionRecord::regular(head, epoch, 10).with_content_refs(vec![unsupported.hash()]);
        let error = medium
            .append(&record, std::slice::from_ref(&unsupported), Some(head))
            .unwrap_err();
        assert!(
            matches!(
                error,
                crate::medium::MediumError::Chain {
                    rule: "reclamation-begin/generic-append-refused"
                }
            ),
            "the unsupported family variant refuses: {error:?}"
        );
        // The ordinary object publication still works.
        let record = RevisionRecord::regular(head, epoch, 11).with_content_refs(vec![plain.hash()]);
        let outcome = medium
            .append(&record, std::slice::from_ref(&plain), Some(head))
            .unwrap();
        assert!(matches!(
            outcome,
            crate::medium::AppendOutcome::Accepted { .. }
        ));
        // The existing completion-marker gate remains enforced.
        let marker = crate::reclaim::ReclamationComplete {
            kind: crate::reclaim::CompletionKind::Batch,
            mk: founding.id(),
            batch: 0,
            source: founding.id(),
            mandatory_count: 0,
            deferred: Vec::new(),
            journal_files: Vec::new(),
        };
        let marker_object = StoredObject::new(marker.canonical_bytes()).unwrap();
        let head2 = medium.head().unwrap().unwrap();
        let epoch2 = medium.read_record(&head2).unwrap().epoch;
        let record = RevisionRecord::regular(head2, epoch2, 12)
            .with_content_refs(vec![marker_object.hash()]);
        let error = medium
            .append(&record, std::slice::from_ref(&marker_object), Some(head2))
            .unwrap_err();
        assert!(
            matches!(
                error,
                crate::medium::MediumError::Chain {
                    rule: "reclamation-complete/generic-append-refused"
                }
            ),
            "the completion gate remains enforced: {error:?}"
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
    }
}
