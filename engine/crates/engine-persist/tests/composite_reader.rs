//! The S6-C4 Phase 2 K2-B dark dependency-closure reader's
//! storage-layer contract matrix — synthetic composite fixtures
//! over isolated mock sources (plus one read-back pass over a REAL
//! `LocalFileMedium` published through the production append, the
//! strongest isolation proof: the dark reader verifies real stored
//! objects while NO production path depends on it). Every failure
//! is typed with the failing identity; no partial success exists.

use std::collections::HashMap;

use engine_persist::composite_manifest::{
    self, CompositeManifest, HistoryKind, ManifestEntry, SectionKind, TAG_BLOCKED,
    TAG_COMPACTED_META, TAG_DIRECTOR, TAG_FAR, TAG_HEADER, TAG_HELD, TAG_LEDGER_META,
    TAG_LEDGER_RECORDS, TAG_NEAR, TAG_WORLD,
};
use engine_persist::composite_reader::{
    self, CompositeReadError, ObjectSource, ObjectSourceError, ReadLimits, VerificationMemo,
};
use engine_persist::local::LocalFileMedium;
use engine_persist::medium::{AppendOutcome, CanonicalMedium, MediumError};
use engine_persist::record::{AuthorityMode, RevisionRecord};
use engine_persist::{Hash, StoredObject};

// ------------------------------------------------------------- fixtures

/// An isolated mock source over an identity-keyed map, counting
/// every read (the memo's verification-cost witness).
struct CountingSource {
    objects: HashMap<Hash, Vec<u8>>,
    reads: usize,
}

impl CountingSource {
    fn new() -> Self {
        CountingSource {
            objects: HashMap::new(),
            reads: 0,
        }
    }

    fn insert(&mut self, identity: Hash, bytes: Vec<u8>) {
        self.objects.insert(identity, bytes);
    }

    fn remove(&mut self, identity: &Hash) {
        self.objects.remove(identity);
    }
}

impl ObjectSource for CountingSource {
    fn read_object(&mut self, identity: &Hash) -> Result<Vec<u8>, ObjectSourceError> {
        self.reads += 1;
        self.objects
            .get(identity)
            .cloned()
            .ok_or(ObjectSourceError::Missing(*identity))
    }
}

/// One synthetic composite: a grammar-valid manifest whose entries
/// name REAL synthetic chunk envelopes (arbitrary payload bytes —
/// the storage-layer reader verifies structure and identity, never
/// session semantics), plus the chunk store.
struct SyntheticComposite {
    manifest: CompositeManifest,
    root: StoredObject,
    chunks: Vec<(Hash, Vec<u8>)>,
}

