//! The SK/MK checkpoint protocol's metadata half (PP-B3, the ratified
//! D-PP-4 two-revision composition): the versioned
//! `engine-persist/checkpoint-metadata/1` object referenced by the
//! MK revision — SK's immediate child on the canonical chain —
//! carrying the skip table, the compact reclaimed-record index, and
//! the digest commitments for ONE checkpoint's own reclaim range.
//!
//! **The non-circular self-retirement skip (the mandatory PP-B3
//! condition):** MK's metadata must allow the walk to bypass a
//! validly retired SK without encoding MK's own content-derived
//! revision hash inside the very object that determines that hash.
//! The encoding: a reserved self marker (`Hash::ZERO` as a skip
//! entry's `retained` field) meaning "the MK revision referencing
//! THIS metadata object", plus the known-SK identity (`sk` —
//! MK's parent, a PRIOR record whose hash is computable before MK
//! exists) with strict contextual validation. No field of this
//! object derives from MK's own bytes.
//!
//! **The membership semantics (D-PP-4b's rejection honored):** the
//! compact reclaimed-record index preserves BOTH distinct historical
//! identity classes for the checkpoint's own range — the idempotency
//! keys (chain-wide deduplication) and the revision hashes with
//! their epochs (what surviving `depends_on` references name:
//! existence plus strictly-earlier order). Each checkpoint carries
//! its own range only; successive checkpoints never cumulatively
//! reserialize their predecessors.
//!
//! **The digest commitments:** a running SHA-256 chain over the
//! range's record canonical bytes (oldest first), seeded from the
//! domain tag and the founding record's identity. Before reclamation
//! the digest is COMPUTED and VERIFIED against the actual bytes;
//! after reclamation it is a previously-verified commitment —
//! attesting what was walked and hashed, never an independent
//! validation of unavailable history.

use std::collections::HashSet;

use sha2::{Digest, Sha256};

use engine_core::wire::MAX_TAG_LEN;
use engine_core::{Decoder, Encoder, SaveError};

use crate::hash::Hash;
use crate::medium::{AppendOutcome, CanonicalMedium, MediumError, StoredObject};
use crate::record::{MAX_IDEMPOTENCY_KEY_LEN, RevisionRecord};

/// The checkpoint-metadata object's versioned domain (the MK
/// revision's single content reference).
pub const CHECKPOINT_METADATA_DOMAIN: &str = "engine-persist/checkpoint-metadata/1";
pub const CHECKPOINT_METADATA_VERSION: u32 = 1;

/// The MK append's idempotency-key domain (deterministically derived
/// from the SK identity — a retried MK over the same SK produces the
/// same key, so the chain-wide uniqueness makes the retry idempotent
/// or the reconcile finds it installed).
pub const CHECKPOINT_MK_KEY_DOMAIN: &str = "engine-persist/checkpoint-mk/1";

/// Documented maxima (bounded decode, checked before allocation).
pub const MAX_CHECKPOINT_SKIPS: u64 = 1 << 16;
pub const MAX_CHECKPOINT_INDEX: u64 = 1 << 16;

/// Peeks an object's leading domain tag — the checkpoint recognition
/// for chain walks (the walk reuses bytes it already loaded and
/// hash-verified; no payload parse). A record whose first content
/// object's domain is [`CHECKPOINT_METADATA_DOMAIN`] is an MK.
pub fn peek_object_domain(bytes: &[u8]) -> Result<&str, SaveError> {
    let mut dec = Decoder::new(bytes);
    dec.read_str(MAX_TAG_LEN, "checkpoint-peek")
}

/// One skip-table entry: a RETAINED revision whose direct parent has
/// been legitimately reclaimed jumps to its NEAREST RETAINED
/// ancestor. The table's size scales with the retained entries
/// requiring skips — it is never a growing skeleton of every
/// reclaimed historical revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkipEntry {
    /// The retained revision needing the skip. The reserved
    /// [`Hash::ZERO`] self marker names "the MK revision referencing
    /// the containing metadata object" — the non-circular
    /// self-retirement skip (at most one such entry per metadata
    /// object, decode-enforced via the sorted-unique rule).
    pub retained: Hash,
    /// The jump target: the nearest retained ancestor, computed from
    /// the fully verified pre-reclamation chain.
    pub nearest_retained_ancestor: Hash,
}

