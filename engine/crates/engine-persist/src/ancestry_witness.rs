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
//! **The A1 reachability boundary (V2X-CP3-A1-2, honest):** the
//! wire codecs, the deterministic construction, and the standalone
//! validation helpers remain library-reachable with NO production
//! caller — no recovery, campaign-crossing, or consumer flow
//! consults them (the A1-3 consumer remains separately gated). The
//! family gate IS production-wired (the generic append refuses
//! every witness-family object typed). The A1-2 publisher
//! ([`publish_ancestry_witness`]) and the internal installed-state
//! kernel ([`verify_installed_witness_internal`]) are TEST-ONLY:
//! compile-time isolated under the `composite-test-fixture`
//! feature — the production build does not compile them at all
//! (the CI activation-isolation step nm-verifies the symbols'
//! absence from the feature-disabled rlib; a public Rust export
//! alone is NOT an adequate activation barrier — the feature gate
//! is). There is NO production publication path, NO engine-slice
//! recognition, and NO destructive connection.
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
use crate::medium::{MediumError, StoredObject, SyncTier};
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
/// family is refused by the generic publication path (the
/// owner-ratified sixth family gate, V2X-CP3-A1).
pub const RECLAMATION_WITNESS_FAMILY: &str = "engine-persist/reclamation-ancestry-witness";

/// Whether a domain belongs to the recognized ancestry-witness
/// family (any version).
pub fn is_reclamation_ancestry_witness_family(domain: &str) -> bool {
    domain.starts_with(RECLAMATION_WITNESS_FAMILY)
}

/// The witness family's generic-append gate (V2X-CP3-A1, the
/// owner-ratified sixth family gate — the completion-marker
/// gate's witness twin): an ancestry-witness object — the exact
/// manifest and shard domains, unsupported family versions, and
/// any variant with a readable family-domain prefix — may enter
/// the chain ONLY through the future admission-controlled
/// publication boundary (A1-2, NOT IMPLEMENTED). The unrestricted
/// generic append refuses every family member typed; the
/// unrelated existing families are untouched.
pub fn enforce_ancestry_witness_family_gate(objects: &[StoredObject]) -> Result<(), MediumError> {
    for object in objects {
        if object.domain().starts_with(RECLAMATION_WITNESS_FAMILY) {
            return Err(MediumError::Chain {
                rule: "ancestry-witness/generic-append-refused",
            });
        }
    }
    Ok(())
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

/// (V2X-CP3-A1) The per-object witness publication outcome —
/// defined for the future publication (A1-2, NOT IMPLEMENTED; no
/// publisher exists in A1-1). The owner-ratified
/// installation-versus-durability contract: ONLY `Accepted`
/// carries an actual acknowledged provider tier (the honest grade,
/// recorded never promoted — the caller compares it against their
/// own requirement); `DuplicateVerified` (the key already occupied
/// by the re-authenticated exact canonical content) and
/// `ReconciledInstalled` (the uncertain append reconciled to a
/// proven canonical installation) establish INSTALLATION ONLY —
/// no durability grade is invented for them, and the destructive
/// consumer must independently establish the provider-durable
/// witness authority before any mutation. `ReconciledNotInstalled`
/// is the verified, retryable absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WitnessPublicationOutcome {
    /// Installed through a durably acknowledged append — the
    /// provider's ACTUAL tier.
    Accepted { record: Hash, tier: SyncTier },
    /// The deterministic key was already occupied by content
    /// verified byte-identical through the complete installed-state
    /// verification — installation only, no grade.
    DuplicateVerified { record: Hash },
    /// The uncertain append reconciled to a proven canonical
    /// installation — installation only, NO tier claimed.
    ReconciledInstalled { record: Hash },
    /// The verified absence after an uncertain append — retryable.
    ReconciledNotInstalled,
}

/// The discovery descent's bound (the documented safety valve —
/// the same policy class as every accepted walk).
const WITNESS_DISCOVERY_DESCENT: u64 = 1 << 20;