impl SyntheticComposite {
    /// Builds a full or compacted composite: the eight (or nine)
    /// singletons, `sealed_count` sealed ledger chunks at exactly
    /// `boundary × 31` bytes, and one partial tail of
    /// `tail_records` whole records. The `variant` seed offsets the
    /// payload bytes — distinct variants build DISTINCT identity
    /// sets over the same grammatical shape.
    fn build(
        history: HistoryKind,
        boundary: u64,
        sealed_count: usize,
        tail_records: usize,
        variant: u8,
    ) -> Self {
        let mut seed = variant;
        let mut entries = Vec::new();
        let mut chunks: Vec<(Hash, Vec<u8>)> = Vec::new();
        let push_singleton = |entries: &mut Vec<ManifestEntry>,
                              chunks: &mut Vec<(Hash, Vec<u8>)>,
                              seed: &mut u8,
                              tag: &'static str| {
            let payload = vec![*seed; 10 + *seed as usize];
            *seed = seed.wrapping_add(1);
            let envelope = composite_manifest::encode_chunk_envelope(tag, &payload);
            entries.push(ManifestEntry {
                tag,
                identity: envelope.hash(),
                payload_length: payload.len() as u64,
                kind: SectionKind::Singleton,
            });
            chunks.push((envelope.hash(), envelope.into_bytes()));
        };
        push_singleton(&mut entries, &mut chunks, &mut seed, TAG_HEADER);
        push_singleton(&mut entries, &mut chunks, &mut seed, TAG_NEAR);
        push_singleton(&mut entries, &mut chunks, &mut seed, TAG_FAR);
        push_singleton(&mut entries, &mut chunks, &mut seed, TAG_BLOCKED);
        push_singleton(&mut entries, &mut chunks, &mut seed, TAG_HELD);
        if history == HistoryKind::Compacted {
            push_singleton(&mut entries, &mut chunks, &mut seed, TAG_COMPACTED_META);
        }
        push_singleton(&mut entries, &mut chunks, &mut seed, TAG_LEDGER_META);
        let sealed_width = boundary * 31;
        for _ in 0..sealed_count {
            let payload = vec![seed; sealed_width as usize];
            seed = seed.wrapping_add(1);
            let envelope = composite_manifest::encode_chunk_envelope(TAG_LEDGER_RECORDS, &payload);
            entries.push(ManifestEntry {
                tag: TAG_LEDGER_RECORDS,
                identity: envelope.hash(),
                payload_length: sealed_width,
                kind: SectionKind::LedgerSealed,
            });
            chunks.push((envelope.hash(), envelope.into_bytes()));
        }
        let tail_bytes = tail_records as u64 * 31;
        let payload = vec![seed; tail_bytes as usize];
        let envelope = composite_manifest::encode_chunk_envelope(TAG_LEDGER_RECORDS, &payload);
        entries.push(ManifestEntry {
            tag: TAG_LEDGER_RECORDS,
            identity: envelope.hash(),
            payload_length: tail_bytes,
            kind: SectionKind::LedgerTail,
        });
        chunks.push((envelope.hash(), envelope.into_bytes()));
        push_singleton(&mut entries, &mut chunks, &mut seed, TAG_WORLD);
        push_singleton(&mut entries, &mut chunks, &mut seed, TAG_DIRECTOR);
        let composed_length = entries.iter().map(|entry| entry.payload_length).sum();
        let manifest = CompositeManifest {
            history_kind: history,
            ledger_boundary: boundary,
            composed_length,
            entries,
        };
        let root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
        SyntheticComposite {
            manifest,
            root,
            chunks,
        }
    }

    /// The chunk identity at the manifest's entry index.
    fn identity(&self, index: usize) -> Hash {
        self.manifest.entries[index].identity
    }

    fn store(&self) -> CountingSource {
        let mut source = CountingSource::new();
        for (identity, bytes) in &self.chunks {
            source.insert(*identity, bytes.clone());
        }
        source
    }
}

fn valid_full() -> SyntheticComposite {
    SyntheticComposite::build(HistoryKind::Full, 4, 2, 2, 0)
}

fn valid_compacted() -> SyntheticComposite {
    SyntheticComposite::build(HistoryKind::Compacted, 4, 2, 2, 0)
}

// ----------------------------------------------------------------- tests

#[test]
fn the_valid_full_and_compacted_manifests_read_fully() {
    for composite in [valid_full(), valid_compacted()] {
        let mut source = composite.store();
        let verified = composite_reader::read_composite_closure(
            &mut source,
            &composite.root,
            &ReadLimits::default(),
        )
        .unwrap_or_else(|error| panic!("the valid composite reads: {error:?}"));
        assert_eq!(verified.root_identity, composite.root.hash());
        assert_eq!(verified.chunks.len(), composite.manifest.entries.len());
        assert_eq!(verified.manifest, composite.manifest);
        let identities: Vec<Hash> = verified.verified_identities();
        let expected: Vec<Hash> = composite
            .manifest
            .entries
            .iter()
            .map(|entry| entry.identity)
            .collect();
        assert_eq!(
            identities, expected,
            "the verified order is the manifest order"
        );
        // The distinctness: the entry identities are a set (the
        // decode's duplicate rejection).
        let mut sorted = identities.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), identities.len());
        // The payloads: the composed stream is the concatenation.
        let composed = verified.composed_payloads();
        let expected_composed: Vec<u8> = verified
            .chunks
            .iter()
            .flat_map(|c| c.payload.iter())
            .copied()
            .collect();
        assert_eq!(composed.len() as u64, composite.manifest.composed_length);
        assert_eq!(composed, expected_composed);
        // Every chunk's tag matches its entry's declaration.
        for (chunk, entry) in verified.chunks.iter().zip(&composite.manifest.entries) {
            assert_eq!(chunk.tag, entry.tag);
            assert_eq!(chunk.payload.len() as u64, entry.payload_length);
        }
    }
}

