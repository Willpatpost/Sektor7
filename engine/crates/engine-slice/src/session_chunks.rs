//! The S6-C4 Phase 2 K1 DARK STRUCTURAL SNAPSHOT CODEC — an
//! internal, deterministic, section-split representation of the
//! authoritative session, **never published to canonical storage**
//! (the authorization is the differential-verification foundation
//! only; every production publication path — the covering append,
//! the snapshot publication, the recovery selection, the sweep, the
//! reclamation — is untouched and remains the accepted C1 behavior).
//!
//! **The representation.** The accepted legacy session snapshot
//! (`engine-slice/session-snapshot/1`) is one content object: the
//! domain tag, the spine's complete save bytes (`sektor7/save/1` or
//! `/2`), and the director's complete save bytes. The chunk codec
//! walks those exact legacy bytes and splits them at the WIRE'S OWN
//! section boundaries into independently identified
//! [`SectionChunk`]s — the scheduler header and counters, the near
//! and far lanes, the blocked and held obligations, the
//! compacted-history metadata (when the `/2` form), the execution
//! ledger's count prefix and its records in bounded immutable
//! chunks, the world section, and the director section. Each chunk
//! carries its own content identity (the SHA-256 of its
//! domain-tagged envelope — the prospective canonical CAS object);
//! a root manifest lists the chunk identities (the prospective
//! `session-snapshot/2` publication root).
//!
//! **The ledger chunk policy.** The execution ledger is append-only
//! in the full-history form: its record stream is split at a fixed
//! record boundary ([`DEFAULT_LEDGER_CHUNK_RECORDS`], the ratified
//! default; the policy is a parameter for the boundary study). Every
//! chunk below the last is SEALED (its bytes immutable for the life
//! of the history); the final chunk is the PARTIAL TAIL (re-emitted
//! per save; it becomes sealed when the next boundary fills). Sealed
//! prefix identities are therefore stable across saves — the
//! content-addressed reuse the staged architecture builds on. An
//! explicit history compaction (the `/2` transition) rewrites the
//! record stream (the summarized prefix + the retained tail): the
//! chunk identities are NOT carried across the transition — the
//! compaction is the documented invalidation point, and the walker
//! re-derives the new form's chunks from the new bytes.
//!
//! **The equivalence contract (the differential discipline).**
//! [`compose_session`] reassembles the EXACT legacy
//! `session-snapshot/1` bytes from the chunks — the concatenation,
//! in the manifest's fixed order, of the unmodified legacy slices —
//! so the existing strict restoration contract (`restore_session`
//! and every §5.2 gate) validates the composed state UNCHANGED.
//! The K1 verification layering: the WALK's local checks (the
//! domain tags, the bounded counts, the wire advance) and the
//! COMPOSE's global validation (the existing decoder's full gate
//! set) — no optimistic cache substitutes for global validity.
//!
//! **What K1 does NOT do:** no canonical appends, no
//! `CanonicalMedium` changes, no covering-publication changes, no
//! recovery-selection changes, no sweep/reclamation changes, no
//! journal/allocation changes, no transition-state-machine changes,
//! no delta snapshots, no background threads. No production path
//! depends on this module; it exists for the differential corpus,
//! the chunk-boundary study, and the microbenchmarks.

use engine_core::wire::{SAVE_DOMAIN, SAVE_DOMAIN_V2, SPINE_DOMAIN, SPINE_HISTORY_DOMAIN};
use engine_core::{Decoder, Encoder, SaveError, Spine, Tick};
use engine_persist::composite_manifest::{
    self, CompositeManifest, HistoryKind, MAX_CHUNK_BYTES, MAX_COMPOSED_LENGTH,
    MAX_LEDGER_BOUNDARY, MAX_MANIFEST_CHUNKS, ManifestEntry, SectionKind, TAG_BLOCKED,
    TAG_COMPACTED_META, TAG_DIRECTOR, TAG_FAR, TAG_HEADER, TAG_HELD, TAG_LEDGER_META,
    TAG_LEDGER_RECORDS, TAG_NEAR, TAG_WORLD,
};
use engine_persist::{Hash, StoredObject};

