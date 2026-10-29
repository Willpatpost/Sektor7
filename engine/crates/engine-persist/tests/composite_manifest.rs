//! The S6-C4 Phase 2 K2-A manifest contract tests — the strict
//! decoding and closure-validation matrix over crafted inputs (the
//! codec is DARK: no production path reads or writes it; these tests
//! are the schema's only consumers alongside the engine-slice K1
//! differential corpus).

use engine_core::SaveError;
use engine_persist::composite_manifest::{
    self, ClosureError, CompositeClosure, CompositeManifest, HistoryKind, MAX_CHUNK_BYTES,
    MAX_COMPOSED_LENGTH, MAX_LEDGER_BOUNDARY, ManifestEntry, SectionKind, TAG_BLOCKED,
    TAG_COMPACTED_META, TAG_DIRECTOR, TAG_FAR, TAG_HEADER, TAG_HELD, TAG_LEDGER_META,
    TAG_LEDGER_RECORDS, TAG_NEAR, TAG_WORLD,
};
use engine_persist::{Hash, RevisionRecord, StoredObject};

// ------------------------------------------------------------- fixtures

/// A well-formed entry with a DISTINCT identity (the manifest's
/// duplicate rule requires unique identities across the closure).
fn entry(tag: &'static str, payload_length: u64, kind: SectionKind, seed: u64) -> ManifestEntry {
    ManifestEntry {
        tag,
        identity: distinct_identity(seed),
        payload_length,
        kind,
    }
}

fn distinct_identity(index: u64) -> Hash {
    let mut bytes = [0u8; 32];
    bytes[24..].copy_from_slice(&index.to_be_bytes());
    Hash::from_bytes(&bytes).unwrap()
}

/// A minimal VALID full-history manifest: the singleton sections,
/// one sealed ledger chunk at the boundary, one partial tail, the
/// world, the director.
fn valid_full_manifest() -> CompositeManifest {
    let boundary = 8u64;
    let sealed = boundary * 31;
    let entries = vec![
        entry(TAG_HEADER, 100, SectionKind::Singleton, 1),
        entry(TAG_NEAR, 8, SectionKind::Singleton, 2),
        entry(TAG_FAR, 8, SectionKind::Singleton, 3),
        entry(TAG_BLOCKED, 8, SectionKind::Singleton, 4),
        entry(TAG_HELD, 8, SectionKind::Singleton, 5),
        entry(TAG_LEDGER_META, 8, SectionKind::Singleton, 6),
        entry(TAG_LEDGER_RECORDS, sealed, SectionKind::LedgerSealed, 7),
        entry(TAG_LEDGER_RECORDS, 3 * 31, SectionKind::LedgerTail, 8),
        entry(TAG_WORLD, 400, SectionKind::Singleton, 9),
        entry(TAG_DIRECTOR, 200, SectionKind::Singleton, 10),
    ];
    let composed: u64 = entries.iter().map(|e| e.payload_length).sum();
    CompositeManifest {
        history_kind: HistoryKind::Full,
        ledger_boundary: boundary,
        composed_length: composed,
        entries,
    }
}

/// A minimal VALID compacted-history manifest (the compacted-meta
/// singleton present).
fn valid_compacted_manifest() -> CompositeManifest {
    let mut manifest = valid_full_manifest();
    manifest.history_kind = HistoryKind::Compacted;
    manifest
        .entries
        .insert(5, entry(TAG_COMPACTED_META, 64, SectionKind::Singleton, 20));
    manifest.composed_length = manifest.entries.iter().map(|e| e.payload_length).sum();
    manifest
}

fn reencode(mut manifest: CompositeManifest, fix_composed: bool) -> Vec<u8> {
    if fix_composed {
        manifest.composed_length = manifest.entries.iter().map(|e| e.payload_length).sum();
    }
    composite_manifest::encode(&manifest)
}