#[test]
fn the_missing_chunk_fails_with_its_identity() {
    let composite = valid_full();
    let mut source = composite.store();
    // The newest root's own chunk: the director (the final entry).
    let director = composite.identity(composite.manifest.entries.len() - 1);
    source.remove(&director);
    let error = composite_reader::read_composite_closure(
        &mut source,
        &composite.root,
        &ReadLimits::default(),
    )
    .unwrap_err();
    assert_eq!(
        error,
        CompositeReadError::Missing { identity: director },
        "the missing dependency reports its identity"
    );
    // A missing SEALED ledger chunk behaves identically.
    let sealed = composite.identity(6);
    let mut source = composite.store();
    source.remove(&sealed);
    assert_eq!(
        composite_reader::read_composite_closure(
            &mut source,
            &composite.root,
            &ReadLimits::default()
        ),
        Err(CompositeReadError::Missing { identity: sealed })
    );
}

#[test]
fn the_corrupt_chunk_fails_the_identity_rehash() {
    let composite = valid_full();
    let mut source = composite.store();
    let director = composite.identity(composite.manifest.entries.len() - 1);
    // Corrupt the stored envelope's FINAL byte (inside the payload
    // region — the framing, domain, tag, and declared length stay
    // intact; only the content hash moves).
    let mut corrupted = source.objects[&director].clone();
    let last = corrupted.len() - 1;
    corrupted[last] ^= 0xff;
    source.insert(director, corrupted);
    assert_eq!(
        composite_reader::read_composite_closure(
            &mut source,
            &composite.root,
            &ReadLimits::default()
        ),
        Err(CompositeReadError::HashMismatch { identity: director }),
        "the reader's own re-hash catches the corruption whatever the source checked"
    );
}

#[test]
fn the_wrong_envelope_tag_fails_before_the_hash() {
    let composite = valid_full();
    let mut source = composite.store();
    // The near entry's identity now holds a WELL-FORMED far-tagged
    // envelope with the same payload length: the tag mismatch must
    // report BEFORE the (also failing) hash so the failure names
    // the specific incoherence.
    let near_index = 1;
    let near = composite.identity(near_index);
    let payload = vec![7u8; 17];
    let mis_tagged = composite_manifest::encode_chunk_envelope(TAG_FAR, &payload);
    source.insert(near, mis_tagged.into_bytes());
    let error = composite_reader::read_composite_closure(
        &mut source,
        &composite.root,
        &ReadLimits::default(),
    )
    .unwrap_err();
    assert_eq!(
        error,
        CompositeReadError::Tag {
            identity: near,
            expected: TAG_NEAR,
            observed: TAG_FAR.to_owned(),
        },
        "the tag mismatch fires before the hash mismatch"
    );
}

#[test]
fn the_declared_length_mismatch_fails_typed() {
    let composite = valid_full();
    // A manifest declaring eight more payload bytes for the near
    // chunk (with the composed total adjusted — the decode's
    // coherence passes; only the reader's per-entry length check
    // can catch the lie).
    let mut manifest = composite.manifest.clone();
    manifest.entries[1].payload_length += 8;
    manifest.composed_length += 8;
    let lying_root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    let mut source = composite.store();
    let near = composite.identity(1);
    let error =
        composite_reader::read_composite_closure(&mut source, &lying_root, &ReadLimits::default())
            .unwrap_err();
    assert_eq!(
        error,
        CompositeReadError::Length {
            identity: near,
            declared: composite.manifest.entries[1].payload_length + 8,
            actual: composite.manifest.entries[1].payload_length,
        }
    );
}

#[test]
fn the_truncated_envelope_fails_typed() {
    let composite = valid_full();
    let mut source = composite.store();
    let world = composite.identity(composite.manifest.entries.len() - 2);
    let intact = source.objects[&world].clone();
    source.insert(world, intact[..intact.len() - 10].to_vec());
    let error = composite_reader::read_composite_closure(
        &mut source,
        &composite.root,
        &ReadLimits::default(),
    )
    .unwrap_err();
    assert!(
        matches!(
            error,
            CompositeReadError::Envelope { identity, .. } if identity == world
        ),
        "the truncated envelope fails closed with its identity: {error:?}"
    );
}

#[test]
fn the_future_version_fails_through_the_strict_decode() {
    let composite = valid_full();
    let mut versioned = composite.root.bytes().to_vec();
    // The manifest's version field: 8 (domain length) + 31 (the
    // domain), the u32's LOW byte (little endian).
    versioned[8 + 31] = 3;
    let future_root = StoredObject::new(versioned).unwrap();
    let mut source = composite.store();
    let error =
        composite_reader::read_composite_closure(&mut source, &future_root, &ReadLimits::default())
            .unwrap_err();
    assert!(matches!(
        error,
        CompositeReadError::Manifest(engine_core::SaveError::Version { observed: 3, .. })
    ));
}