use crate::crossing::SESSION_SNAPSHOT_DOMAIN;
use crate::npc::NpcWorld;
use crate::region::RegionDirector;

/// The prospective `session-snapshot/2` root's domain — the
/// ENGINE-PERSIST-OWNED canonical schema (K2-A: one definition; this
/// module consumes it). Never written to canonical storage.
pub use engine_persist::composite_manifest::SESSION_SNAPSHOT_V2_DOMAIN;

/// The chunk object envelope's domain: every prospective canonical
/// chunk object is `{domain} + {section tag} + {payload}` — the
/// content identity is the SHA-256 of exactly those bytes. The
/// envelope codec and the section tags are owned by the engine-persist
/// manifest schema; this module re-exports them for its producers.
pub use engine_persist::composite_manifest::SESSION_CHUNK_DOMAIN;

/// The ratified default ledger chunk boundary: the record count per
/// sealed chunk. The boundary study compares candidate values; the
/// default matches the measured workload's scale (a 9,000-tick
/// scenario carries ~288k records ≈ 71 chunks).
pub const DEFAULT_LEDGER_CHUNK_RECORDS: u64 = 4096;

/// The ledger record's fixed wire width (the save codec's own
/// layout: seq u64 + class u32 + field u8 + intended-due tick +
/// executed tick + schedule-kind u8 + late u8).
const LEDGER_RECORD_BYTES: usize = 8 + 4 + 1 + 8 + 8 + 1 + 1;

// -------------------------------------------------------- the chunk model

/// One independently identified session section — an unmodified
/// slice of the EXACT legacy serialization, wrapped in the
/// prospective canonical chunk envelope for content addressing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionChunk {
    /// The section's structural tag (the manifest's ordering key).
    pub tag: &'static str,
    /// The exact legacy bytes of the section's span.
    pub payload: Vec<u8>,
    /// The content identity: the SHA-256 of the chunk envelope
    /// (`SESSION_CHUNK_DOMAIN` + tag + payload) — the prospective
    /// CAS object identity.
    pub identity: Hash,
}

/// The ledger chunk policy: the sealed-boundary record count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkPolicy {
    pub ledger_records: u64,
}

impl Default for ChunkPolicy {
    fn default() -> Self {
        ChunkPolicy {
            ledger_records: DEFAULT_LEDGER_CHUNK_RECORDS,
        }
    }
}

/// The history form the walked spine save carried (the manifest
/// field; the `/2` transition is the chunk-identity invalidation
/// point).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryForm {
    Full,
    Compacted,
}

/// The dark structural snapshot: the ordered section chunks of one
/// complete legacy session serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkedSession {
    /// The history form the walked bytes carried.
    pub form: HistoryForm,
    /// The framing + scheduler header and counters (the save
    /// domain, version, spine section domain, clock, next_seq,
    /// budget, near-reserve, and the seven counters).
    pub header: SectionChunk,
    /// The near lane (count + event entries).
    pub near: SectionChunk,
    /// The far lane (count + event entries).
    pub far: SectionChunk,
    /// The blocked obligations (count + event entries).
    pub blocked: SectionChunk,
    /// The held obligations (count + event entries).
    pub held: SectionChunk,
    /// The compacted-history metadata (the `/2` form only: the
    /// carried max, the authenticated prefix summary, and the
    /// summarized count).
    pub compacted_meta: Option<SectionChunk>,
    /// The execution ledger's count prefix (the full-history count,
    /// or the `/2` retained-tail count).
    pub ledger_header: SectionChunk,
    /// The chunk policy's record boundary the walk used (records
    /// per sealed chunk) — the sealed-prefix classification's
    /// reference.
    pub ledger_boundary: u64,
    /// The ledger record stream in bounded chunks: every chunk below
    /// the last is sealed (immutable for the history's lifetime);
    /// the last is the partial tail.
    pub ledger_chunks: Vec<SectionChunk>,
    /// The world section (the world domain tag + the nested blob).
    pub world: SectionChunk,
    /// The director section (the whole director save).
    pub director: SectionChunk,
}