impl SkipEntry {
    pub fn new(retained: Hash, nearest_retained_ancestor: Hash) -> Self {
        SkipEntry {
            retained,
            nearest_retained_ancestor,
        }
    }

    /// The self-retirement skip: "the referencing MK itself skips to
    /// the nearest retained ancestor below SK" — SK's parent while
    /// that parent is retained, or the bottom of SK's reclaim run
    /// when the parent is itself a range publication (SK is retired
    /// together with its range). Used by the walk when SK has been
    /// validly retired; unused (the normal parent chain) while SK
    /// remains retained. Both states validate.
    pub fn self_skip(target: Hash) -> Self {
        SkipEntry {
            retained: Hash::ZERO,
            nearest_retained_ancestor: target,
        }
    }

    /// Whether this is the self-retirement entry.
    pub fn is_self(&self) -> bool {
        self.retained == Hash::ZERO
    }
}

/// One compact reclaimed-record index entry: BOTH distinct historical
/// identity classes for one reclaimed revision of this checkpoint's
/// own range — the idempotency key where the record carried one
/// (chain-wide deduplication) and the revision hash with its epoch
/// (identity and strictly-earlier dependency validation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimedIndexEntry {
    /// The reclaimed record's idempotency key, where present.
    pub key: Option<Vec<u8>>,
    /// The reclaimed revision's identity (its canonical bytes' hash).
    pub hash: Hash,
    /// The reclaimed revision's chain position (epoch).
    pub epoch: u64,
}

impl ReclaimedIndexEntry {
    pub fn new(key: Option<Vec<u8>>, hash: Hash, epoch: u64) -> Self {
        ReclaimedIndexEntry { key, hash, epoch }
    }
}

/// The MK metadata object: the skip table, the compact index, and
/// the digest commitments for ONE checkpoint's own reclaim range.
///
/// Strict coherence, enforced at decode: the sorted-unique skips (by
/// `retained`) and index (by `hash`); at most one self entry — its
/// ancestor equals `sk_parent` while that parent is retained, and is
/// never an indexed revision otherwise (SK stood on a range
/// publication; MK jumps past the whole reclaim run); `sk` and
/// `sk_parent` are real identities (never the zero hash); no skip
/// names `sk` itself (SK's retirement is the self-skip's business —
/// a skip standing on SK is dead by construction); no skip's
/// `retained` field — and no skip's jump target — names an indexed
/// (reclaimed) revision; `range_records` equals the index length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointMetadata {
    /// The known-SK identity: MK's parent. Strict contextual
    /// validation binds the metadata to the MK that references it —
    /// the referencing record's parent must equal this field. A
    /// PRIOR record's hash (computable before MK exists): the
    /// encoding is non-circular.
    pub sk: Hash,
    /// SK's parent — the retained pre-checkpoint head, the
    /// self-retirement skip's target.
    pub sk_parent: Hash,
    /// The skip table (sorted ascending by `retained`, unique).
    pub skips: Vec<SkipEntry>,
    /// The compact reclaimed-record index (sorted ascending by
    /// `hash`, unique) — this checkpoint's OWN range only.
    pub index: Vec<ReclaimedIndexEntry>,
    /// The running SHA-256 chain over the range's record canonical
    /// bytes (oldest first) — the previously-verified commitment.
    pub range_digest: [u8; 32],
    /// The digest chain's seed (the domain tag + the founding
    /// identity), carried for self-contained validation.
    pub seed: [u8; 32],
    /// The range's record count (must equal the index length).
    pub range_records: u64,
}

impl CheckpointMetadata {
    /// Builds the metadata, normalizing the skips (ascending by
    /// `retained`, deduplicated) and the index (ascending by
    /// `hash`, deduplicated); `range_records` derives from the index
    /// length.
    pub fn new(
        sk: Hash,
        sk_parent: Hash,
        mut skips: Vec<SkipEntry>,
        mut index: Vec<ReclaimedIndexEntry>,
        range_digest: [u8; 32],
        seed: [u8; 32],
    ) -> Self {
        skips.sort_unstable_by_key(|entry| entry.retained);
        skips.dedup_by_key(|entry| entry.retained);
        index.sort_unstable_by_key(|entry| entry.hash);
        index.dedup_by_key(|entry| entry.hash);
        let range_records = index.len() as u64;
        CheckpointMetadata {
            sk,
            sk_parent,
            skips,
            index,
            range_digest,
            seed,
            range_records,
        }
    }

