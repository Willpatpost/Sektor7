//! (V2X-CP3-A0) The ancestry-witness wire codecs — the owner-ratified
//! DARK format (the R10 CLOSED-PASS wire specification with the final
//! errata). The witness retains the COMPLETE canonical preimage of
//! every indexed retiring revision of one checkpoint's range, so a
//! post-deletion verifier can recover each member's authenticated
//! original parent linkage from the permanent index commitments
//! (`ReclaimedIndexEntry.hash == SHA-256(RevisionRecord
//! ::canonical_bytes())`) — the record-preimage design that closes
//! the metadata-only ancestry insufficiency of the CP3 design
//! reviews.
//!
//! **DARK (CP3-A0 scope, absolute):** nothing in this module is
//! reachable from any production execution, publication, recovery,
//! or campaign-crossing flow. There is NO publication admission, NO
//! provider persistence, NO generic-append gate, NO crossing
//! recognition, and NO consumer of this format anywhere in the
//! codebase — the codecs, the deterministic construction, and the
//! standalone validation helpers exist for the independently
//! reviewed wire contract and its golden-vector regressions only.
//! Every later integration is separately gated (CP3-A1+).
//!
//! **The ratified wire contract:** two versioned domains under one
//! recognized family; the 273-byte manifest (the MK / plan-revision
//! / CP2-begin / metadata-object bindings, the member and shard
//! counts, and the coverage digest); the sharded member preimages
//! under the fixed 4,194,304-byte payload ceiling (bound to the
//! existing `MIN_SUPPORTED_EFFECTIVE_OBJECT_BYTES` — no new
//! numerical value); the 4,194,121-byte maximum standalone
//! preimage with the fail-closed oversized refusal; the canonical
//! member order (the index epochs descending, the identity
//! ascending as the deterministic tiebreak); the deterministic
//! greedy maximality sharding; the coverage-digest recipe; and the
//! exact identity-by-identity all-shards-complete coverage. The
//! entry validation authenticates the preimage's SHA-256 against
//! its indexed identity, strict-decodes it as a canonical REGULAR
//! revision record (the retirement members are regular
//! publications), and — in the standalone helpers — checks the
//! epoch and idempotency-key fields against the complete index
//! entry and the parent-edge coherence rules that witness-plus-
//! index evidence alone can prove.

use engine_core::{Decoder, Encoder, SaveError};

use crate::checkpoint::ReclaimedIndexEntry;
use crate::hash::{Hash, hash_object};
use crate::record::{RecordKind, RevisionRecord};

/// The witness manifest's versioned domain (45 bytes).
pub const RECLAMATION_WITNESS_MANIFEST_DOMAIN: &str =
    "engine-persist/reclamation-ancestry-witness/1";

/// The witness manifest's version.
pub const RECLAMATION_WITNESS_MANIFEST_VERSION: u32 = 1;

/// The witness shard's versioned domain (51 bytes).
pub const RECLAMATION_WITNESS_SHARD_DOMAIN: &str =
    "engine-persist/reclamation-ancestry-witness-shard/1";

/// The witness shard's version.
pub const RECLAMATION_WITNESS_SHARD_VERSION: u32 = 1;

/// The ancestry-witness family prefix — every version in this
/// family is refused by the future generic publication path (the
/// established W2-A discipline; the gate itself is CP3-A1 scope,
/// NOT implemented here).
pub const RECLAMATION_WITNESS_FAMILY: &str = "engine-persist/reclamation-ancestry-witness";

/// Whether a domain belongs to the recognized ancestry-witness
/// family (any version).
pub fn is_reclamation_ancestry_witness_family(domain: &str) -> bool {
    domain.starts_with(RECLAMATION_WITNESS_FAMILY)
}

/// The manifest record's future idempotency-key domain (defined by
/// the ratified wire; NO publication path exists in CP3-A0).
pub const RECLAMATION_WITNESS_KEY_DOMAIN: &str =
    "engine-persist/reclamation-ancestry-witness-key/1";

/// The shard records' future idempotency-key domain.
pub const RECLAMATION_WITNESS_SHARD_KEY_DOMAIN: &str =
    "engine-persist/reclamation-ancestry-witness-shard-key/1";

/// The manifest publication's deterministic MK-bound key (one
/// witness manifest per checkpoint, ever — the established
/// begin/plan key discipline). Dormant: defined for the future
/// admission only.
#[cfg_attr(not(test), allow(dead_code))]
pub fn reclamation_witness_manifest_idempotency_key(mk: &Hash) -> Vec<u8> {
    format!("{RECLAMATION_WITNESS_KEY_DOMAIN}:{}", mk.hex()).into_bytes()
}

/// The shard publications' deterministic keys (the MK and the shard
/// index). Dormant: defined for the future admission only.
#[cfg_attr(not(test), allow(dead_code))]
pub fn reclamation_witness_shard_idempotency_key(mk: &Hash, shard_index: u64) -> Vec<u8> {
    format!(
        "{RECLAMATION_WITNESS_SHARD_KEY_DOMAIN}:{}:{shard_index}",
        mk.hex()
    )
    .into_bytes()
}

/// The witness-shard payload ceiling — the owner-ratified
/// provider-independent constraint of this wire format, BINDING the
/// existing W3A floor constant (no new numerical value is
/// selected): every provider in a supported configuration can hold
/// every shard, and the deterministic layout never consults a
/// provider's effective capacity.
pub const WITNESS_SHARD_PAYLOAD_CEILING: u64 = crate::reclaim::MIN_SUPPORTED_EFFECTIVE_OBJECT_BYTES;

/// The fixed wire size of a shard's header through the entries
/// count: the 51-byte domain with its 8-byte framing (59), the u32
/// version (4), the 40-byte mk, three u64 fields (shard_index,
/// shard_count, member_count — 24), and the entries count (8) =
/// 135 bytes. A format constant: changing it changes the wire.
pub const WITNESS_SHARD_HEADER_BYTES: u64 = 135;

/// The fixed wire framing of one entry around its preimage: the
/// 40-byte member identity plus the 8-byte preimage length prefix
/// = 48 bytes. A format constant.
pub const WITNESS_ENTRY_FRAMING_BYTES: u64 = 48;

/// The maximum standalone member-preimage size (4,194,121 bytes):
/// the ceiling minus the header and the entry framing. A member
/// whose canonical preimage exceeds it cannot be witnessed — the
/// fail-closed oversized refusal that precedes any publication or
/// destructive operation (the ratified rule: the ~5.3 MiB
/// schema-corner record is a non-production shape; every production
/// snapshot publication is a few hundred bytes).
pub const MAX_WITNESS_MEMBER_PREIMAGE_BYTES: u64 =
    WITNESS_SHARD_PAYLOAD_CEILING - WITNESS_SHARD_HEADER_BYTES - WITNESS_ENTRY_FRAMING_BYTES;

/// The manifest's evidence bindings and census: the exact MK, the
/// installed plan revision, the accepted CP2 begin revision, the
/// checkpoint metadata object, the member and shard counts, and
/// the coverage digest over the canonical member order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AncestryWitnessManifest {
    /// The checkpoint's MK revision identity.
    pub mk: Hash,
    /// The installed plan revision's identity.
    pub plan_record: Hash,
    /// The accepted V2 begin revision's identity.
    pub begin_record: Hash,
    /// The MK's single content object (the checkpoint metadata).
    pub metadata_object: Hash,
    /// The witnessed member count (== the checkpoint index length).
    pub member_count: u64,
    /// The shard count.
    pub shard_count: u64,
    /// The chained SHA-256 over the canonical member order.
    pub coverage_digest: [u8; 32],
}

impl AncestryWitnessManifest {
    /// Canonical bytes (273 bytes at version 1): the domain, the
    /// version, the four bound identities, the two counts, and the
    /// coverage digest — the exact ratified field order.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_str(RECLAMATION_WITNESS_MANIFEST_DOMAIN);
        enc.write_u32(RECLAMATION_WITNESS_MANIFEST_VERSION);
        enc.write_bytes(self.mk.as_bytes());
        enc.write_bytes(self.plan_record.as_bytes());
        enc.write_bytes(self.begin_record.as_bytes());
        enc.write_bytes(self.metadata_object.as_bytes());
        enc.write_u64(self.member_count);
        enc.write_u64(self.shard_count);
        enc.write_bytes(&self.coverage_digest);
        enc.into_bytes()
    }

    /// Strict decode: the exact domain and version, non-zero
    /// identities, the bounded counts (at least one member; at
    /// least one shard; never more shards than members; the member
    /// count within the plan's aggregate ceiling), the exact-width
    /// digest, and no trailing bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, SaveError> {
        const SECTION: &str = "ancestry-witness-manifest";
        let mut dec = Decoder::new(bytes);
        dec.read_domain(RECLAMATION_WITNESS_MANIFEST_DOMAIN)?;
        let version = dec.read_u32()?;
        if version != RECLAMATION_WITNESS_MANIFEST_VERSION {
            return Err(SaveError::Version {
                observed: version,
                supported: RECLAMATION_WITNESS_MANIFEST_VERSION,
            });
        }
        let mk = read_hash(&mut dec, SECTION)?;
        let plan_record = read_hash(&mut dec, SECTION)?;
        let begin_record = read_hash(&mut dec, SECTION)?;
        let metadata_object = read_hash(&mut dec, SECTION)?;
        let member_count = dec.read_u64()?;
        if member_count == 0 || member_count > crate::reclaim::MAX_RECLAMATION_UNITS {
            return Err(SaveError::Integrity(
                "ancestry-witness-manifest/member-count",
            ));
        }
        let shard_count = dec.read_u64()?;
        if shard_count == 0 || shard_count > member_count {
            return Err(SaveError::Integrity(
                "ancestry-witness-manifest/shard-count",
            ));
        }
        let coverage_digest = read_digest(&mut dec, SECTION)?;
        dec.finish()?;
        Ok(AncestryWitnessManifest {
            mk,
            plan_record,
            begin_record,
            metadata_object,
            member_count,
            shard_count,
            coverage_digest,
        })
    }
}

