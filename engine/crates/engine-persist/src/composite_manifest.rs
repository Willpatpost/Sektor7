//! The S6-C4 Phase 2 K2-A DARK COMPOSITE MANIFEST — the canonical,
//! strictly bounded wire contract for `engine-slice/session-snapshot/2`
//! and its immutable chunk dependency closure. **Codec and contract
//! work only**: no `/2` snapshot may become an authoritative revision
//! through K2-A, no composite publication path exists, no provider
//! behavior changes — the schema lives here (the persistence layer,
//! which `engine-slice` consumes — one canonical definition, never
//! two) and is exercised exclusively by tests.
//!
//! **The schema.** One root manifest object names the complete
//! dependency closure of one composite session snapshot: the fixed
//! section grammar (the scheduler header, the near and far lanes, the
//! blocked and held obligations, the compacted-history metadata for
//! the `/2` spine form, the ledger count prefix, the bounded
//! ledger-chunk run, the world, the director), each entry carrying
//! its section tag, content identity, payload length, and section
//! kind. The grammar is POSITIONAL — a manifest omitting, reordering,
//! duplicating, or inventing a required section is structurally
//! invalid, never a valid-looking state. All count and length
//! arithmetic is checked (the K1 corrective's discipline: constant
//! division bounds, checked host conversions, error values naming
//! COUNTS, never derived products); every count is bounded before any
//! allocation trusts it.
//!
//! **The closure.** The root is distinct from its chunks: the
//! [`validate_composite_closure`] contract takes the revision record,
//! the supplied root object, the newly supplied chunk objects, and
//! the REUSED chunk identities (existing stored chunks — carried as
//! explicitly UNVERIFIED dependencies: K2-A validates supplied bytes
//! but never claims an existing chunk is durable merely because its
//! hash appears in a manifest). The validator proves the revision has
//! exactly one root content reference, the root strictly decodes, the
//! declared identities exactly match the supplied ∪ reused set, every
//! new chunk's envelope, tag, length, and calculated identity match
//! its manifest entry, and no duplicate, missing, or extraneous
//! identity exists.
//!
//! **The chunk envelope** (ratified from the K1 dark codec): every
//! chunk object is `{domain} + {section tag} + {payload}` — the
//! payload an unmodified slice of the exact legacy
//! `session-snapshot/1` serialization; the identity is the SHA-256
//! of exactly those bytes. The composed concatenation of the
//! payloads, in the grammar's fixed order under the legacy framing,
//! reproduces the legacy snapshot byte-for-byte — the existing strict
//! `restore_session()` global gates remain the sole authority over
//! the composed state (chunk-local validation never substitutes for
//! global restoration).

use engine_core::wire::MAX_TAG_LEN;
use engine_core::{Decoder, Encoder, SaveError};

use crate::hash::Hash;
use crate::{RevisionRecord, StoredObject};

/// The composite snapshot root's domain. A domain in this family that
/// is not exactly `/2` is RECOGNIZED but unsupported — readers never
/// guess across versions.
pub const SESSION_SNAPSHOT_V2_DOMAIN: &str = "engine-slice/session-snapshot/2";

/// The session-snapshot family prefix (K2-C1: the production domain
/// gate's recognition discipline — every versioned snapshot domain
/// begins with it; the GENERIC append admits only the supported
/// legacy `/1` form and refuses every other family member, so an
/// unsupported future composite version can never bypass the
/// dedicated composite admission).
pub const SESSION_SNAPSHOT_FAMILY: &str = "engine-slice/session-snapshot/";

/// The supported LEGACY whole-object snapshot domain — the existing
/// production `/1` publication path (behaviorally unchanged by
/// K2-C1; the generic append continues to admit it).
pub const SESSION_SNAPSHOT_V1_DOMAIN: &str = "engine-slice/session-snapshot/1";

/// The ratified manifest schema version.
pub const MANIFEST_VERSION: u32 = 2;

/// The chunk object envelope's domain (ratified from the K1 codec).
pub const SESSION_CHUNK_DOMAIN: &str = "engine-slice/session-chunk/1";