    /// The self-retirement entry, when present.
    pub fn self_skip(&self) -> Option<SkipEntry> {
        self.skips.iter().copied().find(SkipEntry::is_self)
    }

    /// The skip for a specific retained revision, when present.
    pub fn skip_for(&self, retained: &Hash) -> Option<SkipEntry> {
        self.skips
            .iter()
            .copied()
            .find(|entry| &entry.retained == retained)
    }

    /// Canonical bytes (the MK's single content object).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_str(CHECKPOINT_METADATA_DOMAIN);
        enc.write_u32(CHECKPOINT_METADATA_VERSION);
        enc.write_bytes(self.sk.as_bytes());
        enc.write_bytes(self.sk_parent.as_bytes());
        enc.write_count(self.skips.len());
        for entry in &self.skips {
            enc.write_bytes(entry.retained.as_bytes());
            enc.write_bytes(entry.nearest_retained_ancestor.as_bytes());
        }
        enc.write_count(self.index.len());
        for entry in &self.index {
            match &entry.key {
                Some(key) => {
                    enc.write_u8(1);
                    enc.write_bytes(key);
                }
                None => enc.write_u8(0),
            }
            enc.write_bytes(entry.hash.as_bytes());
            enc.write_u64(entry.epoch);
        }
        enc.write_bytes(&self.range_digest);
        enc.write_bytes(&self.seed);
        enc.write_u64(self.range_records);
        enc.into_bytes()
    }

    /// Strict decode (fail-closed): domain, version, hash widths,
    /// bounded counts, sorted-unique skips and index, the single
    /// self entry with its ancestor equal to `sk_parent`, real
    /// (non-zero) `sk`/`sk_parent`, no skip standing on `sk` or on
    /// an indexed revision, `range_records` equal to the index
    /// length, no trailing bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, SaveError> {
        const SECTION: &str = "checkpoint-metadata";
        let mut dec = Decoder::new(bytes);
        dec.read_domain(CHECKPOINT_METADATA_DOMAIN)?;
        let version = dec.read_u32()?;
        if version != CHECKPOINT_METADATA_VERSION {
            return Err(SaveError::Version {
                observed: version,
                supported: CHECKPOINT_METADATA_VERSION,
            });
        }
        let sk = read_hash(&mut dec, SECTION)?;
        let sk_parent = read_hash(&mut dec, SECTION)?;
        if sk == Hash::ZERO || sk_parent == Hash::ZERO {
            return Err(SaveError::Integrity("checkpoint-metadata/zero-identity"));
        }
        let skip_count = dec.read_count(MAX_CHECKPOINT_SKIPS, SECTION, "skip-count")?;
        let mut skips = Vec::with_capacity(skip_count as usize);
        let mut prev_retained: Option<Hash> = None;
        for _ in 0..skip_count {
            let retained = read_hash(&mut dec, SECTION)?;
            let nearest_retained_ancestor = read_hash(&mut dec, SECTION)?;
            if let Some(prior) = prev_retained
                && retained <= prior
            {
                return Err(SaveError::Order { section: SECTION });
            }
            prev_retained = Some(retained);
            skips.push(SkipEntry {
                retained,
                nearest_retained_ancestor,
            });
        }
        let index_count = dec.read_count(MAX_CHECKPOINT_INDEX, SECTION, "index-count")?;
        let mut index = Vec::with_capacity(index_count as usize);
        let mut prev_hash: Option<Hash> = None;
        for _ in 0..index_count {
            let key = match dec.read_u8()? {
                0 => None,
                1 => Some(dec.read_bytes(MAX_IDEMPOTENCY_KEY_LEN, SECTION)?.to_vec()),
                discriminant => {
                    return Err(SaveError::Enum {
                        section: SECTION,
                        discriminant: u64::from(discriminant),
                    });
                }
            };
            let hash = read_hash(&mut dec, SECTION)?;
            let epoch = dec.read_u64()?;
            if let Some(prior) = prev_hash
                && hash <= prior
            {
                return Err(SaveError::Order { section: SECTION });
            }
            prev_hash = Some(hash);
            index.push(ReclaimedIndexEntry { key, hash, epoch });
        }
        let range_digest = read_digest(&mut dec, SECTION)?;
        let seed = read_digest(&mut dec, SECTION)?;
        let range_records = dec.read_u64()?;
        dec.finish()?;
        // The single self entry's coherence: when sk_parent is
        // RETAINED (not itself a reclaimed range member) its target
        // IS sk_parent; when sk_parent is reclaimed (SK stood on a
        // range publication — SK is retired together with its range)
        // the target must be a retained revision too — never an
        // indexed one (the nearest retained ancestor past SK's whole
        // reclaim run).
        let self_entries = skips.iter().filter(|entry| entry.is_self()).count();
        if self_entries > 1 {
            return Err(SaveError::Integrity("checkpoint-metadata/duplicate-self"));
        }
        // A skip standing on SK itself is dead by construction (SK's
        // retirement is the self-skip's business — the walk never
        // stands at a retired SK).
        if skips.iter().any(|entry| entry.retained == sk) {
            return Err(SaveError::Integrity("checkpoint-metadata/skip-on-sk"));
        }
        // A skip's retained field must name a RETAINED revision —
        // never one this checkpoint itself indexes as reclaimed.
        let indexed: HashSet<Hash> = index.iter().map(|entry| entry.hash).collect();
        if skips.iter().any(|entry| indexed.contains(&entry.retained)) {
            return Err(SaveError::Integrity(
                "checkpoint-metadata/skip-on-reclaimed",
            ));
        }
        // Nor may a skip TARGET a reclaimed revision — every jump
        // target is retained by construction.
        if skips
            .iter()
            .any(|entry| indexed.contains(&entry.nearest_retained_ancestor))
        {
            return Err(SaveError::Integrity(
                "checkpoint-metadata/skip-target-reclaimed",
            ));
        }
        if let Some(entry) = skips.iter().find(|entry| entry.is_self())
            && !indexed.contains(&sk_parent)
            && entry.nearest_retained_ancestor != sk_parent
        {
            return Err(SaveError::Integrity(
                "checkpoint-metadata/self-ancestor-mismatch",
            ));
        }
        if range_records != index.len() as u64 {
            return Err(SaveError::Integrity("checkpoint-metadata/range-count"));
        }
        Ok(CheckpointMetadata {
            sk,
            sk_parent,
            skips,
            index,
            range_digest,
            seed,
            range_records,
        })
    }
}

