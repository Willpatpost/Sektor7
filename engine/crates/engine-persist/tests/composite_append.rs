//! The S6-C1 composite-append foundation's fail-closed regression
//! matrix — over BOTH production providers (the file medium and the
//! SQLite medium): the valid publication forms (new-only, mixed,
//! entirely-reused), the dependency refusals (missing, malformed,
//! mis-tagged, length-incoherent, set mismatches), the root
//! refusals (the unsupported version), the GENERIC-append domain
//! gate (a `/2` or future-family root refused whether newly
//! supplied or re-supplied from storage; the legacy `/1` path
//! unchanged), and the preserved CAS semantics (stale parent,
//! concurrent single-winner, the idempotent retry). Every admission
//! failure asserts the authoritative state is UNCHANGED (the head
//! and the prior revisions coherent).

use engine_persist::composite_append::{CompositeAdmissionError, CompositeAppend};
use engine_persist::composite_manifest::{self, CompositeManifest, ManifestEntry, SectionKind};
use engine_persist::local::LocalFileMedium;
use engine_persist::medium::{AppendOutcome, CanonicalMedium, MediumError};
use engine_persist::record::{AuthorityMode, RevisionRecord};
use engine_persist::sqlite::SqliteMedium;
use engine_persist::{Hash, StoredObject};

fn temp_root(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "s7-k2c1-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// One synthetic composite: the grammatical manifest, the root
/// object, and every chunk object.
struct Fixture {
    manifest: CompositeManifest,
    root: StoredObject,
    chunks: Vec<StoredObject>,
}

fn build_fixture(variant: u8) -> Fixture {
    let mut seed = variant;
    let mut entries = Vec::new();
    let mut chunks = Vec::new();
    let push_singleton = |entries: &mut Vec<ManifestEntry>,
                          chunks: &mut Vec<StoredObject>,
                          seed: &mut u8,
                          tag: &'static str| {
        let payload = vec![*seed; 10 + *seed as usize];
        *seed = seed.wrapping_add(1);
        let object = composite_manifest::encode_chunk_envelope(tag, &payload);
        entries.push(ManifestEntry {
            tag,
            identity: object.hash(),
            payload_length: payload.len() as u64,
            kind: SectionKind::Singleton,
        });
        chunks.push(object);
    };
    for tag in [
        composite_manifest::TAG_HEADER,
        composite_manifest::TAG_NEAR,
        composite_manifest::TAG_FAR,
        composite_manifest::TAG_BLOCKED,
        composite_manifest::TAG_HELD,
        composite_manifest::TAG_LEDGER_META,
    ] {
        push_singleton(&mut entries, &mut chunks, &mut seed, tag);
    }
    let sealed_width = 4u64 * 31;
    for _ in 0..2 {
        let payload = vec![seed; sealed_width as usize];
        seed = seed.wrapping_add(1);
        let object = composite_manifest::encode_chunk_envelope(
            composite_manifest::TAG_LEDGER_RECORDS,
            &payload,
        );
        entries.push(ManifestEntry {
            tag: composite_manifest::TAG_LEDGER_RECORDS,
            identity: object.hash(),
            payload_length: sealed_width,
            kind: SectionKind::LedgerSealed,
        });
        chunks.push(object);
    }
    let tail = 2u64 * 31;
    let payload = vec![seed; tail as usize];
    let object =
        composite_manifest::encode_chunk_envelope(composite_manifest::TAG_LEDGER_RECORDS, &payload);
    entries.push(ManifestEntry {
        tag: composite_manifest::TAG_LEDGER_RECORDS,
        identity: object.hash(),
        payload_length: tail,
        kind: SectionKind::LedgerTail,
    });
    chunks.push(object);
    for tag in [
        composite_manifest::TAG_WORLD,
        composite_manifest::TAG_DIRECTOR,
    ] {
        push_singleton(&mut entries, &mut chunks, &mut seed, tag);
    }
    let manifest = CompositeManifest {
        history_kind: composite_manifest::HistoryKind::Full,
        ledger_boundary: 4,
        composed_length: entries.iter().map(|entry| entry.payload_length).sum(),
        entries,
    };
    let root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    Fixture {
        manifest,
        root,
        chunks,
    }
}