impl ChunkedSession {
    /// The ordered section list (the manifest's and the
    /// composition's fixed order).
    pub fn section_list(&self) -> Vec<&SectionChunk> {
        let mut sections = vec![
            &self.header,
            &self.near,
            &self.far,
            &self.blocked,
            &self.held,
        ];
        if let Some(meta) = &self.compacted_meta {
            sections.push(meta);
        }
        sections.push(&self.ledger_header);
        sections.extend(self.ledger_chunks.iter());
        sections.push(&self.world);
        sections.push(&self.director);
        sections
    }

    /// The ledger's sealed-prefix chunk identities — the
    /// content-addressed reuse surface (stable across saves while
    /// the history form and the append-only prefix hold). A final
    /// chunk holding EXACTLY the configured boundary's records is
    /// FULL and therefore sealed: its bytes are immutable under the
    /// append-only history (the next record opens a NEW chunk), so
    /// its identity is stable and it joins the sealed prefix. A
    /// partial final chunk (fewer records than the boundary) is
    /// EXCLUDED conservatively: its bytes change on the next save,
    /// and the eventual K2 identity cache must never reuse a partial
    /// chunk whose content has changed.
    pub fn sealed_prefix_identities(&self) -> Vec<Hash> {
        let sealed = match self.ledger_chunks.split_last() {
            Some((last, prefix)) => {
                let last_records = (last.payload.len() / LEDGER_RECORD_BYTES) as u64;
                if last_records == self.ledger_boundary {
                    // The full final chunk is sealed.
                    prefix.len() + 1
                } else {
                    // The partial tail is the mutable surface.
                    prefix.len()
                }
            }
            None => 0,
        };
        self.ledger_chunks[..sealed]
            .iter()
            .map(|chunk| chunk.identity)
            .collect()
    }

    /// The clock the walked header carried (the differential's
    /// structural-state check input).
    pub fn walked_clock(&self) -> Result<Tick, SaveError> {
        let mut dec = Decoder::new(&self.header.payload);
        let _ = dec.read_str(u32::MAX as u64, "domain-tag")?;
        let _ = dec.read_u32()?;
        let _ = dec.read_str(u32::MAX as u64, "domain-tag")?;
        dec.read_tick()
    }
}

// ------------------------------------------------------------- the walker

fn consumed(dec: &Decoder<'_>, total: usize) -> usize {
    total - dec.remaining()
}

/// Converts a wire count to a host index, checked (the same failure
/// class audited across the walker: a count that cannot be
/// represented as a host index fails with a typed `Limit` naming
/// the COUNT — never a derived product, so the error construction
/// itself cannot overflow).
fn wire_count_to_index(value: u64, kind: &'static str) -> Result<usize, SaveError> {
    usize::try_from(value).map_err(|_| SaveError::Limit {
        section: "k1-chunk/walk",
        kind,
        value,
        max: usize::MAX as u64,
    })
}

fn walk_event(dec: &mut Decoder<'_>) -> Result<(), SaveError> {
    dec.read_u64()?; // seq
    dec.read_u32()?; // class
    dec.read_u8()?; // field
    match dec.read_u8()? {
        0 => {
            dec.read_u64()?; // at-logical-time due
        }
        1 => {
            dec.read_u64()?; // execution-relative origin
            dec.read_u64()?; // delay
        }
        other => {
            return Err(SaveError::Enum {
                section: "k1-chunk/walk",
                discriminant: u64::from(other),
            });
        }
    }
    dec.read_u64()?; // payload
    let deps = dec.read_count(u64::MAX, "k1-chunk/walk", "dependency-count")?;
    for _ in 0..deps {
        dec.read_u64()?;
    }
    Ok(())
}