/// Computes the digest chain's seed: the SHA-256 over the domain tag
/// and the founding record's identity (the "chained from the founding
/// record" anchoring — the same seed for every checkpoint of one
/// campaign).
pub fn compute_digest_seed(founding: &Hash) -> [u8; 32] {
    let mut seed_hasher = Sha256::new();
    seed_hasher.update(CHECKPOINT_METADATA_DOMAIN.as_bytes());
    seed_hasher.update(founding.as_bytes());
    seed_hasher.finalize().into()
}

/// Chains the range digest from a seed: the running SHA-256 over the
/// range's record canonical bytes, oldest first. An empty range
/// yields the seed itself.
pub fn chain_range_digest(seed: &[u8; 32], range_record_bytes: &[impl AsRef<[u8]>]) -> [u8; 32] {
    let mut chain = *seed;
    for bytes in range_record_bytes {
        let mut hasher = Sha256::new();
        hasher.update(chain);
        hasher.update(bytes.as_ref());
        chain = hasher.finalize().into();
    }
    chain
}

/// Computes the range digest and its seed in one call (the seed from
/// the founding identity; the digest chained from it).
pub fn compute_range_digest(
    founding: &Hash,
    range_record_bytes: &[impl AsRef<[u8]>],
) -> ([u8; 32], [u8; 32]) {
    let seed = compute_digest_seed(founding);
    let chain = chain_range_digest(&seed, range_record_bytes);
    (chain, seed)
}

/// The MK append's deterministic idempotency key (derived from the
/// SK identity — a retried MK over the same SK carries the same
/// key).
pub fn checkpoint_mk_idempotency_key(sk: &Hash) -> Vec<u8> {
    format!("{CHECKPOINT_MK_KEY_DOMAIN}:{}", sk.hex()).into_bytes()
}