/// The keyed-record discovery shared by the witness manifest and
/// shard discoveries: the gap-aware, cycle-guarded HEAD-to-founding
/// descent (the accepted `find_on_chain` skeleton — the skip-table
/// tracking, the authenticated gap crossing, the uncovered-gap
/// refusal, the bounded descent), matching each encountered
/// record's idempotency key against the expected deterministic
/// form. A match is a CANDIDATE, never authority by key occupancy
/// alone: the revision shape (a regular record with exactly one
/// content reference) is validated at the matched record and the
/// single content object's hash-verified bytes are returned for
/// the caller's strict decode and binding checks.
fn find_witness_record_by_key(
    medium: &mut dyn crate::medium::CanonicalMedium,
    key: &[u8],
) -> Result<Option<(Hash, Vec<u8>)>, MediumError> {
    let mut current = medium
        .head()?
        .ok_or(MediumError::Chain { rule: "unfounded" })?;
    let mut seen = std::collections::BTreeSet::new();
    let mut skip_table: Option<crate::checkpoint::CheckpointMetadata> = None;
    for _ in 0..WITNESS_DISCOVERY_DESCENT {
        if !seen.insert(current) {
            return Err(MediumError::Chain { rule: "cycle" });
        }
        let record = medium.read_record(&current)?;
        let mut first_object: Option<Vec<u8>> = None;
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
            first_object = Some(bytes);
        }
        if record.idempotency_key.as_deref() == Some(key) {
            if !matches!(record.kind, RecordKind::Regular) || record.content_refs.len() != 1 {
                return Err(MediumError::Chain {
                    rule: "ancestry-witness-discovery/shape-mismatch",
                });
            }
            let bytes = first_object.expect("the single content ref was read above");
            return Ok(Some((current, bytes)));
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
    Err(MediumError::Chain {
        rule: "ancestry-witness-discovery/descent-bound",
    })
}

/// (V2X-CP3-A1) The installed witness manifest's discovery: the
/// deterministic MK-bound key lookup over the canonical ancestry,
/// returning the matched revision's identity and the strictly
/// decoded manifest with the MK binding checked. Key occupancy
/// alone is never authority — the canonical membership (the
/// descent), the revision shape, the object's hash-verified read,
/// the strict decode, and the binding are all established here.
/// **Discovery of the manifest alone does NOT establish
/// all-shards-complete authority** — the complete installed-state
/// verification is the separately gated A1-3 consumer.
pub fn find_installed_witness_manifest(
    medium: &mut dyn crate::medium::CanonicalMedium,
    mk: &Hash,
) -> Result<Option<(Hash, AncestryWitnessManifest)>, MediumError> {
    let key = reclamation_witness_manifest_idempotency_key(mk);
    let Some((record, bytes)) = find_witness_record_by_key(medium, &key)? else {
        return Ok(None);
    };
    let manifest = AncestryWitnessManifest::decode(&bytes).map_err(MediumError::Format)?;
    if manifest.mk != *mk {
        return Err(MediumError::Chain {
            rule: "ancestry-witness-discovery/binding-mismatch",
        });
    }
    Ok(Some((record, manifest)))
}

/// (V2X-CP3-A1) The installed witness shard's discovery: the
/// deterministic (MK, shard index) key lookup over the canonical
/// ancestry, returning the matched revision's identity and the
/// strictly decoded shard with the MK and shard-index bindings
/// checked. The same discovery discipline as the manifest's —
/// never key occupancy alone; never a completeness claim.
pub fn find_installed_witness_shard(
    medium: &mut dyn crate::medium::CanonicalMedium,
    mk: &Hash,
    shard_index: u64,
) -> Result<Option<(Hash, AncestryWitnessShard)>, MediumError> {
    let key = reclamation_witness_shard_idempotency_key(mk, shard_index);
    let Some((record, bytes)) = find_witness_record_by_key(medium, &key)? else {
        return Ok(None);
    };
    let shard = AncestryWitnessShard::decode(&bytes).map_err(MediumError::Format)?;
    if shard.mk != *mk || shard.shard_index != shard_index {
        return Err(MediumError::Chain {
            rule: "ancestry-witness-discovery/binding-mismatch",
        });
    }
    Ok(Some((record, shard)))
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
/// twice. The accepted `CheckpointMetadata::decode` enforces
/// strictly increasing, unique index HASHES for decoded metadata
/// objects — it does NOT enforce unique epochs; this helper
/// independently provides the epoch uniqueness — and the
/// standalone helpers accept caller-supplied index slices
/// directly, never through the metadata decode, so BOTH checks
/// close the boundary explicitly rather than assuming the
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

// -------------------------------------------------------------
// (V2X-CP3-A1-2) THE TEST-GATED WITNESS PUBLISHER and the internal
// installed-state kernel. Both compile ONLY under the
// composite-test-fixture feature — the activation barrier the CI
// isolation step nm-verifies against the feature-disabled rlib
// (a public Rust export alone is NOT an adequate barrier). The
// kernel is pub(crate): the future A1-3 consumer reuses it and
// lifts its feature gate as its own separately authorized edit.
// -------------------------------------------------------------

/// (V2X-CP3-A1-2) The completed witness publication: the manifest
/// revision, the per-object outcomes in publication order (the
/// shards in shard-index order, the manifest LAST), and the fully
/// verified discovered manifest. Only `Accepted` carries a provider
/// tier; the duplicate and reconciled forms establish INSTALLATION
/// only (the owner-ratified contract — the destructive consumer
/// must independently establish provider-durable witness authority
/// before any mutation).
#[cfg(feature = "composite-test-fixture")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteWitnessPublication {
    /// The manifest's canonical revision identity (the discovered
    /// installed record).
    pub manifest_record: Hash,
    /// The manifest publication's own outcome.
    pub manifest_outcome: WitnessPublicationOutcome,
    /// The shard publications' outcomes, in shard-index order.
    pub shard_outcomes: Vec<WitnessPublicationOutcome>,
    /// The discovered, completely verified manifest.
    pub manifest: AncestryWitnessManifest,
}

/// The per-member fact authentication shared by PASS 0 and PASS 1:
/// the bytes strict-decode as a canonical REGULAR revision record
/// (the retirement members — SK equally — are regular
/// publications), the decoded epoch equals the indexed epoch, the
/// idempotency key agrees with the index entry's (the Some/None
/// agreement), and the preimage fits the standalone maximum (the
/// fail-closed oversized refusal — before any append). The
/// IDENTITY is the provider's own content-addressed read
/// (hash-verified); PASS 1 adds the explicit re-hash as the
/// pass-divergence detector.
#[cfg(feature = "composite-test-fixture")]
fn verify_member_facts(
    entry: &ReclaimedIndexEntry,
    bytes: &[u8],
    record: &RevisionRecord,
) -> Result<(), MediumError> {
    if !matches!(record.kind, RecordKind::Regular) {
        return Err(MediumError::Chain {
            rule: "ancestry-witness/member-not-regular",
        });
    }
    if record.epoch != entry.epoch {
        return Err(MediumError::Chain {
            rule: "ancestry-witness/member-epoch-mismatch",
        });
    }
    if record.idempotency_key != entry.key {
        return Err(MediumError::Chain {
            rule: "ancestry-witness/member-key-mismatch",
        });
    }
    if bytes.len() as u64 > MAX_WITNESS_MEMBER_PREIMAGE_BYTES {
        return Err(MediumError::Chain {
            rule: "ancestry-witness/member-preimage-oversized",
        });
    }
    Ok(())
}

/// (V2X-CP3-A1-2) THE INTERNAL INSTALLED-STATE COMPLETENESS
/// VERIFICATION KERNEL — pub(crate), feature-gated with the
/// publisher (A1-3 reuses it under its own authorized edit): under
/// the provider's own coordination this establishes the COMPLETE
/// installed witness — the CP2 plan-bound begin authority (the
/// retained-evidence authentication and the tier policy), the
/// installed plan revision, the manifest discovery with the strict
/// bindings (the MK, the plan record, the begin record, and the
/// metadata object), EVERY declared shard `0..shard_count`
/// discovered and strictly decoded (a missing one is the typed
/// installed-state-integrity refusal — an installed manifest
/// claims completeness), and the complete standalone composite over
/// the discovered set (the index shape, the structural round-trip
/// decodes, the per-entry authentication, the parent-edge
/// coherence, the exact coverage, census, digest, and greedy
/// layout). A manifest-present degraded state therefore NEVER
/// verifies; a manifestless shard prefix fails the manifest
/// discovery here and remains resumable by the publisher.
///
/// **The memory contract (honest):** the kernel holds the
/// discovered aggregate — O(total witness bytes) — the A0
/// reference form; the bounded streaming consumer is the
/// separately gated A1-3 obligation.
#[cfg(feature = "composite-test-fixture")]
pub(crate) fn verify_installed_witness_internal(
    medium: &mut dyn crate::medium::CanonicalMedium,
    mk: &Hash,
) -> Result<(Hash, AncestryWitnessManifest, Vec<AncestryWitnessShard>), MediumError> {
    let authority = medium.verify_begin_authority_v2(mk)?;
    let Some((plan_record, _plan)) = crate::reclaim::find_installed_plan(medium, mk)? else {
        return Err(MediumError::Chain {
            rule: "ancestry-witness/plan-absent",
        });
    };
    let Some((manifest_record, manifest)) = find_installed_witness_manifest(medium, mk)? else {
        return Err(MediumError::Chain {
            rule: "ancestry-witness/manifest-absent",
        });
    };
    let mk_record = medium.read_record(mk)?;
    let Some(&metadata_object) = mk_record.content_refs.first() else {
        return Err(MediumError::Chain {
            rule: "ancestry-witness/mk-metadata-binding",
        });
    };
    if manifest.mk != *mk
        || manifest.plan_record != plan_record
        || manifest.begin_record != authority.id()
        || manifest.metadata_object != metadata_object
    {
        return Err(MediumError::Chain {
            rule: "ancestry-witness/manifest-binding-mismatch",
        });
    }
    let meta_bytes = medium.read_object(&metadata_object)?;
    let metadata =
        crate::checkpoint::CheckpointMetadata::decode(&meta_bytes).map_err(MediumError::Format)?;
    let mut shards = Vec::with_capacity(manifest.shard_count as usize);
    for shard_index in 0..manifest.shard_count {
        let Some((_record, shard)) = find_installed_witness_shard(medium, mk, shard_index)? else {
            return Err(MediumError::Chain {
                rule: "ancestry-witness/installed-state-integrity",
            });
        };
        shards.push(shard);
    }
    verify_witness_standalone(&manifest, &shards, &metadata.index).map_err(MediumError::Format)?;
    Ok((manifest_record, manifest, shards))
}

/// (V2X-CP3-A1-2) THE TEST-GATED WITNESS PUBLISHER — the
/// admission-controlled publication boundary the family gate
/// reserves this family for (the in-crate unforgeable receipt
/// through [`crate::medium::CanonicalMedium::append_admitted`],
/// never the unrestricted generic append). The two-pass
/// discipline:
///
/// **PASS 0 — the complete pre-publication feasibility, BEFORE any
/// append:** the CP2 plan-bound begin authority (the complete
/// retained-evidence authentication — the checkpoint, the plan,
/// the complete plan-defined audit set, the begin's temporal
/// position — and the provider tier policy; Windows local storage
/// refuses here); the checkpoint metadata through the MK's
/// authenticated single content object; the index shape (unique
/// identities and epochs); the installed plan revision; the
/// provider capacity (every shard must fit the effective
/// per-object limit); then the member stream in ASCENDING epoch
/// order — every member read ONCE through the trait's
/// hash-verified record read, its canonical re-encoding
/// round-trip re-hashed against the indexed identity,
/// authenticated against its index entry (the regular kind, the
/// epoch, the key, the standalone size ceiling), its size recorded
/// for the boundary derivation, and the original range commitment
/// recomputed as the
/// `chain_range_digest_step` fold over the members EXCLUDING SK —
/// any divergence from the stored `range_digest` refuses. The
/// deterministic DESCENDING-epoch (identity-ascending tiebreak)
/// greedy shard boundaries derive from the recorded sizes. Bounded
/// memory: one member's bytes at a time plus the per-member size
/// table — never the aggregate A0 constructor.
///
/// **PASS 1 — the reauthenticated publication:** every member is
/// re-read and re-verified (the round-trip identity re-hash and
/// the PASS-0 size equality are the pass-divergence detectors —
/// any state change between the passes refuses before any append),
/// the shards publish in shard-index order under the ratified
/// deterministic keys, and the manifest publishes LAST. Each
/// object publication resolves the deterministic key first (an
/// occupied key is a candidate only — byte-identical content is
/// the verified duplicate; ANY difference is the typed conflict),
/// then appends under the CAS with the bounded two-attempt retry
/// (a stale parent retries against the fresh head; an Unknown
/// append reconciles under the lock — Installed carries no
/// invented tier, NotInstalled retries, Unverifiable refuses
/// typed). The complete installed-state kernel verifies the result
/// BEFORE any positive return.
///
/// **The memory contract (honest):** both passes are bounded by
/// one shard's payload at a time; the final kernel verification
/// holds the discovered aggregate (O(total witness bytes)) — the
/// A0 reference form; the bounded streaming consumer is A1-3.
#[cfg(feature = "composite-test-fixture")]
pub fn publish_ancestry_witness(
    medium: &mut dyn crate::medium::CanonicalMedium,
    mk: &Hash,
    tick: u64,
) -> Result<CompleteWitnessPublication, MediumError> {
    // ===== PASS 0 — the complete pre-publication feasibility =====
    // (0a) The CP2 plan-bound begin authority: the complete
    // retained-evidence authentication and the provider tier
    // policy — the Windows ProcessCrashOnly refusal fires HERE,
    // before any witness work.
    let authority = medium.verify_begin_authority_v2(mk)?;
    // (0b) The checkpoint metadata through the MK's authenticated
    // single content object.
    let mk_record = medium.read_record(mk)?;
    if mk_record.content_refs.len() != 1 {
        return Err(MediumError::Chain {
            rule: "ancestry-witness/mk-metadata-binding",
        });
    }
    let metadata_object = mk_record.content_refs[0];
    let meta_bytes = medium.read_object(&metadata_object)?;
    let metadata =
        crate::checkpoint::CheckpointMetadata::decode(&meta_bytes).map_err(MediumError::Format)?;
    // (0c) The index shape: unique member identities and epochs.
    validate_index_shape(&metadata.index).map_err(MediumError::Format)?;
    // (0d) The installed plan revision (the manifest binding).
    let Some((plan_record, _plan)) = crate::reclaim::find_installed_plan(medium, mk)? else {
        return Err(MediumError::Chain {
            rule: "ancestry-witness/plan-absent",
        });
    };
    // (0e) The provider capacity: every shard object must fit the
    // provider's effective per-object limit.
    if medium.effective_object_byte_limit()? < WITNESS_SHARD_PAYLOAD_CEILING {
        return Err(MediumError::Chain {
            rule: "ancestry-witness/provider-capacity-insufficient",
        });
    }
    // (0f) The member stream — ASCENDING epoch order (the range
    // commitment's order), SK EXCLUDED from the digest fold but
    // verified like every member. One hash-verified record read
    // per member (the trait's `read_record` — the provider's own
    // identity check); the canonical re-encoding round-trip is
    // re-hashed against the indexed identity (belt-and-braces —
    // the divergence detector shared with PASS 1); the size is
    // recorded for the boundary derivation.
    let mut ascending: Vec<&ReclaimedIndexEntry> = metadata.index.iter().collect();
    ascending.sort_unstable_by_key(|entry| entry.epoch);
    let mut sizes = std::collections::BTreeMap::<Hash, u64>::new();
    let mut digest = metadata.seed;
    for entry in &ascending {
        let record = medium.read_record(&entry.hash)?;
        let bytes = record.canonical_bytes();
        if hash_object(&bytes) != entry.hash {
            return Err(MediumError::Chain {
                rule: "ancestry-witness/pass-divergence",
            });
        }
        verify_member_facts(entry, &bytes, &record)?;
        sizes.insert(entry.hash, bytes.len() as u64);
        if entry.hash == metadata.sk {
            continue;
        }
        digest = crate::checkpoint::chain_range_digest_step(&digest, &bytes);
    }
    if digest != metadata.range_digest {
        return Err(MediumError::Chain {
            rule: "ancestry-witness/range-digest-mismatch",
        });
    }
    // (0g) The deterministic shard boundaries — the canonical
    // DESCENDING order (epoch DESC, identity ASC — the witness
    // covers EVERY indexed retiring record, SK included) under the
    // greedy maximality rule: identical inputs always produce the
    // identical boundaries.
    let descending = canonical_member_order(&metadata.index);
    let mut boundaries: Vec<(usize, usize)> = Vec::new();
    {
        let mut start = 0usize;
        let mut size = WITNESS_SHARD_HEADER_BYTES;
        for (position, entry) in descending.iter().enumerate() {
            let framed = WITNESS_ENTRY_FRAMING_BYTES + sizes[&entry.hash];
            if WITNESS_SHARD_HEADER_BYTES + framed > WITNESS_SHARD_PAYLOAD_CEILING {
                return Err(MediumError::Chain {
                    rule: "ancestry-witness/member-preimage-oversized",
                });
            }
            if position > start && size + framed > WITNESS_SHARD_PAYLOAD_CEILING {
                boundaries.push((start, position - start));
                start = position;
                size = WITNESS_SHARD_HEADER_BYTES;
            }
            size += framed;
        }
        boundaries.push((start, descending.len() - start));
    }
    let member_count = descending.len() as u64;
    let shard_count = boundaries.len() as u64;
    let coverage_digest =
        witness_coverage_digest(mk, member_count, descending.iter().map(|entry| entry.hash));

    // ===== PASS 1 — the reauthenticated publication =====
    let mut shard_outcomes = Vec::with_capacity(boundaries.len());
    for (shard_index, (start, count)) in boundaries.iter().enumerate() {
        let mut entries: Vec<WitnessEntry> = Vec::with_capacity(*count);
        for entry in &descending[*start..*start + *count] {
            let record = medium.read_record(&entry.hash)?;
            let bytes = record.canonical_bytes();
            // The PASS-0/PASS-1 divergence detectors: the round-trip
            // identity re-hash and the PASS-0 size equality — any
            // state change between the passes refuses BEFORE any
            // append.
            if hash_object(&bytes) != entry.hash {
                return Err(MediumError::Chain {
                    rule: "ancestry-witness/pass-divergence",
                });
            }
            if bytes.len() as u64 != sizes[&entry.hash] {
                return Err(MediumError::Chain {
                    rule: "ancestry-witness/pass-divergence",
                });
            }
            verify_member_facts(entry, &bytes, &record)?;
            entries.push(WitnessEntry {
                member: entry.hash,
                preimage: bytes,
            });
        }
        let shard = AncestryWitnessShard {
            mk: *mk,
            shard_index: shard_index as u64,
            shard_count,
            member_count: *count as u64,
            entries,
        };
        let object = StoredObject::new(shard.canonical_bytes())?;
        let key = reclamation_witness_shard_idempotency_key(mk, shard_index as u64);
        shard_outcomes.push(publish_witness_object(medium, &key, &object, tick)?);
    }
    // The manifest LAST.
    let manifest = AncestryWitnessManifest {
        mk: *mk,
        plan_record,
        begin_record: authority.id(),
        metadata_object,
        member_count,
        shard_count,
        coverage_digest,
    };
    let manifest_object = StoredObject::new(manifest.canonical_bytes())?;
    let manifest_key = reclamation_witness_manifest_idempotency_key(mk);
    let manifest_outcome = publish_witness_object(medium, &manifest_key, &manifest_object, tick)?;
    let manifest_record = match manifest_outcome {
        WitnessPublicationOutcome::Accepted { record, .. }
        | WitnessPublicationOutcome::DuplicateVerified { record }
        | WitnessPublicationOutcome::ReconciledInstalled { record } => record,
        WitnessPublicationOutcome::ReconciledNotInstalled => {
            return Err(MediumError::Chain {
                rule: "ancestry-witness/manifest-not-installed",
            });
        }
    };
    // The complete installed-state verification BEFORE any
    // positive result.
    let (discovered_record, discovered_manifest, _shards) =
        verify_installed_witness_internal(medium, mk)?;
    if discovered_record != manifest_record {
        return Err(MediumError::Chain {
            rule: "ancestry-witness/manifest-record-divergence",
        });
    }
    Ok(CompleteWitnessPublication {
        manifest_record,
        manifest_outcome,
        shard_outcomes,
        manifest: discovered_manifest,
    })
}

/// The single witness-object publication under a deterministic
/// chain-unique key: the duplicate discovery first (key occupancy
/// is a CANDIDATE — byte-identical content is the verified
/// duplicate, ANY difference the typed conflict), then the CAS
/// append through the in-crate unforgeable receipt with the
/// bounded two-attempt retry (stale parent → the fresh-head retry;
/// Unknown → the lock-held reconciliation: Installed carries no
/// invented tier, NotInstalled retries, Unverifiable refuses
/// typed).
#[cfg(feature = "composite-test-fixture")]
fn publish_witness_object(
    medium: &mut dyn crate::medium::CanonicalMedium,
    key: &[u8],
    object: &StoredObject,
    tick: u64,
) -> Result<WitnessPublicationOutcome, MediumError> {
    if let Some((installed_record, installed_bytes)) = find_witness_record_by_key(medium, key)? {
        if installed_bytes == object.bytes() {
            return Ok(WitnessPublicationOutcome::DuplicateVerified {
                record: installed_record,
            });
        }
        return Err(MediumError::Chain {
            rule: "ancestry-witness/duplicate-content-mismatch",
        });
    }
    for attempt in 0..2 {
        let head = medium
            .head()?
            .ok_or(MediumError::Chain { rule: "unfounded" })?;
        let epoch = medium.read_record(&head)?.epoch;
        let record = RevisionRecord::regular(head, epoch, tick)
            .with_idempotency_key(key.to_vec())
            .with_content_refs(vec![object.hash()]);
        let candidate = record.id();
        let admitted =
            crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(object));
        match medium.append_admitted(admitted, Some(head))? {
            crate::medium::AppendOutcome::Accepted { id, tier } => {
                return Ok(WitnessPublicationOutcome::Accepted { record: id, tier });
            }
            crate::medium::AppendOutcome::RejectedStaleParent { .. } => {
                if attempt == 1 {
                    return Err(MediumError::Chain {
                        rule: "ancestry-witness/stale-parent-unresolved",
                    });
                }
                continue;
            }
            crate::medium::AppendOutcome::Unknown { .. } => {
                match medium.reconcile_unknown(&candidate)? {
                    crate::medium::Reconciliation::Installed { id } => {
                        return Ok(WitnessPublicationOutcome::ReconciledInstalled { record: id });
                    }
                    crate::medium::Reconciliation::NotInstalled { .. } => {
                        if attempt == 1 {
                            return Err(MediumError::Chain {
                                rule: "ancestry-witness/unknown-not-installed",
                            });
                        }
                        continue;
                    }
                    crate::medium::Reconciliation::Unverifiable { .. } => {
                        return Err(MediumError::Chain {
                            rule: "ancestry-witness/unknown-unverifiable",
                        });
                    }
                }
            }
        }
    }
    unreachable!("the bounded retry returns within two attempts")
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

    // -------------------------------------------------------------
    // (V2X-CP3-A1) THE DARK FOUNDATION: the family gate, the
    // discovery helpers, and the outcome types. NO publisher
    // exists — the in-crate admitted receipt is the only install
    // route in these fixtures (the established test-only seam).
    // -------------------------------------------------------------

    /// An isolated LocalFile store for the A1 fixtures.
    fn a11_store(label: &str) -> (std::path::PathBuf, crate::LocalFileMedium) {
        let root = std::env::temp_dir().join(format!(
            "s7-a11-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        (root.clone(), crate::LocalFileMedium::open(root).unwrap())
    }

    /// A plain non-witness content object.
    fn plain_object(payload: &[u8]) -> StoredObject {
        StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(payload);
            enc.into_bytes()
        })
        .unwrap()
    }

    /// The in-crate admitted install under a deterministic key
    /// (the only route around the family gate — the established
    /// test-only seam; NOT a publication path).
    fn a11_install_under_key(
        medium: &mut crate::LocalFileMedium,
        key: Vec<u8>,
        object: &StoredObject,
        tick: u64,
    ) -> Hash {
        use crate::medium::CanonicalMedium;
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let record = crate::record::RevisionRecord::regular(head, epoch, tick)
            .with_idempotency_key(key)
            .with_content_refs(vec![object.hash()]);
        let id = record.id();
        let admitted =
            crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(object));
        medium.append_admitted(admitted, Some(head)).unwrap();
        id
    }

    /// A regular step with the given objects.
    fn a11_step(medium: &mut crate::LocalFileMedium, objects: &[StoredObject], tick: u64) -> Hash {
        use crate::medium::CanonicalMedium;
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let refs: Vec<Hash> = objects.iter().map(|o| o.hash()).collect();
        let record =
            crate::record::RevisionRecord::regular(head, epoch, tick).with_content_refs(refs);
        medium.append(&record, objects, Some(head)).unwrap();
        record.id()
    }

    /// A family-variant object with the given readable domain.
    fn family_variant_object(domain: &str) -> StoredObject {
        StoredObject::new({
            let mut enc = engine_core::Encoder::new();
            enc.write_str(domain);
            enc.write_bytes(b"a11-family-variant");
            enc.into_bytes()
        })
        .unwrap()
    }

    #[test]
    fn the_generic_append_refuses_every_witness_family_domain() {
        // (A1-1 §3.1-5) The sixth family gate: the exact manifest
        // and shard domains, the unsupported family versions and
        // variants, and the multi-object form all refuse typed
        // through the ordinary generic append; the non-witness
        // appends remain accepted; no bypass exists through the
        // public call.
        use crate::medium::CanonicalMedium;
        let (root, mut medium) = a11_store("gate");
        let plain = plain_object(b"a11-gate-payload");
        let founding = crate::record::RevisionRecord::founding(
            crate::record::AuthorityMode::Standalone,
            "a11-gate",
            0,
        )
        .with_content_refs(vec![plain.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&plain), None)
            .unwrap();
        // The golden witness objects.
        let (mk, plan, begin, metadata, entries, _index) = golden_fixture();
        let (manifest, shards) = construct_witness(&mk, &plan, &begin, &metadata, entries).unwrap();
        let manifest_object = StoredObject::new(manifest.canonical_bytes()).unwrap();
        let shard_object = StoredObject::new(shards[0].canonical_bytes()).unwrap();
        let cases = [
            ("the manifest domain", manifest_object.clone()),
            ("the shard domain", shard_object.clone()),
            (
                "an unsupported manifest version",
                family_variant_object("engine-persist/reclamation-ancestry-witness/2"),
            ),
            (
                "an unsupported shard version",
                family_variant_object("engine-persist/reclamation-ancestry-witness-shard/9"),
            ),
            (
                "a bare family prefix",
                family_variant_object("engine-persist/reclamation-ancestry-witness"),
            ),
            (
                "a family prefix with a suffix",
                family_variant_object("engine-persist/reclamation-ancestry-witness-bogus/x"),
            ),
        ];
        for (label, object) in cases {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = crate::record::RevisionRecord::regular(head, epoch, 5)
                .with_content_refs(vec![object.hash()]);
            let error = medium
                .append(&record, std::slice::from_ref(&object), Some(head))
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "ancestry-witness/generic-append-refused"
                    }
                ),
                "{label}: the family gate refuses typed: {error:?}"
            );
        }
        // The multi-object form (a plain object beside a witness
        // object) refuses — the gate iterates every supplied
        // object; no bypass through composition.
        {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = crate::record::RevisionRecord::regular(head, epoch, 6)
                .with_content_refs(vec![plain.hash(), manifest_object.hash()]);
            let error = medium
                .append(&record, &[plain.clone(), manifest_object], Some(head))
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "ancestry-witness/generic-append-refused"
                    }
                ),
                "the composed object set refuses: {error:?}"
            );
        }
        // The non-witness append remains ACCEPTED (the unrelated
        // families untouched).
        let head = medium.head().unwrap().unwrap();
        let _ = head;
        a11_step(&mut medium, std::slice::from_ref(&plain), 7);
        assert!(medium.head().unwrap().is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_deterministic_key_discovery_finds_and_authenticates() {
        // (A1-1 §3.6-10) The discovery: the canonical membership,
        // the revision shape, the exact key, the object binding,
        // and the strict decode; the wrong MK and the wrong shard
        // index are the definitive absences; the bad shapes and
        // objects refuse typed.
        use crate::medium::CanonicalMedium;
        let (root, mut medium) = a11_store("discovery");
        let plain = plain_object(b"a11-discovery-payload");
        let founding = crate::record::RevisionRecord::founding(
            crate::record::AuthorityMode::Standalone,
            "a11-discovery",
            0,
        )
        .with_content_refs(vec![plain.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&plain), None)
            .unwrap();
        let _r1 = a11_step(&mut medium, std::slice::from_ref(&plain), 1);
        // The golden witness, installed under the deterministic
        // keys through the in-crate receipt.
        let (mk, plan, begin, metadata, entries, _index) = golden_fixture();
        let (manifest, shards) = construct_witness(&mk, &plan, &begin, &metadata, entries).unwrap();
        let manifest_object = StoredObject::new(manifest.canonical_bytes()).unwrap();
        let shard_object = StoredObject::new(shards[0].canonical_bytes()).unwrap();
        let manifest_record = a11_install_under_key(
            &mut medium,
            reclamation_witness_manifest_idempotency_key(&mk),
            &manifest_object,
            10,
        );
        let shard_record = a11_install_under_key(
            &mut medium,
            reclamation_witness_shard_idempotency_key(&mk, 0),
            &shard_object,
            11,
        );
        // The positive discovery: the exact identities and the
        // decoded objects with the bindings checked.
        let (found_id, found_manifest) = find_installed_witness_manifest(&mut medium, &mk)
            .unwrap()
            .unwrap();
        assert_eq!(found_id, manifest_record);
        assert_eq!(found_manifest, manifest);
        let (found_sid, found_shard) = find_installed_witness_shard(&mut medium, &mk, 0)
            .unwrap()
            .unwrap();
        assert_eq!(found_sid, shard_record);
        assert_eq!(found_shard, shards[0]);
        // The wrong MK and the wrong shard index: the definitive
        // absences (the descent reaches the founding).
        let foreign = fixture_hash(b"a11-foreign-mk");
        assert!(
            find_installed_witness_manifest(&mut medium, &foreign)
                .unwrap()
                .is_none()
        );
        assert!(
            find_installed_witness_shard(&mut medium, &mk, 7)
                .unwrap()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_discovery_refuses_bad_shapes_objects_and_bindings() {
        // (A1-1 §3.8-10) The matched record's shape, the object's
        // decode, and the decoded binding each refuse typed. Each
        // arm uses its own isolated store (the deterministic keys
        // are chain-unique).
        use crate::medium::CanonicalMedium;
        let (mk, plan, begin, metadata, entries, _index) = golden_fixture();
        let (manifest, _shards) =
            construct_witness(&mk, &plan, &begin, &metadata, entries).unwrap();
        let manifest_object = StoredObject::new(manifest.canonical_bytes()).unwrap();
        let plain = plain_object(b"a11-bad-payload");
        // (a) The invalid revision shape: a record under the key
        // with TWO content references.
        {
            let (root, mut medium) = a11_store("discovery-shape");
            let founding = crate::record::RevisionRecord::founding(
                crate::record::AuthorityMode::Standalone,
                "a11-shape",
                0,
            )
            .with_content_refs(vec![plain.hash()]);
            medium
                .append(&founding, std::slice::from_ref(&plain), None)
                .unwrap();
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = crate::record::RevisionRecord::regular(head, epoch, 20)
                .with_idempotency_key(reclamation_witness_manifest_idempotency_key(&mk))
                .with_content_refs(vec![manifest_object.hash(), plain.hash()]);
            let admitted = crate::medium::AdmittedAppend::generic(
                &record,
                &[manifest_object.clone(), plain.clone()],
            );
            medium.append_admitted(admitted, Some(head)).unwrap();
            let error = find_installed_witness_manifest(&mut medium, &mk).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "ancestry-witness-discovery/shape-mismatch"
                    }
                ),
                "the two-reference shape refuses: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // (b) The wrong content-object binding: a record under the
        // key whose single object is not a manifest — the strict
        // decode refuses.
        {
            let (root, mut medium) = a11_store("discovery-object");
            let founding = crate::record::RevisionRecord::founding(
                crate::record::AuthorityMode::Standalone,
                "a11-object",
                0,
            )
            .with_content_refs(vec![plain.hash()]);
            medium
                .append(&founding, std::slice::from_ref(&plain), None)
                .unwrap();
            a11_install_under_key(
                &mut medium,
                reclamation_witness_manifest_idempotency_key(&mk),
                &plain,
                22,
            );
            let error = find_installed_witness_manifest(&mut medium, &mk).unwrap_err();
            assert!(
                matches!(error, MediumError::Format(_)),
                "the non-manifest object refuses at the strict decode: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        // (c) The binding mismatch: a well-formed manifest decoded
        // under the right key but naming a DIFFERENT mk.
        {
            let other_mk = fixture_hash(b"a11-other-mk");
            let doctored = AncestryWitnessManifest {
                mk: other_mk,
                ..manifest.clone()
            };
            let doctored_object = StoredObject::new(doctored.canonical_bytes()).unwrap();
            let (root, mut medium) = a11_store("discovery-binding");
            let founding = crate::record::RevisionRecord::founding(
                crate::record::AuthorityMode::Standalone,
                "a11-binding",
                0,
            )
            .with_content_refs(vec![plain.hash()]);
            medium
                .append(&founding, std::slice::from_ref(&plain), None)
                .unwrap();
            a11_install_under_key(
                &mut medium,
                reclamation_witness_manifest_idempotency_key(&mk),
                &doctored_object,
                23,
            );
            let error = find_installed_witness_manifest(&mut medium, &mk).unwrap_err();
            assert!(
                matches!(
                    error,
                    MediumError::Chain {
                        rule: "ancestry-witness-discovery/binding-mismatch"
                    }
                ),
                "the foreign-mk manifest refuses the binding: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_discovery_crosses_legitimate_gaps_and_refuses_uncovered_ones() {
        // (A1-1 §3.11-12) The gap-aware descent: the positive
        // discovery above a retired range; the definitive absence
        // ACROSS the retired range (the self-skip crossing); the
        // uncovered gap refusing; and the skip-target cycle
        // (a structurally valid table whose self-skip names an
        // already-seen record) refusing through the cycle guard.
        use crate::medium::CanonicalMedium;
        let (root, mut medium) = a11_store("gaps");
        let plain = plain_object(b"a11-gap-payload");
        let founding = crate::record::RevisionRecord::founding(
            crate::record::AuthorityMode::Standalone,
            "a11-gaps",
            0,
        )
        .with_content_refs(vec![plain.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&plain), None)
            .unwrap();
        let founding_id = founding.id();
        let p1 = a11_step(&mut medium, std::slice::from_ref(&plain), 1);
        let r1 = a11_step(&mut medium, std::slice::from_ref(&plain), 2);
        let r2 = a11_step(&mut medium, std::slice::from_ref(&plain), 3);
        let sk = a11_step(&mut medium, std::slice::from_ref(&plain), 4);
        // The checkpoint metadata: the index [r1, r2, sk] with the
        // self-skip to p1 (the retained below-range record).
        let seed = crate::checkpoint::compute_digest_seed(&founding_id);
        let member_bytes: Vec<Vec<u8>> = [r1, r2]
            .iter()
            .map(|id| medium.read_record(id).unwrap().canonical_bytes())
            .collect();
        let digest = crate::checkpoint::chain_range_digest(&seed, &member_bytes);
        let meta = crate::checkpoint::CheckpointMetadata::new(
            sk,
            r1,
            vec![crate::checkpoint::SkipEntry::self_skip(p1)],
            vec![
                crate::checkpoint::ReclaimedIndexEntry::new(None, r1, 2),
                crate::checkpoint::ReclaimedIndexEntry::new(None, r2, 3),
                crate::checkpoint::ReclaimedIndexEntry::new(None, sk, 4),
            ],
            digest,
            seed,
        );
        let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let mk_record = crate::record::RevisionRecord::regular(head, epoch, 5)
            .with_content_refs(vec![meta_object.hash()]);
        medium
            .append(&mk_record, std::slice::from_ref(&meta_object), Some(head))
            .unwrap();
        let _mk = mk_record.id();
        // The witness above the checkpoint.
        let (gmk, plan, begin, metadata, entries, _index) = golden_fixture();
        let (manifest, _shards) =
            construct_witness(&gmk, &plan, &begin, &metadata, entries).unwrap();
        let manifest_object = StoredObject::new(manifest.canonical_bytes()).unwrap();
        let manifest_record = a11_install_under_key(
            &mut medium,
            reclamation_witness_manifest_idempotency_key(&gmk),
            &manifest_object,
            10,
        );
        // The retired range: delete the index members' object
        // files (the legitimate gap under the authenticated
        // self-skip).
        for id in [r1, r2, sk] {
            std::fs::remove_file(medium.object_path_pub(&id)).unwrap();
        }
        // (a) The positive discovery (the witness above the gap).
        let (found_id, found_manifest) = find_installed_witness_manifest(&mut medium, &gmk)
            .unwrap()
            .unwrap();
        assert_eq!(found_id, manifest_record);
        assert_eq!(found_manifest, manifest);
        // (b) The definitive absence ACROSS the gap: the foreign
        // MK's descent must cross the retired range via the
        // self-skip and reach the founding.
        let foreign = fixture_hash(b"a11-gap-foreign-mk");
        assert!(
            find_installed_witness_manifest(&mut medium, &foreign)
                .unwrap()
                .is_none()
        );
        // (c) The uncovered gap: delete p1 (the retained below the
        // range — covered by no skip) — the descent refuses typed.
        std::fs::remove_file(medium.object_path_pub(&p1)).unwrap();
        let error = find_installed_witness_manifest(&mut medium, &foreign).unwrap_err();
        assert!(
            matches!(error, MediumError::MissingObject { .. }),
            "the uncovered gap refuses: {error:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
        // (d) The skip-target cycle: a structurally valid metadata
        // whose self-skip names a record the walk has ALREADY
        // VISITED (the target appended BEFORE the table carrier —
        // no hash circularity). The chain: F → X (the future gap)
        // → T (the target) → C (the carrier, whose content is the
        // metadata with self_skip(T)); deleting X's object makes
        // the walk at T jump back UP to the seen T — the cycle
        // guard fires.
        {
            let (root2, mut medium2) = a11_store("cycle");
            let founding2 = crate::record::RevisionRecord::founding(
                crate::record::AuthorityMode::Standalone,
                "a11-cycle",
                0,
            )
            .with_content_refs(vec![plain.hash()]);
            medium2
                .append(&founding2, std::slice::from_ref(&plain), None)
                .unwrap();
            let c_x = a11_step(&mut medium2, std::slice::from_ref(&plain), 1);
            let c_t = a11_step(&mut medium2, std::slice::from_ref(&plain), 2);
            // The metadata: sk = X (the future missing parent),
            // sk_parent = X (indexed — the self-entry equality rule
            // yields to the indexed form), the self-skip target =
            // T (not indexed, retained — structurally decodable).
            let cyclic_meta = crate::checkpoint::CheckpointMetadata::new(
                c_x,
                c_x,
                vec![crate::checkpoint::SkipEntry::self_skip(c_t)],
                vec![crate::checkpoint::ReclaimedIndexEntry::new(None, c_x, 1)],
                [7u8; 32],
                [7u8; 32],
            );
            let cyclic_object = StoredObject::new(cyclic_meta.canonical_bytes()).unwrap();
            let head = medium2.head().unwrap().unwrap();
            let epoch = medium2.read_record(&head).unwrap().epoch;
            let carrier = crate::record::RevisionRecord::regular(head, epoch, 3)
                .with_content_refs(vec![cyclic_object.hash()]);
            medium2
                .append(&carrier, std::slice::from_ref(&cyclic_object), Some(head))
                .unwrap();
            // The gap: delete X — the walk (the carrier → T →
            // T.parent = X missing) jumps via the tracked table
            // back to the SEEN T.
            std::fs::remove_file(medium2.object_path_pub(&c_x)).unwrap();
            let foreign2 = fixture_hash(b"a11-cycle-foreign-mk");
            let error = find_installed_witness_manifest(&mut medium2, &foreign2).unwrap_err();
            assert!(
                matches!(error, MediumError::Chain { rule: "cycle" }),
                "the skip-target cycle refuses: {error:?}"
            );
            let _ = std::fs::remove_dir_all(&root2);
        }
    }

    #[test]
    fn the_discovery_twin_over_sqlite() {
        // (A1-1 §3.13) The provider-parity twin: the same
        // discovery over SqliteMedium — the positive find with the
        // exact identity and the definitive absence.
        use crate::medium::CanonicalMedium;
        let path = std::env::temp_dir().join(format!(
            "s7-a11-sqlite-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut medium = crate::SqliteMedium::open(&path).unwrap();
        let plain = plain_object(b"a11-sqlite-payload");
        let founding = crate::record::RevisionRecord::founding(
            crate::record::AuthorityMode::Standalone,
            "a11-sqlite",
            0,
        )
        .with_content_refs(vec![plain.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&plain), None)
            .unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let r1 = crate::record::RevisionRecord::regular(head, epoch, 1)
            .with_content_refs(vec![plain.hash()]);
        medium
            .append(&r1, std::slice::from_ref(&plain), Some(head))
            .unwrap();
        let (mk, plan, begin, metadata, entries, _index) = golden_fixture();
        let (manifest, shards) = construct_witness(&mk, &plan, &begin, &metadata, entries).unwrap();
        let manifest_object = StoredObject::new(manifest.canonical_bytes()).unwrap();
        let shard_object = StoredObject::new(shards[0].canonical_bytes()).unwrap();
        // The in-crate installs under the keys.
        let install = |medium: &mut crate::SqliteMedium,
                       key: Vec<u8>,
                       object: &StoredObject,
                       tick: u64|
         -> Hash {
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = crate::record::RevisionRecord::regular(head, epoch, tick)
                .with_idempotency_key(key)
                .with_content_refs(vec![object.hash()]);
            let id = record.id();
            let admitted =
                crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(object));
            medium.append_admitted(admitted, Some(head)).unwrap();
            id
        };
        let manifest_record = install(
            &mut medium,
            reclamation_witness_manifest_idempotency_key(&mk),
            &manifest_object,
            10,
        );
        let shard_record = install(
            &mut medium,
            reclamation_witness_shard_idempotency_key(&mk, 0),
            &shard_object,
            11,
        );
        let (found_id, found_manifest) = find_installed_witness_manifest(&mut medium, &mk)
            .unwrap()
            .unwrap();
        assert_eq!(found_id, manifest_record);
        assert_eq!(found_manifest, manifest);
        let (found_sid, found_shard) = find_installed_witness_shard(&mut medium, &mk, 0)
            .unwrap()
            .unwrap();
        assert_eq!(found_sid, shard_record);
        assert_eq!(found_shard, shards[0]);
        let foreign = fixture_hash(b"a11-sqlite-foreign-mk");
        assert!(
            find_installed_witness_manifest(&mut medium, &foreign)
                .unwrap()
                .is_none()
        );
        drop(medium);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
    }
}

// -------------------------------------------------------------
// (V2X-CP3-A1-2) THE PUBLISHER REGRESSIONS — feature-gated with
// the publisher itself: genuine LocalFile (POSIX) and SQLite
// campaigns through the full CP2 admission chain, the Windows
// begin-tier refusal, the PASS-0 refusals before any append, the
// partial-installation resume, the CAS/reconciliation arms, the
// manifest-present degradation, and the PASS-0/PASS-1 divergence.
// -------------------------------------------------------------

#[cfg(all(test, feature = "composite-test-fixture"))]
mod a12_tests {
    use super::*;
    use crate::medium::CanonicalMedium;

    // ------------------------------------------------- fixtures

    fn fixture_hash(label: &[u8]) -> Hash {
        hash_object(label)
    }

    fn plain_object(payload: &[u8]) -> StoredObject {
        StoredObject::new({
            let mut enc = Encoder::new();
            enc.write_str("sektor7/save/1");
            enc.write_bytes(payload);
            enc.into_bytes()
        })
        .unwrap()
    }

    fn a12_unique(label: &str) -> String {
        format!(
            "{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    fn a12_local_store(label: &str) -> (std::path::PathBuf, crate::LocalFileMedium) {
        let root = std::env::temp_dir().join(format!("s7-a12-{}", a12_unique(label)));
        std::fs::create_dir_all(&root).unwrap();
        (root.clone(), crate::LocalFileMedium::open(root).unwrap())
    }

    fn a12_sqlite_store(label: &str) -> (std::path::PathBuf, crate::SqliteMedium) {
        let path = std::env::temp_dir().join(format!("s7-a12-{}.db", a12_unique(label)));
        let medium = crate::SqliteMedium::open(&path).unwrap();
        (path, medium)
    }

    fn a12_sqlite_cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
    }

    fn a12_journal_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("s7-a12-journal-{}", a12_unique(label)))
    }

    fn a12_padded_key(base: &str, key_len: usize) -> Vec<u8> {
        assert!(
            key_len >= base.len() && key_len <= crate::record::MAX_IDEMPOTENCY_KEY_LEN as usize
        );
        let mut key = base.as_bytes().to_vec();
        key.resize(key_len, b'k');
        key
    }

    fn a12_member_key(label: &str, index: usize, key_len: usize) -> Vec<u8> {
        a12_padded_key(&format!("a12-{label}-member-{index:04}"), key_len)
    }

    /// A fabricated ascending identity (never zero, unique per
    /// counter) — the dependency-resolution fixture's index
    /// material.
    fn fabricated_hash(counter: u64) -> Hash {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&counter.to_be_bytes());
        Hash::from_bytes(&bytes).expect("the fabricated identity is well-formed")
    }

    /// A regular step with the given objects.
    fn a12_step(medium: &mut dyn CanonicalMedium, objects: &[StoredObject], tick: u64) -> Hash {
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let refs: Vec<Hash> = objects.iter().map(|object| object.hash()).collect();
        let record =
            crate::record::RevisionRecord::regular(head, epoch, tick).with_content_refs(refs);
        medium.append(&record, objects, Some(head)).unwrap();
        record.id()
    }

    /// The in-crate admitted install under a deterministic key (the
    /// same unforgeable-receipt route the publisher uses — the only
    /// route around the family gate).
    fn a12_install_under_key(
        medium: &mut dyn CanonicalMedium,
        key: Vec<u8>,
        object: &StoredObject,
        tick: u64,
    ) -> Hash {
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let record = crate::record::RevisionRecord::regular(head, epoch, tick)
            .with_idempotency_key(key)
            .with_content_refs(vec![object.hash()]);
        let id = record.id();
        let admitted =
            crate::medium::AdmittedAppend::generic(&record, std::slice::from_ref(object));
        medium.append_admitted(admitted, Some(head)).unwrap();
        id
    }

    struct A12Campaign {
        mk: Hash,
        members: Vec<Hash>,
        sk: Hash,
    }

    /// The provider-neutral V2 campaign (the accepted
    /// `v2_plan_campaign` shape, generalized): the founding, the
    /// campaign-bound journal with the canonical root binding, p1,
    /// the retiring members (each with a chain-unique padded key —
    /// the key length is the preimage-size control), SK, the
    /// checkpoint metadata (the TRUE record epochs; the
    /// ascending-order range digest over the members EXCLUDING SK;
    /// the self-skip to p1), the MK, the plan, and EVERY
    /// plan-defined audit. The begin admission is the caller's
    /// (the tier policy differs by platform). The fixed tick
    /// sequence makes the campaign a pure function of (label,
    /// journal location, member specs) — the cross-provider
    /// byte-identity regression depends on it.
    fn a12_campaign(
        medium: &mut dyn CanonicalMedium,
        label: &str,
        journal_root: &std::path::Path,
        members: &[(usize, Vec<u8>)],
        ledger_limit: u64,
    ) -> A12Campaign {
        let shared = plain_object(b"a12-shared-payload");
        let founding = crate::record::RevisionRecord::founding(
            crate::record::AuthorityMode::Standalone,
            label,
            0,
        )
        .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        let founding_id = founding.id();
        let location = crate::reclaim::JournalRootBinding::normalize_location(journal_root);
        let root_id = crate::reclaim::journal_root_id(&founding_id, &location);
        drop(
            crate::reclaim::create_campaign_bound_journal(medium, journal_root, &founding_id)
                .unwrap(),
        );
        let (_binding, binding_outcome) = crate::reclaim::publish_journal_root_binding(
            medium,
            &founding_id,
            &root_id,
            &location,
            1,
        )
        .unwrap();
        assert!(
            matches!(
                binding_outcome,
                crate::reclaim::JournalRootBindingOutcome::Accepted { .. }
            ),
            "the fixture's binding publishes: {binding_outcome:?}"
        );
        // p1 — the retained pre-checkpoint head.
        let p1_id = a12_step(medium, std::slice::from_ref(&shared), 2);
        // The retiring members.
        let mut member_ids = Vec::with_capacity(members.len());
        for (index, (key_len, payload)) in members.iter().enumerate() {
            let object = plain_object(payload);
            let key = a12_member_key(label, index, *key_len);
            let head = medium.head().unwrap().unwrap();
            let epoch = medium.read_record(&head).unwrap().epoch;
            let record = crate::record::RevisionRecord::regular(head, epoch, 3)
                .with_idempotency_key(key)
                .with_content_refs(vec![object.hash()]);
            medium
                .append(&record, std::slice::from_ref(&object), Some(head))
                .unwrap();
            member_ids.push(record.id());
        }
        // SK — the range's terminal self-retiring record.
        let sk_id = a12_step(medium, std::slice::from_ref(&shared), 4);
        // The checkpoint metadata: the TRUE record epochs (read
        // from the installed records), the ascending-order range
        // digest over the members EXCLUDING SK, and the self-skip
        // to p1 (the accepted fixture shape — SK's chain parent is
        // itself a retiring range member, so the self-skip jumps to
        // the retained pre-checkpoint head).
        let seed = crate::checkpoint::compute_digest_seed(&founding_id);
        let mut digest = seed;
        let mut index = Vec::with_capacity(members.len() + 1);
        for id in &member_ids {
            let record = medium.read_record(id).unwrap();
            digest = crate::checkpoint::chain_range_digest_step(&digest, &record.canonical_bytes());
            index.push(crate::checkpoint::ReclaimedIndexEntry::new(
                record.idempotency_key.clone(),
                *id,
                record.epoch,
            ));
        }
        let sk_record = medium.read_record(&sk_id).unwrap();
        index.push(crate::checkpoint::ReclaimedIndexEntry::new(
            sk_record.idempotency_key.clone(),
            sk_id,
            sk_record.epoch,
        ));
        let meta = crate::checkpoint::CheckpointMetadata::new(
            sk_id,
            member_ids[0],
            vec![crate::checkpoint::SkipEntry::new(Hash::ZERO, p1_id)],
            index,
            digest,
            seed,
        );
        let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let mk = crate::record::RevisionRecord::regular(head, epoch, 5)
            .with_content_refs(vec![meta_object.hash()]);
        medium
            .append(&mk, std::slice::from_ref(&meta_object), Some(head))
            .unwrap();
        let mk_id = mk.id();
        // The plan and EVERY plan-defined audit (the
        // all-audits-first contract the begin admission enforces).
        let (_plan_record, _plan_outcome) =
            crate::reclaim::publish_reclamation_plan(medium, &mk_id, ledger_limit, journal_root, 6)
                .unwrap();
        let (_plan_id, plan) = crate::reclaim::find_installed_plan(medium, &mk_id)
            .unwrap()
            .unwrap();
        for batch in &plan.batches {
            crate::reclaim::publish_reclamation_ledger_v2(medium, &mk_id, batch.index, 7).unwrap();
        }
        A12Campaign {
            mk: mk_id,
            members: member_ids,
            sk: sk_id,
        }
    }

    /// The two-member default campaign (the member keys at the
    /// small default length).
    fn a12_small_members(label: &str) -> Vec<(usize, Vec<u8>)> {
        vec![
            (48, format!("a12-{label}-payload-m0").into_bytes()),
            (48, format!("a12-{label}-payload-m1").into_bytes()),
        ]
    }

    // ------------------------------ the CAS/reconciliation double

    /// The single-fire interception the stale-parent, Unknown, and
    /// pass-divergence arms drive.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum A12Arm {
        None,
        StaleOnce,
        UnknownNotInstalledOnce,
        UnknownInstalledOnce,
        UnverifiableOnce,
        DivergeMemberRead(Hash),
    }

    /// The delegating wrapper over the SQLite medium: every trait
    /// method delegates unchanged; the armed interception fires
    /// ONCE at the chosen boundary and stands down (the publisher's
    /// bounded retry then converges, or the typed refusal lands).
    struct A12Wrapped {
        inner: crate::SqliteMedium,
        arm: A12Arm,
        diverge_reads: u32,
        unverifiable_reconcile: bool,
    }

    impl A12Wrapped {
        fn armed(inner: crate::SqliteMedium, arm: A12Arm) -> Self {
            A12Wrapped {
                inner,
                arm,
                diverge_reads: 0,
                unverifiable_reconcile: false,
            }
        }

        fn into_inner(self) -> crate::SqliteMedium {
            self.inner
        }
    }

    impl CanonicalMedium for A12Wrapped {
        fn head(&mut self) -> Result<Option<Hash>, MediumError> {
            self.inner.head()
        }

        fn append_admitted(
            &mut self,
            admitted: crate::medium::AdmittedAppend,
            expected_parent: Option<Hash>,
        ) -> Result<crate::medium::AppendOutcome, MediumError> {
            // The read-divergence arm does not intercept appends.
            if matches!(&self.arm, A12Arm::DivergeMemberRead(_)) {
                return self.inner.append_admitted(admitted, expected_parent);
            }
            match std::mem::replace(&mut self.arm, A12Arm::None) {
                A12Arm::None => self.inner.append_admitted(admitted, expected_parent),
                A12Arm::StaleOnce => {
                    // Advance the authoritative head beneath the
                    // publisher's feet, then delegate the real call
                    // against the now-stale expected parent.
                    let decoy = plain_object(b"a12-stale-decoy");
                    let head = self.inner.head()?.unwrap();
                    let epoch = self.inner.read_record(&head)?.epoch;
                    let record = crate::record::RevisionRecord::regular(head, epoch, 99)
                        .with_content_refs(vec![decoy.hash()]);
                    self.inner
                        .append(&record, std::slice::from_ref(&decoy), Some(head))
                        .unwrap();
                    self.inner.append_admitted(admitted, expected_parent)
                }
                A12Arm::UnknownNotInstalledOnce => Ok(crate::medium::AppendOutcome::Unknown {
                    reason: crate::medium::UnknownReason::CommitUncertain,
                }),
                A12Arm::UnknownInstalledOnce => {
                    let outcome = self.inner.append_admitted(admitted, expected_parent)?;
                    if matches!(outcome, crate::medium::AppendOutcome::Accepted { .. }) {
                        return Ok(crate::medium::AppendOutcome::Unknown {
                            reason: crate::medium::UnknownReason::CommitUncertain,
                        });
                    }
                    Ok(outcome)
                }
                A12Arm::UnverifiableOnce => {
                    self.unverifiable_reconcile = true;
                    Ok(crate::medium::AppendOutcome::Unknown {
                        reason: crate::medium::UnknownReason::CommitUncertain,
                    })
                }
                A12Arm::DivergeMemberRead(_) => {
                    self.inner.append_admitted(admitted, expected_parent)
                }
            }
        }

        fn read_record(&mut self, id: &Hash) -> Result<crate::record::RevisionRecord, MediumError> {
            let diverge = matches!(&self.arm, A12Arm::DivergeMemberRead(target) if target == id);
            let record = self.inner.read_record(id)?;
            if diverge {
                self.diverge_reads += 1;
                if self.diverge_reads == 2 {
                    // The doctored second read: PASS 0 saw the
                    // genuine record; PASS 1's re-verification must
                    // refuse the divergence (the doctored record's
                    // canonical re-encoding no longer hashes to the
                    // indexed identity).
                    self.arm = A12Arm::None;
                    let mut doctored = record;
                    doctored.tick += 1;
                    return Ok(doctored);
                }
            }
            Ok(record)
        }

        fn read_object(&mut self, hash: &Hash) -> Result<Vec<u8>, MediumError> {
            self.inner.read_object(hash)
        }

        fn validate(&mut self) -> Result<crate::medium::ChainSummary, MediumError> {
            self.inner.validate()
        }

        fn reconcile_unknown(
            &mut self,
            candidate: &Hash,
        ) -> Result<crate::medium::Reconciliation, MediumError> {
            if self.unverifiable_reconcile {
                self.unverifiable_reconcile = false;
                return Ok(crate::medium::Reconciliation::Unverifiable {
                    reason: "a12-fixture-unverifiable".to_string(),
                });
            }
            self.inner.reconcile_unknown(candidate)
        }

        fn durability_tier(&self) -> crate::medium::SyncTier {
            self.inner.durability_tier()
        }

        fn effective_object_byte_limit(&mut self) -> Result<u64, MediumError> {
            self.inner.effective_object_byte_limit()
        }

        fn reclaim(
            &mut self,
            directive: &crate::reclaim::ReclaimDirective,
        ) -> Result<crate::reclaim::ReclaimOutcome, MediumError> {
            self.inner.reclaim(directive)
        }

        fn reclaim_pending(
            &mut self,
            directive: &crate::reclaim::ReclaimDirective,
        ) -> Result<bool, MediumError> {
            self.inner.reclaim_pending(directive)
        }

        fn checkpoint(&mut self) -> Result<(), MediumError> {
            self.inner.checkpoint()
        }

        fn begin_reclamation(
            &mut self,
            audit: &Hash,
            mk: &Hash,
            tick: u64,
        ) -> Result<crate::reclaim::BeginPublicationOutcome, MediumError> {
            self.inner.begin_reclamation(audit, mk, tick)
        }

        fn verify_begin_authority(
            &mut self,
            audit: &Hash,
            mk: &Hash,
        ) -> Result<crate::reclaim::BeginAuthority, MediumError> {
            self.inner.verify_begin_authority(audit, mk)
        }

        fn begin_reclamation_v2(
            &mut self,
            mk: &Hash,
            tick: u64,
        ) -> Result<crate::reclaim::BeginPublicationOutcome, MediumError> {
            self.inner.begin_reclamation_v2(mk, tick)
        }

        fn verify_begin_authority_v2(
            &mut self,
            mk: &Hash,
        ) -> Result<crate::reclaim::BeginAuthority, MediumError> {
            self.inner.verify_begin_authority_v2(mk)
        }
    }

    // ------------------------------------------------ the tests

    #[cfg(unix)]
    #[test]
    fn the_localfile_publication_completes_and_reverifies() {
        let (root, mut medium) = a12_local_store("lf-positive");
        let journal = a12_journal_root("lf-positive");
        let members = a12_small_members("lf-positive");
        let campaign = a12_campaign(&mut medium, "lf-positive", &journal, &members, 10_000);
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        let before_head = medium.head().unwrap().unwrap();
        let publication = publish_ancestry_witness(&mut medium, &campaign.mk, 9).unwrap();
        // Every object freshly accepted at the honest POSIX tier.
        assert_eq!(publication.shard_outcomes.len(), 1);
        match publication.shard_outcomes[0] {
            WitnessPublicationOutcome::Accepted { tier, .. } => {
                assert_eq!(tier, crate::medium::SyncTier::FullPosixSequence);
            }
            other => panic!("the shard must be freshly accepted: {other:?}"),
        }
        match publication.manifest_outcome {
            WitnessPublicationOutcome::Accepted { tier, .. } => {
                assert_eq!(tier, crate::medium::SyncTier::FullPosixSequence);
            }
            other => panic!("the manifest must be freshly accepted: {other:?}"),
        }
        // The manifest is the LAST append.
        assert_ne!(publication.manifest_record, before_head);
        assert_eq!(medium.head().unwrap().unwrap(), publication.manifest_record);
        assert_eq!(publication.manifest.member_count, 3);
        assert_eq!(publication.manifest.shard_count, 1);
        // The digest-vs-shard-order distinction: the PASS-0 range
        // commitment folded the members in ASCENDING epoch order
        // (the metadata's authenticated digest matched), while the
        // published shard carries them DESCENDING — one
        // publication proves both orders distinctly.
        let (_record, shard) = find_installed_witness_shard(&mut medium, &campaign.mk, 0)
            .unwrap()
            .unwrap();
        let epochs: Vec<u64> = shard
            .entries
            .iter()
            .map(|entry| {
                crate::record::RevisionRecord::decode(&entry.preimage)
                    .unwrap()
                    .epoch
            })
            .collect();
        assert!(
            epochs.windows(2).all(|pair| pair[0] > pair[1]),
            "the shard order is epoch-descending: {epochs:?}"
        );
        let sk_epoch = medium.read_record(&campaign.sk).unwrap().epoch;
        assert!(
            epochs.contains(&sk_epoch),
            "SK's preimage is witnessed with the members: {epochs:?}"
        );
        // The kernel and the fresh-handle restart discipline.
        verify_installed_witness_internal(&mut medium, &campaign.mk).unwrap();
        drop(medium);
        let mut reopened = crate::LocalFileMedium::open(root.clone()).unwrap();
        verify_installed_witness_internal(&mut reopened, &campaign.mk).unwrap();
        drop(reopened);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }

    #[test]
    fn the_sqlite_publication_completes_and_survives_restart() {
        let (path, mut medium) = a12_sqlite_store("sqlite-positive");
        let journal = a12_journal_root("sqlite-positive");
        let members = a12_small_members("sqlite-positive");
        let campaign = a12_campaign(&mut medium, "sqlite-positive", &journal, &members, 10_000);
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        let publication = publish_ancestry_witness(&mut medium, &campaign.mk, 9).unwrap();
        assert_eq!(publication.shard_outcomes.len(), 1);
        match publication.shard_outcomes[0] {
            WitnessPublicationOutcome::Accepted { tier, .. } => {
                assert_eq!(tier, crate::medium::SyncTier::TransactionalWalFull);
            }
            other => panic!("the shard must be freshly accepted: {other:?}"),
        }
        assert_eq!(medium.head().unwrap().unwrap(), publication.manifest_record);
        assert_eq!(publication.manifest.member_count, 3);
        verify_installed_witness_internal(&mut medium, &campaign.mk).unwrap();
        // The restart: a fresh handle re-verifies the complete
        // installed state.
        drop(medium);
        let mut reopened = crate::SqliteMedium::open(&path).unwrap();
        verify_installed_witness_internal(&mut reopened, &campaign.mk).unwrap();
        drop(reopened);
        a12_sqlite_cleanup(&path);
        let _ = std::fs::remove_dir_all(&journal);
    }

    #[cfg(not(unix))]
    #[test]
    fn the_windows_localfile_begin_tier_refusal_preserved() {
        let (root, mut medium) = a12_local_store("win-tier");
        let journal = a12_journal_root("win-tier");
        let members = a12_small_members("win-tier");
        let campaign = a12_campaign(&mut medium, "win-tier", &journal, &members, 10_000);
        // The begin admission refuses the ProcessCrashOnly tier —
        // the CP2 authority boundary is preserved.
        assert!(medium.begin_reclamation_v2(&campaign.mk, 8).is_err());
        // The publisher inherits the refusal BEFORE any witness
        // work: nothing installs and the head never moves.
        let before_head = medium.head().unwrap().unwrap();
        assert!(publish_ancestry_witness(&mut medium, &campaign.mk, 9).is_err());
        assert_eq!(medium.head().unwrap().unwrap(), before_head);
        assert!(
            find_installed_witness_manifest(&mut medium, &campaign.mk)
                .unwrap()
                .is_none()
        );
        assert!(
            find_installed_witness_shard(&mut medium, &campaign.mk, 0)
                .unwrap()
                .is_none()
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }

    #[cfg(unix)]
    #[test]
    fn the_witness_content_is_provider_independent() {
        // The same label, payloads, keys, ticks, and journal-root
        // LOCATION on both providers produce byte-identical chains
        // and therefore byte-identical witness objects — the
        // canonical formats are provider-blind.
        let shared_journal = std::env::temp_dir().join("s7-a12-xprov-shared-journal");
        let _ = std::fs::remove_dir_all(&shared_journal);
        let members = vec![
            (32usize, b"a12-xprov-payload-m0".to_vec()),
            (32, b"a12-xprov-payload-m1".to_vec()),
        ];
        let (root_a, mut medium_a) = a12_local_store("xprov");
        let campaign_a = a12_campaign(&mut medium_a, "xprov", &shared_journal, &members, 10_000);
        medium_a.begin_reclamation_v2(&campaign_a.mk, 8).unwrap();
        let publication_a = publish_ancestry_witness(&mut medium_a, &campaign_a.mk, 9).unwrap();
        let manifest_a_bytes = publication_a.manifest.canonical_bytes();
        let (_record_a, shard_a) = find_installed_witness_shard(&mut medium_a, &campaign_a.mk, 0)
            .unwrap()
            .unwrap();
        let shard_a_bytes = shard_a.canonical_bytes();
        drop(medium_a);
        let _ = std::fs::remove_dir_all(&root_a);
        // The journal root is consumed and removed between the
        // campaigns so the SQLite campaign binds the IDENTICAL
        // location string — the record bytes (and therefore the
        // chains) stay identical.
        let _ = std::fs::remove_dir_all(&shared_journal);
        let (path_b, mut medium_b) = a12_sqlite_store("xprov");
        let campaign_b = a12_campaign(&mut medium_b, "xprov", &shared_journal, &members, 10_000);
        medium_b.begin_reclamation_v2(&campaign_b.mk, 8).unwrap();
        let publication_b = publish_ancestry_witness(&mut medium_b, &campaign_b.mk, 9).unwrap();
        // The identical chains (the identical MK) and the
        // byte-identical witness objects.
        assert_eq!(campaign_a.mk, campaign_b.mk);
        assert_eq!(publication_b.manifest, publication_a.manifest);
        assert_eq!(publication_b.manifest.canonical_bytes(), manifest_a_bytes);
        let (_record_b, shard_b) = find_installed_witness_shard(&mut medium_b, &campaign_b.mk, 0)
            .unwrap()
            .unwrap();
        assert_eq!(shard_b.canonical_bytes(), shard_a_bytes);
        drop(medium_b);
        a12_sqlite_cleanup(&path_b);
        let _ = std::fs::remove_dir_all(&shared_journal);
    }

    #[cfg(unix)]
    #[test]
    fn the_beginless_campaign_refuses_before_any_append() {
        let (root, mut medium) = a12_local_store("beginless");
        let journal = a12_journal_root("beginless");
        let members = a12_small_members("beginless");
        let campaign = a12_campaign(&mut medium, "beginless", &journal, &members, 10_000);
        // No begin admission: the publisher's PASS 0 refuses at
        // the authority — BEFORE the first append.
        let before_head = medium.head().unwrap().unwrap();
        assert!(publish_ancestry_witness(&mut medium, &campaign.mk, 9).is_err());
        assert_eq!(medium.head().unwrap().unwrap(), before_head);
        assert!(
            find_installed_witness_manifest(&mut medium, &campaign.mk)
                .unwrap()
                .is_none()
        );
        assert!(
            find_installed_witness_shard(&mut medium, &campaign.mk, 0)
                .unwrap()
                .is_none()
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }

    #[test]
    fn the_oversized_member_refuses_before_any_append() {
        // The PASS-0 single-entry ceiling: a member whose canonical
        // preimage exceeds MAX_WITNESS_MEMBER_PREIMAGE_BYTES
        // (4,194,121) refuses BEFORE any append. The oversized
        // record is a GENUINE publication — the maximum
        // 65,536-byte idempotency key, 44,000 real content objects
        // (one append), and 60,000 dependency references resolved
        // through the accepted reclaim-index rule (a prior
        // metadata-carrying record's index of never-appended
        // identities — the dependency-resolution discipline the
        // fresh handle's cold walk establishes).
        let (path, mut medium) = a12_sqlite_store("oversized");
        let journal = a12_journal_root("oversized");
        let shared = plain_object(b"a12-shared-payload");
        let founding = crate::record::RevisionRecord::founding(
            crate::record::AuthorityMode::Standalone,
            "a12-oversized",
            0,
        )
        .with_content_refs(vec![shared.hash()]);
        medium
            .append(&founding, std::slice::from_ref(&shared), None)
            .unwrap();
        let founding_id = founding.id();
        let location = crate::reclaim::JournalRootBinding::normalize_location(&journal);
        let root_id = crate::reclaim::journal_root_id(&founding_id, &location);
        drop(
            crate::reclaim::create_campaign_bound_journal(&mut medium, &journal, &founding_id)
                .unwrap(),
        );
        let (_binding, binding_outcome) = crate::reclaim::publish_journal_root_binding(
            &mut medium,
            &founding_id,
            &root_id,
            &location,
            1,
        )
        .unwrap();
        assert!(matches!(
            binding_outcome,
            crate::reclaim::JournalRootBindingOutcome::Accepted { .. }
        ));
        let p1_id = a12_step(&mut medium, std::slice::from_ref(&shared), 2);
        // The dependency-resolution carrier: a metadata-carrying
        // record whose index names 60,000 never-appended ascending
        // identities at epoch 1 — after the fresh handle's cold
        // walk they join the reclaimed universe exactly as a
        // retired range's index would, resolving the oversized
        // member's dependency references without 60,000 real
        // records.
        let dep_index: Vec<crate::checkpoint::ReclaimedIndexEntry> = (1..=65_000u64)
            .map(|counter| {
                crate::checkpoint::ReclaimedIndexEntry::new(None, fabricated_hash(counter), 1)
            })
            .collect();
        let dep_meta = crate::checkpoint::CheckpointMetadata::new(
            fixture_hash(b"a12-oversized-meta-sk"),
            fixture_hash(b"a12-oversized-meta-sk-parent"),
            Vec::new(),
            dep_index,
            [9u8; 32],
            [7u8; 32],
        );
        let dep_meta_object = StoredObject::new(dep_meta.canonical_bytes()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let dep_carrier = crate::record::RevisionRecord::regular(head, epoch, 3)
            .with_content_refs(vec![dep_meta_object.hash()]);
        medium
            .append(
                &dep_carrier,
                std::slice::from_ref(&dep_meta_object),
                Some(head),
            )
            .unwrap();
        // The fresh handle: the cold walk absorbs the carrier's
        // index before the oversized member's dependency
        // resolution.
        drop(medium);
        let mut medium = crate::SqliteMedium::open(&path).unwrap();
        let oversized_objects: Vec<StoredObject> = (0..38_400u32)
            .map(|index| plain_object(format!("a12-oversized-obj-{index}").as_bytes()))
            .collect();
        let oversized_refs: Vec<Hash> = oversized_objects
            .iter()
            .map(|object| object.hash())
            .collect();
        let oversized_deps: Vec<Hash> = (1..=65_000u64).map(fabricated_hash).collect();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let oversized = crate::record::RevisionRecord::regular(head, epoch, 3)
            .with_idempotency_key(a12_padded_key(
                "a12-oversized-member",
                crate::record::MAX_IDEMPOTENCY_KEY_LEN as usize,
            ))
            .with_content_refs(oversized_refs)
            .with_depends_on(oversized_deps);
        let oversized_id = oversized.id();
        let oversized_preimage = oversized.canonical_bytes();
        assert!(
            oversized_preimage.len() as u64 > MAX_WITNESS_MEMBER_PREIMAGE_BYTES,
            "the fixture genuinely exceeds the ceiling: {} bytes",
            oversized_preimage.len()
        );
        medium
            .append(&oversized, &oversized_objects, Some(head))
            .unwrap();
        // A small sibling member and SK close the range.
        let sibling_object = plain_object(b"a12-oversized-sibling");
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let sibling = crate::record::RevisionRecord::regular(head, epoch, 3)
            .with_idempotency_key(a12_member_key("oversized", 0, 32))
            .with_content_refs(vec![sibling_object.hash()]);
        medium
            .append(&sibling, std::slice::from_ref(&sibling_object), Some(head))
            .unwrap();
        let sk_id = a12_step(&mut medium, std::slice::from_ref(&shared), 4);
        // The metadata over [oversized, sibling, sk] — the true
        // epochs and the ascending-order digest excluding SK.
        let seed = crate::checkpoint::compute_digest_seed(&founding_id);
        let mut digest = seed;
        let mut index = Vec::with_capacity(3);
        for id in [&oversized_id, &sibling.id()] {
            let record = medium.read_record(id).unwrap();
            digest = crate::checkpoint::chain_range_digest_step(&digest, &record.canonical_bytes());
            index.push(crate::checkpoint::ReclaimedIndexEntry::new(
                record.idempotency_key.clone(),
                *id,
                record.epoch,
            ));
        }
        let sk_record = medium.read_record(&sk_id).unwrap();
        index.push(crate::checkpoint::ReclaimedIndexEntry::new(
            sk_record.idempotency_key.clone(),
            sk_id,
            sk_record.epoch,
        ));
        let meta = crate::checkpoint::CheckpointMetadata::new(
            sk_id,
            oversized_id,
            vec![crate::checkpoint::SkipEntry::new(Hash::ZERO, p1_id)],
            index,
            digest,
            seed,
        );
        let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let mk = crate::record::RevisionRecord::regular(head, epoch, 5)
            .with_content_refs(vec![meta_object.hash()]);
        medium
            .append(&mk, std::slice::from_ref(&meta_object), Some(head))
            .unwrap();
        let mk_id = mk.id();
        let (_plan_record, _plan_outcome) =
            crate::reclaim::publish_reclamation_plan(&mut medium, &mk_id, 10_000_000, &journal, 6)
                .unwrap();
        let (_plan_id, plan) = crate::reclaim::find_installed_plan(&mut medium, &mk_id)
            .unwrap()
            .unwrap();
        for batch in &plan.batches {
            crate::reclaim::publish_reclamation_ledger_v2(&mut medium, &mk_id, batch.index, 7)
                .unwrap();
        }
        medium.begin_reclamation_v2(&mk_id, 8).unwrap();
        // PASS 0 refuses at the oversized member — BEFORE any
        // witness append.
        let before_head = medium.head().unwrap().unwrap();
        let error = publish_ancestry_witness(&mut medium, &mk_id, 9).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "ancestry-witness/member-preimage-oversized"
                }
            ),
            "the oversized refusal is typed: {error:?}"
        );
        assert_eq!(medium.head().unwrap().unwrap(), before_head);
        assert!(
            find_installed_witness_manifest(&mut medium, &mk_id)
                .unwrap()
                .is_none()
        );
        assert!(
            find_installed_witness_shard(&mut medium, &mk_id, 0)
                .unwrap()
                .is_none()
        );
        drop(medium);
        a12_sqlite_cleanup(&path);
        let _ = std::fs::remove_dir_all(&journal);
    }

    #[cfg(unix)]
    #[test]
    fn the_multishard_publication_splits_deterministically() {
        // ~4.2 MB of member preimages (70 members with 60,000-byte
        // keys) — two greedy shards under the 4,194,304-byte
        // payload ceiling; the kernel's greedy-maximality check
        // certifies the boundary.
        let (root, mut medium) = a12_local_store("multishard");
        let journal = a12_journal_root("multishard");
        let members: Vec<(usize, Vec<u8>)> = (0..70)
            .map(|index| {
                (
                    60_000,
                    format!("a12-multishard-payload-m{index}").into_bytes(),
                )
            })
            .collect();
        let campaign = a12_campaign(&mut medium, "multishard", &journal, &members, 100_000);
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        let publication = publish_ancestry_witness(&mut medium, &campaign.mk, 9).unwrap();
        assert_eq!(publication.shard_outcomes.len(), 2);
        for outcome in &publication.shard_outcomes {
            assert!(
                matches!(outcome, WitnessPublicationOutcome::Accepted { .. }),
                "every shard freshly accepted: {outcome:?}"
            );
        }
        assert_eq!(publication.manifest.member_count, 71); // 70 members + SK
        assert_eq!(publication.manifest.shard_count, 2);
        // The boundary: shard 0 carries the greedy maximum (the
        // next canonical entry would not have fit) and stays within
        // the ceiling.
        let (_record, shard0) = find_installed_witness_shard(&mut medium, &campaign.mk, 0)
            .unwrap()
            .unwrap();
        let (_record, shard1) = find_installed_witness_shard(&mut medium, &campaign.mk, 1)
            .unwrap()
            .unwrap();
        let shard0_size = WITNESS_SHARD_HEADER_BYTES
            + shard0
                .entries
                .iter()
                .map(|entry| WITNESS_ENTRY_FRAMING_BYTES + entry.preimage.len() as u64)
                .sum::<u64>();
        let next_framed = WITNESS_ENTRY_FRAMING_BYTES + shard1.entries[0].preimage.len() as u64;
        assert!(shard0_size <= WITNESS_SHARD_PAYLOAD_CEILING);
        assert!(shard0_size + next_framed > WITNESS_SHARD_PAYLOAD_CEILING);
        // The complete kernel (the census, the coverage, the
        // greedy, the digest) and the restart discipline.
        verify_installed_witness_internal(&mut medium, &campaign.mk).unwrap();
        drop(medium);
        let mut reopened = crate::LocalFileMedium::open(root.clone()).unwrap();
        verify_installed_witness_internal(&mut reopened, &campaign.mk).unwrap();
        drop(reopened);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }

    #[cfg(unix)]
    #[test]
    fn the_republish_reauthenticates_every_duplicate() {
        let (root, mut medium) = a12_local_store("republish");
        let journal = a12_journal_root("republish");
        let members = a12_small_members("republish");
        let campaign = a12_campaign(&mut medium, "republish", &journal, &members, 10_000);
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        let first = publish_ancestry_witness(&mut medium, &campaign.mk, 9).unwrap();
        let head_after_first = medium.head().unwrap().unwrap();
        let second = publish_ancestry_witness(&mut medium, &campaign.mk, 10).unwrap();
        // Every key occupied by byte-identical content: the
        // verified duplicates (installation only, no new appends).
        for outcome in &second.shard_outcomes {
            match outcome {
                WitnessPublicationOutcome::DuplicateVerified { record } => {
                    assert_ne!(*record, Hash::ZERO);
                }
                other => panic!("the re-published shard must be the verified duplicate: {other:?}"),
            }
        }
        match second.manifest_outcome {
            WitnessPublicationOutcome::DuplicateVerified { .. } => {}
            other => panic!("the re-published manifest must be the verified duplicate: {other:?}"),
        }
        // No new appends: the head never moved, and the duplicate's
        // record identities equal the first publication's.
        assert_eq!(medium.head().unwrap().unwrap(), head_after_first);
        assert_eq!(second.manifest_record, first.manifest_record);
        // The complete state still verifies.
        verify_installed_witness_internal(&mut medium, &campaign.mk).unwrap();
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }

    #[cfg(unix)]
    #[test]
    fn the_manifestless_shard_prefix_resumes_idempotently() {
        // A legitimate partial installation — the publisher's own
        // exact shard object hand-installed under the deterministic
        // key, NO manifest — resumes: the shard is the verified
        // duplicate, the manifest publishes fresh, and the complete
        // state verifies.
        let (root, mut medium) = a12_local_store("prefix-resume");
        let journal = a12_journal_root("prefix-resume");
        let members = a12_small_members("prefix-resume");
        let campaign = a12_campaign(&mut medium, "prefix-resume", &journal, &members, 10_000);
        let begin = medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        let begin_id = match begin {
            crate::reclaim::BeginPublicationOutcome::Accepted { id, .. } => id,
            other => panic!("the begin admits: {other:?}"),
        };
        // The expected shard content, derived exactly as the
        // publisher will (the deterministic construction over the
        // same members — SK included).
        let (plan_record, _plan) = crate::reclaim::find_installed_plan(&mut medium, &campaign.mk)
            .unwrap()
            .unwrap();
        let mk_record = medium.read_record(&campaign.mk).unwrap();
        let metadata_object = mk_record.content_refs[0];
        let mut entries = Vec::new();
        for id in campaign.members.iter().chain(std::iter::once(&campaign.sk)) {
            let bytes = medium.read_object(id).unwrap();
            entries.push(WitnessEntry {
                member: *id,
                preimage: bytes,
            });
        }
        let (expected_manifest, expected_shards) = construct_witness(
            &campaign.mk,
            &plan_record,
            &begin_id,
            &metadata_object,
            entries,
        )
        .unwrap();
        assert_eq!(expected_shards.len(), 1);
        // The hand-installed manifestless shard prefix (the
        // in-crate receipt — the same route the publisher uses).
        let shard_object = StoredObject::new(expected_shards[0].canonical_bytes()).unwrap();
        let shard_record = a12_install_under_key(
            &mut medium,
            reclamation_witness_shard_idempotency_key(&campaign.mk, 0),
            &shard_object,
            9,
        );
        // The publication resumes over the prefix.
        let publication = publish_ancestry_witness(&mut medium, &campaign.mk, 9).unwrap();
        match publication.shard_outcomes[0] {
            WitnessPublicationOutcome::DuplicateVerified { record } => {
                assert_eq!(record, shard_record);
            }
            other => panic!("the prefix shard must be the verified duplicate: {other:?}"),
        }
        match publication.manifest_outcome {
            WitnessPublicationOutcome::Accepted { .. } => {}
            other => panic!("the manifest must publish fresh: {other:?}"),
        }
        assert_eq!(publication.manifest, expected_manifest);
        verify_installed_witness_internal(&mut medium, &campaign.mk).unwrap();
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }

    #[test]
    fn the_stale_parent_retry_converges() {
        let (path, mut medium) = a12_sqlite_store("stale-parent");
        let journal = a12_journal_root("stale-parent");
        let members = a12_small_members("stale-parent");
        let campaign = a12_campaign(&mut medium, "stale-parent", &journal, &members, 10_000);
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        let mut wrapped = A12Wrapped::armed(medium, A12Arm::StaleOnce);
        let publication = publish_ancestry_witness(&mut wrapped, &campaign.mk, 9).unwrap();
        // The first shard publication lost the CAS to the injected
        // decoy, retried against the fresh head, and completed.
        assert!(matches!(
            publication.shard_outcomes[0],
            WitnessPublicationOutcome::Accepted { .. }
        ));
        assert_eq!(publication.manifest.shard_count, 1);
        let mut inner = wrapped.into_inner();
        verify_installed_witness_internal(&mut inner, &campaign.mk).unwrap();
        drop(inner);
        a12_sqlite_cleanup(&path);
        let _ = std::fs::remove_dir_all(&journal);
    }

    #[test]
    fn the_unknown_not_installed_retries_to_accepted() {
        let (path, mut medium) = a12_sqlite_store("unknown-absent");
        let journal = a12_journal_root("unknown-absent");
        let members = a12_small_members("unknown-absent");
        let campaign = a12_campaign(&mut medium, "unknown-absent", &journal, &members, 10_000);
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        let mut wrapped = A12Wrapped::armed(medium, A12Arm::UnknownNotInstalledOnce);
        let publication = publish_ancestry_witness(&mut wrapped, &campaign.mk, 9).unwrap();
        // The uncertain append (nothing installed) reconciled to
        // the verified absence and the retry installed.
        assert!(matches!(
            publication.shard_outcomes[0],
            WitnessPublicationOutcome::Accepted { .. }
        ));
        let mut inner = wrapped.into_inner();
        verify_installed_witness_internal(&mut inner, &campaign.mk).unwrap();
        drop(inner);
        a12_sqlite_cleanup(&path);
        let _ = std::fs::remove_dir_all(&journal);
    }

    #[test]
    fn the_unknown_reconciles_installed_without_a_tier() {
        let (path, mut medium) = a12_sqlite_store("unknown-installed");
        let journal = a12_journal_root("unknown-installed");
        let members = a12_small_members("unknown-installed");
        let campaign = a12_campaign(&mut medium, "unknown-installed", &journal, &members, 10_000);
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        let mut wrapped = A12Wrapped::armed(medium, A12Arm::UnknownInstalledOnce);
        let publication = publish_ancestry_witness(&mut wrapped, &campaign.mk, 9).unwrap();
        // The uncertain append (actually installed) reconciled to
        // the proven installation — the tier-less reconciled form
        // (no durability grade invented).
        match publication.shard_outcomes[0] {
            WitnessPublicationOutcome::ReconciledInstalled { record } => {
                assert_ne!(record, Hash::ZERO);
            }
            other => panic!("the shard must be the reconciled installation: {other:?}"),
        }
        let mut inner = wrapped.into_inner();
        verify_installed_witness_internal(&mut inner, &campaign.mk).unwrap();
        drop(inner);
        a12_sqlite_cleanup(&path);
        let _ = std::fs::remove_dir_all(&journal);
    }

    #[test]
    fn the_unverifiable_unknown_refuses_typed() {
        let (path, mut medium) = a12_sqlite_store("unverifiable");
        let journal = a12_journal_root("unverifiable");
        let members = a12_small_members("unverifiable");
        let campaign = a12_campaign(&mut medium, "unverifiable", &journal, &members, 10_000);
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        let mut wrapped = A12Wrapped::armed(medium, A12Arm::UnverifiableOnce);
        let error = publish_ancestry_witness(&mut wrapped, &campaign.mk, 9).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "ancestry-witness/unknown-unverifiable"
                }
            ),
            "the unverifiable refusal is typed: {error:?}"
        );
        // Nothing completes: no manifest, no shard.
        let mut inner = wrapped.into_inner();
        assert!(
            find_installed_witness_manifest(&mut inner, &campaign.mk)
                .unwrap()
                .is_none()
        );
        assert!(
            find_installed_witness_shard(&mut inner, &campaign.mk, 0)
                .unwrap()
                .is_none()
        );
        drop(inner);
        a12_sqlite_cleanup(&path);
        let _ = std::fs::remove_dir_all(&journal);
    }

    #[test]
    fn the_pass1_divergence_refuses_without_publication() {
        // PASS 0 read the member's genuine bytes; PASS 1's re-read
        // returns different bytes (a doctored medium): the
        // pass-divergence refusal fires BEFORE any append — no
        // shard, no manifest.
        let (path, mut medium) = a12_sqlite_store("divergence");
        let journal = a12_journal_root("divergence");
        let members = a12_small_members("divergence");
        let campaign = a12_campaign(&mut medium, "divergence", &journal, &members, 10_000);
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        // The divergence target: the highest-epoch member (the
        // first re-read of PASS 1's descending order).
        let target = campaign.members[1];
        let mut wrapped = A12Wrapped::armed(medium, A12Arm::DivergeMemberRead(target));
        let error = publish_ancestry_witness(&mut wrapped, &campaign.mk, 9).unwrap_err();
        assert!(
            matches!(
                error,
                MediumError::Chain {
                    rule: "ancestry-witness/pass-divergence"
                }
            ),
            "the divergence refusal is typed: {error:?}"
        );
        let mut inner = wrapped.into_inner();
        assert!(
            find_installed_witness_manifest(&mut inner, &campaign.mk)
                .unwrap()
                .is_none()
        );
        assert!(
            find_installed_witness_shard(&mut inner, &campaign.mk, 0)
                .unwrap()
                .is_none()
        );
        drop(inner);
        a12_sqlite_cleanup(&path);
        let _ = std::fs::remove_dir_all(&journal);
    }

    #[cfg(unix)]
    #[test]
    fn the_manifest_present_degradation_refuses_typed() {
        // An installed manifest claims completeness: a degraded
        // shard (its content object deleted beneath the installed
        // record) is the typed integrity refusal — never a false
        // duplicate, never a false complete state.
        let (root, mut medium) = a12_local_store("degraded");
        let journal = a12_journal_root("degraded");
        let members = a12_small_members("degraded");
        let campaign = a12_campaign(&mut medium, "degraded", &journal, &members, 10_000);
        medium.begin_reclamation_v2(&campaign.mk, 8).unwrap();
        publish_ancestry_witness(&mut medium, &campaign.mk, 9).unwrap();
        // The degradation: the shard's CONTENT object deleted (the
        // shard record remains installed under the deterministic
        // key).
        let (_record, shard) = find_installed_witness_shard(&mut medium, &campaign.mk, 0)
            .unwrap()
            .unwrap();
        let shard_object_hash = hash_object(&shard.canonical_bytes());
        std::fs::remove_file(medium.object_path_pub(&shard_object_hash)).unwrap();
        // The kernel refuses the degraded state.
        let kernel_error =
            verify_installed_witness_internal(&mut medium, &campaign.mk).unwrap_err();
        assert!(
            matches!(kernel_error, MediumError::MissingObject { .. }),
            "the degraded shard is the integrity refusal: {kernel_error:?}"
        );
        // The re-publish refuses: the occupied key's record is
        // present but its content is gone — the duplicate
        // discovery fails closed, never a false duplicate.
        let publish_error = publish_ancestry_witness(&mut medium, &campaign.mk, 9).unwrap_err();
        assert!(
            matches!(publish_error, MediumError::MissingObject { .. }),
            "the degraded re-publication fails closed: {publish_error:?}"
        );
        // The manifestless-prefix resumability is NOT granted to
        // the degraded form: the manifest remains installed
        // (degraded), and no repair occurred.
        assert!(
            find_installed_witness_manifest(&mut medium, &campaign.mk)
                .unwrap()
                .is_some()
        );
        drop(medium);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&journal);
    }
}