fn walk_lane(dec: &mut Decoder<'_>) -> Result<(), SaveError> {
    let count = dec.read_count(u64::MAX, "k1-chunk/walk", "lane-count")?;
    for _ in 0..count {
        walk_event(dec)?;
    }
    Ok(())
}

// The section tags are owned by the engine-persist manifest schema
// (K2-A: one canonical definition) — imported above. The ledger-meta
// tag's local historical name is aliased for the walker's use.
const TAG_LEDGER_HEADER: &str = TAG_LEDGER_META;

fn make_chunk(tag: &'static str, payload: &[u8]) -> SectionChunk {
    let envelope = composite_manifest::encode_chunk_envelope(tag, payload);
    SectionChunk {
        tag,
        payload: payload.to_vec(),
        identity: envelope.hash(),
    }
}

/// Walks one complete legacy `session-snapshot/1` byte stream and
/// splits it at the wire's own section boundaries. The local checks
/// are the walk's own (the domain tags, the bounded counts, the wire
/// advance); the global validation is the existing strict
/// restoration over the composed bytes — unchanged.
pub fn chunk_session(bytes: &[u8], policy: &ChunkPolicy) -> Result<ChunkedSession, SaveError> {
    let total = bytes.len();
    let mut dec = Decoder::new(bytes);
    dec.read_domain(SESSION_SNAPSHOT_DOMAIN)?;
    // The spine blob's framing.
    let spine_start = consumed(&dec, total);
    let spine_len = wire_count_to_index(
        dec.read_count(u64::MAX, "k1-chunk/walk", "spine-length")?,
        "spine-length",
    )?;
    let spine_payload_start = spine_start + 8;
    let spine_end = spine_payload_start
        .checked_add(spine_len)
        .ok_or(SaveError::Truncated)?;
    if spine_end > total {
        return Err(SaveError::Truncated);
    }
    let spine = &bytes[spine_payload_start..spine_end];
    // The director blob's framing.
    let mut dec = Decoder::new(&bytes[spine_end..]);
    let director_len = wire_count_to_index(
        dec.read_count(u64::MAX, "k1-chunk/walk", "director-length")?,
        "director-length",
    )?;
    let director_start = spine_end + 8;
    let director_end = director_start
        .checked_add(director_len)
        .ok_or(SaveError::Truncated)?;
    if director_end > total {
        return Err(SaveError::Truncated);
    }
    let director = &bytes[director_start..director_end];
    if director_end != total {
        return Err(SaveError::Trailing {
            leftover: (total - director_end) as u64,
        });
    }

    // ---- the spine payload walk ----
    let spine_total = spine.len();
    let mut dec = Decoder::new(spine);
    let form = {
        let observed = dec.read_str(u32::MAX as u64, "domain-tag")?;
        match observed {
            SAVE_DOMAIN => HistoryForm::Full,
            SAVE_DOMAIN_V2 => HistoryForm::Compacted,
            other => {
                return Err(SaveError::Domain {
                    expected: SAVE_DOMAIN,
                    observed: other.to_owned(),
                });
            }
        }
    };
    dec.read_u32()?; // the save-format version
    let spine_tag = match form {
        HistoryForm::Full => SPINE_DOMAIN,
        HistoryForm::Compacted => SPINE_HISTORY_DOMAIN,
    };
    dec.read_domain(spine_tag)?;
    dec.read_tick()?;
    dec.read_u64()?; // next_seq
    dec.read_u32()?; // budget
    dec.read_u32()?; // near_reserve
    for _ in 0..7 {
        dec.read_u64()?;
    }
    let header_end = consumed(&dec, spine_total);
    let header = make_chunk(TAG_HEADER, &spine[..header_end]);

    // The lanes and obligation sections.
    let mut section_start = header_end;
    walk_lane(&mut dec)?;
    let near = make_chunk(TAG_NEAR, &spine[section_start..consumed(&dec, spine_total)]);
    section_start = consumed(&dec, spine_total);
    walk_lane(&mut dec)?;
    let far = make_chunk(TAG_FAR, &spine[section_start..consumed(&dec, spine_total)]);
    section_start = consumed(&dec, spine_total);
    let count = dec.read_count(u64::MAX, "k1-chunk/walk", "blocked-count")?;
    for _ in 0..count {
        walk_event(&mut dec)?;
    }
    let blocked = make_chunk(
        TAG_BLOCKED,
        &spine[section_start..consumed(&dec, spine_total)],
    );
    section_start = consumed(&dec, spine_total);
    let count = dec.read_count(u64::MAX, "k1-chunk/walk", "held-count")?;
    for _ in 0..count {
        walk_event(&mut dec)?;
    }
    let held = make_chunk(TAG_HELD, &spine[section_start..consumed(&dec, spine_total)]);
    section_start = consumed(&dec, spine_total);

    // The compacted-history metadata (the /2 form only).
    let compacted_meta = if form == HistoryForm::Compacted {
        let has_max = dec.read_u8()?;
        if has_max != 0 {
            dec.read_tick()?;
        }
        let summary_len = wire_count_to_index(
            dec.read_count(u64::MAX, "k1-chunk/walk", "summary-length")?,
            "summary-length",
        )?;
        if dec.remaining() < summary_len {
            return Err(SaveError::Truncated);
        }
        for _ in 0..summary_len {
            dec.read_u8()?;
        }
        dec.read_u64()?; // summarized records
        let meta = make_chunk(
            TAG_COMPACTED_META,
            &spine[section_start..consumed(&dec, spine_total)],
        );
        section_start = consumed(&dec, spine_total);
        Some(meta)
    } else {
        None
    };

    // The ledger: the count prefix, then the fixed-width record
    // stream. The count arithmetic is checked at EVERY step, and the
    // failure values name the COUNT — never a derived byte product,
    // so the error construction itself cannot overflow: a count
    // whose byte length cannot be represented on the wire plane
    // (count × record-width overflowing u64) fails the constant
    // division bound; a count that cannot be represented as a host
    // index fails the checked conversion; a count that fits the
    // arithmetic but exceeds the available encoded records fails
    // the buffer bound. No malformed count panics or wraps, in
    // debug or release.
    let ledger_count_raw = dec.read_count(u64::MAX, "k1-chunk/walk", "ledger-count")?;
    const MAX_LEDGER_RECORDS: u64 = u64::MAX / LEDGER_RECORD_BYTES as u64;
    if ledger_count_raw > MAX_LEDGER_RECORDS {
        return Err(SaveError::Limit {
            section: "k1-chunk/walk",
            kind: "ledger-count",
            value: ledger_count_raw,
            max: MAX_LEDGER_RECORDS,
        });
    }
    let ledger_count = wire_count_to_index(ledger_count_raw, "ledger-count")?;
    let ledger_header = make_chunk(
        TAG_LEDGER_HEADER,
        &spine[section_start..consumed(&dec, spine_total)],
    );
    let records_start = consumed(&dec, spine_total);
    let record_bytes = ledger_count
        .checked_mul(LEDGER_RECORD_BYTES)
        .ok_or(SaveError::Limit {
            section: "k1-chunk/walk",
            kind: "ledger-bytes",
            value: ledger_count_raw,
            max: (usize::MAX / LEDGER_RECORD_BYTES) as u64,
        })?;
    let records_end = records_start
        .checked_add(record_bytes)
        .ok_or(SaveError::Truncated)?;
    if records_end > spine_total {
        return Err(SaveError::Truncated);
    }
    let records = &spine[records_start..records_end];

    // The bounded chunk split (the policy's boundary is a host
    // parameter — checked with the same discipline).
    let boundary = wire_count_to_index(policy.ledger_records.max(1), "chunk-boundary")?;
    let mut ledger_chunks = Vec::new();
    let mut offset = 0usize;
    while offset < record_bytes {
        let remaining_records = (record_bytes - offset) / LEDGER_RECORD_BYTES;
        let take = remaining_records.min(boundary) * LEDGER_RECORD_BYTES;
        let end = offset + take;
        ledger_chunks.push(make_chunk(TAG_LEDGER_RECORDS, &records[offset..end]));
        offset = end;
    }
    if ledger_chunks.is_empty() {
        // The empty ledger: one empty tail chunk keeps the manifest
        // structure uniform (the boundary study's zero case).
        ledger_chunks.push(make_chunk(TAG_LEDGER_RECORDS, &[]));
    }

    // The world section (the world domain tag + the nested blob) —
    // the stream must end exactly at the spine payload's end.
    let mut dec = Decoder::new(&spine[records_end..]);
    let _ = dec.read_str(u32::MAX as u64, "domain-tag")?;
    let world_len = wire_count_to_index(
        dec.read_count(u64::MAX, "k1-chunk/walk", "world-length")?,
        "world-length",
    )?;
    if dec.remaining() < world_len {
        return Err(SaveError::Truncated);
    }
    for _ in 0..world_len {
        dec.read_u8()?;
    }
    if dec.remaining() != 0 {
        return Err(SaveError::Trailing {
            leftover: dec.remaining() as u64,
        });
    }
    let world = make_chunk(TAG_WORLD, &spine[records_end..]);

    let director_chunk = make_chunk(TAG_DIRECTOR, director);

    Ok(ChunkedSession {
        form,
        header,
        near,
        far,
        blocked,
        held,
        compacted_meta,
        ledger_header,
        ledger_boundary: policy.ledger_records.max(1),
        ledger_chunks,
        world,
        director: director_chunk,
    })
}