#[test]
fn the_reordered_manifest_fails_the_grammar() {
    let composite = valid_full();
    let mut manifest = composite.manifest.clone();
    manifest.entries.swap(1, 2);
    let reordered_root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    let mut source = composite.store();
    let error = composite_reader::read_composite_closure(
        &mut source,
        &reordered_root,
        &ReadLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        CompositeReadError::Manifest(engine_core::SaveError::Integrity(
            "composite-manifest/grammar-order"
        ))
    ));
}

#[test]
fn the_duplicate_identity_manifest_fails_the_decode() {
    let composite = valid_full();
    // Two grammatical entries naming the SAME identity: the decode's
    // duplicate-identity rejection fires before any read.
    let mut manifest = composite.manifest.clone();
    manifest.entries[2].identity = manifest.entries[1].identity;
    manifest.entries[2].payload_length = manifest.entries[1].payload_length;
    let duplicate_root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    let mut source = composite.store();
    let error = composite_reader::read_composite_closure(
        &mut source,
        &duplicate_root,
        &ReadLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        CompositeReadError::Manifest(engine_core::SaveError::Duplicate { .. })
    ));
}

#[test]
fn the_extraneous_store_objects_do_not_leak_into_the_closure() {
    let composite = valid_full();
    let mut source = composite.store();
    for seed in 200..203u8 {
        let unrelated = composite_manifest::encode_chunk_envelope(TAG_NEAR, &[seed; 9]);
        source.insert(unrelated.hash(), unrelated.into_bytes());
    }
    let verified = composite_reader::read_composite_closure(
        &mut source,
        &composite.root,
        &ReadLimits::default(),
    )
    .unwrap();
    let expected: Vec<Hash> = composite
        .manifest
        .entries
        .iter()
        .map(|entry| entry.identity)
        .collect();
    assert_eq!(
        verified.dependency_identities(),
        expected,
        "the dependency set is EXACTLY the manifest's declared closure"
    );
}

#[test]
fn the_resource_limits_refuse_before_any_source_read() {
    let composite = valid_full();
    // (a) The caller's tightening: a chunk-count limit below the
    // manifest's entry count refuses with ZERO source reads.
    let mut source = composite.store();
    let error = composite_reader::read_composite_closure(
        &mut source,
        &composite.root,
        &ReadLimits {
            max_chunks: 5,
            ..ReadLimits::default()
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        CompositeReadError::Limit {
            kind: "read/chunk-count",
            value: 11,
            max: 5
        }
    ));
    assert_eq!(
        source.reads, 0,
        "a refused manifest costs zero object reads"
    );
    // (b) The composed-length tightening.
    let mut source = composite.store();
    let error = composite_reader::read_composite_closure(
        &mut source,
        &composite.root,
        &ReadLimits {
            max_composed_length: 32,
            ..ReadLimits::default()
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        CompositeReadError::Limit {
            kind: "read/composed-length",
            ..
        }
    ));
    assert_eq!(source.reads, 0);
    // (c) A misconfigured limit ABOVE the schema's own bound fails
    // typed, never silently clamped.
    let mut source = composite.store();
    let error = composite_reader::read_composite_closure(
        &mut source,
        &composite.root,
        &ReadLimits {
            max_chunks: composite_manifest::MAX_MANIFEST_CHUNKS + 1,
            ..ReadLimits::default()
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        CompositeReadError::Limit {
            kind: "read-limits/chunk-count",
            ..
        }
    ));
    assert_eq!(source.reads, 0);
}

#[test]
fn the_repeated_reads_are_identical() {
    let composite = valid_full();
    let mut first_source = composite.store();
    let mut second_source = composite.store();
    let first = composite_reader::read_composite_closure(
        &mut first_source,
        &composite.root,
        &ReadLimits::default(),
    )
    .unwrap();
    let second = composite_reader::read_composite_closure(
        &mut second_source,
        &composite.root,
        &ReadLimits::default(),
    )
    .unwrap();
    assert_eq!(first, second, "the verified read is deterministic");
}