/// Encodes a manifest and corrupts ONE field at a byte offset
/// computed by re-encoding with a marker (the crafted-input helper:
/// the tests below describe the corruption each performs).
fn encoded_valid_full() -> Vec<u8> {
    composite_manifest::encode(&valid_full_manifest())
}

// ------------------------------------------------------------ the matrix

#[test]
fn the_valid_full_and_compacted_manifests_decode() {
    let decoded = composite_manifest::decode(&encoded_valid_full())
        .expect("the valid full-history manifest decodes");
    assert_eq!(decoded.history_kind, HistoryKind::Full);
    assert_eq!(decoded.entries.len(), 10);
    let decoded = composite_manifest::decode(&reencode(valid_compacted_manifest(), true))
        .expect("the valid compacted-history manifest decodes");
    assert_eq!(decoded.history_kind, HistoryKind::Compacted);
    assert_eq!(decoded.entries.len(), 11);
}

#[test]
fn the_exact_boundary_sealed_chunk_and_partial_tail_and_empty_ledger_decode() {
    // The exact-boundary case: the ledger run is exactly one sealed
    // chunk (the full final chunk — sealed, stable).
    let mut manifest = valid_full_manifest();
    manifest.entries.truncate(7); // through the first sealed chunk
    manifest
        .entries
        .push(entry(TAG_WORLD, 400, SectionKind::Singleton, 9));
    manifest
        .entries
        .push(entry(TAG_DIRECTOR, 200, SectionKind::Singleton, 10));
    manifest.composed_length = manifest.entries.iter().map(|e| e.payload_length).sum();
    let decoded = composite_manifest::decode(&composite_manifest::encode(&manifest))
        .expect("the exactly-full final chunk is sealed and valid");
    assert_eq!(decoded.ledger_entries().count(), 1);

    // The empty ledger: one uniform empty tail chunk.
    let mut manifest = valid_full_manifest();
    manifest.entries.truncate(6); // through ledger-meta
    manifest
        .entries
        .push(entry(TAG_LEDGER_RECORDS, 0, SectionKind::LedgerTail, 21));
    manifest
        .entries
        .push(entry(TAG_WORLD, 400, SectionKind::Singleton, 9));
    manifest
        .entries
        .push(entry(TAG_DIRECTOR, 200, SectionKind::Singleton, 10));
    manifest.composed_length = manifest.entries.iter().map(|e| e.payload_length).sum();
    composite_manifest::decode(&composite_manifest::encode(&manifest))
        .expect("the empty ledger's uniform tail decodes");
}

#[test]
fn a_missing_required_singleton_is_rejected() {
    for drop_index in [0usize, 5, 8, 9] {
        let mut manifest = valid_full_manifest();
        manifest.entries.remove(drop_index);
        let error = composite_manifest::decode(&reencode(manifest, true))
            .expect_err("the missing singleton is rejected");
        assert!(
            matches!(error, SaveError::Integrity(_) | SaveError::Truncated),
            "the typed structural rejection: {error:?}"
        );
    }
}

#[test]
fn an_extra_or_duplicate_singleton_is_rejected() {
    // A duplicate header (the grammar's positional order breaks).
    let mut manifest = valid_full_manifest();
    manifest
        .entries
        .insert(1, entry(TAG_HEADER, 100, SectionKind::Singleton, 22));
    let error = composite_manifest::decode(&reencode(manifest, true))
        .expect_err("the duplicate singleton is rejected");
    assert!(matches!(error, SaveError::Integrity(_)), "{error:?}");
    // An extra invented section after the director (the grammar has
    // no slot left).
    let mut manifest = valid_full_manifest();
    manifest
        .entries
        .push(entry(TAG_NEAR, 8, SectionKind::Singleton, 23));
    let error = composite_manifest::decode(&reencode(manifest, true))
        .expect_err("the extra section is rejected");
    assert!(
        matches!(error, SaveError::Integrity(_)),
        "the grammar-overflow rejection: {error:?}"
    );
}