// ------------------------------------------------------ compose and root

/// Reassembles the EXACT legacy `session-snapshot/1` bytes from the
/// chunks — the unmodified legacy slices concatenated in the fixed
/// section order under the original framing. The composed stream is
/// byte-for-byte the walked input (the differential's foundation);
/// the existing strict restoration validates it unchanged.
pub fn compose_session(chunks: &ChunkedSession) -> Vec<u8> {
    let mut spine = Vec::with_capacity(
        chunks.header.payload.len()
            + chunks.near.payload.len()
            + chunks.far.payload.len()
            + chunks.blocked.payload.len()
            + chunks.held.payload.len()
            + chunks
                .compacted_meta
                .as_ref()
                .map_or(0, |meta| meta.payload.len())
            + chunks.ledger_header.payload.len()
            + chunks
                .ledger_chunks
                .iter()
                .map(|chunk| chunk.payload.len())
                .sum::<usize>()
            + chunks.world.payload.len(),
    );
    spine.extend_from_slice(&chunks.header.payload);
    spine.extend_from_slice(&chunks.near.payload);
    spine.extend_from_slice(&chunks.far.payload);
    spine.extend_from_slice(&chunks.blocked.payload);
    spine.extend_from_slice(&chunks.held.payload);
    if let Some(meta) = &chunks.compacted_meta {
        spine.extend_from_slice(&meta.payload);
    }
    spine.extend_from_slice(&chunks.ledger_header.payload);
    for chunk in &chunks.ledger_chunks {
        spine.extend_from_slice(&chunk.payload);
    }
    spine.extend_from_slice(&chunks.world.payload);

    let mut enc = Encoder::new();
    enc.write_str(SESSION_SNAPSHOT_DOMAIN);
    enc.write_bytes(&spine);
    enc.write_bytes(&chunks.director.payload);
    enc.into_bytes()
}