/// One witnessed member: the indexed identity and the member's
/// complete canonical revision-record preimage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessEntry {
    /// The member's indexed identity (== SHA-256 of the preimage).
    pub member: Hash,
    /// The member's complete canonical `RevisionRecord` bytes.
    pub preimage: Vec<u8>,
}

/// One witness shard: the bound MK, the deterministic index and
/// census fields, and the member entries in the canonical order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AncestryWitnessShard {
    /// The bound checkpoint's MK revision identity.
    pub mk: Hash,
    /// The shard's deterministic index (0-based).
    pub shard_index: u64,
    /// The total shard count (== the manifest's).
    pub shard_count: u64,
    /// The declared member (entry) count of this shard.
    pub member_count: u64,
    /// The entries, in the canonical member order.
    pub entries: Vec<WitnessEntry>,
}

impl AncestryWitnessShard {
    /// Canonical bytes: the domain, the version, the mk, the index
    /// and census fields, the declared member count, the entries
    /// count (== the declared member count), and the entries — the
    /// exact ratified field order. The encoded size never exceeds
    /// the payload ceiling for a legitimately constructed shard.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_str(RECLAMATION_WITNESS_SHARD_DOMAIN);
        enc.write_u32(RECLAMATION_WITNESS_SHARD_VERSION);
        enc.write_bytes(self.mk.as_bytes());
        enc.write_u64(self.shard_index);
        enc.write_u64(self.shard_count);
        enc.write_u64(self.member_count);
        enc.write_count(self.entries.len());
        for entry in &self.entries {
            enc.write_bytes(entry.member.as_bytes());
            enc.write_bytes(&entry.preimage);
        }
        enc.into_bytes()
    }

    /// Strict decode: the exact domain and version, the non-zero
    /// mk, the bounded index (below the shard count), the declared
    /// member count agreeing with the entries count, and every
    /// entry intrinsically validated — the preimage's SHA-256
    /// equals the declared member identity, the preimage
    /// strict-decodes as a canonical REGULAR revision record, and
    /// the accumulated framed size stays within the payload
    /// ceiling. No trailing bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, SaveError> {
        const SECTION: &str = "ancestry-witness-shard";
        let mut dec = Decoder::new(bytes);
        dec.read_domain(RECLAMATION_WITNESS_SHARD_DOMAIN)?;
        let version = dec.read_u32()?;
        if version != RECLAMATION_WITNESS_SHARD_VERSION {
            return Err(SaveError::Version {
                observed: version,
                supported: RECLAMATION_WITNESS_SHARD_VERSION,
            });
        }
        let mk = read_hash(&mut dec, SECTION)?;
        let shard_index = dec.read_u64()?;
        let shard_count = dec.read_u64()?;
        if shard_count == 0 || shard_index >= shard_count {
            return Err(SaveError::Integrity(
                "ancestry-witness-shard/index-incoherent",
            ));
        }
        let member_count = dec.read_u64()?;
        if member_count == 0 {
            return Err(SaveError::Integrity("ancestry-witness-shard/empty"));
        }
        let entries_count = dec.read_count(
            (WITNESS_SHARD_PAYLOAD_CEILING - WITNESS_SHARD_HEADER_BYTES)
                / WITNESS_ENTRY_FRAMING_BYTES,
            SECTION,
            "entries-count",
        )?;
        if entries_count != member_count {
            return Err(SaveError::Integrity(
                "ancestry-witness-shard/count-mismatch",
            ));
        }
        let mut entries = Vec::with_capacity(entries_count as usize);
        let mut framed = WITNESS_SHARD_HEADER_BYTES;
        for _ in 0..entries_count {
            let member = read_hash(&mut dec, SECTION)?;
            let preimage = dec
                .read_bytes(MAX_WITNESS_MEMBER_PREIMAGE_BYTES, SECTION)?
                .to_vec();
            framed += WITNESS_ENTRY_FRAMING_BYTES + preimage.len() as u64;
            if framed > WITNESS_SHARD_PAYLOAD_CEILING {
                return Err(SaveError::Limit {
                    section: SECTION,
                    kind: "shard-payload",
                    value: framed,
                    max: WITNESS_SHARD_PAYLOAD_CEILING,
                });
            }
            let entry = WitnessEntry { member, preimage };
            decode_validated_entry(&entry)?;
            entries.push(entry);
        }
        dec.finish()?;
        Ok(AncestryWitnessShard {
            mk,
            shard_index,
            shard_count,
            member_count,
            entries,
        })
    }
}

/// The intrinsic per-entry validation (the shared authentication
/// point used by the shard decode, every standalone helper, and
/// the construction): the preimage's SHA-256 equals the declared
/// member identity (the indexed commitment — a substituted or
/// altered preimage refuses), the preimage strict-decodes as a
/// canonical REGULAR revision record (the retirement members are
/// regular publications; a founding or malformed record refuses),
/// and a regular retirement member never sits at epoch zero (its
/// parent would need a negative chain height — the impossible
/// ancestry). Returns the decoded record so the callers never
/// re-decode.
fn decode_validated_entry(entry: &WitnessEntry) -> Result<RevisionRecord, SaveError> {
    if hash_object(&entry.preimage) != entry.member {
        return Err(SaveError::Integrity(
            "ancestry-witness-shard/preimage-hash-mismatch",
        ));
    }
    let record = RevisionRecord::decode(&entry.preimage)?;
    if !matches!(record.kind, RecordKind::Regular) {
        return Err(SaveError::Integrity(
            "ancestry-witness-shard/member-not-regular",
        ));
    }
    if record.epoch == 0 {
        return Err(SaveError::Integrity(
            "ancestry-witness-shard/member-epoch-zero",
        ));
    }
    Ok(record)
}

/// The canonical member order: the index entries sorted by epoch
/// DESCENDING with the identity ASCENDING as the deterministic
/// tiebreak (the same derivation as the accepted membership proof;
/// genuine epochs are unique chain heights, and the tiebreak makes
/// the order total even for adversarial duplicate-epoch metadata).
pub fn canonical_member_order(index: &[ReclaimedIndexEntry]) -> Vec<&ReclaimedIndexEntry> {
    let mut ordered: Vec<&ReclaimedIndexEntry> = index.iter().collect();
    ordered.sort_unstable_by(|a, b| b.epoch.cmp(&a.epoch).then_with(|| a.hash.cmp(&b.hash)));
    ordered
}

/// The coverage digest: the chained SHA-256 over the canonical
/// member order — seeded from the raw manifest domain bytes, the
/// MK, and the member count, then folded over each member identity
/// in order. Deterministic and streaming-friendly.
pub fn witness_coverage_digest(
    mk: &Hash,
    member_count: u64,
    ordered_identities: impl Iterator<Item = Hash>,
) -> [u8; 32] {
    let mut seed_input = Vec::with_capacity(RECLAMATION_WITNESS_MANIFEST_DOMAIN.len() + 40);
    seed_input.extend_from_slice(RECLAMATION_WITNESS_MANIFEST_DOMAIN.as_bytes());
    seed_input.extend_from_slice(mk.as_bytes());
    seed_input.extend_from_slice(&member_count.to_le_bytes());
    let mut digest = *hash_object(&seed_input).as_bytes();
    for identity in ordered_identities {
        let mut fold_input = [0u8; 64];
        fold_input[..32].copy_from_slice(&digest);
        fold_input[32..].copy_from_slice(identity.as_bytes());
        digest = *hash_object(&fold_input).as_bytes();
    }
    digest
}

/// The deterministic witness construction. Derives the RATIFIED
/// canonical member order (the decoded epochs descending, the
/// member identity ascending as the tiebreak) from the
/// authenticated member metadata itself — the same evidence
/// supplied in ANY iteration order produces byte-identical
/// canonical witness bytes. Before construction: the manifest
/// binding identities are real (non-zero); the member census is
/// within the plan's aggregate ceiling; no member identity repeats;
/// no two members share an epoch (the unique chain heights — a
/// duplicate epoch is the impossible ancestry); every entry passes
/// the intrinsic validation; and no preimage exceeds the standalone
/// maximum (the fail-closed oversized refusal — before any
/// publication or destructive operation, which do not exist in
/// this checkpoint). Every emitted manifest and shard passes its
/// own strict decode before the constructor returns.
///
/// **The memory contract (honest):** the dark construction holds
/// the ENTIRE witness in memory — every member preimage and every
/// assembled shard (the entries are MOVED into the shards, never
/// cloned, but the aggregate is O(total witness bytes), up to the
/// ~256 GiB schema maximum). The bounded-memory streaming
/// publication and consumption belong to the separately authorized
/// CP3-A1/A2 integration; this constructor is the A0 deterministic
/// reference form.
pub fn construct_witness(
    mk: &Hash,
    plan_record: &Hash,
    begin_record: &Hash,
    metadata_object: &Hash,
    members: Vec<WitnessEntry>,
) -> Result<(AncestryWitnessManifest, Vec<AncestryWitnessShard>), SaveError> {
    if *mk == Hash::ZERO
        || *plan_record == Hash::ZERO
        || *begin_record == Hash::ZERO
        || *metadata_object == Hash::ZERO
    {
        return Err(SaveError::Integrity(
            "ancestry-witness/zero-binding-identity",
        ));
    }
    let member_count = members.len() as u64;
    if member_count == 0 || member_count > crate::reclaim::MAX_RECLAMATION_UNITS {
        return Err(SaveError::Integrity("ancestry-witness/member-count"));
    }
    // The authenticated member metadata: the decoded epoch per
    // entry (the intrinsic validation included), the duplicate
    // checks, and the canonical order derivation.
    let mut decoded: Vec<(WitnessEntry, u64)> = Vec::with_capacity(members.len());
    let mut seen_members = std::collections::BTreeSet::new();
    let mut seen_epochs = std::collections::BTreeSet::new();
    for entry in members {
        let record = decode_validated_entry(&entry)?;
        if !seen_members.insert(entry.member) {
            return Err(SaveError::Integrity(
                "ancestry-witness/duplicate-member-identity",
            ));
        }
        if !seen_epochs.insert(record.epoch) {
            return Err(SaveError::Integrity("ancestry-witness/duplicate-epoch"));
        }
        decoded.push((entry, record.epoch));
    }
    // The ratified canonical order: epoch DESCENDING, identity
    // ASCENDING — derived from the authenticated metadata,
    // independent of the input iteration order.
    decoded.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.member.cmp(&b.0.member)));
    let ordered_identities: Vec<Hash> = decoded.iter().map(|(entry, _)| entry.member).collect();
    let ordered: Vec<WitnessEntry> = decoded.into_iter().map(|(entry, _)| entry).collect();
    let mut shards: Vec<Vec<WitnessEntry>> = Vec::new();
    let mut current: Vec<WitnessEntry> = Vec::new();
    let mut current_size = WITNESS_SHARD_HEADER_BYTES;
    for entry in ordered {
        let framed = WITNESS_ENTRY_FRAMING_BYTES + entry.preimage.len() as u64;
        if WITNESS_SHARD_HEADER_BYTES + framed > WITNESS_SHARD_PAYLOAD_CEILING {
            return Err(SaveError::Limit {
                section: "ancestry-witness",
                kind: "member-preimage-oversized",
                value: entry.preimage.len() as u64,
                max: MAX_WITNESS_MEMBER_PREIMAGE_BYTES,
            });
        }
        if !current.is_empty() && current_size + framed > WITNESS_SHARD_PAYLOAD_CEILING {
            let closed = std::mem::take(&mut current);
            current_size = WITNESS_SHARD_HEADER_BYTES;
            shards.push(closed);
        }
        current_size += framed;
        current.push(entry);
    }
    shards.push(current);
    let shard_count = shards.len() as u64;
    let coverage_digest = witness_coverage_digest(mk, member_count, ordered_identities.into_iter());
    let manifest = AncestryWitnessManifest {
        mk: *mk,
        plan_record: *plan_record,
        begin_record: *begin_record,
        metadata_object: *metadata_object,
        member_count,
        shard_count,
        coverage_digest,
    };
    let shards: Vec<AncestryWitnessShard> = shards
        .into_iter()
        .enumerate()
        .map(|(index, entries)| AncestryWitnessShard {
            mk: *mk,
            shard_index: index as u64,
            shard_count,
            member_count: entries.len() as u64,
            entries,
        })
        .collect();
    // The self-verification: every emitted object passes its own
    // strict decode (the structural coherence, the ceiling, and
    // the intrinsic entry validation) before the constructor
    // returns.
    AncestryWitnessManifest::decode(&manifest.canonical_bytes())?;
    for shard in &shards {
        AncestryWitnessShard::decode(&shard.canonical_bytes())?;
    }
    Ok((manifest, shards))
}