/// Publishes the MK revision: SK's IMMEDIATE child, one content
/// object (the metadata), the deterministic checkpoint key. The head
/// must be exactly `sk` (the just-published snapshot) — a moved head
/// is the typed `checkpoint-mk/parent-not-sk` refusal, never a
/// silent interleaving. The metadata must describe exactly this SK.
///
/// Returns the candidate MK identity alongside the outcome: the
/// caller reconciles an `Unknown` against the candidate under the
/// existing lock-held discipline (the deterministic key makes the
/// retry idempotent). `RejectedStaleParent` maps to the typed
/// adjacency refusal — the head is no longer SK.
pub fn publish_checkpoint_mk(
    medium: &mut dyn CanonicalMedium,
    sk: Hash,
    sk_epoch: u64,
    metadata: &CheckpointMetadata,
    tick: u64,
) -> Result<(Hash, AppendOutcome), MediumError> {
    // The known-SK identity's strict contextual validation
    // (construction side): the metadata must describe exactly the SK
    // this MK will sit on.
    if metadata.sk != sk {
        return Err(MediumError::Chain {
            rule: "checkpoint-metadata/sk-mismatch",
        });
    }
    let object = StoredObject::new(metadata.canonical_bytes())?;
    let head = medium
        .head()?
        .ok_or(MediumError::Chain { rule: "unfounded" })?;
    if head != sk {
        // MK must be SK's immediate child — the two-revision
        // composition's adjacency is structural, never best-effort.
        return Err(MediumError::Chain {
            rule: "checkpoint-mk/parent-not-sk",
        });
    }
    let record = RevisionRecord::regular(sk, sk_epoch, tick)
        .with_idempotency_key(checkpoint_mk_idempotency_key(&sk))
        .with_content_refs(vec![object.hash()]);
    let candidate = record.id();
    let outcome = medium.append(&record, std::slice::from_ref(&object), Some(sk))?;
    Ok((candidate, outcome))
}

fn read_hash(dec: &mut Decoder<'_>, section: &'static str) -> Result<Hash, SaveError> {
    let bytes = dec.read_bytes(Hash::WIDTH, section)?;
    Hash::from_bytes(bytes).ok_or(SaveError::Integrity("checkpoint-metadata/hash-width"))
}

