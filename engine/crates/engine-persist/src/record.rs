//! The canonical hash-chained revision record (`revision-record/1`).
//!
//! A revision's identity is the SHA-256 of its canonical bytes (the
//! content-addressed object under `objects/<hex>`), and the chain is
//! linked through parent references walked from HEAD down to the
//! founding record. Chain invariants — epoch progression, parent
//! coherence, idempotency-key uniqueness, reference existence — are
//! enforced by the medium's validation walks, not by this codec; the
//! codec enforces the intra-record structure strictly and fail-closed.
//!
//! **Provisional format choices (P1, flagged per the authorization):**
//! the idempotency key's shape and semantics (the B3 pipeline assigns
//! meaning; B2 enforces chain-wide uniqueness) and the campaign-id
//! semantics. No signature fields exist and none are fabricated — a
//! content hash is integrity, never authentication.

use engine_core::{Decoder, Encoder, SaveError};

use crate::hash::{Hash, hash_object};

/// The revision-record domain (§5.1 domain separation: this tag is the
/// first field of the canonical bytes and is covered by the hash).
pub const RECORD_DOMAIN: &str = "sektor7/engine-persist/revision-record/1";
pub const RECORD_VERSION: u32 = 1;

/// Documented maxima (bounded decode, checked before allocation).
pub const MAX_IDEMPOTENCY_KEY_LEN: u64 = 1 << 16;
pub const MAX_CAMPAIGN_ID_LEN: u64 = 1 << 16;
pub const MAX_HASH_LIST_LEN: u64 = 1 << 16;

/// Campaign authority mode (§3.3) — founding metadata. `Standalone`
/// campaigns treat the local store as their canonical medium
/// (`T3-local`, never presented as replication); `Shared` campaigns
/// defer to a shared canonical medium (the mode is stored; shared
/// providers and the conversion protocol are future authorized
/// scope). No silent transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityMode {
    Standalone,
    Shared,
}

/// What a revision record is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordKind {
    /// The chain root: authority mode + campaign identity. Exactly one
    /// founding record exists per chain (walk-enforced).
    Founding {
        authority: AuthorityMode,
        campaign_id: String,
    },
    /// A descendant revision.
    Regular,
}

/// One canonical revision. The identity is
/// [`RevisionRecord::id`](Self::id) — the SHA-256 of
/// [`canonical_bytes`](Self::canonical_bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionRecord {
    pub kind: RecordKind,
    /// Chain position: the founding record is epoch 0; every regular
    /// record is parent epoch + 1. Enforced by validation walks — a
    /// regression fails closed.
    pub epoch: u64,
    /// The parent revision's identity (`None` only for the founding
    /// record — decode enforces the kind coherence).
    pub parent: Option<Hash>,
    /// Optional idempotency key, unique across the chain (the B3
    /// pipeline's dedup anchor; B2 enforces uniqueness on walks and
    /// at append).
    pub idempotency_key: Option<Vec<u8>>,
    /// Informational campaign tick at commit time.
    pub tick: u64,
    /// Content objects (snapshots/blobs) referenced by this revision —
    /// strictly ascending canonical order, unique.
    pub content_refs: Vec<Hash>,
    /// Additional prior revisions this record explicitly requires —
    /// strictly ascending canonical order, unique.
    pub depends_on: Vec<Hash>,
}

impl RevisionRecord {
    /// Builds the founding record (epoch 0, no parent).
    pub fn founding(authority: AuthorityMode, campaign_id: impl Into<String>, tick: u64) -> Self {
        RevisionRecord {
            kind: RecordKind::Founding {
                authority,
                campaign_id: campaign_id.into(),
            },
            epoch: 0,
            parent: None,
            idempotency_key: None,
            tick,
            content_refs: Vec::new(),
            depends_on: Vec::new(),
        }
    }

    /// Builds a regular record on top of `parent` (epoch
    /// `parent_epoch + 1`).
    pub fn regular(parent: Hash, parent_epoch: u64, tick: u64) -> Self {
        RevisionRecord {
            kind: RecordKind::Regular,
            epoch: parent_epoch + 1,
            parent: Some(parent),
            idempotency_key: None,
            tick,
            content_refs: Vec::new(),
            depends_on: Vec::new(),
        }
    }