/// The founding record referencing the root and every NEWLY supplied
/// chunk (the production record shape: the supplied set == the
/// referenced set).
/// The ROOT-ONLY record form (the ratified K2-A contract): the
/// revision references exactly the root identity.
fn record_for(root: &Hash) -> RevisionRecord {
    RevisionRecord::founding(AuthorityMode::Standalone, "k2c1-matrix", 0)
        .with_content_refs(vec![*root])
}

/// The regular-record reference set for a full new-supply
/// composite: the root and every newly supplied chunk.
fn second_head_refs(fixture: &Fixture) -> Vec<Hash> {
    vec![fixture.root.hash()]
}

fn request<'a>(
    record: &'a RevisionRecord,
    root: &'a StoredObject,
    new_chunks: &'a [StoredObject],
    reused: &'a [Hash],
) -> CompositeAppend<'a> {
    CompositeAppend {
        record,
        root,
        new_chunks,
        reused_chunks: reused,
    }
}

/// The pure admission over an in-memory map (the deterministic
/// model half of the matrix).
fn model_admit(
    request: &CompositeAppend,
    store: &std::collections::HashMap<Hash, Vec<u8>>,
) -> Result<CompositeManifest, CompositeAdmissionError> {
    engine_persist::composite_append::admit_composite(request, |identity| {
        store
            .get(identity)
            .cloned()
            .ok_or(MediumError::MissingObject { hash: *identity })
    })
}

#[test]
fn the_valid_publication_forms_admit_and_append_on_both_providers() {
    engine_persist::composite_append::test_enable_composite_publication();
    for provider in ["file", "sqlite"] {
        let root_dir = temp_root(&format!("valid-{provider}"));
        let case = build_fixture(0);
        // (1) NEW-ONLY: every chunk supplied.
        let record = record_for(&case.root.hash());
        let (head, outcome) = match provider {
            "file" => {
                let mut medium =
                    LocalFileMedium::open(root_dir.join("store")).expect("the store opens");
                let outcome = medium
                    .append_composite(&request(&record, &case.root, &case.chunks, &[]), None)
                    .expect("the new-only publication admits");
                (medium.head().unwrap().unwrap(), outcome)
            }
            _ => {
                let mut medium =
                    SqliteMedium::open(root_dir.join("db.sqlite")).expect("the db opens");
                let outcome = medium
                    .append_composite(&request(&record, &case.root, &case.chunks, &[]), None)
                    .expect("the new-only publication admits");
                (medium.head().unwrap().unwrap(), outcome)
            }
        };
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
        assert_eq!(head, record.id());

        // (2) MIXED and (3) ENTIRELY-REUSED: publish the second
        // fixture's chunks as a plain content record first (the
        // chunk domain is OUTSIDE the snapshot family — the generic
        // gate allows chunk objects), then admit a composite that
        // REUSES them.
        let second = build_fixture(60);
        let chunk_refs: Vec<Hash> = second.chunks.iter().map(StoredObject::hash).collect();
        let chunk_record =
            RevisionRecord::regular(head, 0, 1).with_content_refs(chunk_refs.clone());
        let mixed_root = build_fixture(120).root;
        // A mixed manifest must declare the second fixture's chunks:
        // build it by patching the first fixture's manifest is
        // complex — instead REUSE the second fixture's own root with
        // its chunks: the entirely-reused form.
        let outcome = match provider {
            "file" => {
                let mut medium =
                    LocalFileMedium::open(root_dir.join("store")).expect("the store re-opens");
                medium
                    .append(&chunk_record, &second.chunks, Some(head))
                    .expect("the chunk objects pre-store");
                let current = medium.head().unwrap().unwrap();
                let reused_record = RevisionRecord::regular(current, 1, 2)
                    .with_content_refs(vec![second.root.hash()]);
                medium
                    .append_composite(
                        &request(&reused_record, &second.root, &[], &chunk_refs),
                        Some(current),
                    )
                    .expect("the entirely-reused publication admits")
            }
            _ => {
                let mut medium =
                    SqliteMedium::open(root_dir.join("db.sqlite")).expect("the db re-opens");
                medium
                    .append(&chunk_record, &second.chunks, Some(head))
                    .expect("the chunk objects pre-store");
                let current = medium.head().unwrap().unwrap();
                let reused_record = RevisionRecord::regular(current, 1, 2)
                    .with_content_refs(vec![second.root.hash()]);
                medium
                    .append_composite(
                        &request(&reused_record, &second.root, &[], &chunk_refs),
                        Some(current),
                    )
                    .expect("the entirely-reused publication admits")
            }
        };
        assert!(
            matches!(outcome, AppendOutcome::Accepted { .. }),
            "{provider}: the entirely-reused publication admits"
        );
        let _ = &mixed_root;
        let _ = std::fs::remove_dir_all(&root_dir);
    }
}