/// The section tags (the grammar's positional keys — ONE canonical
/// definition; the engine-slice K1 codec consumes these).
pub const TAG_HEADER: &str = "engine-slice/session-chunk/header/1";
pub const TAG_NEAR: &str = "engine-slice/session-chunk/near/1";
pub const TAG_FAR: &str = "engine-slice/session-chunk/far/1";
pub const TAG_BLOCKED: &str = "engine-slice/session-chunk/blocked/1";
pub const TAG_HELD: &str = "engine-slice/session-chunk/held/1";
pub const TAG_COMPACTED_META: &str = "engine-slice/session-chunk/compacted-meta/1";
pub const TAG_LEDGER_META: &str = "engine-slice/session-chunk/ledger-meta/1";
pub const TAG_LEDGER_RECORDS: &str = "engine-slice/session-chunk/ledger-records/1";
pub const TAG_WORLD: &str = "engine-slice/session-chunk/world/1";
pub const TAG_DIRECTOR: &str = "engine-slice/session-chunk/director/1";

/// The maximum chunk entries one manifest may declare (the manifest
/// stays small and decodable in bounded time).
pub const MAX_MANIFEST_CHUNKS: u64 = 4096;
/// The maximum payload length one chunk may carry.
pub const MAX_CHUNK_BYTES: u64 = 8 * 1024 * 1024;
/// The maximum ledger chunk boundary (records per sealed chunk): the
/// sealed-chunk byte width must fit [`MAX_CHUNK_BYTES`].
pub const MAX_LEDGER_BOUNDARY: u64 = MAX_CHUNK_BYTES / 31;
/// The fixed ledger record wire width (the K1-audited layout).
pub const LEDGER_RECORD_BYTES: u64 = 31;
/// The maximum reconstructed session byte length the manifest may
/// declare (an honest bound, checked against the payload sum).
pub const MAX_COMPOSED_LENGTH: u64 = 1024 * 1024 * 1024;

const SECTION: &str = "composite-manifest";

/// The history form the manifest's sections carry (the spine's
/// full-versus-compacted distinction, enforced structurally).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryKind {
    Full,
    Compacted,
}

/// The section kind: the grammar's singleton sections versus the
/// ledger-chunk run's sealed and partial-tail entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    /// A grammar singleton (exactly one entry with this tag).
    Singleton,
    /// A sealed ledger chunk: exactly the boundary's records.
    LedgerSealed,
    /// The ledger run's final partial chunk: fewer than the
    /// boundary's records (possibly exactly zero — the empty ledger's
    /// uniform tail).
    LedgerTail,
}

/// One manifest entry: a chunk's section tag, content identity,
/// payload length, and kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    pub tag: &'static str,
    pub identity: Hash,
    pub payload_length: u64,
    pub kind: SectionKind,
}

/// The strictly decoded composite manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositeManifest {
    pub history_kind: HistoryKind,
    /// The ledger chunk policy the sections were split under.
    pub ledger_boundary: u64,
    /// The total reconstructed legacy payload byte length (the sum
    /// of the payload lengths, checked).
    pub composed_length: u64,
    /// The entries in the grammar's fixed order.
    pub entries: Vec<ManifestEntry>,
}

impl CompositeManifest {
    /// The ledger-records run (a borrowed view over the entries).
    pub fn ledger_entries(&self) -> impl Iterator<Item = &ManifestEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.tag == TAG_LEDGER_RECORDS)
    }

    /// The identities the manifest declares (the dependency closure).
    pub fn closure_identities(&self) -> Vec<Hash> {
        self.entries.iter().map(|entry| entry.identity).collect()
    }
}

fn wire_count_to_index(value: u64, kind: &'static str) -> Result<usize, SaveError> {
    usize::try_from(value).map_err(|_| SaveError::Limit {
        section: SECTION,
        kind,
        value,
        max: usize::MAX as u64,
    })
}

fn decode_section_kind(discriminant: u8) -> Result<SectionKind, SaveError> {
    match discriminant {
        0 => Ok(SectionKind::Singleton),
        1 => Ok(SectionKind::LedgerSealed),
        2 => Ok(SectionKind::LedgerTail),
        other => Err(SaveError::Enum {
            section: SECTION,
            discriminant: u64::from(other),
        }),
    }
}

fn encode_section_kind(kind: SectionKind) -> u8 {
    match kind {
        SectionKind::Singleton => 0,
        SectionKind::LedgerSealed => 1,
        SectionKind::LedgerTail => 2,
    }
}