/// The complete index-entry authentication (the standalone
/// helper). For every witnessed member this establishes: the
/// INTRINSIC preimage authentication (the SHA-256 equality with
/// the declared identity, the strict canonical REGULAR record
/// decode, the non-zero epoch) AND the complete index-entry
/// agreement — the indexed hash (by the intrinsic equality), the
/// decoded epoch, and the idempotency key with the Some/None shape
/// agreement in both directions. A member absent from the index
/// refuses. A directly constructed or mutated in-memory entry
/// whose preimage disagrees with its declared identity refuses
/// here — no caller needs the shard decoder for the hash fact.
pub fn authenticate_against_index(
    ordered_members: &[WitnessEntry],
    index: &[ReclaimedIndexEntry],
) -> Result<(), SaveError> {
    for entry in ordered_members {
        let Some(index_entry) = index
            .iter()
            .find(|candidate| candidate.hash == entry.member)
        else {
            return Err(SaveError::Integrity("ancestry-witness/index-member-absent"));
        };
        let record = decode_validated_entry(entry)?;
        if record.epoch != index_entry.epoch {
            return Err(SaveError::Integrity(
                "ancestry-witness/index-epoch-mismatch",
            ));
        }
        let keys_agree = match (&record.idempotency_key, &index_entry.key) {
            (Some(record_key), Some(index_key)) => record_key == index_key,
            (None, None) => true,
            _ => false,
        };
        if !keys_agree {
            return Err(SaveError::Integrity("ancestry-witness/index-key-mismatch"));
        }
    }
    Ok(())
}

/// The standalone parent-edge validation — exactly the rules that
/// witness-plus-index evidence can prove without any live-chain
/// descent (the full live ancestry validation is separately gated
/// for CP3-A1/A2): each member's decoded parent is non-zero; when
/// the parent is itself an indexed member, its indexed epoch must
/// be exactly the child's epoch minus one; and when the parent is
/// NOT indexed while the next canonical member sits at the
/// child's epoch minus one, the state is contradictory (two
/// distinct records claim one chain height) and refuses. An epoch
/// gap of two or more toward the next member is consistent with an
/// unindexed retained anchor at the intervening height, whose live
/// validation is deferred.
pub fn validate_parent_edges_standalone(
    ordered_members: &[WitnessEntry],
    index: &[ReclaimedIndexEntry],
) -> Result<(), SaveError> {
    for (position, entry) in ordered_members.iter().enumerate() {
        let record = decode_validated_entry(entry)?;
        let Some(parent) = record.parent else {
            return Err(SaveError::Integrity("ancestry-witness/parent-edge-missing"));
        };
        if parent == Hash::ZERO {
            return Err(SaveError::Integrity("ancestry-witness/parent-edge-zero"));
        }
        if let Some(parent_entry) = index.iter().find(|candidate| candidate.hash == parent) {
            // The CHECKED arithmetic: an indexed parent at u64::MAX
            // has no successor epoch — the typed refusal, identical
            // in debug and release, never a panic or a wrap.
            if parent_entry.epoch.checked_add(1) != Some(record.epoch) {
                return Err(SaveError::Integrity(
                    "ancestry-witness/parent-epoch-incoherent",
                ));
            }
        } else if let Some(next) = ordered_members.get(position + 1) {
            let next_record = decode_validated_entry(next)?;
            if next_record.epoch.checked_add(1) == Some(record.epoch) {
                return Err(SaveError::Integrity(
                    "ancestry-witness/parent-edge-contradictory",
                ));
            }
        }
    }
    Ok(())
}

/// The exact all-shards-complete coverage verification (the
/// standalone helper over supplied decoded or directly constructed
/// evidence): the shard census and indices form exactly
/// 0..shard_count−1; each shard's entry count equals its declared
/// member count; the summed member counts equal the manifest's;
/// every entry passes the INTRINSIC validation (the preimage
/// SHA-256 equality, the canonical REGULAR decode, the non-zero
/// epoch — a mutated in-memory entry refuses here, never only at
/// the shard decoder); the concatenated entry runs equal the
/// index-derived canonical member sequence identity-by-identity
/// (exactly once, in order — a missing, duplicated, extra,
/// substituted, or reordered member refuses); every shard except
/// the last is maximal under the deterministic greedy rule; and
/// re-folding the coverage digest over the concatenated identities
/// reproduces the manifest's. Nothing unverified, trailing, or
/// unconsumed survives.
///
/// **The memory contract (honest):** this aggregate verification
/// holds ALL the supplied shards (and their entries) for its
/// duration — O(total witness bytes), not a one-shard operation.
/// The bounded streaming consumption belongs to the separately
/// authorized CP3-A1/A2 integration.
pub fn verify_exact_coverage(
    manifest: &AncestryWitnessManifest,
    shards: &[AncestryWitnessShard],
    index: &[ReclaimedIndexEntry],
) -> Result<(), SaveError> {
    if shards.len() as u64 != manifest.shard_count {
        return Err(SaveError::Integrity(
            "ancestry-witness/shard-count-mismatch",
        ));
    }
    let mut sorted: Vec<&AncestryWitnessShard> = shards.iter().collect();
    sorted.sort_unstable_by_key(|shard| shard.shard_index);
    for (expected, shard) in sorted.iter().enumerate() {
        if shard.shard_index != expected as u64
            || shard.shard_count != manifest.shard_count
            || shard.mk != manifest.mk
        {
            return Err(SaveError::Integrity(
                "ancestry-witness/shard-index-incoherent",
            ));
        }
        if shard.member_count != shard.entries.len() as u64 {
            return Err(SaveError::Integrity(
                "ancestry-witness/shard-count-declared-mismatch",
            ));
        }
        for entry in &shard.entries {
            decode_validated_entry(entry)?;
        }
    }
    let total: u64 = sorted.iter().map(|shard| shard.member_count).sum();
    if total != manifest.member_count {
        return Err(SaveError::Integrity(
            "ancestry-witness/member-count-mismatch",
        ));
    }
    let expected_order = canonical_member_order(index);
    if expected_order.len() as u64 != manifest.member_count {
        return Err(SaveError::Integrity(
            "ancestry-witness/index-count-mismatch",
        ));
    }
    let concatenated: Vec<Hash> = sorted
        .iter()
        .flat_map(|shard| shard.entries.iter().map(|entry| entry.member))
        .collect();
    for (position, (witnessed, expected)) in
        concatenated.iter().zip(expected_order.iter()).enumerate()
    {
        if witnessed != &expected.hash {
            return Err(SaveError::Integrity(
                "ancestry-witness/coverage-sequence-mismatch",
            ));
        }
        let _ = position;
    }
    let digest = witness_coverage_digest(
        &manifest.mk,
        manifest.member_count,
        concatenated.iter().copied(),
    );
    if digest != manifest.coverage_digest {
        return Err(SaveError::Integrity(
            "ancestry-witness/coverage-digest-mismatch",
        ));
    }
    // The greedy maximality: every shard except the last must be
    // maximal — the next canonical entry would not have fit.
    for shard_index in 0..sorted.len().saturating_sub(1) {
        let shard = sorted[shard_index];
        let mut size = WITNESS_SHARD_HEADER_BYTES;
        for entry in &shard.entries {
            size += WITNESS_ENTRY_FRAMING_BYTES + entry.preimage.len() as u64;
        }
        let next = &sorted[shard_index + 1].entries[0];
        let next_framed = WITNESS_ENTRY_FRAMING_BYTES + next.preimage.len() as u64;
        if size + next_framed <= WITNESS_SHARD_PAYLOAD_CEILING {
            return Err(SaveError::Integrity("ancestry-witness/shard-not-maximal"));
        }
    }
    Ok(())
}