    /// Attaches an idempotency key (chain-unique).
    pub fn with_idempotency_key(mut self, key: Vec<u8>) -> Self {
        self.idempotency_key = Some(key);
        self
    }

    /// Attaches content references (normalized to ascending canonical
    /// order with duplicates removed — the decode-side strict check
    /// makes any noncanonical construction unrepresentable on the
    /// wire).
    pub fn with_content_refs(mut self, mut refs: Vec<Hash>) -> Self {
        refs.sort_unstable();
        refs.dedup();
        self.content_refs = refs;
        self
    }

    /// Attaches dependency references (normalized like content refs).
    pub fn with_depends_on(mut self, mut deps: Vec<Hash>) -> Self {
        deps.sort_unstable();
        deps.dedup();
        self.depends_on = deps;
        self
    }

    /// Canonical bytes: the domain tag first (covered by the hash),
    /// then the versioned fields. Hash lists are written in the
    /// record's stored order — canonical order is enforced by decode,
    /// so noncanonical input cannot round-trip.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_str(RECORD_DOMAIN);
        enc.write_u32(RECORD_VERSION);
        match &self.kind {
            RecordKind::Founding {
                authority,
                campaign_id,
            } => {
                enc.write_u8(0);
                enc.write_u8(match authority {
                    AuthorityMode::Standalone => 0,
                    AuthorityMode::Shared => 1,
                });
                enc.write_str(campaign_id);
            }
            RecordKind::Regular => enc.write_u8(1),
        }
        enc.write_u64(self.epoch);
        match self.parent {
            Some(parent) => {
                enc.write_u8(1);
                enc.write_bytes(parent.as_bytes());
            }
            None => enc.write_u8(0),
        }
        match &self.idempotency_key {
            Some(key) => {
                enc.write_u8(1);
                enc.write_bytes(key);
            }
            None => enc.write_u8(0),
        }
        enc.write_u64(self.tick);
        enc.write_count(self.content_refs.len());
        for hash in &self.content_refs {
            enc.write_bytes(hash.as_bytes());
        }
        enc.write_count(self.depends_on.len());
        for hash in &self.depends_on {
            enc.write_bytes(hash.as_bytes());
        }
        enc.into_bytes()
    }

    /// The revision's identity: SHA-256 over the canonical bytes.
    pub fn id(&self) -> Hash {
        hash_object(&self.canonical_bytes())
    }

    /// Strict decode from canonical bytes (fail-closed: domain,
    /// version, enum discriminants, kind/parent coherence, bounded
    /// counts, strictly ascending unique hash lists, no trailing).
    pub fn decode(bytes: &[u8]) -> Result<Self, SaveError> {
        const SECTION: &str = "revision-record";
        let mut dec = Decoder::new(bytes);
        dec.read_domain(RECORD_DOMAIN)?;
        let version = dec.read_u32()?;
        if version != RECORD_VERSION {
            return Err(SaveError::Version {
                observed: version,
                supported: RECORD_VERSION,
            });
        }
        let kind = match dec.read_u8()? {
            0 => {
                let authority = match dec.read_u8()? {
                    0 => AuthorityMode::Standalone,
                    1 => AuthorityMode::Shared,
                    discriminant => {
                        return Err(SaveError::Enum {
                            section: SECTION,
                            discriminant: u64::from(discriminant),
                        });
                    }
                };
                let campaign_id = dec.read_str(MAX_CAMPAIGN_ID_LEN, SECTION)?.to_owned();
                RecordKind::Founding {
                    authority,
                    campaign_id,
                }
            }
            1 => RecordKind::Regular,
            discriminant => {
                return Err(SaveError::Enum {
                    section: SECTION,
                    discriminant: u64::from(discriminant),
                });
            }
        };
        let epoch = dec.read_u64()?;
        let parent = match dec.read_u8()? {
            0 => None,
            1 => Some(read_hash(&mut dec, SECTION)?),
            discriminant => {
                return Err(SaveError::Enum {
                    section: SECTION,
                    discriminant: u64::from(discriminant),
                });
            }
        };
        match (&kind, parent) {
            (RecordKind::Founding { .. }, None) | (RecordKind::Regular, Some(_)) => {}
            _ => {
                return Err(SaveError::Integrity(
                    "revision-record/kind-parent-coherence",
                ));
            }
        }
        let idempotency_key = match dec.read_u8()? {
            0 => None,
            1 => Some(dec.read_bytes(MAX_IDEMPOTENCY_KEY_LEN, SECTION)?.to_vec()),
            discriminant => {
                return Err(SaveError::Enum {
                    section: SECTION,
                    discriminant: u64::from(discriminant),
                });
            }
        };
        let tick = dec.read_u64()?;
        let content_refs = read_hash_list(&mut dec, SECTION)?;
        let depends_on = read_hash_list(&mut dec, SECTION)?;
        dec.finish()?;
        Ok(RevisionRecord {
            kind,
            epoch,
            parent,
            idempotency_key,
            tick,
            content_refs,
            depends_on,
        })
    }
}