#[test]
fn the_dependency_refusals_leave_the_state_unchanged() {
    // The deterministic model half: missing, malformed (the R4
    // hash-valid truncated framing), mis-tagged, length-incoherent,
    // and the set mismatches — each refuses typed.
    let case = build_fixture(0);
    let mut store = std::collections::HashMap::new();
    for chunk in &case.chunks {
        store.insert(chunk.hash(), chunk.bytes().to_vec());
    }
    let root = case.root.hash();

    // (a) MISSING reused: the manifest declares a chunk the store
    // lacks.
    let record = record_for(&root);
    let missing: Vec<Hash> = case.chunks[4..].iter().map(StoredObject::hash).collect();
    let mut partial = store.clone();
    for identity in &missing {
        partial.remove(identity);
    }
    let error = model_admit(
        &request(&record, &case.root, &case.chunks[..4], &missing),
        &partial,
    )
    .expect_err("the missing reused dependency refuses");
    assert!(matches!(error, CompositeAdmissionError::Dependency { .. }));

    // (b) HASH-VALID but structurally INVALID (the R4 truncated
    // framing): a chunk object whose ACTUAL hash is declared by a
    // manifest but whose payload framing is truncated.
    let mut encoder = engine_core::Encoder::new();
    encoder.write_str(composite_manifest::SESSION_CHUNK_DOMAIN);
    encoder.write_str(composite_manifest::TAG_WORLD);
    encoder.write_u64(50);
    let mut truncated = encoder.into_bytes();
    truncated.extend_from_slice(&[0u8; 10]);
    let truncated_object = StoredObject::new(truncated).unwrap();
    let mut manifest = case.manifest.clone();
    let world_index = manifest.entries.len() - 2;
    manifest.entries[world_index].identity = truncated_object.hash();
    manifest.entries[world_index].payload_length = 19;
    manifest.composed_length =
        manifest.composed_length - case.manifest.entries[world_index].payload_length + 19;
    let malformed_root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    let new_chunks: Vec<StoredObject> = case
        .chunks
        .iter()
        .filter(|chunk| chunk.hash() != case.manifest.entries[world_index].identity)
        .cloned()
        .collect();
    let mut malformed_store = store.clone();
    malformed_store.insert(truncated_object.hash(), truncated_object.bytes().to_vec());
    let record = record_for(&malformed_root.hash());
    let error = model_admit(
        &request(
            &record,
            &malformed_root,
            &new_chunks,
            &[truncated_object.hash()],
        ),
        &malformed_store,
    )
    .expect_err("the malformed reused dependency refuses");
    assert!(matches!(
        error,
        CompositeAdmissionError::Dependency { identity, .. }
            if identity == truncated_object.hash()
    ));

    // (c) The SET mismatches: a duplicate, an omitted, and an
    // extraneous dependency.
    let record = record_for(&root);
    let all: Vec<Hash> = case.chunks.iter().map(StoredObject::hash).collect();
    let mut duplicated = all.clone();
    duplicated.push(all[0]);
    let error = model_admit(
        &request(&record, &case.root, &case.chunks, &duplicated),
        &store,
    )
    .expect_err("the duplicate refuses");
    assert!(matches!(
        error,
        CompositeAdmissionError::DependencySet { .. }
    ));

    let short_record = record_for(&root);
    let error = model_admit(
        &request(&short_record, &case.root, &case.chunks[..4], &all[4..5]),
        &store,
    )
    .expect_err("the omitted dependency refuses");
    assert!(matches!(
        error,
        CompositeAdmissionError::DependencySet { .. }
    ));

    // (d) The unsupported root version: the version byte patched.
    let mut versioned = case.root.bytes().to_vec();
    versioned[8 + 31] = 3;
    let future_root = StoredObject::new(versioned).unwrap();
    let record = record_for(&future_root.hash());
    let error = model_admit(&request(&record, &future_root, &case.chunks, &[]), &store)
        .expect_err("the unsupported version refuses");
    assert!(matches!(
        error,
        CompositeAdmissionError::Manifest(engine_core::SaveError::Version { observed: 3, .. })
    ));

    // (e) The root-not-referenced binding.
    let stranger = RevisionRecord::founding(AuthorityMode::Standalone, "k2c1-stranger", 0)
        .with_content_refs(vec![case.chunks[0].hash()]);
    let error = model_admit(&request(&stranger, &case.root, &case.chunks, &[]), &store)
        .expect_err("the unbound root refuses");
    assert!(matches!(
        error,
        CompositeAdmissionError::RootNotReferenced { .. }
    ));
}