#[test]
fn the_memo_shares_verification_across_manifests_deterministically() {
    // Two grammatical composites sharing one sealed chunk identity
    // (the same tag and payload — the same content identity).
    let first = valid_full();
    let shared_payload = vec![42u8; 4 * 31];
    let shared_envelope =
        composite_manifest::encode_chunk_envelope(TAG_LEDGER_RECORDS, &shared_payload);
    let shared = shared_envelope.hash();
    // The second composite: identical shape, its first sealed chunk
    // replaced by the shared one.
    let mut manifest = first.manifest.clone();
    for entry in &mut manifest.entries {
        if entry.kind == SectionKind::LedgerSealed {
            entry.identity = shared;
            break;
        }
    }
    let second_root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    let shared_bytes = shared_envelope.into_bytes();
    let mut source = first.store();
    source.insert(shared, shared_bytes.clone());

    // With one threaded memo: the shared identity is read and
    // verified ONCE across the two roots.
    let mut memo = VerificationMemo::new();
    let read_first = composite_reader::read_composite_closure_with_memo(
        &mut source,
        &first.root,
        &ReadLimits::default(),
        &mut memo,
    )
    .unwrap();
    let read_second = composite_reader::read_composite_closure_with_memo(
        &mut source,
        &second_root,
        &ReadLimits::default(),
        &mut memo,
    )
    .unwrap();
    assert_eq!(source.reads, first.chunks.len() + 1);
    assert_eq!(memo.len(), first.chunks.len() + 1);
    assert_eq!(
        read_second.verified_identities(),
        manifest
            .entries
            .iter()
            .map(|entry| entry.identity)
            .collect::<Vec<_>>()
    );

    // Without the memo (fresh memos): every identity of the second
    // root is read again — one full extra read set — and the
    // results are identical.
    let mut source = first.store();
    source.insert(shared, shared_bytes);
    let again_first =
        composite_reader::read_composite_closure(&mut source, &first.root, &ReadLimits::default())
            .unwrap();
    let again_second =
        composite_reader::read_composite_closure(&mut source, &second_root, &ReadLimits::default())
            .unwrap();
    assert_eq!(source.reads, 2 * first.chunks.len());
    assert_eq!(read_first, again_first);
    assert_eq!(read_second, again_second);
}

#[test]
fn the_memoized_chunk_is_still_checked_against_the_new_declaration() {
    // The memo's correctness core: an identity fully verified for
    // ONE manifest (as a sealed ledger chunk) is NEVER blanket-
    // accepted for a DIFFERENT manifest's declaration (as the world
    // singleton) — the entry-level coherence still runs on the
    // memoized path. The mis-declared identity is one the FIRST
    // read verified, so the second read takes the MEMO path and the
    // coherence check is exactly what fails.
    let first = valid_full();
    // A sealed ledger chunk of the first manifest (index 6 — the
    // first entry after the ledger meta).
    let sealed_index = 6;
    let sealed_identity = first.identity(sealed_index);
    let sealed_length = first.manifest.entries[sealed_index].payload_length;
    // A SECOND composite with entirely different identities, whose
    // WORLD singleton names the FIRST manifest's sealed chunk (with
    // its payload length — the composed coherence holds; the
    // identity appears exactly once, so the decode's duplicate
    // rejection does not fire — only the reader's tag coherence can
    // catch the lie, and only on the MEMOIZED path: the first read
    // earned this identity).
    let mut second = SyntheticComposite::build(HistoryKind::Full, 4, 2, 2, 40);
    let world_index = second.manifest.entries.len() - 2;
    let world_length = second.manifest.entries[world_index].payload_length;
    second.manifest.entries[world_index].identity = sealed_identity;
    second.manifest.entries[world_index].payload_length = sealed_length;
    second.manifest.composed_length += sealed_length - world_length;
    let mis_declaring_root =
        StoredObject::new(composite_manifest::encode(&second.manifest)).unwrap();

    let mut source = first.store();
    for (identity, bytes) in &second.chunks {
        source.insert(*identity, bytes.clone());
    }
    let mut memo = VerificationMemo::new();
    composite_reader::read_composite_closure_with_memo(
        &mut source,
        &first.root,
        &ReadLimits::default(),
        &mut memo,
    )
    .unwrap();
    assert!(
        !memo.is_empty(),
        "the first read earned the sealed identity"
    );
    let error = composite_reader::read_composite_closure_with_memo(
        &mut source,
        &mis_declaring_root,
        &ReadLimits::default(),
        &mut memo,
    )
    .unwrap_err();
    assert_eq!(
        error,
        CompositeReadError::Tag {
            identity: sealed_identity,
            expected: TAG_WORLD,
            observed: TAG_LEDGER_RECORDS.to_owned(),
        },
        "the memoized chunk is checked against the new manifest's declaration"
    );
}

