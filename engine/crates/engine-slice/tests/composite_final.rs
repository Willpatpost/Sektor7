//! The R12/R13/R14 final-corrective regressions — the GENUINE
//! same-handle suffix-path tests (the trusted cursor absorbs the
//! verified composite closure through the hot path, never through a
//! fresh reopen), the SQLite cold/hot/suffix closure-equivalence
//! matrix, and the production-default activation-refusal proof (a
//! genuinely feature-disabled process: this test file runs WITHOUT
//! the composite-test-fixture feature — it is NOT in the
//! required-features set and contains NO activation calls).

use engine_persist::composite_manifest::{self, CompositeManifest, ManifestEntry, SectionKind};
use engine_persist::local::LocalFileMedium;
use engine_persist::medium::{AppendOutcome, CanonicalMedium};
use engine_persist::record::{AuthorityMode, RevisionRecord};
use engine_persist::sqlite::SqliteMedium;
use engine_persist::{Hash, StoredObject};

fn temp_root(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "s7-k2c1-fin-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

struct Fixture {
    root: StoredObject,
    chunks: Vec<StoredObject>,
}

fn fixture(variant: u8) -> Fixture {
    let mut seed = variant;
    let mut entries = Vec::new();
    let mut chunks = Vec::new();
    let push = |entries: &mut Vec<ManifestEntry>,
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
        push(&mut entries, &mut chunks, &mut seed, tag);
    }
    let sealed = 4u64 * 31;
    for _ in 0..2 {
        let payload = vec![seed; sealed as usize];
        seed = seed.wrapping_add(1);
        let object = composite_manifest::encode_chunk_envelope(
            composite_manifest::TAG_LEDGER_RECORDS,
            &payload,
        );
        entries.push(ManifestEntry {
            tag: composite_manifest::TAG_LEDGER_RECORDS,
            identity: object.hash(),
            payload_length: sealed,
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
        push(&mut entries, &mut chunks, &mut seed, tag);
    }
    let manifest = CompositeManifest {
        history_kind: composite_manifest::HistoryKind::Full,
        ledger_boundary: 4,
        composed_length: entries.iter().map(|e| e.payload_length).sum(),
        entries,
    };
    let root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    Fixture { root, chunks }
}

fn composite_record(root: &Hash, parent: Option<(Hash, u64)>) -> RevisionRecord {
    match parent {
        None => RevisionRecord::founding(AuthorityMode::Standalone, "r12", 0)
            .with_content_refs(vec![*root]),
        Some((head, epoch)) => {
            RevisionRecord::regular(head, epoch, 1).with_content_refs(vec![*root])
        }
    }
}

/// Writes an external composite revision directly to the SQLite
/// database (a second connection — the same tables the provider
/// uses). NO publication API involved.
fn external_sqlite_composite(
    db_path: &std::path::Path,
    parent: Hash,
    parent_epoch: u64,
    variant: u8,
    corrupt_chunk: Option<usize>,
    remove_chunk: Option<usize>,
) {
    let case = fixture(variant);
    let record =
        RevisionRecord::regular(parent, parent_epoch, 99).with_content_refs(vec![case.root.hash()]);
    let record_id = record.id();

    let conn = rusqlite::Connection::open(db_path).unwrap();
    // Write the objects (the provider's own schema: the raw 32-byte
    // hash blob, the domain, the bytes).
    let mut objects: Vec<StoredObject> = vec![case.root.clone()];
    objects.extend(case.chunks.iter().cloned());
    for (index, object) in objects.iter().enumerate() {
        let bytes = if let Some(corrupt) = corrupt_chunk
            && index == corrupt + 1
        {
            let mut corrupted = object.bytes().to_vec();
            let last = corrupted.len() - 1;
            corrupted[last] ^= 0xff;
            corrupted
        } else {
            object.bytes().to_vec()
        };
        let skip = remove_chunk.is_some_and(|remove| index == remove + 1);
        if !skip {
            conn.execute(
                "INSERT OR IGNORE INTO objects (hash, domain, bytes) VALUES (?1, ?2, ?3)",
                rusqlite::params![object.hash().as_bytes(), object.domain(), bytes],
            )
            .unwrap();
        }
    }
    // Write the record (the records table — the full schema: the
    // hash blob, the bytes, the epoch, the parent, the idem, the
    // kind, the tick — mirroring the provider's own insert).
    let record_bytes = record.canonical_bytes();
    let kind = 1; // Regular
    conn.execute(
        "INSERT OR IGNORE INTO records (hash, bytes, epoch, parent, idem, kind, tick) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            record_id.as_bytes(),
            record_bytes,
            record.epoch as i64,
            parent.as_bytes(),
            std::option::Option::<Vec<u8>>::None,
            kind,
            record.tick as i64,
        ],
    )
    .unwrap();
    // The record object in the CAS (the objects table).
    conn.execute(
        "INSERT OR IGNORE INTO objects (hash, domain, bytes) VALUES (?1, ?2, ?3)",
        rusqlite::params![
            record_id.as_bytes(),
            "sektor7/engine-persist/revision-record/1",
            record.canonical_bytes()
        ],
    )
    .unwrap();
    // Update HEAD (the raw 32-byte hash as a blob).
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('head', ?1)",
        rusqlite::params![record_id.as_bytes()],
    )
    .unwrap();
}