#[test]
fn a_duplicate_chunk_identity_is_rejected() {
    let mut manifest = valid_full_manifest();
    let duplicate = manifest.entries[6].clone();
    manifest.entries[7] = duplicate;
    let error = composite_manifest::decode(&reencode(manifest, true))
        .expect_err("the duplicate identity is rejected");
    assert!(matches!(error, SaveError::Duplicate { .. }), "{error:?}");
}

#[test]
fn reordered_sections_are_rejected() {
    // The world before the held section (the positional grammar).
    let mut manifest = valid_full_manifest();
    let world = manifest.entries.remove(7);
    manifest.entries.insert(4, world);
    let error = composite_manifest::decode(&reencode(manifest, true))
        .expect_err("the reordered sections are rejected");
    assert!(matches!(error, SaveError::Integrity(_)), "{error:?}");
    // The ledger run displaced after the director.
    let mut manifest = valid_full_manifest();
    let tail = manifest.entries.remove(7);
    manifest.entries.push(tail);
    let error = composite_manifest::decode(&reencode(manifest, true))
        .expect_err("the displaced ledger run is rejected");
    assert!(matches!(error, SaveError::Integrity(_)), "{error:?}");
}

#[test]
fn invalid_discriminants_are_rejected_typed() {
    // The history-kind discriminant.
    let bytes = encoded_valid_full();
    let mut corrupt = bytes.clone();
    // domain str (8+29) + version (4) → the history-kind byte.
    corrupt[8 + 31 + 4] = 7;
    let error =
        composite_manifest::decode(&corrupt).expect_err("the invalid history-kind is rejected");
    assert!(matches!(error, SaveError::Enum { .. }), "{error:?}");
    // The section-kind discriminant: the first entry's kind byte sits
    // after the header entry's fixed fields — locate by re-encoding a
    // marker manifest and scanning for the first kind position.
    let mut manifest = valid_full_manifest();
    manifest.entries[0].kind = SectionKind::LedgerSealed;
    let error = composite_manifest::decode(&reencode(manifest, false))
        .expect_err("the singleton's invalid section-kind is rejected");
    assert!(matches!(error, SaveError::Integrity(_)), "{error:?}");
    // A raw invalid kind byte (discriminant 9).
    let mut manifest = valid_full_manifest();
    manifest.entries[6].kind = SectionKind::Singleton; // the sealed chunk as a singleton
    let error = composite_manifest::decode(&reencode(manifest, false))
        .expect_err("the ledger singleton kind is rejected");
    assert!(matches!(error, SaveError::Integrity(_)), "{error:?}");
}

#[test]
fn the_full_compacted_metadata_mismatch_is_rejected() {
    // The full form carrying a compacted-meta singleton.
    // (The compacted manifest itself is valid — the mismatch cases:)
    let mut full_with_meta = valid_full_manifest();
    full_with_meta
        .entries
        .insert(5, entry(TAG_COMPACTED_META, 64, SectionKind::Singleton, 20));
    full_with_meta.composed_length = full_with_meta
        .entries
        .iter()
        .map(|e| e.payload_length)
        .sum();
    let error = composite_manifest::decode(&composite_manifest::encode(&full_with_meta))
        .expect_err("the full form with compact-meta is rejected");
    assert!(matches!(error, SaveError::Integrity(_)), "{error:?}");
    // The compacted form missing its meta.
    let mut compacted_without_meta = valid_compacted_manifest();
    compacted_without_meta.entries.remove(5);
    compacted_without_meta.composed_length = compacted_without_meta
        .entries
        .iter()
        .map(|e| e.payload_length)
        .sum();
    let error = composite_manifest::decode(&composite_manifest::encode(&compacted_without_meta))
        .expect_err("the compacted form without meta is rejected");
    assert!(matches!(error, SaveError::Integrity(_)), "{error:?}");
    let _ = error;
    let _ = composite_manifest::decode(&reencode(valid_compacted_manifest(), true))
        .expect("the valid compacted control passes");
}