/// The grammar's positional expectation for the NEXT entry, given the
/// history form and how far the run has progressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Header,
    Near,
    Far,
    Blocked,
    Held,
    CompactedMeta,
    LedgerMeta,
    LedgerRecords,
    World,
    Director,
    Done,
}

fn expected_tag(slot: Slot) -> Option<&'static str> {
    match slot {
        Slot::Header => Some(TAG_HEADER),
        Slot::Near => Some(TAG_NEAR),
        Slot::Far => Some(TAG_FAR),
        Slot::Blocked => Some(TAG_BLOCKED),
        Slot::Held => Some(TAG_HELD),
        Slot::CompactedMeta => Some(TAG_COMPACTED_META),
        Slot::LedgerMeta => Some(TAG_LEDGER_META),
        Slot::LedgerRecords => Some(TAG_LEDGER_RECORDS),
        Slot::World => Some(TAG_WORLD),
        Slot::Director => Some(TAG_DIRECTOR),
        Slot::Done => None,
    }
}

/// Encodes a manifest canonically (the inverse of [`decode`]; the
/// test corpus and the engine-slice producer use this — no production
/// writer exists).
pub fn encode(manifest: &CompositeManifest) -> Vec<u8> {
    let mut enc = Encoder::new();
    enc.write_str(SESSION_SNAPSHOT_V2_DOMAIN);
    enc.write_u32(MANIFEST_VERSION);
    enc.write_u8(match manifest.history_kind {
        HistoryKind::Full => 0,
        HistoryKind::Compacted => 1,
    });
    enc.write_u64(manifest.ledger_boundary);
    enc.write_u64(manifest.composed_length);
    enc.write_count(manifest.entries.len());
    for entry in &manifest.entries {
        enc.write_str(entry.tag);
        enc.write_bytes(entry.identity.as_bytes());
        enc.write_u64(entry.payload_length);
        enc.write_u8(encode_section_kind(entry.kind));
    }
    enc.into_bytes()
}