#[test]
fn the_failed_read_never_populates_the_memo() {
    // A corrupt chunk fails the verification — the memo stays empty
    // for that identity; a subsequent read of a manifest referencing
    // it fails identically (never accepted through an unearned
    // entry).
    let composite = valid_full();
    let director = composite.identity(composite.manifest.entries.len() - 1);
    let mut source = composite.store();
    let mut corrupted = source.objects[&director].clone();
    let last = corrupted.len() - 1;
    corrupted[last] ^= 0xff;
    source.insert(director, corrupted);
    let mut memo = VerificationMemo::new();
    let error = composite_reader::read_composite_closure_with_memo(
        &mut source,
        &composite.root,
        &ReadLimits::default(),
        &mut memo,
    )
    .unwrap_err();
    assert!(matches!(error, CompositeReadError::HashMismatch { identity } if identity == director));
    // The corrupt identity never earns a memo entry; the entries
    // before it (the director is the final entry) did.
    assert!(!memo.contains(&director));
    assert_eq!(memo.len(), composite.manifest.entries.len() - 1);
    // The same corruption fails identically with the memo.
    let mut memo = VerificationMemo::new();
    assert_eq!(
        composite_reader::read_composite_closure_with_memo(
            &mut source,
            &composite.root,
            &ReadLimits::default(),
            &mut memo
        )
        .unwrap_err(),
        error
    );
}

#[test]
fn the_reader_verifies_objects_published_through_a_real_local_file_medium() {
    // The strongest isolation proof: the PRODUCTION append writes
    // the composite's objects into a real `LocalFileMedium` (a test
    // fixture in a temp directory — no production path is involved),
    // and the dark reader reads them back through a read-only
    // adapter — every hash verified against the real provider's
    // stored bytes.
    let dir = std::env::temp_dir().join(format!(
        "s7-k2b-reader-real-medium-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store_root = dir.join("store");
    let mut medium = LocalFileMedium::open(&store_root).unwrap();
    let composite = valid_full();

    // The founding revision references the root and every chunk as
    // its content objects — the production publication path.
    let mut references: Vec<Hash> = composite
        .manifest
        .entries
        .iter()
        .map(|entry| entry.identity)
        .collect();
    references.push(composite.root.hash());
    let record = RevisionRecord::founding(AuthorityMode::Standalone, "k2b-dark-reader", 1)
        .with_content_refs(references);
    let mut objects: Vec<StoredObject> = composite
        .chunks
        .iter()
        .map(|(_identity, bytes)| StoredObject::new(bytes.clone()).unwrap())
        .collect();
    // with_content_refs sorts; the stored objects may arrive in any
    // order — the medium matches by hash. K2-C1: the publication now
    // goes through the DEDICATED composite admission (the generic
    // append's domain gate refuses /2 roots by design) — the root is
    // the first object, the chunks the new supply.
    objects.insert(0, composite.root.clone());
    let request = engine_persist::composite_append::CompositeAppend {
        record: &record,
        root: &composite.root,
        new_chunks: &objects[1..],
        reused_chunks: &[],
    };
    match medium.append_composite(&request, None).unwrap() {
        AppendOutcome::Accepted { .. } => {}
        other => panic!("the founding composite append is accepted: {other:?}"),
    }

    // The read-only adapter: the dark reader over the real medium.
    struct MediumSource<'a> {
        medium: &'a mut LocalFileMedium,
    }
    impl ObjectSource for MediumSource<'_> {
        fn read_object(&mut self, identity: &Hash) -> Result<Vec<u8>, ObjectSourceError> {
            self.medium
                .read_object(identity)
                .map_err(|error| match error {
                    MediumError::MissingObject { hash } => ObjectSourceError::Missing(hash),
                    other => ObjectSourceError::Unavailable(format!("{other:?}")),
                })
        }
    }
    let mut source = MediumSource {
        medium: &mut medium,
    };
    let verified = composite_reader::read_composite_closure(
        &mut source,
        &composite.root,
        &ReadLimits::default(),
    )
    .unwrap_or_else(|error| panic!("the real-medium read verifies: {error:?}"));
    assert_eq!(verified.root_identity, composite.root.hash());
    assert_eq!(
        verified.verified_identities(),
        composite
            .manifest
            .entries
            .iter()
            .map(|entry| entry.identity)
            .collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