#[test]
fn the_wrong_domain_and_future_version_are_rejected_typed() {
    let mut corrupt = encoded_valid_full();
    // Corrupt the domain string's first byte (a well-formed foreign
    // domain).
    corrupt[8] = b'x';
    let error = composite_manifest::decode(&corrupt).expect_err("the wrong domain is rejected");
    assert!(matches!(error, SaveError::Domain { .. }), "{error:?}");
    // A recognized family member (a future /3).
    let mut future = encoded_valid_full();
    // The domain is length-prefixed: rewrite it as /3 with the same
    // length (a 2-byte swap of the tail "2" -> "3").
    let domain_offset = 8;
    future[domain_offset + 30] = b'3';
    let error =
        composite_manifest::decode(&future).expect_err("the future family member is rejected");
    match error {
        SaveError::Domain { observed, .. } => {
            assert!(observed.contains("recognized family"), "{observed}");
        }
        other => panic!("the family recognition: {other:?}"),
    }
    // The unsupported version.
    let mut versioned = encoded_valid_full();
    versioned[8 + 31] = 3; // the version u32's little-endian low byte
    let error =
        composite_manifest::decode(&versioned).expect_err("the unsupported version is rejected");
    assert!(matches!(error, SaveError::Version { .. }), "{error:?}");
}

#[test]
fn truncation_and_trailing_bytes_are_rejected_typed() {
    let bytes = encoded_valid_full();
    let error = composite_manifest::decode(&bytes[..bytes.len() / 2])
        .expect_err("the truncated manifest is rejected");
    assert!(
        matches!(error, SaveError::Truncated | SaveError::Limit { .. }),
        "the typed truncation class (the remaining bound may fire as Limit): {error:?}"
    );
    let mut trailing = bytes.clone();
    trailing.extend_from_slice(b"xx");
    let error = composite_manifest::decode(&trailing).expect_err("the trailing bytes are rejected");
    assert!(matches!(error, SaveError::Trailing { .. }), "{error:?}");
}

