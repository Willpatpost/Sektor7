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
}