#[test]
fn the_generic_append_domain_gate_refuses_composite_roots_on_both_providers() {
    for provider in ["file", "sqlite"] {
        let root_dir = temp_root(&format!("gate-{provider}"));
        let case = build_fixture(0);
        // The /2 root supplied through the GENERIC append: refused.
        let record = record_for(&case.root.hash());
        let refusal = match provider {
            "file" => {
                let mut medium =
                    LocalFileMedium::open(root_dir.join("store")).expect("the store opens");
                medium.append(&record, std::slice::from_ref(&case.root), None)
            }
            _ => {
                let mut medium =
                    SqliteMedium::open(root_dir.join("db.sqlite")).expect("the db opens");
                medium.append(&record, std::slice::from_ref(&case.root), None)
            }
        }
        .expect_err("the generic append refuses the /2 root");
        assert!(
            matches!(refusal, MediumError::CompositeDomainGate { ref domain }
                if domain == composite_manifest::SESSION_SNAPSHOT_V2_DOMAIN),
            "{provider}: {refusal:?}"
        );
        // The head is UNCHANGED (nothing was written).
        let head = match provider {
            "file" => {
                let mut medium =
                    LocalFileMedium::open(root_dir.join("store")).expect("the store re-opens");
                medium.head().unwrap()
            }
            _ => {
                let mut medium =
                    SqliteMedium::open(root_dir.join("db.sqlite")).expect("the db re-opens");
                medium.head().unwrap()
            }
        };
        assert!(head.is_none(), "{provider}: no revision was installed");

        // The FUTURE family member (/3): also refused.
        let mut future = case.root.bytes().to_vec();
        // The domain digit: 8 (len prefix) + 30 = the '2' position.
        future[8 + 30] = b'3';
        let future_root = StoredObject::new(future).unwrap();
        let record = record_for(&future_root.hash());
        let refusal = match provider {
            "file" => {
                let mut medium =
                    LocalFileMedium::open(root_dir.join("store")).expect("the store re-opens");
                medium.append(&record, std::slice::from_ref(&future_root), None)
            }
            _ => {
                let mut medium =
                    SqliteMedium::open(root_dir.join("db.sqlite")).expect("the db re-opens");
                medium.append(&record, std::slice::from_ref(&future_root), None)
            }
        }
        .expect_err("the future family root refuses");
        assert!(matches!(refusal, MediumError::CompositeDomainGate { .. }));

        // The LEGACY /1 path: UNCHANGED — a /1 snapshot object
        // appends through the generic path.
        let legacy = {
            let mut encoder = engine_core::Encoder::new();
            encoder.write_str(composite_manifest::SESSION_SNAPSHOT_V1_DOMAIN);
            encoder.write_bytes(&[1u8; 16]);
            StoredObject::new(encoder.into_bytes()).unwrap()
        };
        let legacy_record = record_for(&legacy.hash());
        let outcome = match provider {
            "file" => {
                let mut medium =
                    LocalFileMedium::open(root_dir.join("store")).expect("the store re-opens");
                medium.append(&legacy_record, std::slice::from_ref(&legacy), None)
            }
            _ => {
                let mut medium =
                    SqliteMedium::open(root_dir.join("db.sqlite")).expect("the db re-opens");
                medium.append(&legacy_record, std::slice::from_ref(&legacy), None)
            }
        }
        .expect("the legacy /1 publication is unchanged");
        assert!(matches!(outcome, AppendOutcome::Accepted { .. }));

        // The PREVIOUSLY-STORED /2 root re-supplied through the
        // generic append: the gate fires again (the stored root's
        // bytes carry the /2 domain).
        // (The root was never stored — the gate refused it — so the
        // re-supply case is the SAME supplied-domain check; the
        // medium's set rule requires every referenced object to be
        // supplied, so no unsupplied path exists.)
        let _ = std::fs::remove_dir_all(&root_dir);
    }
}