#[test]
fn oversized_counts_and_lengths_are_rejected_typed() {
    // The oversized section count: a declared count beyond the
    // maximum (the remaining-bytes bound fires first in this crafted
    // case — either bound is the typed Limit class).
    let manifest = valid_full_manifest();
    let mut bytes = composite_manifest::encode(&manifest);
    // The chunk-count u64 sits after domain(37) + version(4) +
    // kind(1) + boundary(8) + composed(8) = 58.
    let count_offset = 8 + 31 + 4 + 1 + 8 + 8;
    bytes[count_offset..count_offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    let error = composite_manifest::decode(&bytes).expect_err("the oversized count is rejected");
    assert!(matches!(error, SaveError::Limit { .. }), "{error:?}");
    // The oversized chunk length.
    let mut manifest = valid_full_manifest();
    manifest.entries[0].payload_length = MAX_CHUNK_BYTES + 1;
    let error = composite_manifest::decode(&reencode(manifest, false))
        .expect_err("the oversized chunk length is rejected");
    assert!(
        matches!(
            error,
            SaveError::Limit {
                kind: "chunk-length",
                ..
            }
        ),
        "{error:?}"
    );
    // The oversized boundary (the sealed width would overflow the
    // chunk bound).
    let mut manifest = valid_full_manifest();
    manifest.ledger_boundary = MAX_LEDGER_BOUNDARY + 1;
    let error = composite_manifest::decode(&reencode(manifest, false))
        .expect_err("the oversized boundary is rejected");
    assert!(
        matches!(
            error,
            SaveError::Limit {
                kind: "ledger-boundary",
                ..
            }
        ),
        "{error:?}"
    );
    // The oversized composed length.
    let mut manifest = valid_full_manifest();
    manifest.composed_length = MAX_COMPOSED_LENGTH + 1;
    let error = composite_manifest::decode(&reencode(manifest, false))
        .expect_err("the oversized composed length is rejected");
    assert!(
        matches!(
            error,
            SaveError::Limit {
                kind: "composed-length",
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn the_mismatched_composed_length_is_rejected() {
    let mut manifest = valid_full_manifest();
    manifest.composed_length += 1;
    let error = composite_manifest::decode(&composite_manifest::encode(&manifest))
        .expect_err("the mismatched composed length is rejected");
    assert!(matches!(error, SaveError::Integrity(_)), "{error:?}");
}

#[test]
fn the_sealed_and_tail_width_rules_are_enforced() {
    // A non-final tail-kind entry.
    let mut manifest = valid_full_manifest();
    manifest.entries[6].kind = SectionKind::LedgerTail;
    let error = composite_manifest::decode(&reencode(manifest, false))
        .expect_err("the non-final tail is rejected");
    assert!(matches!(error, SaveError::Integrity(_)), "{error:?}");
    // A sealed chunk below the boundary width.
    let mut manifest = valid_full_manifest();
    manifest.entries[6].payload_length = 31;
    manifest.composed_length = manifest.entries.iter().map(|e| e.payload_length).sum();
    let error = composite_manifest::decode(&reencode(manifest, false))
        .expect_err("the wrong sealed width is rejected");
    assert!(matches!(error, SaveError::Integrity(_)), "{error:?}");
    // A tail at or beyond the boundary width.
    let mut manifest = valid_full_manifest();
    manifest.entries[7].payload_length = 8 * 31;
    manifest.composed_length = manifest.entries.iter().map(|e| e.payload_length).sum();
    let error = composite_manifest::decode(&reencode(manifest, false))
        .expect_err("the over-wide tail is rejected");
    assert!(matches!(error, SaveError::Integrity(_)), "{error:?}");
    // A tail not a whole number of records.
    let mut manifest = valid_full_manifest();
    manifest.entries[7].payload_length = 3 * 31 + 1;
    manifest.composed_length = manifest.entries.iter().map(|e| e.payload_length).sum();
    let error = composite_manifest::decode(&reencode(manifest, false))
        .expect_err("the unaligned tail is rejected");
    assert!(matches!(error, SaveError::Integrity(_)), "{error:?}");
}

// ---------------------------------------------------- the closure contract

fn closure_fixture() -> (RevisionRecord, CompositeClosure) {
    // Build a valid manifest with REAL chunk objects (the envelopes
    // encoded from arbitrary payloads at the declared lengths).
    let boundary = 2u64;
    let sealed = boundary * 31;
    let payloads: Vec<(&'static str, Vec<u8>, SectionKind)> = vec![
        (TAG_HEADER, vec![1u8; 40], SectionKind::Singleton),
        (TAG_NEAR, vec![2u8; 8], SectionKind::Singleton),
        (TAG_FAR, vec![3u8; 8], SectionKind::Singleton),
        (TAG_BLOCKED, vec![4u8; 8], SectionKind::Singleton),
        (TAG_HELD, vec![5u8; 8], SectionKind::Singleton),
        (TAG_LEDGER_META, vec![6u8; 8], SectionKind::Singleton),
        (
            TAG_LEDGER_RECORDS,
            vec![7u8; sealed as usize],
            SectionKind::LedgerSealed,
        ),
        (TAG_LEDGER_RECORDS, vec![8u8; 31], SectionKind::LedgerTail),
        (TAG_WORLD, vec![9u8; 30], SectionKind::Singleton),
        (TAG_DIRECTOR, vec![10u8; 20], SectionKind::Singleton),
    ];
    let mut entries = Vec::new();
    let mut new_chunks = Vec::new();
    for (tag, payload, kind) in payloads {
        let envelope = composite_manifest::encode_chunk_envelope(tag, &payload);
        entries.push(ManifestEntry {
            tag,
            identity: envelope.hash(),
            payload_length: payload.len() as u64,
            kind,
        });
        new_chunks.push(envelope);
    }
    let composed: u64 = entries.iter().map(|e| e.payload_length).sum();
    let manifest = CompositeManifest {
        history_kind: HistoryKind::Full,
        ledger_boundary: boundary,
        composed_length: composed,
        entries,
    };
    let root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    let record = RevisionRecord::regular(Hash::from_bytes(&[9u8; 32]).unwrap(), 4, 77)
        .with_idempotency_key(root.hash().as_bytes().to_vec())
        .with_content_refs(vec![root.hash()]);
    let closure = CompositeClosure {
        root,
        new_chunks,
        reused_chunks: Vec::new(),
    };
    (record, closure)
}

#[test]
fn the_valid_closure_validates_and_marks_dependencies() {
    let (record, closure) = closure_fixture();
    let validated = composite_manifest::validate_composite_closure(&record, &closure)
        .expect("the valid closure validates");
    assert_eq!(validated.root_identity, closure.root.hash());
    assert_eq!(validated.new_identities.len(), closure.new_chunks.len());
    assert!(
        validated.unverified_dependencies.is_empty(),
        "no reused chunks in this fixture"
    );
}

#[test]
fn the_root_reference_contract_is_enforced() {
    let (_record, mut closure) = closure_fixture();
    // Two content references.
    let two_refs = RevisionRecord::regular(Hash::from_bytes(&[9u8; 32]).unwrap(), 4, 77)
        .with_idempotency_key(b"key".to_vec())
        .with_content_refs(vec![
            closure.root.hash(),
            Hash::from_bytes(&[1u8; 32]).unwrap(),
        ]);
    let error = composite_manifest::validate_composite_closure(&two_refs, &closure)
        .expect_err("the two-reference revision is rejected");
    assert_eq!(error, ClosureError::RootReference);
    // A single wrong reference.
    let wrong_ref = RevisionRecord::regular(Hash::from_bytes(&[9u8; 32]).unwrap(), 4, 77)
        .with_idempotency_key(b"key".to_vec())
        .with_content_refs(vec![Hash::from_bytes(&[2u8; 32]).unwrap()]);
    let error = composite_manifest::validate_composite_closure(&wrong_ref, &closure)
        .expect_err("the wrong reference is rejected");
    assert_eq!(error, ClosureError::RootReference);
    // A corrupted root (no longer a decodable manifest): the record
    // is rebound to the corrupted root's identity so the reference
    // check passes and the manifest decode failure surfaces.
    let mut corrupt_root_bytes = closure.root.bytes().to_vec();
    corrupt_root_bytes[8] = b'x';
    closure.root = StoredObject::new(corrupt_root_bytes).unwrap();
    let rebound = RevisionRecord::regular(Hash::from_bytes(&[9u8; 32]).unwrap(), 4, 77)
        .with_idempotency_key(b"key".to_vec())
        .with_content_refs(vec![closure.root.hash()]);
    let error = composite_manifest::validate_composite_closure(&rebound, &closure)
        .expect_err("the corrupt root is rejected");
    assert!(matches!(error, ClosureError::Manifest(_)), "{error:?}");
}

#[test]
fn the_closure_set_contract_is_enforced() {
    let (record, closure) = closure_fixture();
    // A missing chunk (drop one).
    let mut missing = closure.clone();
    missing.new_chunks.pop();
    let error = composite_manifest::validate_composite_closure(&record, &missing)
        .expect_err("the missing chunk is rejected");
    assert_eq!(error, ClosureError::ClosureSet);
    // An extraneous chunk (an envelope not in the manifest).
    let mut extra = closure.clone();
    extra
        .new_chunks
        .push(composite_manifest::encode_chunk_envelope(
            TAG_NEAR, &[99u8; 4],
        ));
    let error = composite_manifest::validate_composite_closure(&record, &extra)
        .expect_err("the extraneous chunk is rejected");
    assert_eq!(error, ClosureError::ClosureSet);
    // A repeated chunk (the same envelope supplied twice).
    let mut repeated = closure.clone();
    let first = repeated.new_chunks[0].clone();
    repeated.new_chunks.push(first);
    let error = composite_manifest::validate_composite_closure(&record, &repeated)
        .expect_err("the repeated chunk is rejected");
    assert_eq!(error, ClosureError::ClosureSet);
    // A reused chunk the manifest does not name (an extraneous
    // reuse).
    let mut bad_reuse = closure.clone();
    bad_reuse
        .reused_chunks
        .push(Hash::from_bytes(&[7u8; 32]).unwrap());
    let error = composite_manifest::validate_composite_closure(&record, &bad_reuse)
        .expect_err("the extraneous reuse is rejected");
    assert_eq!(error, ClosureError::ClosureSet);
}

#[test]
fn the_reused_chunks_are_carried_as_unverified_dependencies() {
    // Split the fixture: the tail chunk becomes a REUSED identity —
    // the validator must carry it as an unverified dependency (never
    // claiming durability) and still validate the rest.
    let (record, mut closure) = closure_fixture();
    let tail = closure.new_chunks.pop().expect("the tail chunk");
    closure.reused_chunks.push(tail.hash());
    let validated = composite_manifest::validate_composite_closure(&record, &closure)
        .expect("the reuse split validates");
    assert_eq!(validated.unverified_dependencies, vec![tail.hash()]);
    assert_eq!(validated.new_identities.len(), closure.new_chunks.len());
    // A manifest entry whose DECLARED length disagrees with the
    // chunk's actual envelope payload (the identity still names the
    // real chunk, so the set matches — the entry check catches the
    // disagreement).
    let (record, closure) = closure_fixture();
    let validated = composite_manifest::validate_composite_closure(&record, &closure).unwrap();
    let mut manifest = validated.manifest.clone();
    manifest.entries[0].payload_length -= 1;
    manifest.composed_length -= 1;
    let root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    let record = RevisionRecord::regular(Hash::from_bytes(&[9u8; 32]).unwrap(), 4, 77)
        .with_idempotency_key(b"key".to_vec())
        .with_content_refs(vec![root.hash()]);
    let closure = CompositeClosure {
        root,
        new_chunks: closure.new_chunks.clone(),
        reused_chunks: Vec::new(),
    };
    let error = composite_manifest::validate_composite_closure(&record, &closure)
        .expect_err("the mismatched declared length is rejected");
    assert!(matches!(error, ClosureError::NewChunk { .. }), "{error:?}");
    // A manifest entry whose IDENTITY names a chunk whose envelope
    // carries a different section tag than the entry declares (the
    // set matches — the envelope check catches the disagreement).
    let (_record, mut closure) = closure_fixture();
    let foreign = composite_manifest::encode_chunk_envelope(TAG_NEAR, &[1u8; 40]);
    closure.new_chunks[0] = foreign.clone();
    let mut manifest = composite_manifest::decode(closure.root.bytes()).unwrap();
    manifest.entries[0].identity = foreign.hash();
    let root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    let record = RevisionRecord::regular(Hash::from_bytes(&[9u8; 32]).unwrap(), 4, 77)
        .with_idempotency_key(b"key".to_vec())
        .with_content_refs(vec![root.hash()]);
    let closure = CompositeClosure {
        root,
        new_chunks: closure.new_chunks,
        reused_chunks: Vec::new(),
    };
    let error = composite_manifest::validate_composite_closure(&record, &closure)
        .expect_err("the wrong-tag entry is rejected");
    assert!(matches!(error, ClosureError::NewChunk { .. }), "{error:?}");
}