fn read_digest(dec: &mut Decoder<'_>, section: &'static str) -> Result<[u8; 32], SaveError> {
    let bytes = dec.read_bytes(32, section)?;
    let mut digest = [0u8; 32];
    digest.copy_from_slice(bytes);
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash_object;

    fn sample_sk() -> Hash {
        hash_object(b"sample-sk")
    }

    fn sample_metadata() -> CheckpointMetadata {
        let sk = sample_sk();
        let sk_parent = hash_object(b"founding-or-prior-head");
        let retained_a = hash_object(b"retained-a");
        let retained_b = hash_object(b"retained-b");
        let reclaimed_x = hash_object(b"reclaimed-x");
        let reclaimed_y = hash_object(b"reclaimed-y");
        let skips = vec![
            SkipEntry::self_skip(sk_parent),
            SkipEntry::new(retained_a, sk_parent),
            SkipEntry::new(retained_b, retained_a),
        ];
        let index = vec![
            ReclaimedIndexEntry::new(Some(b"key-one".to_vec()), reclaimed_x, 3),
            ReclaimedIndexEntry::new(None, reclaimed_y, 5),
        ];
        let (digest, seed) = compute_range_digest(
            &sk_parent,
            &[b"record-x-canonical-bytes", b"record-y-canonical-bytes"],
        );
        CheckpointMetadata::new(sk, sk_parent, skips, index, digest, seed)
    }

    #[test]
    fn round_trip_is_byte_deterministic() {
        let metadata = sample_metadata();
        let bytes = metadata.canonical_bytes();
        let restored = CheckpointMetadata::decode(&bytes).unwrap();
        assert_eq!(restored, metadata);
        assert_eq!(restored.canonical_bytes(), bytes);
        // The builder normalized the skips into ascending order —
        // the self marker (the zero hash) sorts first.
        assert!(restored.skips[0].is_self());
        assert!(
            restored
                .skips
                .windows(2)
                .all(|w| w[0].retained < w[1].retained)
        );
        assert!(restored.index.windows(2).all(|w| w[0].hash < w[1].hash));
    }

    #[test]
    fn domain_and_version_are_refused() {
        let bytes = sample_metadata().canonical_bytes();
        let mut wrong_domain = Vec::new();
        let mut enc = Encoder::new();
        enc.write_str("sektor7/other/1");
        wrong_domain.extend_from_slice(&enc.into_bytes());
        wrong_domain.extend_from_slice(&bytes[8 + CHECKPOINT_METADATA_DOMAIN.len()..]);
        assert!(matches!(
            CheckpointMetadata::decode(&wrong_domain),
            Err(SaveError::Domain { .. })
        ));

        let mut wrong_version = bytes.clone();
        let version_at = 8 + CHECKPOINT_METADATA_DOMAIN.len();
        wrong_version[version_at..version_at + 4].copy_from_slice(&2u32.to_le_bytes());
        assert!(matches!(
            CheckpointMetadata::decode(&wrong_version),
            Err(SaveError::Version { observed: 2, .. })
        ));
    }

    #[test]
    fn truncation_rejected_at_every_boundary() {
        let bytes = sample_metadata().canonical_bytes();
        for cut in 0..bytes.len() {
            assert!(
                CheckpointMetadata::decode(&bytes[..cut]).is_err(),
                "a {cut}-byte prefix must not decode"
            );
        }
        assert!(CheckpointMetadata::decode(&bytes).is_ok());
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = sample_metadata().canonical_bytes();
        bytes.push(0);
        assert!(matches!(
            CheckpointMetadata::decode(&bytes),
            Err(SaveError::Trailing { .. })
        ));
    }

    #[test]
    fn unsorted_skips_are_refused() {
        let mut metadata = sample_metadata();
        // Reverse the non-self entries (the builder sorted them).
        let n = metadata.skips.len();
        metadata.skips.swap(1, 2);
        let bytes = metadata.canonical_bytes();
        assert!(matches!(
            CheckpointMetadata::decode(&bytes),
            Err(SaveError::Order { .. })
        ));
        let _ = n;
    }

    #[test]
    fn unsorted_index_is_refused() {
        let mut metadata = sample_metadata();
        metadata.index.reverse();
        let bytes = metadata.canonical_bytes();
        assert!(matches!(
            CheckpointMetadata::decode(&bytes),
            Err(SaveError::Order { .. })
        ));
    }

    #[test]
    fn the_self_entry_ancestor_must_equal_sk_parent() {
        let mut metadata = sample_metadata();
        let wrong = hash_object(b"not-sk-parent");
        metadata.skips[0] = SkipEntry::self_skip(wrong);
        let bytes = metadata.canonical_bytes();
        assert!(matches!(
            CheckpointMetadata::decode(&bytes),
            Err(SaveError::Integrity(
                "checkpoint-metadata/self-ancestor-mismatch"
            ))
        ));
    }

    #[test]
    fn a_reclaimed_sk_parent_redirects_the_self_target_to_retained() {
        // SK stood on a range publication — sk_parent itself reclaimed
        // with the range (SK is retired together with it): the self
        // entry must jump past the whole reclaim run to a RETAINED
        // revision, and the strict target-equals-sk_parent rule is
        // correctly suspended (it applies only while sk_parent is
        // retained).
        let sk = sample_sk();
        let founding = hash_object(b"founding");
        let reclaimed_pub = hash_object(b"reclaimed-publication");
        let index = vec![ReclaimedIndexEntry::new(
            Some(b"key".to_vec()),
            reclaimed_pub,
            2,
        )];
        let (digest, seed) = compute_range_digest(&founding, &[b"record-bytes"]);

        // The WRONG form: the self entry still targeting the
        // reclaimed parent itself.
        let wrong = CheckpointMetadata::new(
            sk,
            reclaimed_pub,
            vec![SkipEntry::self_skip(reclaimed_pub)],
            index.clone(),
            digest,
            seed,
        );
        assert!(matches!(
            CheckpointMetadata::decode(&wrong.canonical_bytes()),
            Err(SaveError::Integrity(
                "checkpoint-metadata/skip-target-reclaimed"
            ))
        ));

        // The correct form: the self entry jumps to the nearest
        // retained ancestor — the boundary below the run.
        let right = CheckpointMetadata::new(
            sk,
            reclaimed_pub,
            vec![SkipEntry::self_skip(founding)],
            index,
            digest,
            seed,
        );
        let restored = CheckpointMetadata::decode(&right.canonical_bytes())
            .expect("the retained self target decodes");
        assert_eq!(
            restored
                .self_skip()
                .map(|entry| entry.nearest_retained_ancestor),
            Some(founding)
        );
    }

    #[test]
    fn a_skip_standing_on_sk_is_refused() {
        let mut metadata = sample_metadata();
        let sk = metadata.sk;
        // A "retained" entry naming SK itself: dead by construction.
        metadata.skips.push(SkipEntry::new(sk, metadata.sk_parent));
        metadata.skips.sort_unstable_by_key(|entry| entry.retained);
        let bytes = metadata.canonical_bytes();
        assert!(matches!(
            CheckpointMetadata::decode(&bytes),
            Err(SaveError::Integrity("checkpoint-metadata/skip-on-sk"))
        ));
    }

    #[test]
    fn a_skip_standing_on_a_reclaimed_revision_is_refused() {
        let mut metadata = sample_metadata();
        let reclaimed = metadata.index[0].hash;
        metadata
            .skips
            .push(SkipEntry::new(reclaimed, metadata.sk_parent));
        metadata.skips.sort_unstable_by_key(|entry| entry.retained);
        let bytes = metadata.canonical_bytes();
        assert!(matches!(
            CheckpointMetadata::decode(&bytes),
            Err(SaveError::Integrity(
                "checkpoint-metadata/skip-on-reclaimed"
            ))
        ));
    }

    #[test]
    fn range_records_must_match_the_index_length() {
        let metadata = sample_metadata();
        let mut bytes = metadata.canonical_bytes();
        // The final field is the 8-byte record count — flip it.
        let count_at = bytes.len() - 8;
        bytes[count_at..count_at + 8].copy_from_slice(&(metadata.range_records + 1).to_le_bytes());
        assert!(matches!(
            CheckpointMetadata::decode(&bytes),
            Err(SaveError::Integrity("checkpoint-metadata/range-count"))
        ));
    }

    #[test]
    fn the_digest_is_deterministic_and_range_sensitive() {
        let founding = hash_object(b"founding");
        let range_a = [b"record-one" as &[u8], b"record-two"];
        let range_b = [b"record-one" as &[u8], b"record-X"];
        let (digest_a, seed_a) = compute_range_digest(&founding, &range_a);
        let (digest_a2, seed_a2) = compute_range_digest(&founding, &range_a);
        assert_eq!(digest_a, digest_a2);
        assert_eq!(seed_a, seed_a2);
        let (digest_b, seed_b) = compute_range_digest(&founding, &range_b);
        assert_ne!(digest_a, digest_b, "the digest is range-sensitive");
        assert_eq!(seed_a, seed_b, "the seed is founding-derived");
        // The empty range yields the seed itself.
        let empty_range: [&[u8]; 0] = [];
        let (empty, seed_e) = compute_range_digest(&founding, &empty_range);
        assert_eq!(empty, seed_e);
        assert_eq!(seed_e, seed_a);
        // A different founding changes the seed (and the digest).
        let other_founding = hash_object(b"other-founding");
        let (digest_c, seed_c) = compute_range_digest(&other_founding, &range_a);
        assert_ne!(seed_a, seed_c);
        assert_ne!(digest_a, digest_c);
        // The order matters (the oldest-first chain).
        let reversed = [b"record-two" as &[u8], b"record-one"];
        let (digest_r, _) = compute_range_digest(&founding, &reversed);
        assert_ne!(digest_a, digest_r);
    }

    #[test]
    fn the_mk_key_is_deterministic_in_sk() {
        let sk = sample_sk();
        assert_eq!(
            checkpoint_mk_idempotency_key(&sk),
            checkpoint_mk_idempotency_key(&sk)
        );
        assert_ne!(
            checkpoint_mk_idempotency_key(&sk),
            checkpoint_mk_idempotency_key(&hash_object(b"other"))
        );
    }
}