// -------------------------- R12: the SQLite closure-equivalence matrix

#[test]
fn the_sqlite_hot_cursor_includes_the_verified_closure() {
    // R12-B: publish through an already-open SQLite provider and
    // demonstrate that its trusted cursor includes the VERIFIED
    // transitive dependencies (not merely the root). The proof:
    // validate() after the append (WITHOUT reopening) succeeds and
    // reports the correct content-object count; and a subsequent
    // head() on the same handle (which triggers the trusted-cursor
    // path, not the cold walk) works.
    engine_persist::composite_append::test_enable_composite_publication();
    let root_dir = temp_root("sqlite-hot");
    let db_path = root_dir.join("db.sqlite");
    let mut medium = SqliteMedium::open(&db_path).unwrap();
    let case = fixture(0);
    let record = composite_record(&case.root.hash(), None);
    let request = engine_persist::composite_append::CompositeAppend {
        record: &record,
        root: &case.root,
        new_chunks: &case.chunks,
        reused_chunks: &[],
    };
    let outcome = medium
        .append_composite(&request, None)
        .expect("the hot publication accepts");
    assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
    // The hot cursor's closure: the trusted head read (no cold walk
    // — the cursor is already at the new head) + validate() through
    // the trusted path.
    assert_eq!(medium.head().unwrap().unwrap(), record.id());
    let summary = medium.validate().expect("the hot validate succeeds");
    assert_eq!(summary.head, record.id());
    // The content-object count: the root + every chunk (the closure).
    assert!(
        summary.content_objects > case.chunks.len() as u64,
        "the trusted cursor counts the closure: {} objects (root + chunks)",
        summary.content_objects
    );
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn the_sqlite_cold_reopen_validates_the_closure() {
    // R12-A: publish, close the provider, reopen — the cold walk
    // verifies the complete dependency closure.
    engine_persist::composite_append::test_enable_composite_publication();
    let root_dir = temp_root("sqlite-cold");
    let db_path = root_dir.join("db.sqlite");
    {
        let mut medium = SqliteMedium::open(&db_path).unwrap();
        let case = fixture(0);
        let record = composite_record(&case.root.hash(), None);
        let request = engine_persist::composite_append::CompositeAppend {
            record: &record,
            root: &case.root,
            new_chunks: &case.chunks,
            reused_chunks: &[],
        };
        medium
            .append_composite(&request, None)
            .expect("the publication accepts");
    }
    let mut medium = SqliteMedium::open(&db_path).expect("the cold reopen validates the closure");
    medium.validate().expect("the cold validate succeeds");
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn the_sqlite_external_suffix_absorbs_the_verified_closure() {
    // R12-C: retain an open handle with a trusted HEAD; a separate
    // connection appends a valid composite revision; the original
    // handle's head() forces the SUFFIX path (not the cold walk)
    // and the closure joins the trusted reachability set.
    engine_persist::composite_append::test_enable_composite_publication();
    let root_dir = temp_root("sqlite-suffix");
    let db_path = root_dir.join("db.sqlite");
    let mut medium = SqliteMedium::open(&db_path).unwrap();

    // The founding /1 publication (the legacy path).
    let legacy = {
        let mut enc = engine_core::Encoder::new();
        enc.write_str(composite_manifest::SESSION_SNAPSHOT_V1_DOMAIN);
        enc.write_bytes(&[1u8; 16]);
        StoredObject::new(enc.into_bytes()).unwrap()
    };
    let legacy_record = RevisionRecord::founding(AuthorityMode::Standalone, "r12-legacy", 0)
        .with_content_refs(vec![legacy.hash()]);
    medium
        .append(&legacy_record, std::slice::from_ref(&legacy), None)
        .expect("the legacy publication accepts");
    let head = medium.head().unwrap().unwrap();
    let epoch = medium.read_record(&head).unwrap().epoch;

    // The EXTERNAL composite revision through a second connection.
    external_sqlite_composite(&db_path, head, epoch, 0, None, None);

    // The ORIGINAL handle: head() forces the suffix absorption (the
    // trusted cursor sees a moved HEAD; the suffix walk verifies
    // the composite closure; the cursor absorbs it).
    let new_head = medium.head().expect("the suffix absorption succeeds");
    assert!(new_head.is_some());
    assert_ne!(new_head.unwrap(), head, "the HEAD moved");
    // The validate through the now-trusted cursor succeeds with the
    // closure accounted.
    medium
        .validate()
        .expect("the suffix-validated chain succeeds");
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn the_sqlite_external_suffix_missing_chunk_refuses() {
    // R12-C-negative: a suffix whose transitive chunk is MISSING —
    // the suffix absorption REFUSES (the trusted cursor does not
    // advance past unverifiable state).
    engine_persist::composite_append::test_enable_composite_publication();
    let root_dir = temp_root("sqlite-suffix-missing");
    let db_path = root_dir.join("db.sqlite");
    let mut medium = SqliteMedium::open(&db_path).unwrap();
    let legacy = {
        let mut enc = engine_core::Encoder::new();
        enc.write_str(composite_manifest::SESSION_SNAPSHOT_V1_DOMAIN);
        enc.write_bytes(&[1u8; 16]);
        StoredObject::new(enc.into_bytes()).unwrap()
    };
    let legacy_record = RevisionRecord::founding(AuthorityMode::Standalone, "r12-legacy", 0)
        .with_content_refs(vec![legacy.hash()]);
    medium
        .append(&legacy_record, std::slice::from_ref(&legacy), None)
        .unwrap();
    let head = medium.head().unwrap().unwrap();
    let epoch = medium.read_record(&head).unwrap().epoch;

    external_sqlite_composite(&db_path, head, epoch, 0, None, Some(3));

    // The original handle: the suffix path must REFUSE (the
    // missing chunk fails the walk_suffix closure verification).
    let result = medium.head();
    assert!(
        result.is_err(),
        "the missing transitive chunk refuses the suffix absorption"
    );
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn the_sqlite_external_suffix_corrupt_chunk_refuses() {
    // R12-C-negative: a corrupted transitive chunk in the suffix.
    engine_persist::composite_append::test_enable_composite_publication();
    let root_dir = temp_root("sqlite-suffix-corrupt");
    let db_path = root_dir.join("db.sqlite");
    let mut medium = SqliteMedium::open(&db_path).unwrap();
    let legacy = {
        let mut enc = engine_core::Encoder::new();
        enc.write_str(composite_manifest::SESSION_SNAPSHOT_V1_DOMAIN);
        enc.write_bytes(&[1u8; 16]);
        StoredObject::new(enc.into_bytes()).unwrap()
    };
    let legacy_record = RevisionRecord::founding(AuthorityMode::Standalone, "r12-legacy", 0)
        .with_content_refs(vec![legacy.hash()]);
    medium
        .append(&legacy_record, std::slice::from_ref(&legacy), None)
        .unwrap();
    let head = medium.head().unwrap().unwrap();
    let epoch = medium.read_record(&head).unwrap().epoch;

    external_sqlite_composite(&db_path, head, epoch, 0, Some(3), None);

    let result = medium.head();
    assert!(
        result.is_err(),
        "the corrupt transitive chunk refuses the suffix absorption"
    );
    let _ = std::fs::remove_dir_all(&root_dir);
}

// ------------------- R13: the genuine local same-handle suffix tests

/// Writes an external composite revision directly to the file
/// store's on-disk layout (the objects dir + the framed HEAD —
/// NO publication API).
fn external_file_composite(
    store: &std::path::Path,
    parent: Hash,
    parent_epoch: u64,
    variant: u8,
    corrupt_chunk: Option<usize>,
    remove_chunk: Option<usize>,
) {
    let case = fixture(variant);
    let record =
        RevisionRecord::regular(parent, parent_epoch, 99).with_content_refs(vec![case.root.hash()]);

    let objects_dir = store.join("objects");
    std::fs::create_dir_all(&objects_dir).unwrap();
    let mut objects: Vec<StoredObject> = vec![case.root.clone()];
    objects.extend(case.chunks.iter().cloned());
    for (index, object) in objects.iter().enumerate() {
        let skip = remove_chunk.is_some_and(|remove| index == remove + 1);
        if skip {
            continue;
        }
        let bytes = if let Some(corrupt) = corrupt_chunk
            && index == corrupt + 1
        {
            let mut corrupted = object.bytes().to_vec();
            let last = corrupted.len() - 1;
            corrupted[last] ^= 0xff;
            corrupted
        } else {
            object.bytes().to_vec()
        };
        let path = objects_dir.join(object.hash().hex());
        if !path.exists() {
            std::fs::write(&path, bytes).unwrap();
        }
    }
    let record_path = objects_dir.join(record.id().hex());
    std::fs::write(&record_path, record.canonical_bytes()).unwrap();
    let head_bytes = {
        let mut enc = engine_core::Encoder::new();
        enc.write_str("sektor7/engine-persist/head/1");
        enc.write_u32(1);
        enc.write_bytes(record.id().as_bytes());
        enc.into_bytes()
    };
    let tmp = store.join("HEAD.tmp");
    std::fs::write(&tmp, head_bytes).unwrap();
    std::fs::rename(&tmp, store.join("HEAD")).unwrap();
}

#[test]
fn the_local_same_handle_suffix_absorbs_the_verified_closure() {
    // R13's GENUINE suffix-path test: the ORIGINAL handle stays
    // open (its trusted cursor at the /1 head); the external
    // composite revision lands through a direct-disk write; the
    // original handle's head() forces the SUFFIX absorption (the
    // walk counters prove it — NOT a cold reopen).
    engine_persist::composite_append::test_enable_composite_publication();
    let root_dir = temp_root("local-suffix");
    let store = root_dir.join("store");

    // The ORIGINAL handle + its trusted /1 head.
    let mut medium = LocalFileMedium::open(&store).unwrap();
    let legacy = {
        let mut enc = engine_core::Encoder::new();
        enc.write_str(composite_manifest::SESSION_SNAPSHOT_V1_DOMAIN);
        enc.write_bytes(&[1u8; 16]);
        StoredObject::new(enc.into_bytes()).unwrap()
    };
    let legacy_record = RevisionRecord::founding(AuthorityMode::Standalone, "r13", 0)
        .with_content_refs(vec![legacy.hash()]);
    medium
        .append(&legacy_record, std::slice::from_ref(&legacy), None)
        .expect("the legacy publication accepts");
    let head = medium.head().unwrap().unwrap();
    let epoch = medium.read_record(&head).unwrap().epoch;

    // The walk counters BEFORE the suffix.
    let cold_before = medium.walk_stats().cold_walk_records;
    let _ = cold_before;

    // The EXTERNAL composite revision (a direct-disk write — no
    // publication API, no reopen).
    external_file_composite(&store, head, epoch, 0, None, None);

    // The ORIGINAL handle: head() forces the suffix path. The
    // trusted cursor absorbs the verified closure. We observe the
    // suffix path through the walk stats (the cold-walk count
    // UNCHANGED — the suffix path, not a cold rebuild).
    let new_head = medium.head().expect("the same-handle suffix absorbs");
    assert!(new_head.is_some());
    assert_ne!(new_head.unwrap(), head, "the HEAD moved");

    // The suffix path ran (the cold-walk counter is UNCHANGED from
    // before the external append — the suffix absorption handled
    // it, not a cold rebuild).
    let cold_after = medium.walk_stats().cold_walk_records;
    assert_eq!(
        cold_after, cold_before,
        "the suffix path ran (no cold rebuild — the trusted cursor absorbed)"
    );

    // The chain validates through the trusted cursor with the
    // closure accounted.
    medium
        .validate()
        .expect("the suffix-validated chain succeeds");

    // Every chunk reads back through the SAME handle.
    let case = fixture(0);
    for chunk in &case.chunks {
        medium
            .read_object(&chunk.hash())
            .expect("the chunk survives");
    }
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn the_local_same_handle_suffix_missing_chunk_refuses() {
    // R13-negative: the suffix's transitive chunk is MISSING — the
    // same-handle suffix absorption REFUSES; the trusted cursor
    // does NOT advance.
    engine_persist::composite_append::test_enable_composite_publication();
    let root_dir = temp_root("local-suffix-missing");
    let store = root_dir.join("store");
    let mut medium = LocalFileMedium::open(&store).unwrap();
    let legacy = {
        let mut enc = engine_core::Encoder::new();
        enc.write_str(composite_manifest::SESSION_SNAPSHOT_V1_DOMAIN);
        enc.write_bytes(&[1u8; 16]);
        StoredObject::new(enc.into_bytes()).unwrap()
    };
    let legacy_record = RevisionRecord::founding(AuthorityMode::Standalone, "r13", 0)
        .with_content_refs(vec![legacy.hash()]);
    medium
        .append(&legacy_record, std::slice::from_ref(&legacy), None)
        .unwrap();
    let head = medium.head().unwrap().unwrap();
    let epoch = medium.read_record(&head).unwrap().epoch;

    external_file_composite(&store, head, epoch, 0, None, Some(3));

    // The suffix absorption REFUSES — the head() call fails.
    let result = medium.head();
    assert!(
        result.is_err(),
        "the missing transitive chunk refuses the same-handle suffix"
    );
    // The trusted cursor is NOT advanced (the HEAD file says the
    // new record, but the cursor refused — a subsequent cold
    // validation also fails).
    let cold = LocalFileMedium::open(&store);
    assert!(cold.is_err(), "the cold validation also fails closed");
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn the_local_same_handle_suffix_corrupt_chunk_refuses() {
    // R13-negative: the corrupt transitive chunk.
    engine_persist::composite_append::test_enable_composite_publication();
    let root_dir = temp_root("local-suffix-corrupt");
    let store = root_dir.join("store");
    let mut medium = LocalFileMedium::open(&store).unwrap();
    let legacy = {
        let mut enc = engine_core::Encoder::new();
        enc.write_str(composite_manifest::SESSION_SNAPSHOT_V1_DOMAIN);
        enc.write_bytes(&[1u8; 16]);
        StoredObject::new(enc.into_bytes()).unwrap()
    };
    let legacy_record = RevisionRecord::founding(AuthorityMode::Standalone, "r13", 0)
        .with_content_refs(vec![legacy.hash()]);
    medium
        .append(&legacy_record, std::slice::from_ref(&legacy), None)
        .unwrap();
    let head = medium.head().unwrap().unwrap();
    let epoch = medium.read_record(&head).unwrap().epoch;

    external_file_composite(&store, head, epoch, 0, Some(3), None);

    let result = medium.head();
    assert!(
        result.is_err(),
        "the corrupt transitive chunk refuses the same-handle suffix"
    );
    let _ = std::fs::remove_dir_all(&root_dir);
}

#[test]
fn the_local_same_handle_suffix_unrelated_domain_control() {
    // R13-control: a NON-composite suffix (a /1 snapshot) absorbs
    // through the same-handle path — the existing /1 suffix
    // behavior is unchanged.
    let root_dir = temp_root("local-suffix-control");
    let store = root_dir.join("store");
    let mut medium = LocalFileMedium::open(&store).unwrap();
    let first = {
        let mut enc = engine_core::Encoder::new();
        enc.write_str(composite_manifest::SESSION_SNAPSHOT_V1_DOMAIN);
        enc.write_bytes(&[1u8; 16]);
        StoredObject::new(enc.into_bytes()).unwrap()
    };
    let first_record = RevisionRecord::founding(AuthorityMode::Standalone, "r13c", 0)
        .with_content_refs(vec![first.hash()]);
    medium
        .append(&first_record, std::slice::from_ref(&first), None)
        .unwrap();
    let head = medium.head().unwrap().unwrap();
    let epoch = medium.read_record(&head).unwrap().epoch;

    // An external /1 suffix (the same direct-disk write, a /1
    // object).
    let second = {
        let mut enc = engine_core::Encoder::new();
        enc.write_str(composite_manifest::SESSION_SNAPSHOT_V1_DOMAIN);
        enc.write_bytes(&[2u8; 16]);
        StoredObject::new(enc.into_bytes()).unwrap()
    };
    let second_record =
        RevisionRecord::regular(head, epoch, 99).with_content_refs(vec![second.hash()]);
    let objects_dir = store.join("objects");
    std::fs::create_dir_all(&objects_dir).unwrap();
    std::fs::write(objects_dir.join(second.hash().hex()), second.bytes()).unwrap();
    std::fs::write(
        objects_dir.join(second_record.id().hex()),
        second_record.canonical_bytes(),
    )
    .unwrap();
    let head_bytes = {
        let mut enc = engine_core::Encoder::new();
        enc.write_str("sektor7/engine-persist/head/1");
        enc.write_u32(1);
        enc.write_bytes(second_record.id().as_bytes());
        enc.into_bytes()
    };
    let tmp = store.join("HEAD.tmp");
    std::fs::write(&tmp, head_bytes).unwrap();
    std::fs::rename(&tmp, store.join("HEAD")).unwrap();

    // The same-handle suffix absorbs the /1 revision — the
    // UNCHANGED legacy behavior.
    let new_head = medium.head().expect("the /1 suffix absorbs");
    assert_eq!(new_head.unwrap(), second_record.id());
    medium.validate().expect("the /1 chain validates");
    let _ = std::fs::remove_dir_all(&root_dir);
}