/// Builds the RATIFIED K2-A root manifest (the prospective
/// `session-snapshot/2` publication root — NEVER written to
/// canonical storage): the engine-persist-owned
/// [`CompositeManifest`] schema with the history form, the walked
/// ledger boundary, the checked composed length (the payload sum),
/// and the entries in the grammar's fixed order with their section
/// kinds (the singletons; the ledger run's sealed and partial-tail
/// classification).
///
/// **The producer/decoder seam (the K2-A corrective):** the manifest
/// is a SCHEMA-BOUNDED object, not a mechanical transcription of the
/// walk. The strict decoder enforces its bounds over untrusted
/// bytes; this producer refuses, TYPED, any walked session it
/// cannot represent — the ledger boundary within the schema's
/// maximum, the entry count within the schema's chunk maximum, every
/// payload within the schema's per-chunk maximum, and the composed
/// total within the schema's overall maximum. An out-of-schema
/// [`ChunkPolicy`] therefore NEVER panics (in debug or release) and
/// never yields root bytes the decoder would reject: every
/// successful construction is decodable by construction. The K1
/// WALK keeps its standalone boundary-study behavior — a policy
/// outside the manifest schema still splits and composes (the
/// representation exists); only the RATIFIED ROOT refuses it.
pub fn try_build_manifest(chunks: &ChunkedSession) -> Result<CompositeManifest, SaveError> {
    const SECTION: &str = "k1-chunk/manifest";
    // (1) The ledger boundary: a policy the schema cannot carry (an
    // extreme per-chunk record count, or a hand-built session)
    // refuses naming the COUNT — never a derived byte product, so
    // the error construction itself cannot overflow.
    let boundary = chunks.ledger_boundary;
    if boundary == 0 || boundary > MAX_LEDGER_BOUNDARY {
        return Err(SaveError::Limit {
            section: SECTION,
            kind: "ledger-boundary",
            value: boundary,
            max: MAX_LEDGER_BOUNDARY,
        });
    }
    // (2) The entry count: the manifest lists every section chunk —
    // a deliberately small boundary over a large ledger exceeds the
    // schema's entry maximum and refuses naming the count.
    let sections = chunks.section_list();
    if sections.len() > MAX_MANIFEST_CHUNKS as usize {
        return Err(SaveError::Limit {
            section: SECTION,
            kind: "chunk-count",
            value: sections.len() as u64,
            max: MAX_MANIFEST_CHUNKS,
        });
    }
    // The sealed width on the schema's own u64 plane: the boundary
    // check above bounds it below MAX_CHUNK_BYTES, so the checked
    // multiply cannot fail — it is kept checked so the invariant is
    // structural, not incidental.
    let sealed_width = boundary
        .checked_mul(composite_manifest::LEDGER_RECORD_BYTES)
        .ok_or(SaveError::Limit {
            section: SECTION,
            kind: "ledger-boundary",
            value: boundary,
            max: MAX_LEDGER_BOUNDARY,
        })?;
    let mut entries = Vec::with_capacity(sections.len());
    let mut composed_length = 0u64;
    for section in &sections {
        // (3) The per-chunk payload bound.
        let payload_length = section.payload.len() as u64;
        if payload_length > MAX_CHUNK_BYTES {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "chunk-length",
                value: payload_length,
                max: MAX_CHUNK_BYTES,
            });
        }
        // (4) The composed total: `composed_length <=
        // MAX_COMPOSED_LENGTH` holds before every increment (the
        // check below maintains it), so the subtraction cannot
        // underflow, and the refusal names the true exceeded total
        // (bounded by MAX_COMPOSED_LENGTH + MAX_CHUNK_BYTES — the
        // error construction itself cannot overflow).
        if payload_length > MAX_COMPOSED_LENGTH - composed_length {
            return Err(SaveError::Limit {
                section: SECTION,
                kind: "composed-length",
                value: composed_length + payload_length,
                max: MAX_COMPOSED_LENGTH,
            });
        }
        composed_length += payload_length;
        let kind = if section.tag == TAG_LEDGER_RECORDS {
            if payload_length == sealed_width {
                SectionKind::LedgerSealed
            } else {
                SectionKind::LedgerTail
            }
        } else {
            SectionKind::Singleton
        };
        entries.push(ManifestEntry {
            tag: section.tag,
            identity: section.identity,
            payload_length,
            kind,
        });
    }
    Ok(CompositeManifest {
        history_kind: match chunks.form {
            HistoryForm::Full => HistoryKind::Full,
            HistoryForm::Compacted => HistoryKind::Compacted,
        },
        ledger_boundary: boundary,
        composed_length,
        entries,
    })
}