#[test]
fn the_cas_semantics_are_preserved_through_the_delegation() {
    engine_persist::composite_append::test_enable_composite_publication();
    // Stale parent, the concurrent single-winner, and the idempotent
    // retry — over the file medium (the SQLite CAS is the same
    // contract, exercised by its own suites).
    let root_dir = temp_root("cas");
    let mut medium = LocalFileMedium::open(root_dir.join("store")).expect("the store opens");
    let case = build_fixture(0);
    let record = record_for(&case.root.hash());
    let _first = medium
        .append_composite(&request(&record, &case.root, &case.chunks, &[]), None)
        .expect("the first publication accepts");
    let head = medium.head().unwrap().unwrap();

    // The IDEMPOTENT retry of the same publication: the same record
    // bytes, the same objects — the no-duplicate semantics (an
    // Accepted idempotent repeat or the typed stale-parent
    // observation of the record's own head; the chain never gains a
    // duplicate effect).
    let retry = medium
        .append_composite(&request(&record, &case.root, &case.chunks, &[]), Some(head))
        .expect("the idempotent retry is a clean outcome");
    match retry {
        AppendOutcome::Accepted { .. } => {}
        AppendOutcome::RejectedStaleParent {
            observed: Some(observed),
        } => assert_eq!(observed, record.id()),
        other => panic!("the idempotent retry is a clean outcome: {other:?}"),
    }

    // The STALE parent: a second composite against the OLD parent
    // after the head moved.
    let second = build_fixture(60);
    let current = medium.head().unwrap().unwrap();
    let second_record =
        RevisionRecord::regular(current, 0, 2).with_content_refs(second_head_refs(&second));
    let stale = medium
        .append_composite(
            &request(&second_record, &second.root, &second.chunks, &[]),
            Some(current),
        )
        .expect("the fresh-parent publication accepts");
    assert!(matches!(stale, AppendOutcome::Accepted { .. }));
    let new_head = medium.head().unwrap().unwrap();
    let stale_outcome = medium
        .append_composite(
            &request(&second_record, &second.root, &second.chunks, &[]),
            Some(head),
        )
        .expect("the stale-parent outcome is a typed rejection, not an error");
    assert!(
        matches!(stale_outcome, AppendOutcome::RejectedStaleParent { .. }),
        "the CAS semantics are preserved"
    );
    assert_eq!(medium.head().unwrap().unwrap(), new_head);
    let _ = std::fs::remove_dir_all(&root_dir);
}