fn read_hash(dec: &mut Decoder<'_>, section: &'static str) -> Result<Hash, SaveError> {
    let bytes = dec.read_bytes(Hash::WIDTH, section)?;
    Hash::from_bytes(bytes).ok_or(SaveError::Integrity("revision-record/hash-width"))
}

fn read_hash_list(dec: &mut Decoder<'_>, section: &'static str) -> Result<Vec<Hash>, SaveError> {
    let count = dec.read_count(MAX_HASH_LIST_LEN, section, "entry-count")?;
    // Each encoded reference is a length prefix (8) plus 32 bytes.
    let per_entry = 8 + Hash::WIDTH;
    let remaining = dec.remaining() as u64;
    if count > remaining / per_entry {
        return Err(SaveError::Limit {
            section,
            kind: "entry-count",
            value: count,
            max: remaining / per_entry,
        });
    }
    let mut list = Vec::with_capacity(count as usize);
    let mut prev: Option<Hash> = None;
    for _ in 0..count {
        let hash = read_hash(dec, section)?;
        if let Some(prior) = prev
            && hash <= prior
        {
            // Strictly ascending canonical order: duplicates and
            // disorder are integrity failures, never repaired.
            return Err(SaveError::Order { section });
        }
        prev = Some(hash);
        list.push(hash);
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_founding() -> RevisionRecord {
        RevisionRecord::founding(AuthorityMode::Standalone, "test-campaign", 7)
            .with_idempotency_key(b"key-alpha".to_vec())
    }

    fn sample_regular() -> RevisionRecord {
        let parent = sample_founding().id();
        let first = hash_object(b"content-one");
        let second = hash_object(b"content-two");
        RevisionRecord::regular(parent, 0, 12)
            .with_idempotency_key(b"key-beta".to_vec())
            .with_content_refs(vec![second, first])
            .with_depends_on(vec![parent])
    }

    #[test]
    fn founding_round_trips() {
        let record = sample_founding();
        let bytes = record.canonical_bytes();
        assert_eq!(RevisionRecord::decode(&bytes).unwrap(), record);
    }

    #[test]
    fn regular_round_trips_with_canonical_ref_order() {
        let record = sample_regular();
        // The builder normalized [second, first] to ascending order.
        assert!(record.content_refs.windows(2).all(|w| w[0] < w[1]));
        let bytes = record.canonical_bytes();
        assert_eq!(RevisionRecord::decode(&bytes).unwrap(), record);
    }

    #[test]
    fn identity_is_deterministic_and_sensitive() {
        let a = sample_regular();
        let b = sample_regular();
        assert_eq!(a.id(), b.id());

        let mut shifted = sample_regular();
        shifted.tick += 1;
        assert_ne!(a.id(), shifted.id());

        let rekeyed = sample_regular().with_idempotency_key(b"other".to_vec());
        assert_ne!(a.id(), rekeyed.id());
    }

    #[test]
    fn domain_and_version_are_rejected_by_name() {
        let bytes = sample_founding().canonical_bytes();

        let mut wrong_domain = Vec::new();
        let mut enc = Encoder::new();
        enc.write_str("sektor7/other/1");
        wrong_domain.extend_from_slice(&enc.into_bytes());
        wrong_domain.extend_from_slice(&bytes[8 + RECORD_DOMAIN.len()..]);
        assert!(matches!(
            RevisionRecord::decode(&wrong_domain),
            Err(SaveError::Domain { .. })
        ));

        let mut wrong_version = bytes.clone();
        let version_at = 8 + RECORD_DOMAIN.len();
        wrong_version[version_at..version_at + 4].copy_from_slice(&2u32.to_le_bytes());
        assert!(matches!(
            RevisionRecord::decode(&wrong_version),
            Err(SaveError::Version { observed: 2, .. })
        ));
    }

    #[test]
    fn truncation_rejected_at_every_byte_boundary() {
        let bytes = sample_regular().canonical_bytes();
        for cut in 0..bytes.len() {
            assert!(
                RevisionRecord::decode(&bytes[..cut]).is_err(),
                "a {cut}-byte prefix must not decode"
            );
        }
        assert!(RevisionRecord::decode(&bytes).is_ok());
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = sample_founding().canonical_bytes();
        bytes.push(0);
        assert!(matches!(
            RevisionRecord::decode(&bytes),
            Err(SaveError::Trailing { .. })
        ));
    }

    #[test]
    fn kind_parent_coherence_is_enforced() {
        // A founding record claiming a parent provides no hash bytes —
        // truncated.
        let founding_claims_parent = {
            let mut enc = Encoder::new();
            enc.write_str(RECORD_DOMAIN);
            enc.write_u32(RECORD_VERSION);
            enc.write_u8(0); // founding
            enc.write_u8(0); // standalone
            enc.write_str("c");
            enc.write_u64(0); // epoch
            enc.write_u8(1); // claims a parent, but no hash follows
            enc.into_bytes()
        };
        assert!(RevisionRecord::decode(&founding_claims_parent).is_err());

        // A regular record claiming no parent is structurally
        // incoherent.
        let regular_without_parent = {
            let mut enc = Encoder::new();
            enc.write_str(RECORD_DOMAIN);
            enc.write_u32(RECORD_VERSION);
            enc.write_u8(1); // regular
            enc.write_u64(1); // epoch
            enc.write_u8(0); // no parent — incoherent for regular
            enc.write_u8(0); // no key
            enc.write_u64(0); // tick
            enc.write_count(0);
            enc.write_count(0);
            enc.into_bytes()
        };
        assert!(matches!(
            RevisionRecord::decode(&regular_without_parent),
            Err(SaveError::Integrity(
                "revision-record/kind-parent-coherence"
            ))
        ));
    }

    #[test]
    fn noncanonical_hash_lists_are_rejected() {
        let first = hash_object(b"one");
        let second = hash_object(b"two");
        let (low, high) = if first < second {
            (first, second)
        } else {
            (second, first)
        };

        // Descending order.
        let mut enc = Encoder::new();
        enc.write_str(RECORD_DOMAIN);
        enc.write_u32(RECORD_VERSION);
        enc.write_u8(1);
        enc.write_u64(1);
        enc.write_u8(1);
        enc.write_bytes(high.as_bytes());
        enc.write_u8(0);
        enc.write_u64(0);
        enc.write_count(2);
        enc.write_bytes(high.as_bytes());
        enc.write_bytes(low.as_bytes());
        enc.write_count(0);
        let bytes = enc.into_bytes();
        assert!(matches!(
            RevisionRecord::decode(&bytes),
            Err(SaveError::Order { .. })
        ));

        // Duplicate identity.
        let mut enc = Encoder::new();
        enc.write_str(RECORD_DOMAIN);
        enc.write_u32(RECORD_VERSION);
        enc.write_u8(1);
        enc.write_u64(1);
        enc.write_u8(1);
        enc.write_bytes(low.as_bytes());
        enc.write_u8(0);
        enc.write_u64(0);
        enc.write_count(2);
        enc.write_bytes(low.as_bytes());
        enc.write_bytes(low.as_bytes());
        enc.write_count(0);
        let bytes = enc.into_bytes();
        assert!(matches!(
            RevisionRecord::decode(&bytes),
            Err(SaveError::Order { .. })
        ));
    }

    #[test]
    fn oversized_counts_are_rejected_before_allocation() {
        let mut enc = Encoder::new();
        enc.write_str(RECORD_DOMAIN);
        enc.write_u32(RECORD_VERSION);
        enc.write_u8(1);
        enc.write_u64(1);
        enc.write_u8(1);
        enc.write_bytes(hash_object(b"p").as_bytes());
        enc.write_u8(0);
        enc.write_u64(0);
        enc.write_u64(1 << 20); // a count far beyond the remaining bytes
        enc.write_count(0);
        let bytes = enc.into_bytes();
        assert!(matches!(
            RevisionRecord::decode(&bytes),
            Err(SaveError::Limit { .. })
        ));
    }
}