/// The root manifest's canonical bytes (the prospective
/// `session-snapshot/2` publication root — NEVER written to
/// canonical storage; the K2-A ratified schema, superseding the K1
/// prototype root). Fallible with the producer's typed refusals: an
/// out-of-schema chunk policy yields NO root bytes, not a panic.
pub fn try_root_manifest(chunks: &ChunkedSession) -> Result<Vec<u8>, SaveError> {
    Ok(composite_manifest::encode(&try_build_manifest(chunks)?))
}

/// The root's content identity — the prospective snapshot identity
/// (the exact-state equivalence key's future form; the C1 covering's
/// idempotency semantics carry over unchanged: the root identity IS
/// the state identity). Fallible with the producer's typed refusals.
pub fn try_root_identity(chunks: &ChunkedSession) -> Result<Hash, SaveError> {
    let root = StoredObject::new(try_root_manifest(chunks)?)
        .map_err(|_| SaveError::Integrity("k1-chunk/manifest/root-object"))?;
    Ok(root.hash())
}

/// Strictly decodes this module's own root through the ratified
/// schema (the differential's self-check: the produced manifest is
/// decodable and grammar-valid).
pub fn decode_root_manifest(bytes: &[u8]) -> Result<CompositeManifest, SaveError> {
    composite_manifest::decode(bytes)
}