/// The index-shape validation (the standalone API boundary): the
/// supplied index must carry UNIQUE member identities and UNIQUE
/// epochs — two different records cannot occupy one height on a
/// linear canonical ancestry, and one identity cannot appear
/// twice. The accepted `CheckpointMetadata::decode` enforces this
/// shape for decoded metadata objects (the sorted-unique index
/// rule), but the standalone helpers accept caller-supplied index
/// slices directly — never through the metadata decode — so this
/// check closes the boundary explicitly rather than assuming the
/// decode's guarantee.
pub fn validate_index_shape(index: &[ReclaimedIndexEntry]) -> Result<(), SaveError> {
    let mut identities = std::collections::BTreeSet::new();
    let mut epochs = std::collections::BTreeSet::new();
    for entry in index {
        if !identities.insert(entry.hash) {
            return Err(SaveError::Integrity(
                "ancestry-witness/index-duplicate-identity",
            ));
        }
        if !epochs.insert(entry.epoch) {
            return Err(SaveError::Integrity(
                "ancestry-witness/index-duplicate-epoch",
            ));
        }
    }
    Ok(())
}

/// THE COMPLETE STANDALONE WITNESS VALIDATION — the composite every
/// positive result of which establishes the full contract: the
/// INDEX SHAPE (the unique identities and epochs — two records
/// cannot share one canonical height, one identity cannot appear
/// twice, even when the supplied index itself repeats them); the
/// manifest's structural coherence (via its own strict round-trip
/// decode — the non-zero bindings, the bounded counts); every
/// shard's structural coherence, payload-ceiling conformity, and
/// intrinsic entry validation (via each shard's own strict
/// round-trip decode); the complete index-entry authentication
/// (the intrinsic preimage SHA-256, the canonical REGULAR decode,
/// the non-zero epoch, the indexed epoch, and the idempotency key
/// with the Some/None agreement); the standalone parent-edge
/// consistency (the checked arithmetic); and the exact
/// all-shards-complete coverage with the deterministic greedy
/// layout. A directly constructed or mutated manifest, shard,
/// index, or entry that fails ANY component refuses here — no
/// subset helper or decoder invocation is required alongside it.
///
/// **The memory contract (honest):** the composite holds the
/// supplied aggregate for its duration and re-decodes every
/// emitted object — O(total witness bytes), the A0 reference form;
/// the bounded streaming consumption is the separately authorized
/// CP3-A1/A2 obligation.
pub fn verify_witness_standalone(
    manifest: &AncestryWitnessManifest,
    shards: &[AncestryWitnessShard],
    index: &[ReclaimedIndexEntry],
) -> Result<(), SaveError> {
    // The index shape FIRST (pure, no I/O): the unique identities
    // and epochs — the impossible-ancestry duplicates refuse
    // before any other work.
    validate_index_shape(index)?;
    // The structural coherence via the strict decodes.
    let decoded_manifest = AncestryWitnessManifest::decode(&manifest.canonical_bytes())?;
    if decoded_manifest != *manifest {
        return Err(SaveError::Integrity(
            "ancestry-witness/manifest-noncanonical",
        ));
    }
    let mut sorted: Vec<&AncestryWitnessShard> = shards.iter().collect();
    sorted.sort_unstable_by_key(|shard| shard.shard_index);
    let mut concatenated: Vec<WitnessEntry> = Vec::new();
    for shard in &sorted {
        let decoded = AncestryWitnessShard::decode(&shard.canonical_bytes())?;
        if decoded != **shard {
            return Err(SaveError::Integrity("ancestry-witness/shard-noncanonical"));
        }
        concatenated.extend(decoded.entries.iter().cloned());
    }
    // The complete index-entry authentication (the intrinsic
    // included) over the canonical-order concatenation.
    authenticate_against_index(&concatenated, index)?;
    // The standalone parent-edge consistency (the checked
    // arithmetic, the intrinsic included).
    validate_parent_edges_standalone(&concatenated, index)?;
    // The exact coverage, the census, and the greedy layout.
    verify_exact_coverage(manifest, shards, index)?;
    Ok(())
}

fn read_hash(dec: &mut Decoder, section: &'static str) -> Result<Hash, SaveError> {
    let bytes = dec.read_bytes(Hash::WIDTH, section)?;
    let hash =
        Hash::from_bytes(bytes).ok_or(SaveError::Integrity("ancestry-witness/hash-width"))?;
    if hash == Hash::ZERO {
        return Err(SaveError::Integrity("ancestry-witness/zero-identity"));
    }
    Ok(hash)
}