/// Strictly decodes and validates one root manifest. Every failure is
/// typed; no panic, wrap, unbounded allocation, or silently ignored
/// field. The grammar is positional; the arithmetic is checked; the
/// input is consumed completely.
pub fn decode(bytes: &[u8]) -> Result<CompositeManifest, SaveError> {
    let mut dec = Decoder::new(bytes);
    // The domain gate with family recognition: an exact match passes;
    // a recognizable family member fails typed (never guessed); a
    // foreign domain fails the exact-match error.
    let observed = dec.read_str(MAX_TAG_LEN, SECTION)?;
    if observed != SESSION_SNAPSHOT_V2_DOMAIN {
        return Err(SaveError::Domain {
            expected: SESSION_SNAPSHOT_V2_DOMAIN,
            observed: if observed.starts_with(SESSION_SNAPSHOT_FAMILY) {
                format!("{observed} (recognized family member, unsupported)")
            } else {
                observed.to_owned()
            },
        });
    }
    let version = dec.read_u32()?;
    if version != MANIFEST_VERSION {
        return Err(SaveError::Version {
            observed: version,
            supported: MANIFEST_VERSION,
        });
    }
    let history_kind = match dec.read_u8()? {
        0 => HistoryKind::Full,
        1 => HistoryKind::Compacted,
        other => {
            return Err(SaveError::Enum {
                section: SECTION,
                discriminant: u64::from(other),
            });
        }
    };
    let ledger_boundary = dec.read_u64()?;
    if ledger_boundary == 0 || ledger_boundary > MAX_LEDGER_BOUNDARY {
        return Err(SaveError::Limit {
            section: SECTION,
            kind: "ledger-boundary",
            value: ledger_boundary,
            max: MAX_LEDGER_BOUNDARY,
        });
    }
    let declared_composed = dec.read_u64()?;
    if declared_composed > MAX_COMPOSED_LENGTH {
        return Err(SaveError::Limit {
            section: SECTION,
            kind: "composed-length",
            value: declared_composed,
            max: MAX_COMPOSED_LENGTH,
        });
    }
    let entry_count = dec.read_count(MAX_MANIFEST_CHUNKS, SECTION, "chunk-count")?;
    let count = wire_count_to_index(entry_count, "chunk-count")?;
    // The allocation is bounded by the declared count against the
    // bytes actually remaining (read_count's remaining discipline) —
    // no untrusted count allocates before the input proves it.
    let mut entries = Vec::with_capacity(count);
    let mut seen: Vec<Hash> = Vec::with_capacity(count);
    let mut slot = Slot::Header;
    let mut composed_sum: u64 = 0;
    for _ in 0..count {
        let tag = dec.read_str(MAX_TAG_LEN, SECTION)?;
        let identity_bytes = dec.read_bytes(32, SECTION)?;
        let Some(identity) = Hash::from_bytes(identity_bytes) else {
            return Err(SaveError::Truncated);
        };
        let payload_length = dec.read_u64()?;
        if payload_length > MAX_CHUNK_BYTES {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "chunk-length",
                value: payload_length,
                max: MAX_CHUNK_BYTES,
            });
        }
        let kind = decode_section_kind(dec.read_u8()?)?;
        // The ledger run's terminator: after the ledger entries, the
        // grammar accepts the World section — the run ends where the
        // next tag stops being ledger-records.
        if slot == Slot::LedgerRecords && tag == TAG_WORLD {
            slot = Slot::World;
        }
        // The positional grammar: every entry must fill the expected
        // slot's tag with the expected kind, in order.
        let entry_tag = expected_tag(slot)
            .ok_or(SaveError::Integrity("composite-manifest/grammar-overflow"))?;
        if tag != entry_tag {
            return Err(SaveError::Integrity("composite-manifest/grammar-order"));
        }
        let expected_kind = match slot {
            Slot::Header
            | Slot::Near
            | Slot::Far
            | Slot::Blocked
            | Slot::Held
            | Slot::CompactedMeta
            | Slot::LedgerMeta
            | Slot::World
            | Slot::Director => SectionKind::Singleton,
            Slot::LedgerRecords => kind, // validated by the run rules below
            Slot::Done => unreachable!("the grammar-overflow check above"),
        };
        if slot != Slot::LedgerRecords && kind != expected_kind {
            return Err(SaveError::Integrity("composite-manifest/section-kind"));
        }
        // Duplicate identity rejection (the closure is a set).
        if seen.contains(&identity) {
            return Err(SaveError::Duplicate {
                section: SECTION,
                seq: 0,
            });
        }
        seen.push(identity);
        composed_sum = composed_sum
            .checked_add(payload_length)
            .ok_or(SaveError::Overflow { section: SECTION })?;
        if composed_sum > MAX_COMPOSED_LENGTH {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "composed-length",
                value: composed_sum,
                max: MAX_COMPOSED_LENGTH,
            });
        }
        let entry = ManifestEntry {
            tag: entry_tag,
            identity,
            payload_length,
            kind,
        };
        entries.push(entry);
        // Advance the grammar.
        slot = match slot {
            Slot::Header => Slot::Near,
            Slot::Near => Slot::Far,
            Slot::Far => Slot::Blocked,
            Slot::Blocked => Slot::Held,
            Slot::Held => match history_kind {
                HistoryKind::Full => Slot::LedgerMeta,
                HistoryKind::Compacted => Slot::CompactedMeta,
            },
            Slot::CompactedMeta => Slot::LedgerMeta,
            Slot::LedgerMeta => Slot::LedgerRecords,
            Slot::LedgerRecords => Slot::LedgerRecords,
            Slot::World => Slot::Director,
            Slot::Director => Slot::Done,
            Slot::Done => Slot::Done,
        };
    }
    // The run must terminate into World exactly once the ledger
    // entries end: the final slot must have advanced past Director's
    // predecessors. The grammar advance above left us in
    // LedgerRecords or Done; a manifest that never left the run (no
    // World/Director) is incomplete.
    if slot != Slot::Done {
        return Err(SaveError::Integrity(
            "composite-manifest/grammar-incomplete",
        ));
    }
    // The ledger run rules: every entry below the last is sealed with
    // EXACTLY the boundary's byte width; the last is either exactly
    // sealed (a full final chunk) or a partial tail of whole records
    // below the boundary (possibly zero — the empty ledger's uniform
    // tail). At least one ledger entry must exist.
    let ledger: Vec<&ManifestEntry> = entries
        .iter()
        .filter(|entry| entry.tag == TAG_LEDGER_RECORDS)
        .collect();
    if ledger.is_empty() {
        return Err(SaveError::Integrity("composite-manifest/ledger-run-empty"));
    }
    let sealed_width = ledger_boundary
        .checked_mul(LEDGER_RECORD_BYTES)
        .ok_or(SaveError::Overflow { section: SECTION })?;
    let run_len = ledger.len();
    for (index, entry) in ledger.iter().enumerate() {
        let is_last = index + 1 == run_len;
        match entry.kind {
            SectionKind::LedgerSealed => {
                if entry.payload_length != sealed_width {
                    return Err(SaveError::Integrity(
                        "composite-manifest/sealed-chunk-width",
                    ));
                }
            }
            SectionKind::LedgerTail => {
                if !is_last {
                    return Err(SaveError::Integrity("composite-manifest/tail-not-final"));
                }
                if entry.payload_length >= sealed_width
                    || entry.payload_length % LEDGER_RECORD_BYTES != 0
                {
                    return Err(SaveError::Integrity("composite-manifest/tail-width"));
                }
            }
            SectionKind::Singleton => {
                return Err(SaveError::Integrity(
                    "composite-manifest/ledger-section-kind",
                ));
            }
        }
    }
    // The singleton cardinalities: exactly one of each (the grammar
    // advance guarantees presence; the count check guarantees
    // uniqueness — a second singleton would break the positional
    // order, but assert the cardinality explicitly for clarity).
    for tag in [
        TAG_HEADER,
        TAG_NEAR,
        TAG_FAR,
        TAG_BLOCKED,
        TAG_HELD,
        TAG_LEDGER_META,
        TAG_WORLD,
        TAG_DIRECTOR,
    ] {
        let cardinality = entries.iter().filter(|e| e.tag == tag).count();
        if cardinality != 1 {
            return Err(SaveError::Integrity(
                "composite-manifest/singleton-cardinality",
            ));
        }
    }
    let compacted_cardinality = entries
        .iter()
        .filter(|e| e.tag == TAG_COMPACTED_META)
        .count();
    match history_kind {
        HistoryKind::Full => {
            if compacted_cardinality != 0 {
                return Err(SaveError::Integrity(
                    "composite-manifest/full-form-compact-meta",
                ));
            }
        }
        HistoryKind::Compacted => {
            if compacted_cardinality != 1 {
                return Err(SaveError::Integrity(
                    "composite-manifest/compacted-form-meta",
                ));
            }
        }
    }
    // The composed-length coherence: the declared total equals the
    // checked sum of the payload lengths.
    if declared_composed != composed_sum {
        return Err(SaveError::Integrity("composite-manifest/composed-length"));
    }
    dec.finish()?;
    Ok(CompositeManifest {
        history_kind,
        ledger_boundary,
        composed_length: declared_composed,
        entries,
    })
}