/// Reconstructs the EXACT legacy `session-snapshot/1` bytes from a
/// fully verified composite read (the K2-B dark reader's output).
/// The manifest's positional grammar fixes the FINAL entry as the
/// director section; every payload before it concatenates, in the
/// grammar's fixed order, as the spine payload; the legacy framing
/// (the domain tag, the spine blob's length prefix, the director
/// blob's length prefix) wraps them — byte-exact with
/// [`compose_session`] over the same chunks (the K2-A differential
/// proved the manifest's entry order IS the composition order).
/// The legacy framing is engine-slice's (the ratified dependency
/// direction: engine-persist never knows the session format), which
/// is why this lives here and not in the storage-layer reader.
pub fn reconstruct_legacy_bytes(
    verified: &engine_persist::composite_reader::VerifiedComposite,
) -> Result<Vec<u8>, SaveError> {
    let Some((director, spine_sections)) = verified.chunks.split_last() else {
        return Err(SaveError::Integrity(
            "k2b-reconstruction/empty-verified-closure",
        ));
    };
    if director.tag != TAG_DIRECTOR {
        // Unrepresentable through the ratified decode (the grammar
        // pins the director last) — refused typed, never guessed.
        return Err(SaveError::Integrity("k2b-reconstruction/director-section"));
    }
    let mut spine = Vec::new();
    for section in spine_sections {
        spine.extend_from_slice(&section.payload);
    }
    let mut enc = Encoder::new();
    enc.write_str(SESSION_SNAPSHOT_DOMAIN);
    enc.write_bytes(&spine);
    enc.write_bytes(&director.payload);
    Ok(enc.into_bytes())
}

/// The local chunk verification: the payload re-hashes to the
/// chunk's envelope identity (the prospective CAS read check — the
/// content-addressing's own verification, distinct from the global
/// composed validation).
pub fn verify_chunk(chunk: &SectionChunk) -> bool {
    make_chunk(chunk.tag, &chunk.payload).identity == chunk.identity
}

/// The global composed validation through the existing strict
/// restoration contract — the K1 discipline's unchanged global gate
/// (no optimistic cache substitutes for it).
pub fn validate_composed(
    chunks: &ChunkedSession,
) -> Result<(Spine<NpcWorld>, RegionDirector), SaveError> {
    let bytes = compose_session(chunks);
    crate::crossing::restore_session(&bytes)
}