fn read_digest(dec: &mut Decoder, section: &'static str) -> Result<[u8; 32], SaveError> {
    let bytes = dec.read_bytes(32, section)?;
    if bytes.len() != 32 {
        return Err(SaveError::Integrity("ancestry-witness/digest-width"));
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(bytes);
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::Hash;

    fn fixture_hash(label: &[u8]) -> Hash {
        hash_object(label)
    }

    fn hex(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    // The approved golden fixture (the FINAL WITNESS-WIRE ERRATA
    // report — authoritative; never modified to accommodate an
    // implementation).
    const GOLDEN_MK: &str = "607c3ec71d55c5ea2e978f561cee2c4c990db31e679c8bc93e85af344c95915b";
    const GOLDEN_PLAN: &str = "132387275de6368052e4c6437eb9b5019ae46f365e3b4a6e12871b3eb8ea2e41";
    const GOLDEN_BEGIN: &str = "b8da3be7d6f3f8f32abd001232458a230aab0974e45021b19a0605f5dd656856";
    const GOLDEN_METADATA: &str =
        "6ae143e464e9e74ea97251500222c6baf94ff03f6030f252741d5ac60bdadcad";
    const GOLDEN_ANCHOR_A: &str =
        "96b0166db6093e88646ca8e62b57b8d649f8c525cc4396410d15326f6fccca86";
    const GOLDEN_BOUNDARY_B: &str =
        "7a508bc7f6247c0b9e79141352cc736bde62ec49d53a8eaea7a676e588744572";
    const GOLDEN_X0: &str = "915018790d654efe1c973660684e75a4d3cdf271b4714d96113073bd3bdfb0c5";
    const GOLDEN_X1: &str = "2bccbd30ba5399b501e72a4ff3bf190d9a536ad5f3f2d210002e5a42e5e83887";
    const GOLDEN_M0_PREIMAGE: &str = "280000000000000073656b746f72372f656e67696e652d706572736973742f7265766973696f6e2d7265636f72642f310100000001040000000000000001200000000000000096b0166db6093e88646ca8e62b57b8d649f8c525cc4396410d15326f6fccca8601140000000000000063703361302d666978747572652d6b65792d6d30640000000000000001000000000000002000000000000000915018790d654efe1c973660684e75a4d3cdf271b4714d96113073bd3bdfb0c50000000000000000";
    const GOLDEN_M0_ID: &str = "a34b2deda65903d36e6ea4f7cb80f36551f3bc6b8bd88ef3540486692ac0533d";
    const GOLDEN_M1_PREIMAGE: &str = "280000000000000073656b746f72372f656e67696e652d706572736973742f7265766973696f6e2d7265636f72642f31010000000102000000000000000120000000000000007a508bc7f6247c0b9e79141352cc736bde62ec49d53a8eaea7a676e588744572006500000000000000010000000000000020000000000000002bccbd30ba5399b501e72a4ff3bf190d9a536ad5f3f2d210002e5a42e5e838870000000000000000";
    const GOLDEN_M1_ID: &str = "95e0e747bb38e01bb3c50f3e2226dae50ca68d7343c15011210ef9a555bba5c4";
    const GOLDEN_COVERAGE_DIGEST: &str =
        "d2ec4f5d3005e4a7d9fc05ca0bf4d03da859e7f6096f5c471a39c2e09e75ad31";
    const GOLDEN_MANIFEST_HEX: &str = "2d00000000000000656e67696e652d706572736973742f7265636c616d6174696f6e2d616e6365737472792d7769746e6573732f31010000002000000000000000607c3ec71d55c5ea2e978f561cee2c4c990db31e679c8bc93e85af344c95915b2000000000000000132387275de6368052e4c6437eb9b5019ae46f365e3b4a6e12871b3eb8ea2e412000000000000000b8da3be7d6f3f8f32abd001232458a230aab0974e45021b19a0605f5dd65685620000000000000006ae143e464e9e74ea97251500222c6baf94ff03f6030f252741d5ac60bdadcad020000000000000001000000000000002000000000000000d2ec4f5d3005e4a7d9fc05ca0bf4d03da859e7f6096f5c471a39c2e09e75ad31";
    const GOLDEN_MANIFEST_ID: &str =
        "af0a1f3c520561ced790deae01f06fa19654c7c458425e0ad8bcf76498c187fe";
    const GOLDEN_SHARD_HEX: &str = "3300000000000000656e67696e652d706572736973742f7265636c616d6174696f6e2d616e6365737472792d7769746e6573732d73686172642f31010000002000000000000000607c3ec71d55c5ea2e978f561cee2c4c990db31e679c8bc93e85af344c95915b00000000000000000100000000000000020000000000000002000000000000002000000000000000a34b2deda65903d36e6ea4f7cb80f36551f3bc6b8bd88ef3540486692ac0533dc300000000000000280000000000000073656b746f72372f656e67696e652d706572736973742f7265766973696f6e2d7265636f72642f310100000001040000000000000001200000000000000096b0166db6093e88646ca8e62b57b8d649f8c525cc4396410d15326f6fccca8601140000000000000063703361302d666978747572652d6b65792d6d30640000000000000001000000000000002000000000000000915018790d654efe1c973660684e75a4d3cdf271b4714d96113073bd3bdfb0c50000000000000000200000000000000095e0e747bb38e01bb3c50f3e2226dae50ca68d7343c15011210ef9a555bba5c4a700000000000000280000000000000073656b746f72372f656e67696e652d706572736973742f7265766973696f6e2d7265636f72642f31010000000102000000000000000120000000000000007a508bc7f6247c0b9e79141352cc736bde62ec49d53a8eaea7a676e588744572006500000000000000010000000000000020000000000000002bccbd30ba5399b501e72a4ff3bf190d9a536ad5f3f2d210002e5a42e5e838870000000000000000";
    const GOLDEN_SHARD_ID: &str =
        "0157035e8d395c0ebae60612f0bfb845b732a30bad3baf44214eddf05d7353c3";

    /// Builds the approved golden fixture: the identities from the
    /// fixed labels, the two member records, the index, and the
    /// ordered entries — all reproduced through the crate's own
    /// primitives so the golden expectations are checked, never
    /// accommodated.
    fn golden_fixture() -> (
        Hash,
        Hash,
        Hash,
        Hash,
        Vec<WitnessEntry>,
        Vec<crate::checkpoint::ReclaimedIndexEntry>,
    ) {
        let mk = fixture_hash(b"cp3a0-fixture-mk");
        let plan = fixture_hash(b"cp3a0-fixture-plan-record");
        let begin = fixture_hash(b"cp3a0-fixture-begin-record");
        let metadata = fixture_hash(b"cp3a0-fixture-metadata-object");
        let anchor_a = fixture_hash(b"cp3a0-fixture-anchor-A");
        let boundary_b = fixture_hash(b"cp3a0-fixture-boundary-B");
        let x0 = fixture_hash(b"cp3a0-fixture-object-X0");
        let x1 = fixture_hash(b"cp3a0-fixture-object-X1");
        // The fixture identities must equal the golden hex exactly.
        assert_eq!(mk.hex(), GOLDEN_MK);
        assert_eq!(plan.hex(), GOLDEN_PLAN);
        assert_eq!(begin.hex(), GOLDEN_BEGIN);
        assert_eq!(metadata.hex(), GOLDEN_METADATA);
        assert_eq!(anchor_a.hex(), GOLDEN_ANCHOR_A);
        assert_eq!(boundary_b.hex(), GOLDEN_BOUNDARY_B);
        assert_eq!(x0.hex(), GOLDEN_X0);
        assert_eq!(x1.hex(), GOLDEN_X1);
        let m0 = crate::record::RevisionRecord::regular(anchor_a, 3, 100)
            .with_idempotency_key(b"cp3a0-fixture-key-m0".to_vec())
            .with_content_refs(vec![x0]);
        let m1 =
            crate::record::RevisionRecord::regular(boundary_b, 1, 101).with_content_refs(vec![x1]);
        let m0_preimage = m0.canonical_bytes();
        let m1_preimage = m1.canonical_bytes();
        assert_eq!(hex(&m0_preimage), GOLDEN_M0_PREIMAGE);
        assert_eq!(hex(&m1_preimage), GOLDEN_M1_PREIMAGE);
        assert_eq!(m0.id().hex(), GOLDEN_M0_ID);
        assert_eq!(m1.id().hex(), GOLDEN_M1_ID);
        let index = vec![
            crate::checkpoint::ReclaimedIndexEntry::new(
                Some(b"cp3a0-fixture-key-m0".to_vec()),
                m0.id(),
                4,
            ),
            crate::checkpoint::ReclaimedIndexEntry::new(None, m1.id(), 2),
        ];
        let entries = vec![
            WitnessEntry {
                member: m0.id(),
                preimage: m0_preimage,
            },
            WitnessEntry {
                member: m1.id(),
                preimage: m1_preimage,
            },
        ];
        (mk, plan, begin, metadata, entries, index)
    }

    #[test]
    fn the_golden_fixture_reproduces_the_ratatified_vectors_exactly() {
        let (mk, plan, begin, metadata, entries, index) = golden_fixture();
        let (manifest, shards) =
            construct_witness(&mk, &plan, &begin, &metadata, entries.clone()).unwrap();
        assert_eq!(manifest.member_count, 2);
        assert_eq!(manifest.shard_count, 1);
        let manifest_bytes = manifest.canonical_bytes();
        assert_eq!(manifest_bytes.len(), 273, "the ratified manifest length");
        assert_eq!(hex(&manifest_bytes), GOLDEN_MANIFEST_HEX);
        assert_eq!(hash_object(&manifest_bytes).hex(), GOLDEN_MANIFEST_ID);
        assert_eq!(hex(&manifest.coverage_digest), GOLDEN_COVERAGE_DIGEST);
        let shard = &shards[0];
        let shard_bytes = shard.canonical_bytes();
        assert_eq!(shard_bytes.len(), 593, "135 + (48+195) + (48+167)");
        assert_eq!(hex(&shard_bytes), GOLDEN_SHARD_HEX);
        assert_eq!(hash_object(&shard_bytes).hex(), GOLDEN_SHARD_ID);
        // The deterministic construction is a pure function of the
        // evidence: repeated construction yields identical bytes.
        let (manifest2, shards2) =
            construct_witness(&mk, &plan, &begin, &metadata, entries.clone()).unwrap();
        assert_eq!(manifest2.canonical_bytes(), manifest_bytes);
        assert_eq!(shards2[0].canonical_bytes(), shard_bytes);
        // The round trips: decode reproduces the objects and the
        // re-encode reproduces the exact bytes.
        let decoded_manifest = AncestryWitnessManifest::decode(&manifest_bytes).unwrap();
        assert_eq!(decoded_manifest, manifest);
        assert_eq!(decoded_manifest.canonical_bytes(), manifest_bytes);
        let decoded_shard = AncestryWitnessShard::decode(&shard_bytes).unwrap();
        assert_eq!(decoded_shard, *shard);
        assert_eq!(decoded_shard.canonical_bytes(), shard_bytes);
        // The standalone verifications over the fixture.
        authenticate_against_index(&entries, &index).unwrap();
        validate_parent_edges_standalone(&entries, &index).unwrap();
        verify_exact_coverage(&manifest, &shards, &index).unwrap();
        verify_witness_standalone(&manifest, &shards, &index).unwrap();
        // The deterministic key derivations (dormant, defined).
        assert_eq!(
            reclamation_witness_manifest_idempotency_key(&mk),
            format!(
                "engine-persist/reclamation-ancestry-witness-key/1:{}",
                mk.hex()
            )
            .into_bytes()
        );
        assert_eq!(
            reclamation_witness_shard_idempotency_key(&mk, 0),
            format!(
                "engine-persist/reclamation-ancestry-witness-shard-key/1:{}:0",
                mk.hex()
            )
            .into_bytes()
        );
        // The family recognition (the exact /1 forms recognized;
        // the family prefix matches both domains and future
        // versions).
        assert!(is_reclamation_ancestry_witness_family(
            RECLAMATION_WITNESS_MANIFEST_DOMAIN
        ));
        assert!(is_reclamation_ancestry_witness_family(
            RECLAMATION_WITNESS_SHARD_DOMAIN
        ));
        assert!(is_reclamation_ancestry_witness_family(
            "engine-persist/reclamation-ancestry-witness/2"
        ));
        assert!(!is_reclamation_ancestry_witness_family(
            "engine-persist/reclamation-ledger/2"
        ));
    }

    #[test]
    fn the_manifest_refuses_domain_version_and_field_violations() {
        let (mk, plan, begin, metadata, entries, _index) = golden_fixture();
        let (manifest, _shards) =
            construct_witness(&mk, &plan, &begin, &metadata, entries).unwrap();
        let bytes = manifest.canonical_bytes();
        // The exact domain: an unrecognized family version refuses
        // at read_domain (the recognized-but-unsupported class).
        let doctored = {
            let mut bytes = bytes.clone();
            // The domain's "/1" suffix (bytes 43..45 of the payload,
            // after the 8-byte length prefix) -> "/2".
            bytes[8 + 43] = b'2';
            bytes
        };
        assert!(AncestryWitnessManifest::decode(&doctored).is_err());
        // The version discrimination.
        let doctored = {
            let mut bytes = bytes.clone();
            bytes[8 + 45] = 2; // the u32 version's first byte
            bytes
        };
        assert!(AncestryWitnessManifest::decode(&doctored).is_err());
        // The truncation and the trailing bytes.
        assert!(AncestryWitnessManifest::decode(&bytes[..bytes.len() - 1]).is_err());
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(AncestryWitnessManifest::decode(&trailing).is_err());
        // A zero identity refuses.
        let zero = AncestryWitnessManifest {
            mk: Hash::ZERO,
            ..manifest.clone()
        };
        assert!(AncestryWitnessManifest::decode(&zero.canonical_bytes()).is_err());
        // The invalid counts.
        let bad_count = AncestryWitnessManifest {
            member_count: 0,
            ..manifest.clone()
        };
        assert!(AncestryWitnessManifest::decode(&bad_count.canonical_bytes()).is_err());
        let bad_count = AncestryWitnessManifest {
            shard_count: 3, // > member_count
            ..manifest.clone()
        };
        assert!(AncestryWitnessManifest::decode(&bad_count.canonical_bytes()).is_err());
        let bad_count = AncestryWitnessManifest {
            member_count: u64::MAX, // > MAX_RECLAMATION_UNITS
            ..manifest.clone()
        };
        assert!(AncestryWitnessManifest::decode(&bad_count.canonical_bytes()).is_err());
    }

    #[test]
    fn the_shard_refuses_malformed_and_incoherent_entries() {
        let (mk, _plan, _begin, _metadata, entries, _index) = golden_fixture();
        let (_manifest, shards) = construct_witness(&mk, &mk, &mk, &mk, entries.clone()).unwrap();
        let bytes = shards[0].canonical_bytes();
        // The domain and version refusals.
        let mut doctored = bytes.clone();
        doctored[8 + 49] = b'2'; // the shard domain's "/1" suffix
        assert!(AncestryWitnessShard::decode(&doctored).is_err());
        let mut doctored = bytes.clone();
        doctored[8 + 51] = 2; // the version's first byte
        assert!(AncestryWitnessShard::decode(&doctored).is_err());
        // The truncation and the trailing bytes.
        assert!(AncestryWitnessShard::decode(&bytes[..bytes.len() - 1]).is_err());
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(AncestryWitnessShard::decode(&trailing).is_err());
        // A substituted preimage byte: the hash mismatch refuses.
        let mut doctored = bytes.clone();
        let last = doctored.len() - 1;
        doctored[last] ^= 0xff;
        assert!(AncestryWitnessShard::decode(&doctored).is_err());
        // A declared member identity that does not match the
        // preimage's SHA-256 refuses.
        let doctored_shard = AncestryWitnessShard {
            entries: vec![WitnessEntry {
                member: fixture_hash(b"not-the-preimage-hash"),
                preimage: entries[0].preimage.clone(),
            }],
            ..shards[0].clone()
        };
        assert!(AncestryWitnessShard::decode(&doctored_shard.canonical_bytes()).is_err());
        // A founding record as a member preimage refuses (the
        // retirement members are regular publications).
        let founding = crate::record::RevisionRecord::founding(
            crate::record::AuthorityMode::Standalone,
            "cp3a0-founding",
            0,
        );
        let doctored_shard = AncestryWitnessShard {
            entries: vec![WitnessEntry {
                member: founding.id(),
                preimage: founding.canonical_bytes(),
            }],
            ..shards[0].clone()
        };
        assert!(AncestryWitnessShard::decode(&doctored_shard.canonical_bytes()).is_err());
        // A garbage preimage refuses at the record decode.
        let garbage = fixture_hash(b"garbage-preimage");
        let doctored_shard = AncestryWitnessShard {
            entries: vec![WitnessEntry {
                member: garbage,
                preimage: b"garbage-preimage".to_vec(),
            }],
            ..shards[0].clone()
        };
        assert!(AncestryWitnessShard::decode(&doctored_shard.canonical_bytes()).is_err());
        // The declared-count mismatch (member_count vs the entries
        // count) refuses.
        let doctored_shard = AncestryWitnessShard {
            member_count: 1,
            ..shards[0].clone()
        };
        assert!(AncestryWitnessShard::decode(&doctored_shard.canonical_bytes()).is_err());
        // The index incoherence (shard_index >= shard_count).
        let doctored_shard = AncestryWitnessShard {
            shard_index: 1,
            ..shards[0].clone()
        };
        assert!(AncestryWitnessShard::decode(&doctored_shard.canonical_bytes()).is_err());
    }

    #[test]
    fn the_index_authentication_refuses_every_field_mismatch() {
        let (_mk, _plan, _begin, _metadata, entries, index) = golden_fixture();
        // The genuine state verifies.
        authenticate_against_index(&entries, &index).unwrap();
        // A member absent from the index.
        let foreign = crate::record::RevisionRecord::regular(fixture_hash(b"foreign-parent"), 9, 1);
        let foreign_entry = WitnessEntry {
            member: foreign.id(),
            preimage: foreign.canonical_bytes(),
        };
        assert!(authenticate_against_index(std::slice::from_ref(&foreign_entry), &index).is_err());
        // An epoch mismatch: the index claims a different epoch.
        let doctored_index = vec![
            crate::checkpoint::ReclaimedIndexEntry::new(
                Some(b"cp3a0-fixture-key-m0".to_vec()),
                entries[0].member,
                5, // the record's epoch is 4
            ),
            index[1].clone(),
        ];
        assert!(authenticate_against_index(&entries, &doctored_index).is_err());
        // A key mismatch: both Some, different bytes.
        let doctored_index = vec![
            crate::checkpoint::ReclaimedIndexEntry::new(
                Some(b"a-different-key".to_vec()),
                entries[0].member,
                4,
            ),
            index[1].clone(),
        ];
        assert!(authenticate_against_index(&entries, &doctored_index).is_err());
        // The Some/None mismatch, both directions.
        let doctored_index = vec![
            crate::checkpoint::ReclaimedIndexEntry::new(None, entries[0].member, 4),
            index[1].clone(),
        ];
        assert!(authenticate_against_index(&entries, &doctored_index).is_err());
        let doctored_index = vec![
            index[0].clone(),
            crate::checkpoint::ReclaimedIndexEntry::new(
                Some(b"key-on-a-keyless-record".to_vec()),
                entries[1].member,
                2,
            ),
        ];
        assert!(authenticate_against_index(&entries, &doctored_index).is_err());
    }

    #[test]
    fn the_standalone_parent_edge_rules_refuse_contradictions() {
        let (mk, _plan, _begin, _metadata, _entries, index) = golden_fixture();
        // The fixture's genuine state: m0's parent is the unindexed
        // anchor A with an epoch gap of 2 toward m1 — consistent.
        let (_m, shards) = construct_witness(&mk, &mk, &mk, &mk, golden_fixture().4).unwrap();
        validate_parent_edges_standalone(&shards[0].entries, &index).unwrap();
        // The contradictory case: two members at CONTIGUOUS epochs
        // whose upper member's parent is NOT the indexed lower
        // member (an unindexed record claims the same chain height).
        let anchor = fixture_hash(b"contradiction-anchor");
        let upper = crate::record::RevisionRecord::regular(anchor, 2, 1);
        let lower =
            crate::record::RevisionRecord::regular(fixture_hash(b"contradiction-below"), 1, 2);
        let doctored_entries = vec![
            WitnessEntry {
                member: upper.id(),
                preimage: upper.canonical_bytes(),
            },
            WitnessEntry {
                member: lower.id(),
                preimage: lower.canonical_bytes(),
            },
        ];
        let doctored_index = vec![
            crate::checkpoint::ReclaimedIndexEntry::new(None, upper.id(), 3),
            crate::checkpoint::ReclaimedIndexEntry::new(None, lower.id(), 2),
        ];
        assert!(
            validate_parent_edges_standalone(&doctored_entries, &doctored_index).is_err(),
            "the contiguous-epoch non-indexed parent refuses (the R9-F1 rule)"
        );
        // The indexed-parent epoch incoherence: the parent IS
        // indexed but at a non-contiguous epoch.
        let parent = fixture_hash(b"indexed-parent-wrong-epoch");
        let child = crate::record::RevisionRecord::regular(parent, 5, 1);
        let doctored_entries = vec![WitnessEntry {
            member: child.id(),
            preimage: child.canonical_bytes(),
        }];
        let doctored_index = vec![
            crate::checkpoint::ReclaimedIndexEntry::new(None, child.id(), 6),
            crate::checkpoint::ReclaimedIndexEntry::new(None, parent, 2), // != 6-1
        ];
        assert!(validate_parent_edges_standalone(&doctored_entries, &doctored_index).is_err());
        // The zero parent refuses.
        let zero_parent = crate::record::RevisionRecord {
            parent: Some(Hash::ZERO),
            ..crate::record::RevisionRecord::regular(fixture_hash(b"zero-parent-below"), 0, 3)
        };
        let doctored_entries = vec![WitnessEntry {
            member: zero_parent.id(),
            preimage: zero_parent.canonical_bytes(),
        }];
        let doctored_index = vec![crate::checkpoint::ReclaimedIndexEntry::new(
            None,
            zero_parent.id(),
            1,
        )];
        assert!(validate_parent_edges_standalone(&doctored_entries, &doctored_index).is_err());
    }

    #[test]
    fn the_coverage_verification_refuses_every_sequence_violation() {
        let (mk, _plan, _begin, _metadata, entries, index) = golden_fixture();
        let (manifest, shards) = construct_witness(&mk, &mk, &mk, &mk, entries.clone()).unwrap();
        // The genuine state verifies.
        verify_exact_coverage(&manifest, &shards, &index).unwrap();
        // A missing member (one entry dropped): the counts and the
        // sequence refuse.
        let truncated = AncestryWitnessManifest {
            member_count: 1,
            shard_count: 1,
            ..manifest.clone()
        };
        let short_shard = AncestryWitnessShard {
            member_count: 1,
            entries: vec![entries[0].clone()],
            ..shards[0].clone()
        };
        assert!(
            verify_exact_coverage(&truncated, std::slice::from_ref(&short_shard), &index).is_err()
        );
        // A duplicated member: the sequence refuses.
        let duplicated = AncestryWitnessShard {
            member_count: 3,
            entries: vec![entries[0].clone(), entries[1].clone(), entries[0].clone()],
            ..shards[0].clone()
        };
        let dup_manifest = AncestryWitnessManifest {
            member_count: 3,
            ..manifest.clone()
        };
        assert!(verify_exact_coverage(&dup_manifest, &[duplicated], &index).is_err());
        // A reordered run: the identity-by-identity comparison
        // refuses (m1 before m0).
        let reordered_manifest = manifest.clone();
        let reordered_shards = vec![AncestryWitnessShard {
            entries: vec![entries[1].clone(), entries[0].clone()],
            ..shards[0].clone()
        }];
        assert!(verify_exact_coverage(&reordered_manifest, &reordered_shards, &index).is_err());
        // A substituted member identity: the sequence refuses.
        let substituted = vec![AncestryWitnessShard {
            entries: vec![
                WitnessEntry {
                    member: fixture_hash(b"substituted-member"),
                    preimage: entries[0].preimage.clone(),
                },
                entries[1].clone(),
            ],
            ..shards[0].clone()
        }];
        assert!(verify_exact_coverage(&manifest, &substituted, &index).is_err());
        // A digest mismatch: the manifest's digest doctored.
        let mut bad_digest = manifest.clone();
        bad_digest.coverage_digest[0] ^= 0xff;
        assert!(verify_exact_coverage(&bad_digest, &shards, &index).is_err());
        // A non-maximal middle shard (the greedy rule violated).
        // The synthetic members' index entries are derived from the
        // records themselves (the epochs and keys agreeing by
        // construction — the internally consistent positive
        // fixture).
        let large_a = synthetic_entry(0, 60_000);
        let large_b = synthetic_entry(1, 60_000);
        let large_c = synthetic_entry(2, 60_000);
        let multi_index = vec![
            synthetic_index_entry(&large_a),
            synthetic_index_entry(&large_b),
            synthetic_index_entry(&large_c),
        ];
        let (multi_manifest, multi_shards) = construct_witness(
            &mk,
            &mk,
            &mk,
            &mk,
            vec![large_a.clone(), large_b.clone(), large_c.clone()],
        )
        .unwrap();
        verify_exact_coverage(&multi_manifest, &multi_shards, &multi_index).unwrap();
        verify_witness_standalone(&multi_manifest, &multi_shards, &multi_index).unwrap();
        assert!(multi_shards.len() >= 2, "the synthetic sizes force a split");
        // A hand-built non-maximal split refuses.
        let mut non_maximal = multi_shards.clone();
        if non_maximal.len() >= 2 {
            let moved = non_maximal[0].entries.pop().unwrap();
            non_maximal[0].member_count = non_maximal[0].entries.len() as u64;
            non_maximal[1].entries.insert(0, moved);
            non_maximal[1].member_count = non_maximal[1].entries.len() as u64;
            assert!(
                verify_exact_coverage(&multi_manifest, &non_maximal, &multi_index).is_err(),
                "a non-maximal leading shard refuses"
            );
        }
    }

    /// A synthetic member with a given reference count and a small
    /// deterministic key — used for the shard-split and boundary
    /// tests. The identity derives from the content (the hash
    /// check holds by construction). The epoch steps by TWO per
    /// slot so consecutive members leave an intervening height for
    /// their unindexed synthetic parents (the standalone
    /// parent-edge consistency holds by construction).
    fn synthetic_entry(slot: u8, refs: usize) -> WitnessEntry {
        let parent = fixture_hash(format!("synthetic-parent-{slot}").as_bytes());
        let record = crate::record::RevisionRecord::regular(parent, 100 + 2 * slot as u64, 1)
            .with_idempotency_key(format!("synthetic-key-{slot}").into_bytes())
            .with_content_refs(ascending_hashes(refs));
        let preimage = record.canonical_bytes();
        WitnessEntry {
            member: hash_object(&preimage),
            preimage,
        }
    }

    /// The internally consistent index entry for a synthetic
    /// member: the key and epoch read from the member's own decoded
    /// record (never fabricated independently).
    fn synthetic_index_entry(entry: &WitnessEntry) -> crate::checkpoint::ReclaimedIndexEntry {
        let record = crate::record::RevisionRecord::decode(&entry.preimage).unwrap();
        crate::checkpoint::ReclaimedIndexEntry::new(
            record.idempotency_key.clone(),
            entry.member,
            record.epoch,
        )
    }

    /// Strictly ascending, distinct, non-zero 32-byte identities
    /// (a big-endian counter in the leading bytes over a fixed
    /// non-zero tail).
    fn ascending_hashes(count: usize) -> Vec<Hash> {
        (0..count)
            .map(|i| {
                let mut bytes = [0u8; 32];
                bytes[0..8].copy_from_slice(&(i as u64).to_be_bytes());
                for (position, byte) in bytes.iter_mut().enumerate().skip(8) {
                    *byte = (position % 251) as u8 | 1;
                }
                Hash::from_bytes(&bytes).unwrap()
            })
            .collect()
    }

    #[test]
    fn the_greedy_partition_is_deterministic_and_maximal() {
        let (mk, _plan, _begin, _metadata, _entries, _index) = golden_fixture();
        let members = vec![
            synthetic_entry(0, 60_000),
            synthetic_entry(1, 60_000),
            synthetic_entry(2, 60_000),
        ];
        let (manifest, shards) = construct_witness(&mk, &mk, &mk, &mk, members.clone()).unwrap();
        assert!(shards.len() >= 2, "the sizes force a multi-shard split");
        assert_eq!(manifest.shard_count, shards.len() as u64);
        // Every shard except the last is maximal: the next entry
        // would not have fit (checked by re-deriving the sizes).
        for shard in &shards[..shards.len() - 1] {
            let mut size = WITNESS_SHARD_HEADER_BYTES;
            for entry in &shard.entries {
                size += WITNESS_ENTRY_FRAMING_BYTES + entry.preimage.len() as u64;
            }
            let next = shards[shard.shard_index as usize + 1].entries[0].clone();
            let next_framed = WITNESS_ENTRY_FRAMING_BYTES + next.preimage.len() as u64;
            assert!(size + next_framed > WITNESS_SHARD_PAYLOAD_CEILING);
        }
        // Every shard fits the ceiling.
        for shard in &shards {
            assert!(shard.canonical_bytes().len() as u64 <= WITNESS_SHARD_PAYLOAD_CEILING);
        }
        // The deterministic reconstruction: byte-identical shards.
        let (_m2, shards2) = construct_witness(&mk, &mk, &mk, &mk, members).unwrap();
        for (a, b) in shards.iter().zip(shards2.iter()) {
            assert_eq!(a.canonical_bytes(), b.canonical_bytes());
        }
    }

    #[test]
    fn the_four_mib_boundaries_accept_exact_fit_and_refuse_one_over() {
        let (mk, _plan, _begin, _metadata, _entries, _index) = golden_fixture();
        // A VALID canonical revision preimage of exactly
        // 4,194,121 bytes: 65,536 content references + 39,313
        // dependency references + a 26-byte idempotency key
        // (135 + 26 + 40 * 104,849 = 4,194,121).
        let exact_fit = boundary_record(26);
        assert_eq!(exact_fit.preimage.len(), 4_194_121);
        assert_eq!(MAX_WITNESS_MEMBER_PREIMAGE_BYTES, 4_194_121);
        // The single-entry shard totals exactly the ceiling and is
        // accepted by the construction AND the decode.
        let (manifest, shards) = construct_witness(&mk, &mk, &mk, &mk, vec![exact_fit]).unwrap();
        assert_eq!(shards.len(), 1);
        let bytes = shards[0].canonical_bytes();
        assert_eq!(bytes.len() as u64, WITNESS_SHARD_PAYLOAD_CEILING);
        assert_eq!(AncestryWitnessShard::decode(&bytes).unwrap(), shards[0]);
        let _ = manifest;
        // One byte over: the same record with a 27-byte key is
        // 4,194,122 bytes — the construction refuses the oversized
        // member (fail-closed, before any publication or deletion),
        // and a doctored shard over the ceiling refuses at decode.
        let one_over = boundary_record(27);
        assert_eq!(one_over.preimage.len(), 4_194_122);
        let error = construct_witness(&mk, &mk, &mk, &mk, vec![one_over.clone()]).unwrap_err();
        assert!(
            matches!(
                error,
                SaveError::Limit {
                    kind: "member-preimage-oversized",
                    ..
                }
            ),
            "the oversized refusal is typed: {error:?}"
        );
        let doctored = AncestryWitnessShard {
            mk,
            shard_index: 0,
            shard_count: 1,
            member_count: 1,
            entries: vec![one_over.clone()],
        };
        assert!(AncestryWitnessShard::decode(&doctored.canonical_bytes()).is_err());
    }

    /// The boundary record: a valid canonical revision with the
    /// given key length, 65,536 content references, and 39,313
    /// dependency references — sized so key length 26 yields
    /// exactly 4,194,121 preimage bytes.
    fn boundary_record(key_len: usize) -> WitnessEntry {
        let parent = fixture_hash(b"boundary-parent");
        let record = crate::record::RevisionRecord::regular(parent, 1_000, 7)
            .with_idempotency_key(vec![0x42u8; key_len])
            .with_content_refs(ascending_hashes(65_536))
            .with_depends_on(ascending_hashes(39_313));
        let preimage = record.canonical_bytes();
        WitnessEntry {
            member: hash_object(&preimage),
            preimage,
        }
    }

    #[test]
    fn the_altered_tick_entry_refuses_every_standalone_path() {
        // (C1-F1) The mandatory counterexample: a DIRECTLY
        // CONSTRUCTED entry whose preimage was altered (only the
        // tick — the epoch, parent, key, and references intact)
        // while the declared member identity, the index, and the
        // manifest digest stay original. Every standalone helper
        // AND the composite refuse — the intrinsic preimage-hash
        // authentication is included in each, never only at the
        // shard decoder.
        let (mk, plan, begin, metadata, entries, index) = golden_fixture();
        let original = entries[0].clone();
        let mut record = crate::record::RevisionRecord::decode(&original.preimage).unwrap();
        record.tick += 1;
        let altered_preimage = record.canonical_bytes();
        assert_ne!(
            hash_object(&altered_preimage),
            original.member,
            "the altered tick changes the preimage hash"
        );
        let altered = WitnessEntry {
            member: original.member,
            preimage: altered_preimage,
        };
        let pair = vec![altered.clone(), entries[1].clone()];
        // Each subset helper refuses on its own (no decoder
        // invocation involved).
        assert!(
            authenticate_against_index(&pair, &index).is_err(),
            "the index authentication includes the intrinsic hash check"
        );
        assert!(
            validate_parent_edges_standalone(&pair, &index).is_err(),
            "the parent-edge validation includes the intrinsic hash check"
        );
        // The directly constructed shard carrying the altered entry
        // refuses at the coverage helper and at the composite.
        let (manifest, _shards) =
            construct_witness(&mk, &plan, &begin, &metadata, entries.clone()).unwrap();
        let doctored_shards = vec![AncestryWitnessShard {
            mk,
            shard_index: 0,
            shard_count: 1,
            member_count: 2,
            entries: pair.clone(),
        }];
        assert!(
            verify_exact_coverage(&manifest, &doctored_shards, &index).is_err(),
            "the coverage verification includes the intrinsic hash check"
        );
        assert!(
            verify_witness_standalone(&manifest, &doctored_shards, &index).is_err(),
            "the complete standalone validation refuses the altered entry"
        );
    }

    #[test]
    fn the_checked_epoch_arithmetic_refuses_impossible_ancestry() {
        // (C1-F2) The checked arithmetic: the u64::MAX epochs
        // refuse typed (never a panic or a wrap — identical in
        // debug and release); the epoch-zero regular member
        // refuses; the valid indexed-adjacent and gapped forms
        // pass (the golden fixture's two-height gap is proven in
        // the parent-edge test above).
        // (a) An indexed parent at u64::MAX: no successor epoch
        // exists — the typed refusal (pre-fix this panicked in
        // debug and wrapped in release).
        {
            let parent = fixture_hash(b"max-epoch-parent");
            let child = crate::record::RevisionRecord::regular(parent, 5, 1);
            let entries = vec![WitnessEntry {
                member: child.id(),
                preimage: child.canonical_bytes(),
            }];
            let index = vec![
                crate::checkpoint::ReclaimedIndexEntry::new(None, child.id(), 6),
                crate::checkpoint::ReclaimedIndexEntry::new(None, parent, u64::MAX),
            ];
            assert!(validate_parent_edges_standalone(&entries, &index).is_err());
        }
        // (b) A next canonical member at u64::MAX with an unindexed
        // parent above: the checked add yields None (never equal to
        // any record epoch) — no panic, no wrap, no contradiction
        // fired; the state is consistently gapped.
        {
            let anchor = fixture_hash(b"max-next-anchor");
            let upper = crate::record::RevisionRecord::regular(anchor, 4, 1);
            let lower = crate::record::RevisionRecord::regular(
                fixture_hash(b"max-next-below"),
                u64::MAX - 1,
                2,
            );
            let entries = vec![
                WitnessEntry {
                    member: lower.id(),
                    preimage: lower.canonical_bytes(),
                },
                WitnessEntry {
                    member: upper.id(),
                    preimage: upper.canonical_bytes(),
                },
            ];
            let index = vec![
                crate::checkpoint::ReclaimedIndexEntry::new(None, lower.id(), u64::MAX),
                crate::checkpoint::ReclaimedIndexEntry::new(None, upper.id(), 5),
            ];
            validate_parent_edges_standalone(&entries, &index).unwrap();
        }
        // (c) A regular retirement member at epoch zero: the
        // parent would need a negative chain height — the
        // impossible-ancestry refusal.
        {
            let zero_epoch = crate::record::RevisionRecord {
                epoch: 0,
                ..crate::record::RevisionRecord::regular(fixture_hash(b"zero-epoch-below"), 5, 1)
            };
            let entry = WitnessEntry {
                member: zero_epoch.id(),
                preimage: zero_epoch.canonical_bytes(),
            };
            let index = vec![crate::checkpoint::ReclaimedIndexEntry::new(
                None,
                entry.member,
                0,
            )];
            assert!(
                authenticate_against_index(std::slice::from_ref(&entry), &index).is_err(),
                "the epoch-zero regular member refuses"
            );
        }
        // (d) The valid indexed-adjacent form: the parent indexed
        // at exactly the child's epoch minus one.
        {
            let parent = fixture_hash(b"adjacent-parent");
            let child = crate::record::RevisionRecord::regular(parent, 4, 1);
            let entries = vec![WitnessEntry {
                member: child.id(),
                preimage: child.canonical_bytes(),
            }];
            let index = vec![
                crate::checkpoint::ReclaimedIndexEntry::new(None, child.id(), 5),
                crate::checkpoint::ReclaimedIndexEntry::new(None, parent, 4),
            ];
            validate_parent_edges_standalone(&entries, &index).unwrap();
        }
    }

    #[test]
    fn the_construction_derives_the_canonical_order_and_refuses_bad_census() {
        // (C1-F3) The canonical order is DERIVED from the
        // authenticated member metadata — the same evidence in ANY
        // iteration order produces the identical canonical bytes;
        // the duplicate identity, the duplicate epoch, the
        // above-ceiling census, and the zero bindings refuse typed;
        // and the successful construction passes the COMPLETE
        // standalone validation.
        let (mk, plan, begin, metadata, entries, index) = golden_fixture();
        // The REVERSE input order produces the identical output.
        let (forward_manifest, forward_shards) =
            construct_witness(&mk, &plan, &begin, &metadata, entries.clone()).unwrap();
        let mut reversed = entries.clone();
        reversed.reverse();
        let (reverse_manifest, reverse_shards) =
            construct_witness(&mk, &plan, &begin, &metadata, reversed).unwrap();
        assert_eq!(
            reverse_manifest.canonical_bytes(),
            forward_manifest.canonical_bytes()
        );
        for (a, b) in forward_shards.iter().zip(reverse_shards.iter()) {
            assert_eq!(a.canonical_bytes(), b.canonical_bytes());
        }
        // The duplicate member identity refuses.
        let mut duplicated = entries.clone();
        duplicated.push(entries[0].clone());
        assert!(
            construct_witness(&mk, &plan, &begin, &metadata, duplicated)
                .unwrap_err()
                .to_string()
                .contains("duplicate-member-identity")
        );
        // The duplicate epoch refuses (a distinct record at m0's
        // epoch — the unique chain heights).
        let twin = crate::record::RevisionRecord::regular(fixture_hash(b"twin-parent"), 3, 9);
        let twin_entry = WitnessEntry {
            member: twin.id(),
            preimage: twin.canonical_bytes(),
        };
        let error = construct_witness(
            &mk,
            &plan,
            &begin,
            &metadata,
            vec![entries[0].clone(), twin_entry],
        )
        .unwrap_err();
        assert!(error.to_string().contains("duplicate-epoch"));
        // The above-ceiling census refuses (one member over the
        // plan's aggregate ceiling — minimal distinct-epoch
        // records).
        let over: Vec<WitnessEntry> = (0..=crate::reclaim::MAX_RECLAMATION_UNITS)
            .map(|position| {
                let parent = fixture_hash(format!("over-census-parent-{position}").as_bytes());
                let record = crate::record::RevisionRecord::regular(parent, position, 1);
                let preimage = record.canonical_bytes();
                WitnessEntry {
                    member: hash_object(&preimage),
                    preimage,
                }
            })
            .collect();
        let error = construct_witness(&mk, &plan, &begin, &metadata, over).unwrap_err();
        assert!(error.to_string().contains("member-count"));
        // The zero binding identities refuse.
        assert!(construct_witness(&Hash::ZERO, &plan, &begin, &metadata, entries.clone()).is_err());
        assert!(construct_witness(&mk, &Hash::ZERO, &begin, &metadata, entries.clone()).is_err());
        assert!(construct_witness(&mk, &plan, &Hash::ZERO, &metadata, entries.clone()).is_err());
        assert!(construct_witness(&mk, &plan, &begin, &Hash::ZERO, entries.clone()).is_err());
        // The successful construction passes the complete
        // standalone validation (the strict round-trip included).
        verify_witness_standalone(&forward_manifest, &forward_shards, &index).unwrap();
    }

    #[test]
    fn the_standalone_composite_rejects_duplicate_index_shapes() {
        // (C2-F1) The supplied index's shape is validated at the
        // standalone API boundary: two different records at the
        // SAME epoch cannot occupy one height on a linear canonical
        // ancestry, and one identity cannot appear twice — even
        // when the supplied index itself repeats them and every
        // per-entry fact is genuine. The metadata decode's
        // sorted-unique guarantee never reaches this boundary (the
        // helpers accept caller-supplied slices), so the composite
        // enforces it explicitly.
        // (Fixture A) Two DIFFERENT valid records at the same
        // epoch, a supplied index containing both, a directly
        // constructed structurally valid manifest and shard with
        // the correct digest — the composite refuses typed.
        let (mk, plan, begin, metadata, entries, index) = golden_fixture();
        let parent = fixture_hash(b"c2-shared-parent");
        let ra = crate::record::RevisionRecord::regular(parent, 1, 10);
        let rb = crate::record::RevisionRecord::regular(parent, 1, 11);
        assert_ne!(
            ra.id(),
            rb.id(),
            "the different ticks yield different identities"
        );
        let mut pair = vec![
            WitnessEntry {
                member: ra.id(),
                preimage: ra.canonical_bytes(),
            },
            WitnessEntry {
                member: rb.id(),
                preimage: rb.canonical_bytes(),
            },
        ];
        let dup_epoch_index = vec![
            crate::checkpoint::ReclaimedIndexEntry::new(None, ra.id(), 2),
            crate::checkpoint::ReclaimedIndexEntry::new(None, rb.id(), 2),
        ];
        // The per-entry subset still passes (each entry is
        // genuinely authenticated against its index row) — the
        // violation is the INDEX SHAPE alone.
        authenticate_against_index(&pair, &dup_epoch_index).unwrap();
        // The directly constructed witness: the canonical order
        // (both epoch 2 — the identity tiebreak ascending), the
        // correct digest, the structurally valid manifest and
        // shard.
        pair.sort_unstable_by_key(|entry| entry.member);
        let digest = witness_coverage_digest(
            &mk,
            2,
            pair.iter()
                .map(|entry| entry.member)
                .collect::<Vec<_>>()
                .into_iter(),
        );
        let manifest = AncestryWitnessManifest {
            mk,
            plan_record: plan,
            begin_record: begin,
            metadata_object: metadata,
            member_count: 2,
            shard_count: 1,
            coverage_digest: digest,
        };
        let shard = AncestryWitnessShard {
            mk,
            shard_index: 0,
            shard_count: 1,
            member_count: 2,
            entries: pair,
        };
        let error =
            verify_witness_standalone(&manifest, std::slice::from_ref(&shard), &dup_epoch_index)
                .unwrap_err();
        assert!(
            error.to_string().contains("index-duplicate-epoch"),
            "the duplicate-epoch index refuses typed: {error:?}"
        );
        // (Fixture B) The duplicate indexed IDENTITY: the supplied
        // index carries the same identity twice, with two matching
        // valid entries and a correct digest — the composite
        // refuses typed.
        let rr = crate::record::RevisionRecord::regular(parent, 1, 12);
        let rr_entry = WitnessEntry {
            member: rr.id(),
            preimage: rr.canonical_bytes(),
        };
        let dup_identity_index = vec![
            crate::checkpoint::ReclaimedIndexEntry::new(None, rr.id(), 2),
            crate::checkpoint::ReclaimedIndexEntry::new(None, rr.id(), 2),
        ];
        let digest = witness_coverage_digest(&mk, 2, [rr.id(), rr.id()].into_iter());
        let manifest = AncestryWitnessManifest {
            mk,
            plan_record: plan,
            begin_record: begin,
            metadata_object: metadata,
            member_count: 2,
            shard_count: 1,
            coverage_digest: digest,
        };
        let shard = AncestryWitnessShard {
            mk,
            shard_index: 0,
            shard_count: 1,
            member_count: 2,
            entries: vec![rr_entry.clone(), rr_entry],
        };
        let error =
            verify_witness_standalone(&manifest, std::slice::from_ref(&shard), &dup_identity_index)
                .unwrap_err();
        assert!(
            error.to_string().contains("index-duplicate-identity"),
            "the duplicate-identity index refuses typed: {error:?}"
        );
        // The genuine interleaved-anchor control (the golden
        // fixture — the unindexed anchor occupying the intervening
        // height, the unique-epoch/identity index) still passes the
        // complete composite.
        let (manifest, shards) = construct_witness(&mk, &plan, &begin, &metadata, entries).unwrap();
        verify_witness_standalone(&manifest, &shards, &index).unwrap();
    }
}