// ------------------------------------------------------ the closure contract

/// One composite publication's supplied closure: the root object, the
/// newly supplied chunk objects, and the REUSED chunk identities
/// (existing stored chunks — **unverified dependencies** until a
/// future storage operation verifies their bytes; K2-A never claims
/// durability for an existing chunk merely because its hash appears).
#[derive(Debug, Clone)]
pub struct CompositeClosure {
    pub root: StoredObject,
    pub new_chunks: Vec<StoredObject>,
    pub reused_chunks: Vec<Hash>,
}

/// The validated closure: the strictly decoded manifest, the root's
/// content identity, the new chunks' verified identities, and the
/// reused identities carried explicitly as UNVERIFIED dependencies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedClosure {
    pub manifest: CompositeManifest,
    pub root_identity: Hash,
    pub new_identities: Vec<Hash>,
    /// Existing stored chunks the manifest requires — NOT verified
    /// here; a future storage operation must read and hash-check
    /// their bytes before the publication may be accepted.
    pub unverified_dependencies: Vec<Hash>,
}

/// The closure validation's typed failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosureError {
    /// The revision does not carry exactly one root content
    /// reference, or that reference is not the supplied root's
    /// content identity.
    RootReference,
    /// The supplied root object does not strictly decode as a
    /// supported composite manifest.
    Manifest(SaveError),
    /// The manifest's declared identities do not exactly match the
    /// supplied ∪ reused set — a missing, extraneous, or repeated
    /// identity (the union itself is a set: a repeated supplied
    /// identity is a set mismatch by construction).
    ClosureSet,
    /// A newly supplied chunk's envelope does not decode, or its
    /// tag, payload length, or calculated identity does not match its
    /// manifest entry.
    NewChunk { identity: Hash },
}

/// The pure closure validator (no storage, no I/O — the contract a
/// future composite publication will enforce inside its atomic
/// window):
///
/// 1. The revision has exactly one root content reference, and it IS
///    the supplied root's content identity.
/// 2. The root strictly decodes as a supported composite snapshot.
/// 3. The root's declared chunk identities exactly match the union of
///    the newly supplied and reused identities — no duplicate,
///    missing, or extraneous identity.
/// 4. Every new chunk's envelope, section tag, payload length, and
///    calculated identity match its manifest entry.
/// 5. The reused identities are carried as unverified dependencies —
///    never assumed durable.
pub fn validate_composite_closure(
    record: &RevisionRecord,
    closure: &CompositeClosure,
) -> Result<ValidatedClosure, ClosureError> {
    // (1) Exactly one root content reference, and it is the supplied
    // root's content identity.
    if record.content_refs.len() != 1 || record.content_refs[0] != closure.root.hash() {
        return Err(ClosureError::RootReference);
    }
    // (2) The strict manifest decode.
    let manifest = decode(closure.root.bytes()).map_err(ClosureError::Manifest)?;
    // (3) The exact-set rule: declared == new ∪ reused.
    let mut supplied: Vec<Hash> = closure
        .new_chunks
        .iter()
        .map(|chunk| chunk.hash())
        .collect();
    supplied.extend(closure.reused_chunks.iter().copied());
    let declared = manifest.closure_identities();
    let mut supplied_sorted = supplied.clone();
    supplied_sorted.sort();
    let mut declared_sorted = declared.clone();
    declared_sorted.sort();
    // A repeated supplied identity collapses in the sort — detect it
    // first: the manifest itself rejects duplicates, so a repeated
    // supplied identity is a closure-set mismatch.
    supplied_sorted.dedup();
    if supplied_sorted.len() != supplied.len() {
        return Err(ClosureError::ClosureSet);
    }
    if supplied_sorted != declared_sorted {
        return Err(ClosureError::ClosureSet);
    }
    // (4) Every new chunk: the envelope decodes as the chunk domain +
    // tag + payload; the tag and payload length match the manifest
    // entry; the object's content identity IS the manifest identity.
    for chunk in &closure.new_chunks {
        let identity = chunk.hash();
        let Some(entry) = manifest.entries.iter().find(|e| e.identity == identity) else {
            return Err(ClosureError::NewChunk { identity });
        };
        let bytes = chunk.bytes();
        let mut dec = Decoder::new(bytes);
        let observed_domain = dec
            .read_str(MAX_TAG_LEN, "chunk-envelope")
            .map_err(|_| ClosureError::NewChunk { identity })?;
        if observed_domain != SESSION_CHUNK_DOMAIN {
            return Err(ClosureError::NewChunk { identity });
        }
        let observed_tag = dec
            .read_str(MAX_TAG_LEN, "chunk-envelope")
            .map_err(|_| ClosureError::NewChunk { identity })?;
        if observed_tag != entry.tag {
            return Err(ClosureError::NewChunk { identity });
        }
        let payload = dec
            .read_bytes(MAX_CHUNK_BYTES, "chunk-envelope")
            .map_err(|_| ClosureError::NewChunk { identity })?;
        if payload.len() as u64 != entry.payload_length {
            return Err(ClosureError::NewChunk { identity });
        }
        dec.finish()
            .map_err(|_| ClosureError::NewChunk { identity })?;
    }
    Ok(ValidatedClosure {
        manifest,
        root_identity: closure.root.hash(),
        new_identities: closure.new_chunks.iter().map(|c| c.hash()).collect(),
        unverified_dependencies: closure.reused_chunks.clone(),
    })
}

/// Encodes one chunk object envelope canonically (the ratified K1
/// form — the engine-slice producer and the tests use this; no
/// production writer exists).
pub fn encode_chunk_envelope(tag: &'static str, payload: &[u8]) -> StoredObject {
    let mut enc = Encoder::new();
    enc.write_str(SESSION_CHUNK_DOMAIN);
    enc.write_str(tag);
    enc.write_bytes(payload);
    StoredObject::new(enc.into_bytes()).expect("the chunk envelope is a valid content object")
}
